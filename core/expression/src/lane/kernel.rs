use crate::compiler::Compare;
use crate::functions::registry::FunctionRegistry;
use crate::functions::{ClosureFunction, DateMethod, FunctionKind, InternalFunction, MethodKind, MethodRegistry};
use crate::lane::builtins::{Arg, Builtins, Dates, Out};
use crate::lane::date::Date;
use crate::lane::columns::Columns;
use crate::lane::compile::{Hints, LaneCompiler};
use crate::lane::interval::{Interval, IntervalData};
use crate::lane::ops::Ops;
use crate::lane::program::{Binding, Const, Kind, NumCmp, NumOp, Program};
use crate::lane::scaled::Scaled;
use crate::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use crate::parser::Node;
use crate::scope::Scope;
use crate::variable::Variable;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use smallvec::SmallVec;
use std::sync::atomic::{AtomicU64, Ordering};
use std::cell::RefCell;
use std::str::FromStr;
use std::sync::Arc;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;

type Sc = (i64, u8);
type NumFn = Box<dyn Fn(&Cx) -> Option<Sc> + Send + Sync>;
type BoolFn = Box<dyn Fn(&Cx) -> Option<bool> + Send + Sync>;
type ValFn = Box<dyn Fn(&Cx) -> Option<Variable> + Send + Sync>;
type DateFn = Box<dyn Fn(&Cx) -> Option<Date> + Send + Sync>;

struct Slot(AtomicU64);

impl Slot {
    const LIMIT: u64 = 1 << 55;
    const ABSENT: u64 = 0xFF;

    fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    #[inline(always)]
    fn get<'v>(&self, map: &'v VariableMap, key: &str) -> Option<&'v Variable> {
        let Some((shape, values)) = map.slots() else {
            return map.get_str(key);
        };
        let id = shape.id();
        if id >= Self::LIMIT {
            return map.get_str(key);
        }
        let tag = (id + 1) << 8;
        let packed = self.0.load(Ordering::Relaxed);
        if packed & !Self::ABSENT == tag {
            return match packed & Self::ABSENT {
                Self::ABSENT => None,
                index => values.get(index as usize),
            };
        }
        let index = shape.index_of(key);
        let stored = match index {
            Some(i) if (i as u64) < Self::ABSENT => i as u64,
            Some(_) => return index.and_then(|i| values.get(i)),
            None => Self::ABSENT,
        };
        self.0.store(tag | stored, Ordering::Relaxed);
        index.and_then(|i| values.get(i))
    }
}

enum Step {
    Key(Box<str>, Slot),
    Index(usize),
}

pub(crate) struct Env<'a> {
    row: usize,
    columns: Option<&'a Columns<'a>>,
    bound: &'a [Binding],
    outs: &'a [Variable],
    current: Option<&'a RefCell<Scope>>,
    roots: Option<&'a RefCell<Scope>>,
}

impl Env<'_> {
    const EMPTY: Env<'static> = Env {
        row: 0,
        columns: None,
        bound: &[],
        outs: &[],
        current: None,
        roots: None,
    };
}

pub(crate) struct Cx<'a> {
    scope: &'a Scope,
    env: &'a Env<'a>,
    element: Option<&'a Variable>,
    up: Option<&'a Cx<'a>>,
}

impl<'a> Cx<'a> {
    #[inline]
    fn pointer(&self, depth: u32) -> Option<&'a Variable> {
        let mut cx = self;
        for _ in 0..depth {
            cx = cx.up?;
        }
        cx.element
    }

    #[inline]
    fn child<'b>(&'b self, element: &'b Variable) -> Cx<'b> {
        Cx {
            scope: self.scope,
            env: self.env,
            element: Some(element),
            up: Some(self),
        }
    }
}

enum Origin {
    Env(Box<str>, Slot, usize),
    Out(usize),
    Pointer(u32),
    Root,
}

enum Root<'a> {
    Env(&'a str),
    Pointer(u32),
    Root,
}

struct Load {
    origin: Origin,
    rest: Box<[Step]>,
}

impl Load {
    #[inline(always)]
    fn with<T>(&self, cx: &Cx, f: impl FnOnce(&Variable) -> Option<T>) -> Option<T> {
        let Origin::Env(key, slot, id) = &self.origin else {
            return self.elsewhere(cx, f);
        };
        let env = cx.env;
        if !env.bound.is_empty() {
            match env.bound.get(*id) {
                Some(Binding::Column(index)) => {
                    let (_, column) = env.columns?.columns.get(*index)?;
                    return f(&column.variable(env.row));
                }
                Some(Binding::Absent) => return f(&Variable::Null),
                _ => {}
            }
        }
        match env.current {
            Some(cell) => self.scoped(&cell.borrow(), key, slot, f),
            None => self.scoped(cx.scope, key, slot, f),
        }
    }

    #[inline(always)]
    fn scoped<T>(&self, scope: &Scope, key: &str, slot: &Slot, f: impl FnOnce(&Variable) -> Option<T>) -> Option<T> {
        if !scope.locals().is_empty() {
            if let Some(v) = scope.local_str(key) {
                return Self::walk(v, &self.rest, f);
            }
        }
        match scope.base() {
            Variable::Object(o) => {
                let o = o.borrow();
                match slot.get(&o, key) {
                    Some(v) => Self::walk(v, &self.rest, f),
                    None => f(&Variable::Null),
                }
            }
            Variable::Null => f(&Variable::Null),
            _ => None,
        }
    }

    #[inline(never)]
    fn elsewhere<T>(&self, cx: &Cx, f: impl FnOnce(&Variable) -> Option<T>) -> Option<T> {
        match &self.origin {
            Origin::Out(index) => Self::walk(cx.env.outs.get(*index)?, &self.rest, f),
            Origin::Pointer(depth) => Self::walk(cx.pointer(*depth)?, &self.rest, f),
            Origin::Root => {
                let root = match cx.env.roots {
                    Some(cell) => cell.borrow().materialize(),
                    None => cx.scope.materialize(),
                };
                Self::walk(&root, &self.rest, f)
            }
            Origin::Env(..) => None,
        }
    }

    #[inline]
    fn walk<T>(v: &Variable, rest: &[Step], f: impl FnOnce(&Variable) -> Option<T>) -> Option<T> {
        let Some((step, tail)) = rest.split_first() else {
            return f(v);
        };
        match (step, v) {
            (Step::Key(key, slot), Variable::Object(o)) => {
                let o = o.borrow();
                match slot.get(&o, key) {
                    Some(next) => Self::walk(next, tail, f),
                    None => f(&Variable::Null),
                }
            }
            (Step::Key(..), _) => f(&Variable::Null),
            (Step::Index(_), Variable::Dynamic(_)) => None,
            (Step::Index(i), Variable::Array(a)) => {
                let a = a.borrow();
                match a.get(*i) {
                    Some(next) => Self::walk(next, tail, f),
                    None => f(&Variable::Null),
                }
            }
            (Step::Index(i), Variable::String(s)) => {
                let next = i
                    .checked_add(1)
                    .and_then(|end| s.get(*i..end))
                    .map_or(Variable::Null, |c| Variable::String(c.into()));
                Self::walk(&next, tail, f)
            }
            (Step::Index(_), _) => f(&Variable::Null),
        }
    }
}

enum Code {
    Const(Const),
    Load(Arc<Load>),
    Num(NumFn),
    Bool(BoolFn),
    Date(DateFn),
    Val(ValFn),
}

impl Code {
    fn numeric(&self) -> bool {
        match self {
            Code::Num(_) => true,
            Code::Const(Const::Number(n)) => Scaled::parts(n).is_some(),
            _ => false,
        }
    }

    fn boolean_typed(&self) -> bool {
        matches!(self, Code::Bool(_) | Code::Const(Const::Bool(_)))
    }

    fn dated(&self) -> bool {
        matches!(self, Code::Date(_) | Code::Const(Const::Date(_)))
    }

    fn guardable(&self) -> bool {
        self.numeric() || matches!(self, Code::Load(_))
    }

    fn date(self) -> DateFn {
        match self {
            Code::Date(f) => f,
            Code::Const(Const::Date(d)) => Box::new(move |_| Some(Date(d))),
            _ => Box::new(|_| None),
        }
    }

