use crate::context::EvaluationContext;
use crate::lane::{Columns, LaneProgram, LaneRunner};
use crate::{ExpressionKind, IsolateError, Scope, Variable};
use std::cell::RefCell;

#[derive(Debug, Clone)]
pub struct BatchExpression {
    program: LaneProgram,
}

thread_local! {
    static RUNNER: RefCell<LaneRunner> = RefCell::new(LaneRunner::new());
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
        Ok(Self {
            program: LaneProgram::compile(source, kind)?,
        })
    }

    pub fn program(&self) -> &LaneProgram {
        &self.program
    }

    pub fn evaluate(&self, inputs: &[Variable]) -> Vec<Result<Variable, IsolateError>> {
        self.evaluate_with(inputs, &EvaluationContext::default())
    }

    pub fn evaluate_with(
        &self,
        inputs: &[Variable],
        context: &EvaluationContext,
    ) -> Vec<Result<Variable, IsolateError>> {
        let scopes: Vec<Scope> = inputs
            .iter()
            .map(|input| Scope::new(input.clone()))
            .collect();
        self.evaluate_scopes(&scopes, context)
    }

    pub fn evaluate_scopes(
        &self,
        scopes: &[Scope],
        context: &EvaluationContext,
    ) -> Vec<Result<Variable, IsolateError>> {
        let specialized = match scopes.len() > Self::SAMPLE {
            true => self.program.specialize(&scopes[..Self::SAMPLE]).ok(),
            false => None,
        };
        let program = specialized.as_ref().unwrap_or(&self.program);
        RUNNER.with_borrow_mut(|runner| runner.evaluate(program, context, scopes))
    }

    pub fn evaluate_columns(
        &self,
        columns: &Columns,
        context: &EvaluationContext,
    ) -> Vec<Result<Variable, IsolateError>> {
        let specialized = self.program.specialize_columns(columns).ok();
        let program = specialized.as_ref().unwrap_or(&self.program);
        let mut out: Vec<Option<Result<Variable, IsolateError>>> =
            (0..columns.rows).map(|_| None).collect();
        RUNNER.with_borrow_mut(|runner| {
            runner.evaluate_columns(program, context, columns, |row, result| {
                out[row] = Some(result)
            })
        });
        out.into_iter()
            .map(|result| result.unwrap_or(Ok(Variable::Null)))
            .collect()
    }
}

pub fn evaluate_expression_batch(
    source: &str,
    inputs: &[Variable],
) -> Result<Vec<Result<Variable, IsolateError>>, IsolateError> {
    Ok(BatchExpression::standard(source)?.evaluate(inputs))
}
