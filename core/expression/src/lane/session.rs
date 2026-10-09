use crate::lane::{LaneProgram, LaneRunner};
use crate::scope::Scope;
use crate::variable::Variable;
use crate::{ExpressionKind, IsolateError};
use ahash::HashMap;
use std::cell::RefCell;
use std::sync::Arc;
use zen_types::symbol::Symbol;

#[derive(Debug, Clone, Default)]
pub struct Programs {
    standard: HashMap<Arc<str>, Option<LaneProgram>>,
    unary: HashMap<Arc<str>, Option<LaneProgram>>,
}

impl Programs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compile<'s>(sources: impl IntoIterator<Item = (&'s str, ExpressionKind)>) -> Self {
        let mut programs = Self::new();
        for (source, kind) in sources {
            programs.insert(source, kind);
        }
        programs
    }

    pub fn insert(&mut self, source: &str, kind: ExpressionKind) -> Option<&LaneProgram> {
        let map = match kind {
            ExpressionKind::Standard => &mut self.standard,
            ExpressionKind::Unary => &mut self.unary,
        };
        if !map.contains_key(source) {
            let program = LaneProgram::compile_exact(source, kind).ok();
            map.insert(Arc::from(source), program);
        }
        map.get(source).and_then(Option::as_ref)
    }

    pub fn get(&self, source: &str, kind: &ExpressionKind) -> Option<Option<&LaneProgram>> {
        let map = match kind {
            ExpressionKind::Standard => &self.standard,
            ExpressionKind::Unary => &self.unary,
        };
        map.get(source).map(Option::as_ref)
    }

    pub fn len(&self) -> usize {
        self.standard.len() + self.unary.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

}

#[derive(Debug, Default)]
pub struct Session {
    scope: Scope,
    references: HashMap<String, Variable>,
    programs: Option<Arc<Programs>>,
    fallback: Programs,
}

impl Session {
    thread_local! {
        static RUNNER: RefCell<LaneRunner> = RefCell::new(LaneRunner::new());
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_environment(variable: Variable) -> Self {
        let mut session = Self::new();
        session.set_environment(variable);
        session
    }

    pub fn with_programs(mut self, programs: Option<Arc<Programs>>) -> Self {
        self.programs = programs;
        self
    }

    pub fn set_programs(&mut self, programs: Arc<Programs>) {
        self.programs = Some(programs);
    }

    pub fn set_environment(&mut self, variable: Variable) {
        self.scope.set_base(variable);
        self.references.clear();
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn set_local(&mut self, name: Symbol, value: Variable) {
        self.scope.set_local(name, value);
    }

    pub fn insert_dollar(&mut self, path: &str, value: Variable) {
        let dollar = match self.scope.local(&Variable::dollar_key()) {
            Some(existing @ Variable::Object(_)) => existing.shallow_clone(),
            _ => {
                let created = Variable::empty_object();
                self.scope.set_local(Variable::dollar_key(), created.clone());
                created
            }
        };

        let _ = dollar.dot_insert(path, value);
    }

    pub fn set_reference(&mut self, reference: &str) -> Result<(), IsolateError> {
        let value = match self.references.get(reference) {
            Some(value) => value.clone(),
            None => {
                let result = self.run_standard(reference)?;
                self.references.insert(reference.to_string(), result.clone());
                result
            }
        };

        self.set_reference_value(value)
    }

    pub fn set_reference_value(&mut self, value: Variable) -> Result<(), IsolateError> {
        self.scope.set_local(Variable::dollar_key(), value);

        Ok(())
    }

    pub fn get_reference(&self, reference: &str) -> Option<Variable> {
        self.references.get(reference).cloned()
    }

    pub fn run_standard(&mut self, source: &str) -> Result<Variable, IsolateError> {
        self.run(source, ExpressionKind::Standard)
    }

    pub fn run_unary(&mut self, source: &str) -> Result<bool, IsolateError> {
        self.run(source, ExpressionKind::Unary)?
            .as_bool()
            .ok_or(IsolateError::ValueCastError)
    }

    fn run(&mut self, source: &str, kind: ExpressionKind) -> Result<Variable, IsolateError> {
        let program = match self.programs.as_deref().and_then(|p| p.get(source, &kind)) {
            Some(program) => program,
            None => self.fallback.insert(source, kind.clone()),
        };
        match program {
            Some(program) => Self::evaluate(program, &self.scope),
            None => Self::evaluate(&LaneProgram::compile_exact(source, kind)?, &self.scope),
        }
    }

    fn evaluate(program: &LaneProgram, scope: &Scope) -> Result<Variable, IsolateError> {
        let mut runner = Self::RUNNER.take();
        let result = runner.evaluate_one(program, scope);
        Self::RUNNER.set(runner);
        result
    }
}