    fn scaled(&self) -> Option<Sc> {
        match self {
            Code::Const(Const::Number(n)) => Scaled::parts(n),
            _ => None,
        }
    }

    fn num(self) -> NumFn {
        match self {
            Code::Num(f) => f,
            Code::Const(Const::Number(n)) => match Scaled::parts(&n) {
                Some(k) => Box::new(move |_| Some(k)),
                None => Box::new(|_| None),
            },
            Code::Const(_) | Code::Bool(_) | Code::Date(_) => Box::new(|_| None),
            Code::Load(l) => Box::new(move |s| {
                l.with(s, |v| match v {
                    Variable::Number(n) => Scaled::parts(n),
                    _ => None,
                })
            }),
            Code::Val(f) => Box::new(move |s| match f(s)? {
                Variable::Number(n) => Scaled::parts(&n),
                _ => None,
            }),
        }
    }

    fn boolean(self) -> BoolFn {
        match self {
            Code::Bool(f) => f,
            Code::Const(Const::Bool(b)) => Box::new(move |_| Some(b)),
            Code::Const(_) | Code::Num(_) | Code::Date(_) => Box::new(|_| None),
            Code::Load(l) => Box::new(move |s| {
                l.with(s, |v| match v {
                    Variable::Bool(b) => Some(*b),
                    _ => None,
                })
            }),
            Code::Val(f) => Box::new(move |s| match f(s)? {
                Variable::Bool(b) => Some(b),
                _ => None,
            }),
        }
    }

    fn val(self) -> ValFn {
        match self {
            Code::Val(f) => f,
            Code::Num(f) => Box::new(move |s| f(s).map(|(m, sc)| Variable::Number(Scaled::decimal(m, sc)))),
            Code::Bool(f) => Box::new(move |s| f(s).map(Variable::Bool)),
            Code::Date(f) => Box::new(move |s| f(s).map(Date::variable)),
            Code::Load(l) => Box::new(move |s| l.with(s, |v| Some(v.clone()))),
            Code::Const(c) => match c {
                Const::Null => Box::new(|_| Some(Variable::Null)),
                Const::Bool(b) => Box::new(move |_| Some(Variable::Bool(b))),
                Const::Number(n) => Box::new(move |_| Some(Variable::Number(n))),
                c => Box::new(move |_| Some(c.variable())),
            },
        }
    }

    fn view<T: 'static>(
        self,
        f: impl Fn(&Cx, &Variable) -> Option<T> + Send + Sync + 'static,
    ) -> Box<dyn Fn(&Cx) -> Option<T> + Send + Sync> {
        match self {
            Code::Load(l) => Box::new(move |s| l.with(s, |v| f(s, v))),
            other => {
                let g = other.val();
                Box::new(move |s| {
                    let v = g(s)?;
                    f(s, &v)
                })
            }
        }
    }
}

enum Held {
    Var(Variable),
    Date(Date),
}

impl Held {
    #[inline]
    fn arg(&self) -> Arg<'_> {
        match self {
            Held::Var(v) => Arg::of(v),
            Held::Date(d) => Arg::Date(*d),
        }
    }
}

type HeldFn = Box<dyn Fn(&Cx) -> Option<Held> + Send + Sync>;

struct Members {
    nums: Vec<Decimal>,
    strs: Vec<Box<str>>,
    bools: [bool; 2],
    null: bool,
}

impl Members {
    fn of(items: &[Const]) -> Self {
        let mut members = Members {
            nums: Vec::new(),
            strs: Vec::new(),
            bools: [false; 2],
            null: false,
        };
        for item in items {
            match item {
                Const::Number(n) => members.nums.push(*n),
                Const::String(s) => members.strs.push(Box::from(s.as_ref())),
                Const::Bool(b) => members.bools[*b as usize] = true,
                Const::Null => members.null = true,
                Const::Array(_) | Const::Date(_) => {}
            }
        }
        members
    }

    #[inline]
    fn test(&self, v: &Variable) -> Option<bool> {
        match v {
            Variable::Number(n) => Some(self.nums.iter().any(|x| Ops::same_number(n, x))),
            Variable::String(s) => Some(self.strs.iter().any(|x| x.as_ref() == s.as_str())),
            Variable::Bool(b) => Some(self.bools[*b as usize]),
            Variable::Null => Some(self.null),
            _ => None,
        }
    }
}

enum Mode {
    Plain,
    Owned,
    Dynamic { keys: Box<[Box<str>]>, rewind: bool },
}

pub(crate) struct Kernel {
    entries: Vec<ValFn>,
    keys: Vec<Option<Box<str>>>,
    sites: Vec<Option<usize>>,
    mode: Mode,
}

impl std::fmt::Debug for Kernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Kernel")
    }
}

pub(crate) type Values = SmallVec<[Variable; 4]>;

impl Kernel {
    const DEPTH: usize = 192;

    fn builder<'h, 'a>(hints: Option<&'h Hints>) -> KernelBuilder<'h, 'a> {
        KernelBuilder {
            hints,
            folder: LaneCompiler::folder(),
            depth: 0,
            keys: Vec::new(),
            dollar: None,
            unresolved: false,
            aliases: Vec::new(),
            owned: false,
        }
    }

