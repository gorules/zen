use crate::functions::{DateMethod, DeprecatedFunction};
use crate::lane::date::Date;
use crate::typed::num::Dec;
use chrono::{DateTime, SecondsFormat};
use chrono_tz::Tz;
use std::fmt;

pub type NodeId = u32;
pub type DictId = u16;
pub type ListId = u16;
pub type ColId = u16;
pub type FieldId = u16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Num(Option<u8>),
    Bool,
    Code(DictId),
    Text,
    Date,
    Null,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ty {
    pub kind: Kind,
    pub nullable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Row,
    Elem(ListId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Lit {
    Null,
    Num(Dec),
    Bool(bool),
    Code(DictId, i32),
    Text(String),
    Date(Stamp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stamp(Option<(i64, u32, Tz)>);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Host {
    Construct,
    Method(DateMethod),
    Legacy(DeprecatedFunction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Clock {
    Now,
    Coerce,
    Strict,
    Matches,
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fold {
    Sum,
    Count,
    Any,
    All,
    None,
    One,
    Min,
    Max,
    Avg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Func {
    StartsWith,
    EndsWith,
    Len,
    Abs,
    Floor,
    Ceil,
    Round(u8),
    Trunc(u8),
    Min,
    Max,
    Upper,
    Lower,
    Trim,
    Contains,
    Matches,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Range {
    pub lo: Dec,
    pub lo_closed: bool,
    pub hi: Dec,
    pub hi_closed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    Column(ColId),
    Field(ListId, FieldId),
    Parent(NodeId),
    Const(Lit),
    Neg(NodeId),
    Not(NodeId),
    Arith(Arith, NodeId, NodeId),
    Cmp(Cmp, NodeId, NodeId),
    And(NodeId, NodeId),
    Or(NodeId, NodeId),
    If(NodeId, NodeId, NodeId),
    Coalesce(NodeId, NodeId),
    InSet(NodeId, Vec<Lit>),
    InRange(NodeId, Range),
    Fold {
        fold: Fold,
        list: ListId,
        filter: Option<NodeId>,
        value: Option<NodeId>,
        empty: bool,
    },
    FirstMatch {
        conds: Vec<NodeId>,
        strict: bool,
    },
    Pick(NodeId, Vec<NodeId>),
    Call(Func, Vec<NodeId>),
    Classify(NodeId, DictId),
    Str(NodeId),
    Decode(NodeId),
    Concat(Vec<NodeId>),
    Host(Host, Vec<NodeId>),
    Clock(Clock, Vec<NodeId>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Node {
    pub op: Op,
    pub ty: Ty,
    pub scope: Scope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Collect {
    pub list: ListId,
    pub keep: Option<NodeId>,
    pub item: NodeId,
    pub empty: bool,
}

#[derive(Clone, Debug)]
pub struct Output {
    pub name: String,
    pub node: NodeId,
    pub scope: Scope,
    pub ty: Ty,
    pub collect: Option<Collect>,
}

#[derive(Clone, Debug)]
pub struct ColumnDef {
    pub path: String,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub struct ListDef {
    pub path: String,
    pub alias: Option<String>,
    pub fields: Vec<ColumnDef>,
    pub parent: Option<ListId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictKind {
    Literals,
    Classes,
}

#[derive(Clone, Debug)]
pub struct Dict {
    pub kind: DictKind,
    pub values: Vec<Box<str>>,
}

#[derive(Clone, Debug, Default)]
pub struct Schema {
    pub columns: Vec<ColumnDef>,
    pub lists: Vec<ListDef>,
    pub dicts: Vec<Dict>,
}

#[derive(Clone, Debug, Default)]
pub struct Hir {
    pub schema: Schema,
    pub nodes: Vec<Node>,
    pub outputs: Vec<Output>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsupported(pub String);

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported: {}", self.0)
    }
}

impl std::error::Error for Unsupported {}

impl Unsupported {
    pub fn new(what: impl Into<String>) -> Self {
        Self(what.into())
    }
}

impl Ty {
    pub const fn new(kind: Kind, nullable: bool) -> Self {
        Self { kind, nullable }
    }

    pub const fn num(scale: Option<u8>) -> Self {
        Self::new(Kind::Num(scale), false)
    }

    pub const fn boolean() -> Self {
        Self::new(Kind::Bool, false)
    }
}

impl Dict {
    pub fn code(&self, value: &str) -> Option<i32> {
        self.values
            .iter()
            .position(|v| v.as_ref() == value)
            .and_then(|i| i32::try_from(i).ok())
    }

    pub fn text(&self, code: i32) -> Option<&str> {
        usize::try_from(code)
            .ok()
            .and_then(|i| self.values.get(i))
            .map(|v| v.as_ref())
    }
}

impl Schema {
    pub fn column(&self, path: &str) -> Option<ColId> {
        self.columns
            .iter()
            .position(|c| c.path == path)
            .and_then(|i| ColId::try_from(i).ok())
    }

    pub fn list(&self, path: &str) -> Option<ListId> {
        self.lists
            .iter()
            .position(|l| l.parent.is_none() && l.path == path)
            .and_then(|i| ListId::try_from(i).ok())
    }

    pub fn field(&self, list: ListId, name: &str) -> Option<FieldId> {
        self.lists
            .get(list as usize)?
            .fields
            .iter()
            .position(|f| f.path == name)
            .and_then(|i| FieldId::try_from(i).ok())
    }

    pub fn nested(&self, parent: ListId, name: &str) -> Option<ListId> {
        self.lists
            .iter()
            .position(|l| l.parent == Some(parent) && l.path == name)
            .and_then(|i| ListId::try_from(i).ok())
    }

    pub fn owner(&self, scope: Scope) -> Option<Scope> {
        match scope {
            Scope::Row => None,
            Scope::Elem(l) => Some(
                self.lists
                    .get(l as usize)?
                    .parent
                    .map_or(Scope::Row, Scope::Elem),
            ),
        }
    }

    pub fn depth(&self, scope: Scope) -> u32 {
        let mut depth = 0;
        let mut at = scope;
        while let Some(up) = self.owner(at) {
            depth += 1;
            at = up;
        }
        depth
    }

    pub fn within(&self, inner: Scope, outer: Scope) -> bool {
        let mut at = Some(inner);
        while let Some(scope) = at {
            if scope == outer {
                return true;
            }
            at = self.owner(scope);
        }
        false
    }

    pub fn chain(&self, list: ListId) -> Vec<&str> {
        let mut chain = Vec::new();
        let mut at = Some(list);
        while let Some(l) = at.and_then(|l| self.lists.get(l as usize)) {
            chain.push(l.path.as_str());
            at = l.parent;
        }
        chain.reverse();
        chain
    }
}

impl Op {
    pub fn inputs(&self) -> Vec<NodeId> {
        match self {
            Op::Column(_) | Op::Field(..) | Op::Const(_) => Vec::new(),
            Op::Parent(a)
            | Op::Neg(a)
            | Op::Not(a)
            | Op::InSet(a, _)
            | Op::InRange(a, _)
            | Op::Classify(a, _)
            | Op::Str(a)
            | Op::Decode(a) => vec![*a],
            Op::Arith(_, a, b)
            | Op::Cmp(_, a, b)
            | Op::And(a, b)
            | Op::Or(a, b)
            | Op::Coalesce(a, b) => vec![*a, *b],
            Op::If(c, t, e) => vec![*c, *t, *e],
            Op::Fold { filter, value, .. } => filter.iter().chain(value.iter()).copied().collect(),
            Op::FirstMatch { conds, .. } => conds.clone(),
            Op::Pick(sel, arms) => std::iter::once(*sel).chain(arms.iter().copied()).collect(),
            Op::Call(_, args) | Op::Concat(args) | Op::Host(_, args) | Op::Clock(_, args) => {
                args.clone()
            }
        }
    }

    pub fn remap(&self, f: impl Fn(NodeId) -> NodeId) -> Op {
        match self {
            Op::Column(_) | Op::Field(..) | Op::Const(_) => self.clone(),
            Op::Parent(a) => Op::Parent(f(*a)),
            Op::Neg(a) => Op::Neg(f(*a)),
            Op::Not(a) => Op::Not(f(*a)),
            Op::Classify(a, d) => Op::Classify(f(*a), *d),
            Op::Str(a) => Op::Str(f(*a)),
            Op::Decode(a) => Op::Decode(f(*a)),
            Op::Concat(args) => Op::Concat(args.iter().map(|a| f(*a)).collect()),
            Op::Arith(op, a, b) => Op::Arith(*op, f(*a), f(*b)),
            Op::Cmp(op, a, b) => Op::Cmp(*op, f(*a), f(*b)),
            Op::And(a, b) => Op::And(f(*a), f(*b)),
            Op::Or(a, b) => Op::Or(f(*a), f(*b)),
            Op::If(c, t, e) => Op::If(f(*c), f(*t), f(*e)),
            Op::Coalesce(a, b) => Op::Coalesce(f(*a), f(*b)),
            Op::InSet(a, set) => Op::InSet(f(*a), set.clone()),
            Op::InRange(a, r) => Op::InRange(f(*a), *r),
            Op::Fold {
                fold,
                list,
                filter,
                value,
                empty,
            } => Op::Fold {
                fold: *fold,
                list: *list,
                filter: filter.map(&f),
                value: value.map(&f),
                empty: *empty,
            },
            Op::FirstMatch { conds, strict } => Op::FirstMatch {
                conds: conds.iter().map(|c| f(*c)).collect(),
                strict: *strict,
            },
            Op::Pick(sel, arms) => Op::Pick(f(*sel), arms.iter().map(|a| f(*a)).collect()),
            Op::Call(func, args) => Op::Call(*func, args.iter().map(|a| f(*a)).collect()),
            Op::Host(host, args) => Op::Host(*host, args.iter().map(|a| f(*a)).collect()),
            Op::Clock(clock, args) => Op::Clock(*clock, args.iter().map(|a| f(*a)).collect()),
        }
    }
}

impl Fold {
    pub fn quantifier(self) -> bool {
        matches!(self, Fold::Any | Fold::All | Fold::None | Fold::One)
    }
}

impl Func {
    pub fn text(self) -> bool {
        matches!(
            self,
            Func::StartsWith
                | Func::EndsWith
                | Func::Len
                | Func::Upper
                | Func::Lower
                | Func::Trim
                | Func::Contains
                | Func::Matches
        )
    }

    pub fn mapping(self) -> bool {
        matches!(self, Func::Upper | Func::Lower | Func::Trim)
    }
}

impl Stamp {
    pub(crate) fn of(date: Date) -> Self {
        Self(
            date.0
                .map(|at| (at.timestamp(), at.timestamp_subsec_nanos(), at.timezone())),
        )
    }

    pub(crate) fn date(self) -> Date {
        Date(self.0.and_then(|(secs, nanos, zone)| {
            DateTime::from_timestamp(secs, nanos).map(|at| at.with_timezone(&zone))
        }))
    }

    pub fn is_valid(self) -> bool {
        self.0.is_some()
    }
}

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.date().0 {
            Some(at) => write!(f, "{}", at.to_rfc3339_opts(SecondsFormat::Secs, true)),
            None => write!(f, "Invalid date"),
        }
    }
}

impl Hir {
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn dict(&self, id: DictId) -> &Dict {
        &self.schema.dicts[id as usize]
    }

    pub fn output(&self, name: &str) -> Option<usize> {
        self.outputs.iter().position(|o| o.name == name)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Num(Some(s)) => write!(f, "num/{s}"),
            Kind::Num(None) => write!(f, "num/*"),
            Kind::Bool => write!(f, "bool"),
            Kind::Code(d) => write!(f, "code#{d}"),
            Kind::Text => write!(f, "text"),
            Kind::Date => write!(f, "date"),
            Kind::Null => write!(f, "null"),
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.nullable {
            true => write!(f, "{}?", self.kind),
            false => write!(f, "{}", self.kind),
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Row => write!(f, "row"),
            Scope::Elem(l) => write!(f, "elem{l}"),
        }
    }
}

impl fmt::Display for Lit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Lit::Null => write!(f, "null"),
            Lit::Num(d) => write!(f, "{d}"),
            Lit::Bool(b) => write!(f, "{b}"),
            Lit::Code(d, c) => write!(f, "code#{d}:{c}"),
            Lit::Text(t) => write!(f, "{t:?}"),
            Lit::Date(d) => write!(f, "date {d}"),
        }
    }
}

impl Hir {
    fn body(&self, op: &Op) -> String {
        let ids = |xs: &[NodeId]| {
            xs.iter()
                .map(|x| format!("%{x}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match op {
            Op::Column(c) => format!(
                "column {}",
                self.schema
                    .columns
                    .get(*c as usize)
                    .map_or("?", |c| c.path.as_str())
            ),
            Op::Field(l, x) => {
                format!(
                    "field {l}.{}",
                    self.schema
                        .lists
                        .get(*l as usize)
                        .and_then(|l| l.fields.get(*x as usize))
                        .map_or("?", |c| c.path.as_str())
                )
            }
            Op::Parent(a) => format!("parent %{a}"),
            Op::Const(l) => format!("const {l}"),
            Op::Neg(a) => format!("neg %{a}"),
            Op::Not(a) => format!("not %{a}"),
            Op::Classify(a, d) => format!("classify %{a} dict {d}"),
            Op::Str(a) => format!("string %{a}"),
            Op::Decode(a) => format!("decode %{a}"),
            Op::Concat(args) => format!("concat [{}]", ids(args)),
            Op::Arith(op, a, b) => format!("{op:?} %{a}, %{b}").to_lowercase(),
            Op::Cmp(op, a, b) => format!("{op:?} %{a}, %{b}").to_lowercase(),
            Op::And(a, b) => format!("and %{a}, %{b}"),
            Op::Or(a, b) => format!("or %{a}, %{b}"),
            Op::If(c, t, e) => format!("if %{c} then %{t} else %{e}"),
            Op::Coalesce(a, b) => format!("coalesce %{a}, %{b}"),
            Op::InSet(a, set) => format!(
                "inset %{a} [{}]",
                set.iter()
                    .map(|l| l.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Op::InRange(a, r) => format!(
                "inrange %{a} {}{}..{}{}",
                if r.lo_closed { "[" } else { "(" },
                r.lo,
                r.hi,
                if r.hi_closed { "]" } else { ")" }
            ),
            Op::Fold {
                fold,
                list,
                filter,
                value,
                empty,
            } => format!(
                "fold {fold:?}{} list {list} filter {} value {}",
                if *empty { " empty" } else { "" },
                filter.map_or("-".to_string(), |x| format!("%{x}")),
                value.map_or("-".to_string(), |x| format!("%{x}"))
            )
            .to_lowercase(),
            Op::FirstMatch { conds, strict } => format!(
                "firstmatch{} [{}]",
                if *strict { " strict" } else { "" },
                ids(conds)
            ),
            Op::Pick(sel, arms) => format!("pick %{sel} [{}]", ids(arms)),
            Op::Call(func, args) => format!("call {func:?}({})", ids(args)).to_lowercase(),
            Op::Host(host, args) => format!("host {host:?}({})", ids(args)).to_lowercase(),
            Op::Clock(clock, args) => format!("clock {clock:?}({})", ids(args)).to_lowercase(),
        }
    }
}

impl fmt::Display for Hir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, c) in self.schema.columns.iter().enumerate() {
            writeln!(f, "column {i} {} : {}", c.path, c.ty)?;
        }
        for (l, list) in self.schema.lists.iter().enumerate() {
            match list.parent {
                Some(p) => writeln!(f, "list {l} {} in {p}", list.path)?,
                None => writeln!(
                    f,
                    "list {l} {} as {}",
                    list.path,
                    list.alias.as_deref().unwrap_or("-")
                )?,
            }
            for (i, c) in list.fields.iter().enumerate() {
                writeln!(f, "  field {i} {} : {}", c.path, c.ty)?;
            }
        }
        for (d, dict) in self.schema.dicts.iter().enumerate() {
            writeln!(f, "dict {d} {:?} {:?}", dict.kind, dict.values)?;
        }
        for (i, node) in self.nodes.iter().enumerate() {
            writeln!(
                f,
                "%{i} = {} : {} @{}",
                self.body(&node.op),
                node.ty,
                node.scope
            )?;
        }
        for (i, o) in self.outputs.iter().enumerate() {
            match o.collect {
                Some(c) => writeln!(
                    f,
                    "out {i} {:?} = collect list {} keep {}{} %{} : {} @{}",
                    o.name,
                    c.list,
                    c.keep.map_or("-".to_string(), |k| format!("%{k}")),
                    if c.empty { " empty" } else { "" },
                    o.node,
                    o.ty,
                    o.scope
                )?,
                None => writeln!(
                    f,
                    "out {i} {:?} = %{} : {} @{}",
                    o.name, o.node, o.ty, o.scope
                )?,
            }
        }
        Ok(())
    }
}
