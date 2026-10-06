use crate::compiler::{Compare, FetchFastTarget};
use crate::functions::{ClosureFunction, FunctionKind, MethodKind};
use crate::lexer::Bracket;
use crate::variable::Variable;
use crate::vm::VMError;
use rust_decimal::Decimal;
use std::sync::Arc;
use zen_types::symbol::Symbol;

pub type Reg = u16;
pub type MaskId = u16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Dyn,
    Num,
    Bool,
    Str,
    Date,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    Column(usize),
    Absent,
    Row,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Operand {
    Reg(Reg),
    Num(Decimal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumCmp {
    Order(Compare),
    Equal,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Null,
    Bool(bool),
    Number(Decimal),
    String(Arc<str>),
    Array(Vec<Const>),
    Date(Option<chrono::DateTime<chrono_tz::Tz>>),
}

impl Const {
    pub fn same(&self, other: &Const) -> bool {
        match (self, other) {
            (Const::Number(a), Const::Number(b)) => a.serialize() == b.serialize(),
            (Const::Date(a), Const::Date(b)) => {
                let key = |d: chrono::DateTime<chrono_tz::Tz>| {
                    (d.timestamp(), d.timestamp_subsec_nanos(), d.timezone())
                };
                a.map(key) == b.map(key)
            }
            (Const::Array(a), Const::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same(y))
            }
            (a, b) => a == b,
        }
    }

    pub fn variable(&self) -> Variable {
        match self {
            Const::Null => Variable::Null,
            Const::Bool(b) => Variable::Bool(*b),
            Const::Number(n) => Variable::Number(*n),
            Const::String(s) => Variable::String(Symbol::from(s.as_ref())),
            Const::Array(items) => {
                Variable::from_array(items.iter().map(Const::variable).collect())
            }
            Const::Date(d) => crate::lane::date::Date(*d).variable(),
        }
    }

    pub fn number(node: &crate::parser::Node) -> Option<Decimal> {
        use crate::lexer::{ArithmeticOperator, Operator};
        use crate::parser::Node;
        match node {
            Node::Number(n) => Some(*n),
            Node::Parenthesized(inner) => Const::number(inner),
            Node::Unary {
                node,
                operator: Operator::Arithmetic(ArithmeticOperator::Subtract),
            } => Const::number(node).map(|n| -n),
            Node::Unary {
                node,
                operator: Operator::Arithmetic(ArithmeticOperator::Add),
            } => Const::number(node),
            _ => None,
        }
    }

    pub fn of(node: &crate::parser::Node) -> Option<Const> {
        use crate::parser::Node;
        if let Some(n) = Const::number(node) {
            return Some(Const::Number(n));
        }
        match node {
            Node::Null => Some(Const::Null),
            Node::Bool(b) => Some(Const::Bool(*b)),
            Node::String(s) => Some(Const::String(Arc::from(*s))),
            Node::Array(items) => items
                .iter()
                .map(|n| Const::of(n))
                .collect::<Option<Vec<_>>>()
                .map(Const::Array),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Exponent,
    Equal,
    In,
    Compare(Compare),
}

#[derive(Debug, Clone)]
pub enum Op {
    Const {
        dst: Reg,
        id: u16,
    },
    Env {
        dst: Reg,
        key: Arc<str>,
        site: u16,
    },
    RootEnv {
        dst: Reg,
    },
    Path {
        dst: Reg,
        path: Box<[FetchFastTarget]>,
        site: u16,
    },
    Fetch {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Negate {
        dst: Reg,
        a: Reg,
    },
    Not {
        dst: Reg,
        a: Reg,
    },
    Binary {
        dst: Reg,
        op: Binary,
        a: Reg,
        b: Reg,
    },
    Interval {
        dst: Reg,
        a: Reg,
        b: Reg,
        left: Bracket,
        right: Bracket,
    },
    InRange {
        dst: Reg,
        a: Reg,
        lo: Decimal,
        hi: Decimal,
        left: Bracket,
        right: Bracket,
    },
    Slice {
        dst: Reg,
        a: Reg,
        to: Reg,
        from: Reg,
    },
    Len {
        dst: Reg,
        a: Reg,
    },
    Array {
        dst: Reg,
        items: Box<[Reg]>,
    },
    Object {
        dst: Reg,
        pairs: Box<[(ObjectKey, Reg)]>,
    },
    Join {
        dst: Reg,
        parts: Box<[Input]>,
    },
    Concat {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Extreme {
        dst: Reg,
        items: Box<[Operand]>,
        largest: bool,
    },
    Call {
        dst: Reg,
        kind: FunctionKind,
        args: Box<[Input]>,
    },
    Method {
        dst: Reg,
        kind: MethodKind,
        args: Box<[Input]>,
    },
    Branch {
        cond: Reg,
        opcode: &'static str,
        on_true: MaskId,
        on_false: MaskId,
    },
    NullBranch {
        a: Reg,
        null: MaskId,
        other: MaskId,
    },
    Merge {
        dst: Reg,
        mask: MaskId,
        a: Reg,
        b: Reg,
    },
    Move {
        dst: Reg,
        src: Reg,
    },
    AssignBegin {
        dst: Reg,
    },
    AssignStep {
        object: Reg,
        key: Reg,
        value: Reg,
    },
    Closure(Box<ClosureOp>),
    Fail {
        error: VMError,
    },
    Num {
        dst: Reg,
        op: NumOp,
        a: Operand,
        b: Operand,
    },
    Cmp {
        dst: Reg,
        op: NumCmp,
        a: Operand,
        b: Operand,
    },
    EqConst {
        dst: Reg,
        a: Reg,
        value: Const,
        id: u16,
        not: bool,
    },
    Field {
        dst: Reg,
        src: Reg,
        key: Arc<str>,
        site: u16,
    },
    LoadEq {
        dst: Reg,
        load: Load,
        site: u16,
        id: u16,
        not: bool,
    },
    LoadIn {
        dst: Reg,
        a: Reg,
        load: Load,
        site: u16,
    },
    LoadCall(Box<LoadCall>),
    EqAny {
        dst: Reg,
        a: Reg,
        id: u16,
    },
    InConst {
        dst: Reg,
        a: Reg,
        id: u16,
    },
    Coalesce {
        dst: Reg,
        a: Reg,
        id: u16,
    },
    SelectConst {
        dst: Reg,
        cond: Reg,
        a: u16,
        b: u16,
    },
    Stage {
        index: u16,
    },
    Rewind,
    DollarInsert {
        key: Arc<str>,
        value: Reg,
    },
}

#[derive(Debug, Clone)]
pub struct LoadCall {
    pub dst: Reg,
    pub load: Load,
    pub site: u16,
    pub kind: FunctionKind,
    pub args: Box<[u16]>,
    pub memo: u16,
    pub scratch: Reg,
    pub call: Op,
}

#[derive(Debug, Clone, Copy)]
pub enum Input {
    Reg(Reg),
    Const(u16),
}

#[derive(Debug, Clone)]
pub enum ObjectKey {
    Static(Arc<str>),
    Reg(Reg),
}

#[derive(Debug, Clone)]
pub enum Load {
    Env(Arc<str>),
    Path(Box<[FetchFastTarget]>),
}

#[derive(Debug, Clone)]
pub struct ClosureOp {
    pub kind: ClosureFunction,
    pub dst: Reg,
    pub list: Reg,
    pub source: Option<(Load, u16)>,
    pub body: Program,
    pub imports: Box<[(Reg, Reg)]>,
    pub sequential: bool,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub mask: MaskId,
    pub op: Op,
}

#[derive(Debug, Clone, Default)]
pub struct Program {
    pub steps: Vec<Step>,
    pub regs: u16,
    pub masks: u16,
    pub out: Reg,
    pub element: Option<Reg>,
    pub writes_env: bool,
    pub chain: bool,
    pub frames: usize,
    pub kinds: Vec<Kind>,
    pub sites: u16,
    pub id: u64,
    pub pinned: Vec<bool>,
    pub fixed: Vec<bool>,
    pub keys: Vec<String>,
    pub site_keys: Vec<Option<String>>,
    pub rows: bool,
    pub consts: Vec<Const>,
    pub outputs: Vec<Reg>,
    pub memos: u16,
    pub layout: Option<Layout>,
    pub isolated: bool,
}

#[derive(Debug, Clone)]
pub enum Layout {
    Value(Reg),
    Struct(Box<[(Arc<str>, Layout)]>),
    List(Box<[Layout]>),
}

impl Program {
    pub fn finish(&mut self, sites: u16) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.frames = self.depth() + 1;
        self.sites = sites;
        self.id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for step in &mut self.steps {
            if let Op::Closure(c) = &mut step.op {
                c.body.finish(sites);
            }
        }
    }

    pub fn pure(&self) -> bool {
        self.steps.iter().all(|step| {
            !matches!(
                step.op,
                Op::Env { .. }
                    | Op::Path { .. }
                    | Op::RootEnv { .. }
                    | Op::LoadEq { .. }
                    | Op::LoadIn { .. }
                    | Op::LoadCall(_)
                    | Op::AssignBegin { .. }
                    | Op::AssignStep { .. }
                    | Op::Closure(_)
                    | Op::Rewind
                    | Op::DollarInsert { .. }
            )
        })
    }

    pub fn opaque(&self) -> bool {
        self.opaque_with(&self.site_keys)
    }

    pub fn stable(&self) -> bool {
        use crate::functions::InternalFunction;
        let random = |kind: &FunctionKind| matches!(kind, FunctionKind::Internal(InternalFunction::Rand));
        self.steps.iter().all(|step| match &step.op {
            Op::Call { kind, .. } => !random(kind),
            Op::LoadCall(call) => !random(&call.kind),
            Op::Closure(closure) => closure.body.stable(),
            _ => true,
        })
    }

    pub fn timeless(&self) -> bool {
        use crate::functions::{DateMethod, DeprecatedFunction, InternalFunction, MethodKind};
        let clocked = |kind: &FunctionKind| {
            matches!(
                kind,
                FunctionKind::Internal(InternalFunction::Rand | InternalFunction::Date)
                    | FunctionKind::Deprecated(DeprecatedFunction::Date | DeprecatedFunction::Time | DeprecatedFunction::DateString)
            )
        };
        self.steps.iter().all(|step| match &step.op {
            Op::Call { kind, .. } => !clocked(kind),
            Op::LoadCall(call) => !clocked(&call.kind),
            Op::Method { kind, .. } => !matches!(
                kind,
                MethodKind::DateMethod(DateMethod::IsToday | DateMethod::IsYesterday | DateMethod::IsTomorrow)
            ),
            Op::Closure(closure) => closure.body.timeless(),
            _ => true,
        })
    }

    fn opaque_with(&self, keys: &[Option<String>]) -> bool {
        let unkeyed = |site: u16| keys.get(site as usize).is_none_or(Option::is_none);
        self.steps.iter().any(|step| match &step.op {
            Op::Env { site, .. } | Op::Path { site, .. } | Op::LoadEq { site, .. } | Op::LoadIn { site, .. } => {
                unkeyed(*site)
            }
            Op::LoadCall(c) => unkeyed(c.site),
            Op::RootEnv { .. } | Op::AssignBegin { .. } | Op::AssignStep { .. } | Op::Rewind | Op::DollarInsert { .. } => true,
            Op::Closure(c) => c.source.as_ref().is_some_and(|(_, site)| unkeyed(*site)) || c.body.opaque_with(keys),
            _ => false,
        })
    }

    pub fn needs_rows(&self, bound: &[Binding]) -> bool {
        self.steps
            .iter()
            .any(|step| Self::op_needs_rows(&step.op, bound))
    }

    fn op_needs_rows(op: &Op, bound: &[Binding]) -> bool {
        match op {
            Op::Env { site, .. }
            | Op::Path { site, .. }
            | Op::LoadEq { site, .. }
            | Op::LoadIn { site, .. } => {
                matches!(bound.get(*site as usize), None | Some(Binding::Row))
            }
            Op::RootEnv { .. }
            | Op::AssignBegin { .. }
            | Op::AssignStep { .. }
            | Op::Rewind
            | Op::DollarInsert { .. } => true,
            Op::Closure(c) => {
                c.source.as_ref().is_some_and(|(_, site)| {
                    matches!(bound.get(*site as usize), None | Some(Binding::Row))
                }) || c.body.needs_rows(bound)
            }
            Op::LoadCall(c) => matches!(bound.get(c.site as usize), None | Some(Binding::Row)),
            _ => false,
        }
    }

    pub fn depth(&self) -> usize {
        self.steps
            .iter()
            .filter_map(|s| match &s.op {
                Op::Closure(c) => Some(1 + c.body.depth()),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }
}
