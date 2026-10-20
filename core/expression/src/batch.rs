use crate::compiler::Opcode;
use crate::lane::{Columns, LaneProgram, LaneRunner};
use crate::vm::VM;
use crate::{ExpressionKind, Isolate, IsolateError, Scope, Variable};
use std::sync::Arc;

#[derive(Debug, Clone)]
enum Engine {
    Lane(LaneProgram),
    Stack(Arc<[Opcode]>),
}

#[derive(Debug, Clone)]
pub struct BatchExpression {
    kind: ExpressionKind,
    engine: Engine,
}

impl BatchExpression {
    const SAMPLE: usize = 64;

    pub fn standard(source: &str) -> Result<Self, IsolateError> {
        Self::compile(source, ExpressionKind::Standard)
    }

    pub fn unary(source: &str) -> Result<Self, IsolateError> {
        Self::compile(source, ExpressionKind::Unary)
    }

    pub fn compile(source: &str, kind: ExpressionKind) -> Result<Self, IsolateError> {
        let engine = match LaneProgram::compile(source, kind.clone()) {
            Ok(program) => Engine::Lane(program),
            Err(_) => Engine::Stack(Self::bytecode(source, &kind)?),
        };
        Ok(Self { kind, engine })
    }

    fn bytecode(source: &str, kind: &ExpressionKind) -> Result<Arc<[Opcode]>, IsolateError> {
        let mut isolate = Isolate::new();
        Ok(match kind {
            ExpressionKind::Standard => isolate.compile_standard(source)?.bytecode().clone(),
            ExpressionKind::Unary => isolate.compile_unary(source)?.bytecode().clone(),
        })
    }

    pub fn is_lane(&self) -> bool {
        matches!(self.engine, Engine::Lane(_))
    }

    pub fn evaluate(&self, inputs: &[Variable]) -> Vec<Result<Variable, IsolateError>> {
        let scopes: Vec<Scope> = inputs.iter().map(|input| Scope::new(input.clone())).collect();
        self.evaluate_scopes(&scopes)
    }

    pub fn evaluate_scopes(&self, scopes: &[Scope]) -> Vec<Result<Variable, IsolateError>> {
        match &self.engine {
            Engine::Lane(program) => {
                let sample = &scopes[..scopes.len().min(Self::SAMPLE)];
                let specialized = match scopes.len() > Self::SAMPLE {
                    true => program.specialize(sample).ok(),
                    false => None,
                };
                let mut runner = LaneRunner::new();
                runner.evaluate(specialized.as_ref().unwrap_or(program), scopes)
            }
            Engine::Stack(bytecode) => {
                let mut vm = VM::new();
                scopes
                    .iter()
                    .map(|scope| self.stack(bytecode, scope, &mut vm))
                    .collect()
            }
        }
    }

    pub fn evaluate_columns(&self, columns: &Columns) -> Vec<Result<Variable, IsolateError>> {
        match &self.engine {
            Engine::Lane(program) => {
                let specialized = program.specialize_columns(columns).ok();
                let mut out: Vec<Option<Result<Variable, IsolateError>>> =
                    (0..columns.rows).map(|_| None).collect();
                LaneRunner::new().evaluate_columns(
                    specialized.as_ref().unwrap_or(program),
                    columns,
                    |row, result| out[row] = Some(result),
                );
                out.into_iter()
                    .map(|result| result.unwrap_or(Ok(Variable::Null)))
                    .collect()
            }
            Engine::Stack(_) => {
                let scopes: Vec<Scope> =
                    (0..columns.rows).map(|row| Scope::new(columns.row(row))).collect();
                self.evaluate_scopes(&scopes)
            }
        }
    }

    fn stack(
        &self,
        bytecode: &Arc<[Opcode]>,
        scope: &Scope,
        vm: &mut VM,
    ) -> Result<Variable, IsolateError> {
        let result = vm.run(bytecode.as_ref(), scope)?;
        match self.kind {
            ExpressionKind::Standard => Ok(result),
            ExpressionKind::Unary => result
                .as_bool()
                .map(Variable::Bool)
                .ok_or(IsolateError::ValueCastError),
        }
    }
}

pub fn evaluate_expression_batch(
    source: &str,
    inputs: &[Variable],
) -> Result<Vec<Result<Variable, IsolateError>>, IsolateError> {
    Ok(BatchExpression::standard(source)?.evaluate(inputs))
}
