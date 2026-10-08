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
        nested: bool,
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
    Fold(Box<FoldOp>),
    Switch(Box<SwitchOp>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Reg(Reg),
    Site(u16),
}

#[derive(Debug, Clone)]
pub struct SwitchOp {
    pub dst: Reg,
    pub subject: Subject,
    pub cases: Box<[Arc<str>]>,
    pub values: Box<[u16]>,
    pub rest: MaskId,
}

#[derive(Debug, Clone)]
pub struct FoldOp {
    pub dst: Reg,
    pub function: crate::functions::InternalFunction,
    pub site: u16,
    pub filter: Option<Arc<str>>,
    pub field: Arc<str>,
    pub rest: MaskId,
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
    pub select: bool,
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

impl Op {
    pub(crate) fn dst(&self) -> Option<Reg> {
        match self {
            Op::Const { dst, .. }
            | Op::Env { dst, .. }
            | Op::RootEnv { dst }
            | Op::Path { dst, .. }
            | Op::Fetch { dst, .. }
            | Op::Negate { dst, .. }
            | Op::Not { dst, .. }
            | Op::Binary { dst, .. }
            | Op::Interval { dst, .. }
            | Op::InRange { dst, .. }
            | Op::Slice { dst, .. }
            | Op::Len { dst, .. }
            | Op::Array { dst, .. }
            | Op::Object { dst, .. }
            | Op::Join { dst, .. }
            | Op::Concat { dst, .. }
            | Op::Extreme { dst, .. }
            | Op::Call { dst, .. }
            | Op::Method { dst, .. }
            | Op::Merge { dst, .. }
            | Op::Move { dst, .. }
            | Op::AssignBegin { dst }
            | Op::Num { dst, .. }
            | Op::Cmp { dst, .. }
            | Op::EqConst { dst, .. }
            | Op::Field { dst, .. }
            | Op::LoadEq { dst, .. }
            | Op::LoadIn { dst, .. }
            | Op::EqAny { dst, .. }
            | Op::InConst { dst, .. }
            | Op::Coalesce { dst, .. }
            | Op::SelectConst { dst, .. } => Some(*dst),
            Op::Closure(c) => Some(c.dst),
            Op::Fold(fold) => Some(fold.dst),
            Op::Switch(switch) => Some(switch.dst),
            Op::LoadCall(call) => Some(call.dst),
            Op::Branch { .. }
            | Op::NullBranch { .. }
            | Op::AssignStep { .. }
            | Op::Fail { .. }
            | Op::Stage { .. }
            | Op::Rewind
            | Op::DollarInsert { .. } => None,
        }
    }

    pub(crate) fn reads(&self, reg: Reg) -> bool {
        let input = |i: &Input| matches!(i, Input::Reg(r) if *r == reg);
        let operand = |o: &Operand| matches!(o, Operand::Reg(r) if *r == reg);
        match self {
            Op::Const { .. }
            | Op::Env { .. }
            | Op::RootEnv { .. }
            | Op::Path { .. }
            | Op::LoadEq { .. }
            | Op::Fail { .. }
            | Op::Stage { .. }
            | Op::Rewind
            | Op::Fold(_)
            | Op::AssignBegin { .. } => false,
            Op::Fetch { a, b, .. } | Op::Binary { a, b, .. } | Op::Interval { a, b, .. } | Op::Concat { a, b, .. } | Op::Merge { a, b, .. } => {
                *a == reg || *b == reg
            }
            Op::Negate { a, .. }
            | Op::Not { a, .. }
            | Op::InRange { a, .. }
            | Op::Len { a, .. }
            | Op::NullBranch { a, .. }
            | Op::EqConst { a, .. }
            | Op::LoadIn { a, .. }
            | Op::EqAny { a, .. }
            | Op::InConst { a, .. }
            | Op::Coalesce { a, .. } => *a == reg,
            Op::Slice { a, to, from, .. } => *a == reg || *to == reg || *from == reg,
            Op::Array { items, .. } => items.contains(&reg),
            Op::Object { pairs, .. } => pairs
                .iter()
                .any(|(k, v)| *v == reg || matches!(k, ObjectKey::Reg(r) if *r == reg)),
            Op::Join { parts, .. } => parts.iter().any(input),
            Op::Extreme { items, .. } => items.iter().any(operand),
            Op::Call { args, .. } | Op::Method { args, .. } => args.iter().any(input),
            Op::Branch { cond, .. } | Op::SelectConst { cond, .. } => *cond == reg,
            Op::Move { src, .. } | Op::Field { src, .. } => *src == reg,
            Op::AssignStep { object, key, value } => *object == reg || *key == reg || *value == reg,
            Op::Closure(c) => c.list == reg || c.imports.iter().any(|(from, _)| *from == reg),
            Op::Switch(switch) => switch.subject == Subject::Reg(reg),
            Op::Num { a, b, .. } | Op::Cmp { a, b, .. } => operand(a) || operand(b),
            Op::LoadCall(call) => call.args.contains(&reg) || call.scratch == reg || call.call.reads(reg),
            Op::DollarInsert { value, .. } => *value == reg,
        }
    }
}

impl Program {
    pub(crate) fn whole(&self, reg: Reg) -> bool {
        self.out == reg
            || self.outputs.contains(&reg)
            || self
                .steps
                .iter()
                .any(|step| !matches!(&step.op, Op::Field { src, .. } if *src == reg) && step.op.reads(reg))
    }