    pub(crate) fn compile<'a>(root: &'a Node<'a>, hints: Option<&Hints>, program: &Program) -> Option<Self> {
        let mut builder = Self::builder(hints);
        builder.owned = LaneCompiler::assigns(root);
        let code = builder.node(root)?;
        let mode = match builder.owned {
            true => Mode::Owned,
            false => Mode::Plain,
        };
        Some(Self::finish(vec![code.val()], builder.keys, program, mode))
    }

    pub(crate) fn compile_many<'a>(
        roots: &[(&'a str, &'a Node<'a>)],
        chain: bool,
        hints: Option<&Hints>,
        program: &Program,
    ) -> Option<Self> {
        if roots.is_empty() {
            return None;
        }
        let assigns = roots.iter().any(|(_, n)| LaneCompiler::assigns(n));
        let nested = roots.iter().any(|(a, _)| {
            roots
                .iter()
                .any(|(b, _)| b.strip_prefix(*a).is_some_and(|rest| rest.starts_with('.')))
        });
        let resolvable = chain
            && !assigns
            && !nested
            && roots.iter().all(|(_, n)| LaneCompiler::dollar_static(n, false));
        if resolvable {
            if let Some(kernel) = Self::entries(roots, hints, program, true, false) {
                return Some(kernel);
            }
        }
        Self::entries(roots, hints, program, false, chain)
    }

    fn entries<'a>(
        roots: &[(&'a str, &'a Node<'a>)],
        hints: Option<&Hints>,
        program: &Program,
        resolvable: bool,
        dynamic: bool,
    ) -> Option<Self> {
        let assigns = roots.iter().any(|(_, n)| LaneCompiler::assigns(n));
        let mut builder = Self::builder(hints);
        builder.dollar = resolvable.then(Vec::new);
        builder.owned = assigns;
        let mut entries = Vec::with_capacity(roots.len());
        for (index, (key, root)) in roots.iter().enumerate() {
            entries.push(builder.node(root)?.val());
            if builder.unresolved {
                return None;
            }
            if let Some(dollar) = builder.dollar.as_mut() {
                match dollar.iter_mut().find(|(k, _)| k == key) {
                    Some(entry) => entry.1 = index,
                    None => dollar.push((key.to_string(), index)),
                }
            }
        }
        let mode = match (dynamic, assigns) {
            (true, rewind) => Mode::Dynamic {
                keys: roots.iter().map(|(k, _)| Box::from(*k)).collect(),
                rewind,
            },
            (false, true) => Mode::Owned,
            (false, false) => Mode::Plain,
        };
        Some(Self::finish(entries, builder.keys, program, mode))
    }

    fn finish(entries: Vec<ValFn>, keys: Vec<Option<Box<str>>>, program: &Program, mode: Mode) -> Self {
        let sites = keys
            .iter()
            .map(|key| {
                let key = key.as_deref()?;
                program.site_keys.iter().position(|k| k.as_deref() == Some(key))
            })
            .collect();
        Self { entries, keys, sites, mode }
    }

    pub(crate) fn bind(&self, bind: &dyn Fn(&str) -> Binding) -> Vec<Binding> {
        self.keys
            .iter()
            .map(|key| key.as_deref().map_or(Binding::Row, bind))
            .collect()
    }

    pub(crate) fn bind_sites(&self, bound: &[Binding]) -> Vec<Binding> {
        self.sites
            .iter()
            .map(|site| site.and_then(|i| bound.get(i).copied()).unwrap_or(Binding::Row))
            .collect()
    }

    #[inline(always)]
    pub(crate) fn run(&self, scope: &Scope) -> Option<Variable> {
        match (self.entries.as_slice(), &self.mode) {
            ([entry], Mode::Plain) => entry(&Cx {
                scope,
                env: &Env::EMPTY,
                element: None,
                up: None,
            }),
            _ => self.run_all(scope, 0, None, &[])?.pop(),
        }
    }

    #[inline]
    pub(crate) fn run_all(
        &self,
        scope: &Scope,
        row: usize,
        columns: Option<&Columns>,
        bound: &[Binding],
    ) -> Option<Values> {
        let mut values = Values::new();
        let (current, roots, bound) = match &self.mode {
            Mode::Plain => (None, None, bound),
            Mode::Owned => (Some(RefCell::new(scope.clone())), None, &[][..]),
            Mode::Dynamic { .. } => (Some(RefCell::new(scope.clone())), Some(RefCell::new(scope.clone())), &[][..]),
        };
        for (index, entry) in self.entries.iter().enumerate() {
            if let (Mode::Dynamic { rewind: true, .. }, Some(current), Some(roots), true) = (&self.mode, &current, &roots, index > 0) {
                *current.borrow_mut() = roots.borrow().clone();
            }
            let env = Env {
                row,
                columns,
                bound,
                outs: &values,
                current: current.as_ref(),
                roots: roots.as_ref(),
            };
            let value = entry(&Cx {
                scope,
                env: &env,
                element: None,
                up: None,
            })?;
            if let (Mode::Dynamic { keys, .. }, Some(current), Some(roots)) = (&self.mode, &current, &roots) {
                Self::insert_dollar(keys.get(index)?, &value, current, roots)?;
            }
            values.push(value);
        }
        Some(values)
    }

    fn insert_dollar(key: &str, value: &Variable, current: &RefCell<Scope>, roots: &RefCell<Scope>) -> Option<()> {
        let mut root = roots.borrow_mut();
        let dollar = match root.local(&Variable::dollar_key()) {
            Some(existing @ Variable::Object(_)) => existing.shallow_clone(),
            _ => {
                let created = Variable::empty_object();
                root.set_local(Variable::dollar_key(), created.clone());
                created
            }
        };
        let _ = dollar.dot_insert(key, value.clone());
        current.borrow_mut().set_local(Variable::dollar_key(), dollar);
        Some(())
    }
}

struct KernelBuilder<'h, 'a> {
    hints: Option<&'h Hints>,
    folder: LaneCompiler<'a>,
    depth: usize,
    keys: Vec<Option<Box<str>>>,
    dollar: Option<Vec<(String, usize)>>,
    unresolved: bool,
    aliases: Vec<Option<&'a str>>,
    owned: bool,
}

impl<'h, 'a> KernelBuilder<'h, 'a> {
    fn node(&mut self, node: &'a Node<'a>) -> Option<Code> {
        self.depth += 1;
        let code = match self.depth > Kernel::DEPTH {
            true => None,
            false => self.compile(node),
        };
        self.depth -= 1;
        code
    }

    fn compile(&mut self, node: &'a Node<'a>) -> Option<Code> {
        if let Some(n) = Const::number(node) {
            return Some(Code::Const(Const::Number(n)));
        }
        match node {
            Node::Null => return Some(Code::Const(Const::Null)),
            Node::Bool(b) => return Some(Code::Const(Const::Bool(*b))),
            Node::String(s) => return Some(Code::Const(Const::String(Arc::from(*s)))),
            Node::Parenthesized(inner) => return self.node(inner),
            _ => {}
        }
        if let Some(resolved) = self.resolve_dollar(node) {
            return Some(resolved);
        }
        if let Some(value) = self.folder.fold_value(node) {
            return Some(Code::Const(value));
        }
        match node {
            Node::Root => Some(Code::Val(Box::new(|s| {
                Some(match s.env.current {
                    Some(cell) => cell.borrow().materialize(),
                    None => s.scope.materialize(),
                })
            }))),
            Node::Assignments { list, output } => self.assignments(list, *output),
            Node::Identifier(_) | Node::Member { .. } | Node::Pointer if self.path(node).is_some() => {
                let (root, rest) = self.path(node)?;
                Some(match root {
                    Root::Env(key) => self.load(key, rest),
                    Root::Root => Code::Val(
                        Code::Load(Arc::new(Load {
                            origin: Origin::Root,
                            rest: rest
                                .iter()
                                .map(|p| match p {
                                    Node::String(k) => Step::Key(Box::from(*k), Slot::new()),
                                    Node::Number(n) => Step::Index(n.to_u32().map_or(usize::MAX, |i| i as usize)),
                                    _ => Step::Index(usize::MAX),
                                })
                                .collect(),
                        }))
                        .val(),
                    ),
                    Root::Pointer(depth) => Code::Load(Arc::new(Load {
                        origin: Origin::Pointer(depth),
                        rest: rest
                            .iter()
                            .filter_map(|p| match p {
                                Node::String(k) => Some(Step::Key(Box::from(*k), Slot::new())),
                                _ => None,
                            })
                            .collect(),
                    })),
                })
            }
            Node::Object(pairs) => self.object(pairs),
            Node::Member {
                node: object,
                property: Node::String(key),
            } => {
                let key: Box<str> = Box::from(*key);
                let slot = Slot::new();
                Some(Code::Val(self.node(object)?.view(move |_, v| {
                    Some(match v {
                        Variable::Object(o) => slot.get(&o.borrow(), &key).cloned().unwrap_or(Variable::Null),
                        _ => Variable::Null,
                    })
                })))
            }
            Node::Member { node: object, property } => {
                let (a, b) = (self.node(object)?.val(), self.node(property)?.val());
                Some(Code::Val(Box::new(move |s| Ops::fetch(a(s)?, b(s)?).ok())))
            }
            Node::Array(items) => {
                let items = items
                    .iter()
                    .map(|n| self.node(n).map(Code::val))
                    .collect::<Option<Vec<_>>>()?;
                Some(Code::Val(Box::new(move |s| {
                    let values = items.iter().map(|f| f(s)).collect::<Option<Vec<_>>>()?;
                    Some(Variable::from_array(values))
                })))
            }
            Node::TemplateString(parts) => self.template(parts),
            Node::Slice { node, to, from } => {
                let a = self.node(node)?.val();
                let to = match to {
                    Some(t) => Some(self.node(t)?.val()),
                    None => None,
                };
                let from = match from {
                    Some(f) => Some(self.node(f)?.val()),
                    None => None,
                };
                Some(Code::Val(Box::new(move |s| {
                    let a = a(s)?;
                    let to = match &to {
                        Some(t) => t(s)?,
                        None => Ops::subtract(Ops::len(&a).ok()?, Variable::Number(Decimal::ONE)).ok()?,
                    };
                    let from = match &from {
                        Some(f) => f(s)?,
                        None => Variable::Number(Decimal::ZERO),
                    };
                    Ops::slice(a, to, from).ok()
                })))
            }
            Node::Interval {
                left,
                right,
                left_bracket,
                right_bracket,
            } => {
                let (a, b) = (self.node(left)?.val(), self.node(right)?.val());
                let (l, r) = (*left_bracket, *right_bracket);
                Some(Code::Val(Box::new(move |s| Ops::interval(&a(s)?, &b(s)?, l, r).ok())))
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                let c = self.node(condition)?.boolean();
                let (t, f) = (self.node(on_true)?, self.node(on_false)?);
                Some(match (t.numeric(), f.numeric(), t.boolean_typed(), f.boolean_typed()) {
                    (true, true, _, _) => {
                        let (t, f) = (t.num(), f.num());
                        Code::Num(Box::new(move |s| if c(s)? { t(s) } else { f(s) }))
                    }
                    (_, _, true, true) => {
                        let (t, f) = (t.boolean(), f.boolean());
                        Code::Bool(Box::new(move |s| if c(s)? { t(s) } else { f(s) }))
                    }
                    _ => {
                        let (t, f) = (t.val(), f.val());
                        Code::Val(Box::new(move |s| if c(s)? { t(s) } else { f(s) }))
                    }
                })
            }
            Node::Unary { node, operator } => {
                let a = self.node(node)?;
                match operator {
                    Operator::Arithmetic(ArithmeticOperator::Add) => Some(a),
                    Operator::Arithmetic(ArithmeticOperator::Subtract) => Some(match a.numeric() {
                        true => {
                            let a = a.num();
                            Code::Num(Box::new(move |s| {
                                let (m, sc) = a(s)?;
                                (m != 0).then_some(())?;
                                Some((m.checked_neg()?, sc))
                            }))
                        }
                        false => {
                            let a = a.val();
                            Code::Val(Box::new(move |s| Ops::negate(a(s)?).ok()))
                        }
                    }),
                    Operator::Logical(LogicalOperator::Not) => {
                        let a = a.boolean();
                        Some(Code::Bool(Box::new(move |s| a(s).map(|b| !b))))
                    }
                    _ => None,
                }
            }
            Node::Binary {
                left,
                operator,
                right,
            } => self.binary(left, *operator, right),
            Node::FunctionCall { kind, arguments } => self.call(kind, arguments),
            Node::MethodCall {
                kind,
                this,
                arguments,
            } => self.method(kind, this, arguments),
            _ => None,
        }
    }

    fn assignments(&mut self, list: &'a [(&'a Node<'a>, &'a Node<'a>)], output: Option<&'a Node<'a>>) -> Option<Code> {
        let steps = list
            .iter()
            .map(|(k, v)| Some((self.node(k)?.val(), self.node(v)?.val())))
            .collect::<Option<Vec<_>>>()?;
        let output = match output {
            Some(o) => Some(self.node(o)?.val()),
            None => None,
        };
        Some(Code::Val(Box::new(move |s| {
            let object = Variable::empty_object();
            for (key, value) in &steps {
                let key = Ops::assigned_key(key(s)?).ok()?;
                let value = value(s)?;
                let cell = s.env.current?;
                Ops::assign(&mut cell.borrow_mut(), &object, &key, value).ok()?;
            }
            match &output {
                Some(o) => o(s),
                None => Some(object),
            }
        })))
    }

    fn lookup_alias(&self, name: &str) -> Option<u32> {
        self.aliases
            .iter()
            .rev()
            .position(|alias| *alias == Some(name))
            .map(|depth| depth as u32)
    }

    fn object(&mut self, pairs: &'a [(&'a Node<'a>, &'a Node<'a>)]) -> Option<Code> {
        enum Key {
            Static(Box<str>),
            Dynamic(ValFn),
        }
        let string = Builtins::of(&FunctionKind::Internal(InternalFunction::String))?;
        let mut compiled = Vec::with_capacity(pairs.len());
        for (key, value) in pairs.iter() {
            let key = match key {
                Node::String(k) => Key::Static(Box::from(*k)),
                other => Key::Dynamic(self.node(other)?.val()),
            };
            compiled.push((key, self.node(value)?.val()));
        }
        Some(Code::Val(Box::new(move |s| {
            let mut pairs: SmallVec<[(Symbol, Variable); 8]> = SmallVec::with_capacity(compiled.len());
            for (key, value) in compiled.iter() {
                let key = match key {
                    Key::Static(k) => Symbol::from(k.as_ref()),
                    Key::Dynamic(f) => {
                        let k = f(s)?;
                        let text = Builtins::raw(string, &[Arg::of(&k)]).ok()?.variable();
                        Ops::object_key(text).ok()?
                    }
                };
                pairs.push((key, value(s)?));
            }
            let mut map = crate::variable::VariableMap::with_capacity(pairs.len());
            for (key, value) in pairs.into_iter().rev() {
                map.insert(key, value);
            }
            Some(Variable::from_object(map))
        })))
    }

    fn each_owned(v: &Variable, each: &mut dyn FnMut(&Variable) -> Option<bool>) -> Option<()> {
        let list = match v {
            Variable::Array(_) => v.clone(),
            other => Ops::elements(other).ok()?,
        };
        let Variable::Array(items) = &list else {
            return None;
        };
        let mut index = 0;
        loop {
            let Some(item) = items.borrow().get(index).cloned() else {
                return Some(());
            };
            if !each(&item)? {
                return Some(());
            }
            index += 1;
        }
    }

    fn each(v: &Variable, each: &mut dyn FnMut(&Variable) -> Option<bool>) -> Option<()> {
        let owned;
        let items = match v {
            Variable::Array(a) => a,
            other => {
                owned = Ops::elements(other).ok()?;
                match &owned {
                    Variable::Array(a) => a,
                    _ => return None,
                }
            }
        };
        let items = items.borrow();
        for item in items.iter() {
            if !each(item)? {
                break;
            }
        }
        Some(())
    }

    fn closure(&mut self, kind: ClosureFunction, arguments: &'a [&'a Node<'a>]) -> Option<Code> {
        let [list, body] = arguments else {
            return None;
        };
        let (body, alias) = match body {
            Node::Closure { body, alias } => (*body, *alias),
            other => (*other, None),
        };
        let each: fn(&Variable, &mut dyn FnMut(&Variable) -> Option<bool>) -> Option<()> = match self.owned {
            true => Self::each_owned,
            false => Self::each,
        };
        let list = self.node(list)?;
        self.aliases.push(alias);
        let body = self.node(body);
        self.aliases.pop();
        let body = body?;
        Some(match kind {
            ClosureFunction::Some | ClosureFunction::All | ClosureFunction::None => {
                let body = body.boolean();
                Code::Bool(list.view(move |s, v| {
                    let mut verdict = !matches!(kind, ClosureFunction::Some);
                    each(v, &mut |item| {
                        let hit = body(&s.child(item))?;
                        match (kind, hit) {
                            (ClosureFunction::Some, true) => verdict = true,
                            (ClosureFunction::All, false) | (ClosureFunction::None, true) => verdict = false,
                            _ => return Some(true),
                        }
                        Some(false)
                    })?;
                    Some(verdict)
                }))
            }
            ClosureFunction::Count | ClosureFunction::One => {
                let body = body.boolean();
                let count = list.view(move |s, v| {
                    let mut count = 0i64;
                    each(v, &mut |item| {
                        count += body(&s.child(item))? as i64;
                        Some(true)
                    })?;
                    Some(count)
                });
                match kind {
                    ClosureFunction::One => Code::Bool(Box::new(move |s| count(s).map(|n| n == 1))),
                    _ => Code::Num(Box::new(move |s| count(s).map(|n| (n, 0)))),
                }
            }
            ClosureFunction::Filter => {
                let body = body.boolean();
                Code::Val(list.view(move |s, v| {
                    let mut kept = Vec::new();
                    each(v, &mut |item| {
                        if body(&s.child(item))? {
                            kept.push(item.clone());
                        }
                        Some(true)
                    })?;
                    Some(Variable::from_array(kept))
                }))
            }
            ClosureFunction::Map | ClosureFunction::FlatMap => {
                let body = body.val();
                Code::Val(list.view(move |s, v| {
                    let mut values = Vec::new();
                    each(v, &mut |item| {
                        values.push(body(&s.child(item))?);
                        Some(true)
                    })?;
                    let array = Variable::from_array(values);
                    match kind {
                        ClosureFunction::Map => Some(array),
                        _ => Ops::flatten(array).ok(),
                    }
                }))
            }
        })
    }

    fn resolve_dollar(&mut self, node: &'a Node<'a>) -> Option<Code> {
        if !self.aliases.is_empty() {
            return None;
        }
        let dollar = self.dollar.as_ref().filter(|d| !d.is_empty())?;
        let mut segments = Vec::new();
        let mut base = node;
        while let Node::Member {
            node: inner,
            property: Node::String(p),
        } = base
        {
            segments.push(*p);
            base = inner;
        }
        if !matches!(base, Node::Identifier("$")) || segments.is_empty() {
            return None;
        }
        segments.reverse();
        let found = (1..=segments.len()).rev().find_map(|i| {
            let key = segments[..i].join(".");
            dollar.iter().find(|(k, _)| *k == key).map(|(_, index)| (*index, i))
        });
        let Some((index, used)) = found else {
            let interior = (1..=segments.len()).any(|i| {
                let prefix = format!("{}.", segments[..i].join("."));
                dollar.iter().any(|(k, _)| k.starts_with(&prefix))
            });
            if interior {
                self.unresolved = true;
            }
            return Some(Code::Const(Const::Null));
        };
        let rest: Box<[Step]> = segments[used..]
            .iter()
            .map(|key| Step::Key(Box::from(*key), Slot::new()))
            .collect();
        Some(Code::Load(Arc::new(Load {
            origin: Origin::Out(index),
            rest,
        })))
    }

    fn path(&self, node: &'a Node<'a>) -> Option<(Root<'a>, Vec<&'a Node<'a>>)> {
        match node {
            Node::Identifier("$") if self.dollar.as_ref().is_some_and(|d| !d.is_empty()) => None,
            Node::Identifier(v) => match self.lookup_alias(v) {
                Some(depth) => Some((Root::Pointer(depth), Vec::new())),
                None => Some((Root::Env(v), Vec::new())),
            },
            Node::Pointer if !self.aliases.is_empty() => Some((Root::Pointer(0), Vec::new())),
            Node::Root => Some((Root::Root, Vec::new())),
            Node::Member { node, property } => {
                let (root, mut rest) = self.path(node)?;
                match (&root, property) {
                    (_, Node::String(_)) => rest.push(property),
                    (Root::Env(_) | Root::Root, Node::Number(n)) if n.to_u32().is_some() => rest.push(property),
                    _ => return None,
                }
                Some((root, rest))
            }
            _ => None,
        }
    }

    fn load(&mut self, key: &'a str, rest: Vec<&'a Node<'a>>) -> Code {
        let mut name = key.to_string();
        let mut keyed = true;
        let mut dotted = false;
        let steps: Box<[Step]> = rest
            .iter()
            .map(|p| match p {
                Node::String(s) => {
                    dotted |= s.contains('.');
                    name.push('.');
                    name.push_str(s);
                    Step::Key(Box::from(*s), Slot::new())
                }
                _ => {
                    keyed = false;
                    let index = match p {
                        Node::Number(n) => n.to_u32().map_or(usize::MAX, |i| i as usize),
                        _ => usize::MAX,
                    };
                    Step::Index(index)
                }
            })
            .collect();
        let bind_key = (keyed && !dotted).then(|| Box::<str>::from(name.as_str()));
        let id = self.keys.len();
        self.keys.push(bind_key);
        let code = Code::Load(Arc::new(Load {
            origin: Origin::Env(Box::from(key), Slot::new(), id),
            rest: steps,
        }));
        let hint = match keyed {
            true => self.hints.and_then(|h| h.get(&name).copied()),
            false => None,
        };
        match (hint, code, self.owned) {
            (Some(Kind::Num), code @ Code::Load(_), _) => Code::Num(code.num()),
            (Some(Kind::Bool), code @ Code::Load(_), _) => Code::Bool(code.boolean()),
            (_, code @ Code::Load(_), true) => Code::Val(code.val()),
            (_, code, _) => code,
        }
    }

    fn template(&mut self, parts: &'a [&'a Node<'a>]) -> Option<Code> {
        enum Part {
            Text(Box<str>),
            Num(NumFn),
            Value(ValFn),
        }
        let string = Builtins::of(&FunctionKind::Internal(InternalFunction::String))?;
        let parts = parts
            .iter()
            .map(|p| match p {
                Node::String(text) => Some(Part::Text(Box::from(*text))),
                other => self.node(other).map(|c| match c.numeric() {
                    true => Part::Num(c.num()),
                    false => Part::Value(c.val()),
                }),
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Code::Val(Box::new(move |s| {
            let mut out = String::new();
            for part in &parts {
                match part {
                    Part::Text(t) => out.push_str(t),
                    Part::Num(f) => {
                        let (m, sc) = f(s)?;
                        Scaled::write(m, sc, &mut out);
                    }
                    Part::Value(f) => match f(s)? {
                        Variable::String(t) => out.push_str(t.as_str()),
                        Variable::Number(n) if Scaled::parts(&n).is_some() => {
                            let (m, sc) = Scaled::parts(&n)?;
                            Scaled::write(m, sc, &mut out);
                        }
                        v => match Builtins::raw(string, &[Arg::of(&v)]).ok()?.variable() {
                            Variable::String(t) => out.push_str(t.as_str()),
                            _ => return None,
                        },
                    },
                }
            }
            Some(Variable::String(Symbol::from(out.as_str())))
        })))
    }

    fn held(code: Code) -> HeldFn {
        match code {
            Code::Date(f) => Box::new(move |s| f(s).map(Held::Date)),
            Code::Const(Const::Date(d)) => Box::new(move |_| Some(Held::Date(Date(d)))),
            other => {
                let f = other.val();
                Box::new(move |s| f(s).map(Held::Var))
            }
        }
    }

    fn typed_out(returns: Kind, f: Box<dyn Fn(&Cx) -> Option<Out> + Send + Sync>) -> Code {
        match returns {
            Kind::Date => Code::Date(Box::new(move |s| match f(s)? {
                Out::Date(d) => Some(d),
                Out::Var(v) => Date::of(&v).filter(|_| !Date::sourced(&v)),
                _ => None,
            })),
            _ => Code::Val(Box::new(move |s| f(s).map(Out::variable))),
        }
    }

    fn returns(t: &crate::variable::VariableType) -> Kind {
        match t {
            crate::variable::VariableType::Date => Kind::Date,
            _ => Kind::Dyn,
        }
    }

    fn call(&mut self, kind: &FunctionKind, arguments: &'a [&'a Node<'a>]) -> Option<Code> {
        let function = match kind {
            FunctionKind::Internal(function) => function,
            FunctionKind::Closure(closure) => return self.closure(*closure, arguments),
            FunctionKind::Deprecated(_) => {
                let builtin = Builtins::of(kind)?;
                let returns = FunctionRegistry::get_definition(kind)
                    .map(|f| Self::returns(&f.return_type()))
                    .unwrap_or(Kind::Dyn);
                let codes = arguments.iter().map(|a| self.node(a)).collect::<Option<Vec<_>>>()?;
                return self.generic_call(builtin, returns, codes);
            }
        };
        match (function, arguments) {
            (InternalFunction::Date, []) => {}
            (InternalFunction::Date, [first, ..]) if Const::of(first).is_some() => {}
            (InternalFunction::Date, [text]) => return self.date_of(kind, text, None),
            (InternalFunction::Date, [text, Node::String(zone)]) => {
                let parsed = chrono_tz::Tz::from_str(zone).ok()?;
                return self.date_of(kind, text, Some((parsed, zone)));
            }
            (InternalFunction::Max | InternalFunction::Min, [Node::Array(items)]) if !items.is_empty() => {
                return self.extreme(kind, *function == InternalFunction::Max, items)
            }
            _ => {}
        }
        let builtin = Builtins::of(kind)?;
        let returns = FunctionRegistry::get_definition(kind)
            .map(|f| Self::returns(&f.return_type()))
            .unwrap_or(Kind::Dyn);
        let codes = arguments
            .iter()
            .map(|a| self.node(a))
            .collect::<Option<Vec<_>>>()?;
        let codes = match Self::fast_call(*function, builtin, codes) {
            Ok(code) => return Some(code),
            Err(codes) => codes,
        };
        if returns != Kind::Date {
            match Self::borrowed_call(builtin, codes) {
                Ok(code) => return Some(code),
                Err(codes) => return self.generic_call(builtin, returns, codes),
            }
        }
        self.generic_call(builtin, returns, codes)
    }

    fn const_arg(c: &Const) -> Option<Arg<'_>> {
        Some(match c {
            Const::Null => Arg::Null,
            Const::Bool(b) => Arg::Bool(*b),
            Const::Number(n) => Arg::Num(*n),
            Const::String(t) => Arg::Str(t),
            Const::Date(d) => Arg::Date(Date(*d)),
            Const::Array(_) => return None,
        })
    }

    fn borrowed_call(builtin: &'static crate::lane::builtins::Builtin, codes: Vec<Code>) -> Result<Code, Vec<Code>> {
        let call = move |views: &[Arg]| Builtins::raw(builtin, views).ok().map(Out::variable);
        let mut codes = codes;
        match codes.as_slice() {
            [Code::Load(_)] => {
                let Some(Code::Load(l)) = codes.pop() else {
                    return Err(codes);
                };
                Ok(Code::Val(Box::new(move |s| l.with(s, |v| call(&[Arg::of(v)])))))
            }
            [Code::Load(_), Code::Const(c)] if Self::const_arg(c).is_some() => {
                let (Some(Code::Const(c)), Some(Code::Load(l))) = (codes.pop(), codes.pop()) else {
                    return Err(codes);
                };
                Ok(Code::Val(Box::new(move |s| {
                    l.with(s, |v| call(&[Arg::of(v), Self::const_arg(&c)?]))
                })))
            }
            [Code::Load(_), Code::Load(_)] => {
                let (Some(Code::Load(b)), Some(Code::Load(a))) = (codes.pop(), codes.pop()) else {
                    return Err(codes);
                };
                Ok(Code::Val(Box::new(move |s| {
                    a.with(s, |x| b.with(s, |y| call(&[Arg::of(x), Arg::of(y)])))
                })))
            }
            _ => Err(codes),
        }
    }

    fn fast_call(
        function: InternalFunction,
        builtin: &'static crate::lane::builtins::Builtin,
        codes: Vec<Code>,
    ) -> Result<Code, Vec<Code>> {
        let mut codes = codes;
        let places = match codes.as_slice() {
            [_, Code::Const(Const::Number(n))] => n.to_u32().filter(|p| *p <= 28).map(|p| Some(p as u8)),
            [_] => Some(None),
            _ => None,
        };
        let numeric: Option<fn(i64, u8, u8) -> Option<Sc>> = match (function, places) {
            (InternalFunction::Abs, Some(None)) => Some(|m, s, _| Scaled::abs(m, s)),
            (InternalFunction::Floor, Some(None)) => Some(|m, s, _| Scaled::floor(m, s)),
            (InternalFunction::Ceil, Some(None)) => Some(|m, s, _| Scaled::ceil(m, s)),
            (InternalFunction::Round, Some(_)) => Some(Scaled::round),
            (InternalFunction::Trunc, Some(_)) => Some(Scaled::trunc),
            _ => None,
        };
        if let (Some(op), Some(first)) = (numeric, codes.first()) {
            if first.guardable() {
                let places = places.flatten().unwrap_or(0);
                codes.truncate(1);
                let Some(a) = codes.pop() else {
                    return Err(codes);
                };
                let a = a.num();
                return Ok(Code::Num(Box::new(move |s| {
                    let (m, sc) = a(s)?;
                    op(m, sc, places)
                })));
            }
        }
        match (function, codes.as_slice()) {
            (InternalFunction::Len, [Code::Load(_)]) => {
                let Some(Code::Load(l)) = codes.pop() else {
                    return Err(codes);
                };
                Ok(Code::Val(Box::new(move |s| {
                    l.with(s, |v| match v {
                        Variable::String(t) => Some(Variable::Number(Decimal::from(t.len()))),
                        Variable::Array(a) => Some(Variable::Number(Decimal::from(a.borrow().len()))),
                        other => Builtins::raw(builtin, &[Arg::of(other)]).ok().map(Out::variable),
                    })
                })))
            }
            (
                InternalFunction::StartsWith | InternalFunction::EndsWith | InternalFunction::Contains,
                [Code::Load(_), Code::Const(Const::String(_))],
            ) => {
                let (Some(Code::Const(Const::String(needle))), Some(Code::Load(l))) = (codes.pop(), codes.pop()) else {
                    return Err(codes);
                };
                let test: fn(&str, &str) -> bool = match function {
                    InternalFunction::StartsWith => |a, b| a.starts_with(b),
                    InternalFunction::EndsWith => |a, b| a.ends_with(b),
                    _ => |a, b| a.contains(b),
                };
                Ok(Code::Val(Box::new(move |s| {
                    l.with(s, |v| match v {
                        Variable::String(t) => Some(Variable::Bool(test(t.as_str(), &needle))),
                        other => Builtins::raw(builtin, &[Arg::of(other), Arg::Str(&needle)]).ok().map(Out::variable),
                    })
                })))
            }
            _ => Err(codes),
        }
    }

    fn generic_call(
        &mut self,
        builtin: &'static crate::lane::builtins::Builtin,
        returns: Kind,
        codes: Vec<Code>,
    ) -> Option<Code> {
        let args: Vec<HeldFn> = codes.into_iter().map(Self::held).collect();
        Some(Self::typed_out(
            returns,
            Box::new(move |s| {
                let values = args.iter().map(|f| f(s)).collect::<Option<SmallVec<[Held; 4]>>>()?;
                let views: SmallVec<[Arg; 4]> = values.iter().map(Held::arg).collect();
                Builtins::raw(builtin, &views).ok()
            }),
        ))
    }

    fn date_of(&mut self, kind: &FunctionKind, text: &'a Node<'a>, zone: Option<(chrono_tz::Tz, &str)>) -> Option<Code> {
        let builtin = Builtins::of(kind)?;
        let zone_text: Option<Box<str>> = zone.map(|(_, z)| Box::from(z));
        let zone = zone.map(|(z, _)| z);
        Some(Code::Date(self.node(text)?.view(move |_, v| match v {
            Variable::String(t) => Some(Date::text(t.as_str(), zone)),
            other => {
                let mut views: SmallVec<[Arg; 2]> = SmallVec::new();
                views.push(Arg::of(other));
                if let Some(z) = &zone_text {
                    views.push(Arg::Str(z));
                }
                match Builtins::raw(builtin, &views).ok()? {
                    Out::Date(d) => Some(d),
                    Out::Var(v) => Date::of(&v).filter(|_| !Date::sourced(&v)),
                    _ => None,
                }
            }
        })))
    }

    fn extreme(&mut self, kind: &FunctionKind, largest: bool, items: &'a [&'a Node<'a>]) -> Option<Code> {
        let builtin = Builtins::of(kind)?;
        let items = items
            .iter()
            .map(|n| self.node(n).map(Code::val))
            .collect::<Option<Vec<_>>>()?;
        Some(Code::Val(Box::new(move |s| {
            let values = items.iter().map(|f| f(s)).collect::<Option<SmallVec<[Variable; 4]>>>()?;
            let mut best: Option<Sc> = None;
            let mut fast = true;
            for v in &values {
                let Some(x) = (match v {
                    Variable::Number(n) => Scaled::parts(n),
                    _ => None,
                }) else {
                    fast = false;
                    break;
                };
                best = match best {
                    None => Some(x),
                    Some(b) => {
                        let order = Scaled::compare(x, b)?;
                        Some(match largest {
                            true if order.is_ge() => x,
                            false if order.is_lt() => x,
                            _ => b,
                        })
                    }
                };
            }
            if let (true, Some((m, sc))) = (fast, best) {
                return Some(Variable::Number(Scaled::decimal(m, sc)));
            }
            let array = Variable::from_array(values.into_vec());
            Builtins::raw(builtin, &[Arg::of(&array)]).ok().map(Out::variable)
        })))
    }

    fn method(&mut self, kind: &MethodKind, this: &'a Node<'a>, arguments: &'a [&'a Node<'a>]) -> Option<Code> {
        let MethodKind::DateMethod(method) = kind;
        let builtin = Builtins::method(kind);
        let returns = MethodRegistry::get_definition(kind)
            .map(|m| Self::returns(&m.return_type()))
            .unwrap_or(Kind::Dyn);
        let this = self.node(this)?;
        let parts = matches!(
            method,
            DateMethod::Second
                | DateMethod::Minute
                | DateMethod::Hour
                | DateMethod::Day
                | DateMethod::Weekday
                | DateMethod::DayOfYear
                | DateMethod::Week
                | DateMethod::Month
                | DateMethod::Quarter
                | DateMethod::Year
                | DateMethod::Timestamp
        );
        if this.dated() && arguments.is_empty() && parts {
            let this = this.date();
            let kind = kind.clone();
            return Some(Code::Num(Box::new(move |s| {
                Dates::part_of(&kind, &this(s)?).map(|n| (n, 0))
            })));
        }
        if this.dated() && !arguments.is_empty() {
            let consts: Option<Vec<Variable>> = arguments.iter().map(|a| Const::of(a).map(|c| c.variable())).collect();
            if let Some(consts) = consts {
                let views: SmallVec<[Arg; 4]> = std::iter::once(Arg::Null).chain(consts.iter().map(Arg::of)).collect();
                if let Some(shift) = Dates::shift(kind, &views) {
                    let this = this.date();
                    return Some(Code::Date(Box::new(move |s| Some(shift.apply(&this(s)?)))));
                }
            }
        }
        if let [other] = arguments {
            let probe = Date(None);
            if this.dated() && Dates::order(kind, &probe, &probe).is_some() {
                let other = self.node(other)?;
                if other.dated() {
                    let (this, other, kind) = (this.date(), other.date(), kind.clone());
                    return Some(Code::Bool(Box::new(move |s| {
                        Dates::order(&kind, &this(s)?, &other(s)?)
                    })));
                }
                return self.method_call(kind, builtin, returns, this, std::iter::once(other).collect());
            }
        }
        let args = arguments
            .iter()
            .map(|a| self.node(a))
            .collect::<Option<Vec<_>>>()?;
        self.method_call(kind, builtin, returns, this, args)
    }

    fn method_call(
        &mut self,
        kind: &MethodKind,
        builtin: &'static crate::lane::builtins::Builtin,
        returns: Kind,
        this: Code,
        args: Vec<Code>,
    ) -> Option<Code> {
        let kind = kind.clone();
        let this = Self::held(this);
        let args: Vec<HeldFn> = args.into_iter().map(Self::held).collect();
        Some(Self::typed_out(
            returns,
            Box::new(move |s| {
                let mut values: SmallVec<[Held; 4]> = SmallVec::new();
                values.push(this(s)?);
                for f in &args {
                    values.push(f(s)?);
                }
                let views: SmallVec<[Arg; 4]> = values.iter().map(Held::arg).collect();
                Builtins::call_method(builtin, &kind, &views).ok()
            }),
        ))
    }

    fn binary(&mut self, left: &'a Node<'a>, operator: Operator, right: &'a Node<'a>) -> Option<Code> {
        match operator {
            Operator::Logical(LogicalOperator::And) => {
                let a = self.node(left)?.boolean();
                let b = self.node(right)?;
                Some(match b.boolean_typed() {
                    true => {
                        let b = b.boolean();
                        Code::Bool(Box::new(move |s| if a(s)? { b(s) } else { Some(false) }))
                    }
                    false => {
                        let b = b.val();
                        Code::Val(Box::new(move |s| if a(s)? { b(s) } else { Some(Variable::Bool(false)) }))
                    }
                })
            }
            Operator::Logical(LogicalOperator::Or) => {
                let a = self.node(left)?.boolean();
                let b = self.node(right)?;
                Some(match b.boolean_typed() {
                    true => {
                        let b = b.boolean();
                        Code::Bool(Box::new(move |s| if a(s)? { Some(true) } else { b(s) }))
                    }
                    false => {
                        let b = b.val();
                        Code::Val(Box::new(move |s| if a(s)? { Some(Variable::Bool(true)) } else { b(s) }))
                    }
                })
            }
            Operator::Logical(LogicalOperator::NullishCoalescing) => {
                let a = self.node(left)?;
                if a.numeric() || a.boolean_typed() {
                    return Some(a);
                }
                let (a, b) = (a.val(), self.node(right)?.val());
                Some(Code::Val(Box::new(move |s| match a(s)? {
                    Variable::Null => b(s),
                    v => Some(v),
                })))
            }
            Operator::Comparison(ComparisonOperator::Equal) => self.equal(left, right, false),
            Operator::Comparison(ComparisonOperator::NotEqual) => self.equal(left, right, true),
            Operator::Comparison(ComparisonOperator::In) => self.member(left, right, false),
            Operator::Comparison(ComparisonOperator::NotIn) => self.member(left, right, true),
            Operator::Comparison(c) => {
                let order = match c {
                    ComparisonOperator::LessThan => Compare::Less,
                    ComparisonOperator::LessThanOrEqual => Compare::LessOrEqual,
                    ComparisonOperator::GreaterThan => Compare::More,
                    ComparisonOperator::GreaterThanOrEqual => Compare::MoreOrEqual,
                    _ => return None,
                };
                self.compare(left, right, order)
            }
            Operator::Arithmetic(op) => self.arithmetic(left, op, right),
            _ => None,
        }
    }

    fn negated(code: BoolFn, not: bool) -> Code {
        match not {
            true => Code::Bool(Box::new(move |s| code(s).map(|b| !b))),
            false => Code::Bool(code),
        }
    }

    fn equal(&mut self, left: &'a Node<'a>, right: &'a Node<'a>, not: bool) -> Option<Code> {
        let (a, b) = (self.node(left)?, self.node(right)?);
        let test: BoolFn = match (a, b) {
            (x, y) if x.dated() && y.dated() => {
                let (x, y) = (x.date(), y.date());
                Box::new(move |s| {
                    let (p, q) = (x(s)?, y(s)?);
                    Some(p == q)
                })
            }
            (x, Code::Const(c)) | (Code::Const(c), x) if !matches!(c, Const::Array(_) | Const::Date(_)) => {
                Self::eq_const(x, c)
            }
            (x, y) if x.numeric() && y.numeric() => {
                let (x, y) = (x.num(), y.num());
                Box::new(move |s| Scaled::compare(x(s)?, y(s)?).map(|o| o.is_eq()))
            }
            (x, y) => {
                let y = y.val();
                x.view(move |s, a| {
                    let b = y(s)?;
                    Some(Ops::equal(a, &b))
                })
            }
        };
        Some(Self::negated(test, not))
    }

    fn eq_const(x: Code, c: Const) -> BoolFn {
        match c {
            Const::Number(k) => match (x.numeric(), Scaled::parts(&k)) {
                (true, Some(k)) => {
                    let x = x.num();
                    Box::new(move |s| Scaled::compare(x(s)?, k).map(|o| o.is_eq()))
                }
                _ => x.view(move |_, v| {
                    Some(match v {
                        Variable::Number(n) => Ops::same_number(n, &k),
                        _ => false,
                    })
                }),
            },
            Const::String(t) => x.view(move |_, v| {
                Some(match v {
                    Variable::String(s) => s.as_str() == t.as_ref(),
                    Variable::Dynamic(_) => Ops::equal(v, &Variable::String(Symbol::from(t.as_ref()))),
                    _ => false,
                })
            }),
            Const::Bool(b) => match x.boolean_typed() {
                true => {
                    let x = x.boolean();
                    Box::new(move |s| x(s).map(|v| v == b))
                }
                false => x.view(move |_, v| Some(matches!(v, Variable::Bool(v) if *v == b))),
            },
            Const::Null => x.view(|_, v| Some(matches!(v, Variable::Null))),
            other => x.view(move |_, v| Some(Ops::equal(v, &other.variable()))),
        }
    }

    fn compare(&mut self, left: &'a Node<'a>, right: &'a Node<'a>, order: Compare) -> Option<Code> {
        let (a, b) = (self.node(left)?, self.node(right)?);
        if a.dated() && b.dated() {
            let (a, b) = (a.date(), b.date());
            return Some(Code::Bool(Box::new(move |s| {
                let (x, y) = (a(s)?, b(s)?);
                (x.0.is_some() && y.0.is_some()).then(|| Ops::ordered(&x, &y, order))
            })));
        }
        let test = NumCmp::Order(order);
        if a.numeric() && b.numeric() {
            let f: BoolFn = match (a.scaled(), b.scaled()) {
                (_, Some(k)) => {
                    let a = a.num();
                    Box::new(move |s| Scaled::compare(a(s)?, k).map(|o| test.test(o)))
                }
                (Some(k), _) => {
                    let b = b.num();
                    Box::new(move |s| Scaled::compare(k, b(s)?).map(|o| test.test(o)))
                }
                _ => {
                    let (a, b) = (a.num(), b.num());
                    Box::new(move |s| Scaled::compare(a(s)?, b(s)?).map(|o| test.test(o)))
                }
            };
            return Some(Code::Bool(f));
        }
        let f: BoolFn = match (a, b) {
            (x, Code::Const(Const::Number(k))) => x.view(move |_, v| match v {
                Variable::Number(n) => Some(Ops::ordered_number(n, &k, order)),
                other => Ops::compare(other, &Variable::Number(k), order).ok(),
            }),
            (Code::Const(Const::Number(k)), y) => y.view(move |_, v| match v {
                Variable::Number(n) => Some(Ops::ordered_number(&k, n, order)),
                other => Ops::compare(&Variable::Number(k), other, order).ok(),
            }),
            (x, y) => {
                let y = y.val();
                x.view(move |s, a| {
                    let b = y(s)?;
                    Ops::compare(a, &b, order).ok()
                })
            }
        };
        Some(Code::Bool(f))
    }

    fn member(&mut self, left: &'a Node<'a>, right: &'a Node<'a>, not: bool) -> Option<Code> {
        if let Node::Interval {
            left: lo,
            right: hi,
            left_bracket,
            right_bracket,
        } = right
        {
            if let (Some(lo), Some(hi)) = (Const::number(lo), Const::number(hi)) {
                let interval = Interval {
                    left_bracket: *left_bracket,
                    right_bracket: *right_bracket,
                    left: IntervalData::Number(lo),
                    right: IntervalData::Number(hi),
                };
                let a = self.node(left)?;
                let test: BoolFn = match a.numeric() {
                    true => {
                        let a = a.num();
                        Box::new(move |s| {
                            let (m, sc) = a(s)?;
                            interval.includes(IntervalData::Number(Scaled::decimal(m, sc))).ok()
                        })
                    }
                    false => a.view(move |_, v| match v {
                        Variable::Number(n) => interval.includes(IntervalData::Number(*n)).ok(),
                        _ => None,
                    }),
                };
                return Some(Self::negated(test, not));
            }
        }
        if let (Node::Array(_), Some(Const::Array(items))) = (right, Const::of(right)) {
            if !items.iter().any(|i| matches!(i, Const::Date(_))) {
                let members = Members::of(&items);
                let a = self.node(left)?;
                let test: BoolFn = match (a.numeric(), items.iter().all(|i| matches!(i, Const::Number(_)))) {
                    (true, true) => {
                        let keys: Vec<Sc> = members.nums.iter().filter_map(Scaled::parts).collect();
                        match keys.len() == members.nums.len() {
                            true => {
                                let a = a.num();
                                Box::new(move |s| {
                                    let x = a(s)?;
                                    keys.iter()
                                        .try_fold(false, |hit, k| Some(hit || Scaled::compare(x, *k)?.is_eq()))
                                })
                            }
                            false => a.view(move |_, v| members.test(v)),
                        }
                    }
                    _ => a.view(move |_, v| members.test(v)),
                };
                return Some(Self::negated(test, not));
            }
        }
        let (a, b) = (self.node(left)?, self.node(right)?.val());
        let test = a.view(move |s, x| {
            let y = b(s)?;
            Ops::membership(x.clone(), &y).ok()
        });
        Some(Self::negated(test, not))
    }

    fn wide(op: NumOp, a: Sc, b: Sc) -> Option<Sc> {
        let (x, y) = (Scaled::decimal(a.0, a.1), Scaled::decimal(b.0, b.1));
        let r = match op {
            NumOp::Add => x.checked_add(y),
            NumOp::Subtract => x.checked_sub(y),
            NumOp::Multiply => x.checked_mul(y),
            NumOp::Divide | NumOp::Modulo => None,
        }?;
        Scaled::parts(&r)
    }

    fn arithmetic(&mut self, left: &'a Node<'a>, op: ArithmeticOperator, right: &'a Node<'a>) -> Option<Code> {
        if op == ArithmeticOperator::Add && Self::textual(left, right) {
            let mut parts = Vec::new();
            self.addends(left, &mut parts)?;
            parts.push(self.node(right)?.val());
            return Some(Code::Val(Self::concat(parts)));
        }
        let (a, b) = (self.node(left)?, self.node(right)?);
        let op = match op {
            ArithmeticOperator::Add => NumOp::Add,
            ArithmeticOperator::Subtract => NumOp::Subtract,
            ArithmeticOperator::Multiply => NumOp::Multiply,
            ArithmeticOperator::Divide => NumOp::Divide,
            ArithmeticOperator::Modulus => NumOp::Modulo,
            ArithmeticOperator::Power => {
                let (a, b) = (a.val(), b.val());
                return Some(Code::Val(Box::new(move |s| Ops::exponent(a(s)?, b(s)?).ok())));
            }
        };
        let typed = match op {
            NumOp::Divide | NumOp::Modulo => false,
            NumOp::Add => (a.numeric() || b.numeric()) && a.guardable() && b.guardable(),
            _ => a.guardable() && b.guardable(),
        };
        if !typed {
            let (a, b) = (a.val(), b.val());
            return Some(Code::Val(Box::new(move |s| Self::boxed(op, a(s)?, b(s)?))));
        }
        let f: NumFn = match (a.scaled(), b.scaled()) {
            (_, Some(k)) => {
                let a = a.num();
                Box::new(move |s| {
                    let x = a(s)?;
                    op.scaled(x, k).or_else(|| Self::wide(op, x, k))
                })
            }
            (Some(k), _) => {
                let b = b.num();
                Box::new(move |s| {
                    let y = b(s)?;
                    op.scaled(k, y).or_else(|| Self::wide(op, k, y))
                })
            }
            _ => {
                let (a, b) = (a.num(), b.num());
                Box::new(move |s| {
                    let (x, y) = (a(s)?, b(s)?);
                    op.scaled(x, y).or_else(|| Self::wide(op, x, y))
                })
            }
        };
        Some(Code::Num(f))
    }

    fn textual(left: &Node, right: &Node) -> bool {
        let text = |n: &Node| matches!(n, Node::String(_) | Node::TemplateString(_));
        let mut node = left;
        let mut any = text(right);
        while let Node::Binary {
            left,
            operator: Operator::Arithmetic(ArithmeticOperator::Add),
            right,
        } = node
        {
            any |= text(right);
            node = left;
        }
        any || text(node)
    }

    fn addends(&mut self, node: &'a Node<'a>, out: &mut Vec<ValFn>) -> Option<()> {
        match node {
            Node::Binary {
                left,
                operator: Operator::Arithmetic(ArithmeticOperator::Add),
                right,
            } if self.folder.fold_value(node).is_none() => {
                self.depth += 1;
                let done = match self.depth > Kernel::DEPTH {
                    true => None,
                    false => self.addends(left, out),
                };
                self.depth -= 1;
                done?;
                out.push(self.node(right)?.val());
                Some(())
            }
            other => {
                out.push(self.node(other)?.val());
                Some(())
            }
        }
    }

    fn concat(parts: Vec<ValFn>) -> ValFn {
        Box::new(move |s| {
            let values = parts.iter().map(|f| f(s)).collect::<Option<SmallVec<[Variable; 6]>>>()?;
            if values.iter().all(|v| matches!(v, Variable::String(_))) {
                let mut out = String::new();
                for v in &values {
                    if let Variable::String(t) = v {
                        out.push_str(t.as_str());
                    }
                }
                return Some(Variable::String(Symbol::from(out.as_str())));
            }
            let mut values = values.into_iter();
            let first = values.next()?;
            values.try_fold(first, |acc, v| Ops::add(acc, v).ok())
        })
    }

    #[inline]
    fn boxed(op: NumOp, x: Variable, y: Variable) -> Option<Variable> {
        if let (Variable::Number(p), Variable::Number(q)) = (&x, &y) {
            if let Some((m, sc)) = Scaled::parts(p).zip(Scaled::parts(q)).and_then(|(u, v)| op.scaled(u, v)) {
                return Some(Variable::Number(Scaled::decimal(m, sc)));
            }
        }
        match op {
            NumOp::Add => Ops::add(x, y),
            NumOp::Subtract => Ops::subtract(x, y),
            NumOp::Multiply => Ops::multiply(x, y),
            NumOp::Divide => Ops::divide(x, y),
            NumOp::Modulo => Ops::modulo(x, y),
        }
        .ok()
    }
}
