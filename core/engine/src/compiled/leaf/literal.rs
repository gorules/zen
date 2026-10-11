use zen_expression::lane::{LaneProgram, LaneRunner};
use zen_expression::{Scope, Variable};

#[derive(Debug, Clone)]
pub(crate) enum Literal {
    Null,
    Bool(bool),
    Num(rust_decimal::Decimal),
    Str(std::sync::Arc<str>),
    Array(Vec<Literal>),
    Object(Vec<(std::sync::Arc<str>, Literal)>),
}

impl Literal {
    pub fn constant(runner: &mut LaneRunner, program: &LaneProgram) -> Option<Literal> {
        let p = program.program();
        if !p.site_keys.is_empty() || p.opaque() || p.writes_env || p.chain || !p.timeless() {
            return None;
        }
        match runner.evaluate_one(program, &Scope::default()).ok()? {
            Variable::Null => Some(Literal::Null),
            Variable::Bool(b) => Some(Literal::Bool(b)),
            Variable::Number(n) => Some(Literal::Num(n)),
            Variable::String(s) => Some(Literal::Str(std::sync::Arc::from(&*s))),
            _ => None,
        }
    }

    pub fn variable(&self) -> Variable {
        match self {
            Literal::Null => Variable::Null,
            Literal::Bool(b) => Variable::Bool(*b),
            Literal::Num(n) => Variable::Number(*n),
            Literal::Str(s) => Variable::String(s.as_ref().into()),
            Literal::Array(items) => Variable::from_array(items.iter().map(Literal::variable).collect()),
            Literal::Object(fields) => {
                let mut map = zen_types::variable::VariableMap::with_capacity(fields.len());
                for (key, value) in fields {
                    map.insert(key.as_ref().into(), value.variable());
                }
                Variable::from_object(map)
            }
        }
    }

    pub fn deep(&self) -> bool {
        match self {
            Literal::Object(_) => true,
            Literal::Array(items) => items.iter().any(Literal::deep),
            _ => false,
        }
    }

    pub fn scaled(&self) -> Option<(i64, u8)> {
        match self {
            Literal::Num(n) => Some((i64::try_from(n.mantissa()).ok()?, u8::try_from(n.scale()).ok()?)),
            _ => None,
        }
    }

    pub fn of(value: &Variable) -> Option<Literal> {
        match value {
            Variable::Null => Some(Literal::Null),
            Variable::Bool(b) => Some(Literal::Bool(*b)),
            Variable::Number(n) => Some(Literal::Num(*n)),
            Variable::String(s) => Some(Literal::Str(std::sync::Arc::from(s.as_ref() as &str))),
            Variable::Array(items) => items.borrow().iter().map(Literal::of).collect::<Option<Vec<_>>>().map(Literal::Array),
            Variable::Object(fields) => fields
                .borrow()
                .iter()
                .map(|(key, value)| Some((std::sync::Arc::from(key.as_ref() as &str), Literal::of(value)?)))
                .collect::<Option<Vec<_>>>()
                .map(Literal::Object),
            _ => None,
        }
    }
}