    pub fn finish(&mut self, sites: u16) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.frames = self.depth() + 1;
        self.sites = sites;
        self.id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let flags: Vec<bool> = self
            .steps
            .iter()
            .enumerate()
            .map(|(at, step)| match &step.op {
                Op::Closure(c) => matches!(c.kind, ClosureFunction::Filter) && self.selects(at, c.dst),
                Op::Field { dst, .. } => self.fields_only(at, *dst),
                _ => false,
            })
            .collect();
        for (step, flag) in self.steps.iter_mut().zip(flags) {
            match &mut step.op {
                Op::Closure(c) => {
                    c.select = flag;
                    c.body.finish(sites);
                }
                Op::Field { nested, .. } => *nested = flag,
                _ => {}
            }
        }
    }

    fn fields_only(&self, at: usize, reg: Reg) -> bool {
        let mask = self.steps[at].mask;
        for step in &self.steps[at + 1..] {
            let read = !matches!(&step.op, Op::Field { src, .. } if *src == reg) && step.op.reads(reg);
            if read {
                return false;
            }
            if step.op.dst() == Some(reg) && step.mask == mask {
                return true;
            }
        }
        self.out != reg && !self.outputs.contains(&reg)
    }

    fn selects(&self, at: usize, reg: Reg) -> bool {
        let mask = self.steps[at].mask;
        for step in &self.steps[at + 1..] {
            let allowed = match &step.op {
                Op::Closure(c) if c.list == reg => c.source.is_none() && !c.imports.iter().any(|(from, _)| *from == reg),
                Op::Len { a, .. } if *a == reg => true,
                Op::Call {
                    kind: FunctionKind::Internal(crate::functions::InternalFunction::Len),
                    args,
                    ..
                } if matches!(args.as_ref(), [Input::Reg(r)] if *r == reg) => true,
                op => !op.reads(reg),
            };
            if !allowed {
                return false;
            }
            if step.op.dst() == Some(reg) && step.mask == mask {
                return true;
            }
        }
        self.out != reg && !self.outputs.contains(&reg)
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

    pub fn row_roots(&self, bound: &[Binding]) -> Option<Vec<Arc<str>>> {
        let mut roots = Vec::new();
        self.collect_roots(bound, &mut roots).then_some(roots)
    }

    fn root_of(load: &Load) -> Option<Arc<str>> {
        match load {
            Load::Env(key) => Some(key.clone()),
            Load::Path(path) => match path.as_ref() {
                [FetchFastTarget::Begin, FetchFastTarget::String(key), ..] => Some(key.clone()),
                _ => None,
            },
        }
    }

    fn collect_roots(&self, bound: &[Binding], roots: &mut Vec<Arc<str>>) -> bool {
        let rowed = |site: u16| matches!(bound.get(site as usize), None | Some(Binding::Row));
        let add = |load: Load, roots: &mut Vec<Arc<str>>| match Self::root_of(&load) {
            Some(root) => {
                if !roots.contains(&root) {
                    roots.push(root);
                }
                true
            }
            None => false,
        };
        for step in &self.steps {
            let ok = match &step.op {
                Op::Env { key, site, .. } if rowed(*site) => add(Load::Env(key.clone()), roots),
                Op::Path { path, site, .. } if rowed(*site) => add(Load::Path(path.clone()), roots),
                Op::LoadEq { load, site, .. } | Op::LoadIn { load, site, .. } if rowed(*site) => add(load.clone(), roots),
                Op::LoadCall(c) if rowed(c.site) => add(c.load.clone(), roots),
                Op::RootEnv { .. } | Op::AssignBegin { .. } | Op::AssignStep { .. } | Op::Rewind | Op::DollarInsert { .. } => false,
                Op::Closure(c) => {
                    let source = match &c.source {
                        Some((load, site)) if rowed(*site) => add(load.clone(), roots),
                        _ => true,
                    };
                    source && c.body.collect_roots(bound, roots)
                }
                _ => true,
            };
            if !ok {
                return false;
            }
        }
        true
    }

    fn site_of(op: &Op) -> Option<u16> {
        match op {
            Op::Env { site, .. } | Op::Path { site, .. } | Op::LoadEq { site, .. } | Op::LoadIn { site, .. } => Some(*site),
            Op::LoadCall(call) => Some(call.site),
            Op::Closure(c) => c.source.as_ref().map(|(_, site)| *site),
            Op::Fold(fold) => Some(fold.site),
            Op::Switch(switch) => match switch.subject {
                Subject::Site(site) => Some(site),
                Subject::Reg(_) => None,
            },
            _ => None,
        }
    }

    pub fn source_fields(&self, key: &str) -> Option<Vec<Arc<str>>> {
        let mut fields = Vec::new();
        self.collect_fields(key, &self.site_keys, &mut fields).then_some(fields)
    }

    fn collect_fields(&self, key: &str, keys: &[Option<String>], fields: &mut Vec<Arc<str>>) -> bool {
        let related = |other: &str| {
            other == key
                || other.strip_prefix(key).is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
                || key.strip_prefix(other).is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
        };
        let site_key = |site: u16| keys.get(site as usize).and_then(|k| k.as_deref());
        let mut lists: Vec<Reg> = Vec::new();
        for step in &self.steps {
            let op = &step.op;
            let source = Self::site_of(op).and_then(site_key);
            let derived = match op {
                Op::Closure(c) if source == Some(key) || (c.source.is_none() && lists.contains(&c.list)) => {
                    let element = c.body.element;
                    let clean = c.imports.iter().all(|(from, _)| !lists.contains(from))
                        && element != Some(c.body.out)
                        && c.body.steps.iter().all(|inner| match (&inner.op, element) {
                            (Op::Field { src, key, .. }, Some(el)) if *src == el => {
                                if !fields.contains(key) {
                                    fields.push(key.clone());
                                }
                                true
                            }
                            (op, Some(el)) => !op.reads(el),
                            (_, None) => true,
                        });
                    if !clean {
                        return false;
                    }
                    matches!(c.kind, ClosureFunction::Filter)
                }
                Op::Fold(fold) if source == Some(key) => {
                    for field in std::iter::once(&fold.field).chain(fold.filter.iter()) {
                        if !fields.contains(field) {
                            fields.push(field.clone());
                        }
                    }
                    false
                }
                _ if source.is_some_and(related) => return false,
                Op::Len { .. } => false,
                Op::Call {
                    kind: FunctionKind::Internal(crate::functions::InternalFunction::Len),
                    ..
                } => false,
                op if lists.iter().any(|r| op.reads(*r)) => return false,
                _ => false,
            };
            if let Op::Closure(c) = op {
                if !c.body.collect_nested(key, keys) {
                    return false;
                }
            }
            if let Some(dst) = op.dst() {
                lists.retain(|r| *r != dst);
                if derived {
                    lists.push(dst);
                }
            }
        }
        true
    }

    fn collect_nested(&self, key: &str, keys: &[Option<String>]) -> bool {
        let related = |other: &str| {
            other == key
                || other.strip_prefix(key).is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
                || key.strip_prefix(other).is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
        };
        self.steps.iter().all(|step| {
            let own = Self::site_of(&step.op)
                .and_then(|site| keys.get(site as usize).and_then(|k| k.as_deref()))
                .is_none_or(|k| !related(k));
            let nested = match &step.op {
                Op::Closure(c) => c.body.collect_nested(key, keys),
                _ => true,
            };
            own && nested
        })
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
