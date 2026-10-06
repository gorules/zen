use crate::compiler::{Compare, CompilerError, FetchFastTarget};
use crate::functions::registry::FunctionRegistry;
use crate::functions::{ClosureFunction, FunctionKind, InternalFunction, MethodRegistry};
use crate::intellisense::type_provider::TypesProvider;
use crate::lane::ops::Ops;
use crate::lane::program::{
    Binary, ClosureOp, Const, Input, Kind, Layout, Load, LoadCall, MaskId, NumCmp, NumOp,
    ObjectKey, Op, Operand, Program, Reg, Step,
};
use crate::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use crate::parser::Node;
use crate::lane::date::Date;
use crate::variable::{Variable, VariableType};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::sync::Arc;

type Result<T> = std::result::Result<T, CompilerError>;

#[derive(Default)]
struct Builder {
    program: Program,
    free: [Vec<Reg>; 6],
    pinned: Vec<Reg>,
    fixed: Vec<Reg>,
    mask: MaskId,
    imports: Vec<(Reg, Reg)>,
    element_key: Option<String>,
}

impl Builder {
    fn alloc(&mut self, kind: Kind) -> Reg {
        match self.free[kind as usize].pop() {
            Some(r) => r,
            None => self.fresh(kind),
        }
    }

    fn fresh(&mut self, kind: Kind) -> Reg {
        self.program.regs += 1;
        self.program.kinds.push(kind);
        self.program.regs - 1
    }

    fn kind(&self, r: Reg) -> Kind {
        self.program.kinds[r as usize]
    }

    fn pin(&mut self, kind: Kind) -> Reg {
        let r = self.fresh(kind);
        self.pinned.push(r);
        self.fixed.push(r);
        r
    }

    fn seal(mut self) -> Program {
        self.program.pinned = vec![false; self.program.regs as usize];
        for r in &self.pinned {
            self.program.pinned[*r as usize] = true;
        }
        self.program.fixed = vec![false; self.program.regs as usize];
        for r in &self.fixed {
            self.program.fixed[*r as usize] = true;
        }
        self.program
    }

    fn release(&mut self, r: Reg) {
        let free = &mut self.free[self.program.kinds[r as usize] as usize];
        if !self.pinned.contains(&r) && !free.contains(&r) {
            free.push(r);
        }
    }

    fn mask(&mut self) -> MaskId {
        self.program.masks += 1;
        self.program.masks
    }

    fn emit(&mut self, op: Op) {
        self.program.steps.push(Step {
            mask: self.mask,
            op,
        });
    }
}

pub type Hints = ahash::HashMap<String, Kind>;

pub struct LaneCompiler<'a> {
    stack: Vec<Builder>,
    aliases: Vec<Option<&'a str>>,
    writes_env: bool,
    hints: Option<&'a Hints>,
    types: Option<&'a TypesProvider>,
    keys: Vec<String>,
    site_keys: Vec<Option<String>>,
    shared: Option<Vec<(String, Reg)>>,
    dollar: Option<Vec<(String, Reg)>>,
    folding: bool,
    pure: ahash::HashMap<usize, bool>,
    unresolved: bool,
}

impl<'a> LaneCompiler<'a> {
    pub fn compile(root: &'a Node<'a>) -> Result<Program> {
        Self::compile_with(root, None)
    }

    pub fn compile_with(root: &'a Node<'a>, hints: Option<&'a Hints>) -> Result<Program> {
        Self::compile_typed(root, hints, None)
    }

    pub(crate) fn compile_typed(
        root: &'a Node<'a>,
        hints: Option<&'a Hints>,
        types: Option<&'a TypesProvider>,
    ) -> Result<Program> {
        Self::build(root, hints, types, false)
    }

    fn build(
        root: &'a Node<'a>,
        hints: Option<&'a Hints>,
        types: Option<&'a TypesProvider>,
        folding: bool,
    ) -> Result<Program> {
        let mut compiler = LaneCompiler {
            stack: vec![Builder::default()],
            aliases: Vec::new(),
            writes_env: false,
            hints,
            types,
            keys: Vec::new(),
            site_keys: Vec::new(),
            shared: None,
            dollar: None,
            folding,
            pure: Default::default(),
            unresolved: false,
        };
        let (out, layout) = match Self::shaped(root) {
            true => {
                let layout = compiler.layout(root, &mut Vec::new())?;
                (Reg::default(), Some(layout))
            }
            false => (compiler.node(root)?, None),
        };
        let mut builder = compiler.stack.pop().unwrap_or_default();
        builder.program.out = out;
        builder.program.layout = layout;
        builder.program.writes_env = compiler.writes_env;
        let mut program = builder.seal();
        program.keys = compiler.keys;
        let sites = compiler.site_keys.len() as u16;
        program.site_keys = compiler.site_keys;
        program.finish(sites);
        Ok(program)
    }

    fn shaped(node: &Node) -> bool {
        match node {
            Node::Array(_) => true,
            Node::Object(pairs) => {
                let keys: Vec<&str> = pairs
                    .iter()
                    .filter_map(|(k, _)| match k {
                        Node::String(k) => Some(*k),
                        _ => None,
                    })
                    .collect();
                keys.len() == pairs.len()
                    && keys.iter().enumerate().all(|(i, k)| !keys[..i].contains(k))
            }
            _ => false,
        }
    }

    fn layout(&mut self, node: &'a Node<'a>, used: &mut Vec<Reg>) -> Result<Layout> {
        if !Self::shaped(node) {
            let reg = self.node(node)?;
            let reg = match !self.top().pinned.contains(&reg) && used.contains(&reg) {
                true => {
                    let kind = self.top().kind(reg);
                    let copy = self.top().fresh(kind);
                    self.emit(Op::Move {
                        dst: copy,
                        src: reg,
                    });
                    copy
                }
                false => reg,
            };
            used.push(reg);
            return Ok(Layout::Value(reg));
        }
        match node {
            Node::Array(items) => Ok(Layout::List(
                items
                    .iter()
                    .map(|n| self.layout(n, used))
                    .collect::<Result<Vec<_>>>()?
                    .into(),
            )),
            Node::Object(pairs) => {
                let mut fields = Vec::with_capacity(pairs.len());
                for (key, value) in pairs.iter() {
                    let Node::String(key) = key else {
                        return Err(CompilerError::UnexpectedErrorNode);
                    };
                    fields.push((Arc::from(*key), self.layout(value, used)?));
                }
                Ok(Layout::Struct(fields.into()))
            }
            _ => Err(CompilerError::UnexpectedErrorNode),
        }
    }

    pub fn compile_many(
        roots: &[(&'a str, &'a Node<'a>)],
        chain: bool,
        hints: Option<&'a Hints>,
    ) -> Result<Program> {
        let assigns = roots.iter().any(|(_, n)| Self::assigns(n));
        let nested = roots.iter().any(|(a, _)| {
            roots
                .iter()
                .any(|(b, _)| b.strip_prefix(*a).is_some_and(|rest| rest.starts_with('.')))
        });
        let resolvable = chain
            && !assigns
            && !nested
            && roots.iter().all(|(_, n)| Self::dollar_static(n, false));
        match Self::compile_entries(roots, chain, hints, assigns, resolvable) {
            Ok(program) => Ok(program),
            Err(CompilerError::UnexpectedErrorNode) if resolvable => {
                Self::compile_entries(roots, chain, hints, assigns, false)
            }
            Err(e) => Err(e),
        }
    }

    pub fn compile_isolated(roots: &[(&'a str, &'a Node<'a>)], hints: Option<&'a Hints>) -> Result<Program> {
        let mut program = Self::compile_entries(roots, false, hints, true, false)?;
        program.isolated = true;
        Ok(program)
    }

    fn compile_entries(
        roots: &[(&'a str, &'a Node<'a>)],
        chain: bool,
        hints: Option<&'a Hints>,
        assigns: bool,
        resolvable: bool,
    ) -> Result<Program> {
        let dynamic = chain && !resolvable;
        let mut compiler = LaneCompiler {
            stack: vec![Builder::default()],
            aliases: Vec::new(),
            writes_env: dynamic,
            hints,
            types: None,
            keys: Vec::new(),
            site_keys: Vec::new(),
            shared: (!assigns).then(Vec::new),
            dollar: resolvable.then(Vec::new),
            folding: false,
            pure: Default::default(),
            unresolved: false,
        };
        let mut outputs = Vec::with_capacity(roots.len());
        for (index, (key, root)) in roots.iter().enumerate() {
            if dynamic && assigns && index > 0 {
                compiler.emit(Op::Rewind);
            }
            compiler.emit(Op::Stage {
                index: index as u16,
            });
            let out = compiler.node(root)?;
            if compiler.unresolved {
                return Err(CompilerError::UnexpectedErrorNode);
            }
            let out = match compiler.top().pinned.contains(&out) {
                true => {
                    let kind = compiler.top().kind(out);
                    let copy = compiler.alloc_kind(kind);
                    compiler.emit(Op::Move {
                        dst: copy,
                        src: out,
                    });
                    copy
                }
                false => out,
            };
            compiler.top().pinned.push(out);
            outputs.push(out);
            if let Some(dollar) = compiler.dollar.as_mut() {
                match dollar.iter_mut().find(|(k, _)| k == key) {
                    Some(entry) => entry.1 = out,
                    None => dollar.push((key.to_string(), out)),
                }
            }
            if dynamic {
                compiler.emit(Op::DollarInsert {
                    key: Arc::from(*key),
                    value: out,
                });
                if let Some(shared) = compiler.shared.as_mut() {
                    shared.retain(|(k, _)| k != "env:$" && !k.starts_with("path:$"));
                }
            }
        }
        if outputs.is_empty() {
            let out = compiler.constant(Const::Null);
            outputs.push(out);
        }
        let mut builder = compiler.stack.pop().unwrap_or_default();
        builder.program.out = outputs.last().copied().unwrap_or_default();
        builder.program.outputs = outputs;
        builder.program.writes_env = compiler.writes_env;
        builder.program.chain = dynamic;
        let mut program = builder.seal();
        program.keys = compiler.keys;
        let sites = compiler.site_keys.len() as u16;
        program.site_keys = compiler.site_keys;
        program.finish(sites);
        Ok(program)
    }

    fn dollar_static(node: &Node, closure: bool) -> bool {
        let mut base = node;
        let mut chain = false;
        let mut dotted = false;
        while let Node::Member {
            node: inner,
            property: Node::String(segment),
        } = base
        {
            dotted |= segment.contains('.') || segment.is_empty();
            base = inner;
            chain = true;
        }
        if chain && matches!(base, Node::Identifier("$")) {
            return !closure && !dotted;
        }
        let ok = std::cell::Cell::new(true);
        match node {
            Node::Identifier("$") | Node::Root => false,
            Node::Closure { body, .. } => Self::dollar_static(body, true),
            Node::Member { node, property } => {
                Self::dollar_static(node, closure) && Self::dollar_static(property, closure)
            }
            Node::Parenthesized(n) | Node::Unary { node: n, .. } => Self::dollar_static(n, closure),
            Node::Binary { left, right, .. } | Node::Interval { left, right, .. } => {
                Self::dollar_static(left, closure) && Self::dollar_static(right, closure)
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                Self::dollar_static(condition, closure)
                    && Self::dollar_static(on_true, closure)
                    && Self::dollar_static(on_false, closure)
            }
            Node::Array(items) | Node::TemplateString(items) => {
                items.iter().all(|n| Self::dollar_static(n, closure))
            }
            Node::Object(pairs) => pairs
                .iter()
                .all(|(k, v)| Self::dollar_static(k, closure) && Self::dollar_static(v, closure)),
            Node::FunctionCall { arguments, .. } => {
                arguments.iter().all(|n| Self::dollar_static(n, closure))
            }
            Node::MethodCall {
                this, arguments, ..
            } => {
                Self::dollar_static(this, closure)
                    && arguments.iter().all(|n| Self::dollar_static(n, closure))
            }
            Node::Slice { node, to, from } => {
                Self::dollar_static(node, closure)
                    && to.is_none_or(|n| Self::dollar_static(n, closure))
                    && from.is_none_or(|n| Self::dollar_static(n, closure))
            }
            Node::Assignments { .. } | Node::Error { .. } => false,
            _ => ok.get(),
        }
    }

    fn dollar_rooted(node: &Node) -> bool {
        match node {
            Node::Member { node, .. } => Self::dollar_rooted(node),
            Node::Identifier(v) => *v == "$",
            _ => false,
        }
    }

    fn resolve_dollar(&mut self, node: &'a Node<'a>) -> Option<Result<Reg>> {
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
            dollar
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, r)| (*r, i))
        });
        let Some((mut reg, used)) = found else {
            let interior = (1..=segments.len()).any(|i| {
                let prefix = format!("{}.", segments[..i].join("."));
                dollar.iter().any(|(k, _)| k.starts_with(&prefix))
            });
            if interior {
                self.unresolved = true;
            }
            return Some(Ok(self.constant(Const::Null)));
        };
        for key in &segments[used..] {
            let site = self.site(None);
            let dst = self.alloc();
            self.emit(Op::Field {
                dst,
                src: reg,
                key: Arc::from(*key),
                site,
            });
            reg = dst;
        }
        if used == segments.len() {
            let kind = self.top().kind(reg);
            let dst = self.alloc_kind(kind);
            self.emit(Op::Move { dst, src: reg });
            reg = dst;
        }
        Some(Ok(reg))
    }

    fn cached_load(&mut self, key: &Option<String>) -> Option<Reg> {
        let key = key.as_ref()?;
        if self.stack.len() != 1 || self.top().mask != 0 {
            return None;
        }
        let shared = self.shared.as_ref()?;
        shared.iter().find(|(k, _)| k == key).map(|(_, r)| *r)
    }

    fn remember_load(&mut self, key: Option<String>, reg: Reg) {
        if self.stack.len() != 1 || self.top().mask != 0 {
            return;
        }
        let (Some(key), true) = (key, self.shared.is_some()) else {
            return;
        };
        self.top().pinned.push(reg);
        if let Some(shared) = self.shared.as_mut() {
            shared.push((key, reg));
        }
    }

    fn top(&mut self) -> &mut Builder {
        let last = self.stack.len() - 1;
        &mut self.stack[last]
    }

    fn alloc(&mut self) -> Reg {
        self.top().alloc(Kind::Dyn)
    }

    fn site(&mut self, key: Option<String>) -> u16 {
        self.site_keys.push(key);
        (self.site_keys.len() - 1) as u16
    }

    fn path_key(path: &[FetchFastTarget]) -> Option<String> {
        let mut key = String::new();
        for p in path {
            match p {
                FetchFastTarget::Begin => {}
                FetchFastTarget::String(s) if s.contains('.') => return None,
                FetchFastTarget::String(s) => {
                    if !key.is_empty() {
                        key.push('.');
                    }
                    key.push_str(s);
                }
                _ => return None,
            }
        }
        (!key.is_empty()).then_some(key)
    }

    fn alloc_kind(&mut self, kind: Kind) -> Reg {
        self.top().alloc(kind)
    }

    fn kind(&mut self, r: Reg) -> Kind {
        self.top().kind(r)
    }

    fn hint(&mut self, path: &[FetchFastTarget]) -> Kind {
        let mut key = String::new();
        for p in path {
            match p {
                FetchFastTarget::Begin => {}
                FetchFastTarget::String(s) => {
                    if !key.is_empty() {
                        key.push('.');
                    }
                    key.push_str(s);
                }
                _ => return Kind::Dyn,
            }
        }
        self.hint_key(key)
    }

    fn hint_key(&mut self, key: String) -> Kind {
        let kind = self
            .hints
            .and_then(|h| h.get(&key).copied())
            .unwrap_or(Kind::Dyn);
        if !self.keys.contains(&key) {
            self.keys.push(key);
        }
        kind
    }

    fn returns(t: &VariableType) -> Kind {
        match t {
            VariableType::Number => Kind::Num,
            VariableType::Bool => Kind::Bool,
            VariableType::String => Kind::Str,
            VariableType::Date => Kind::Date,
            _ => Kind::Dyn,
        }
    }

    fn type_kind(t: &VariableType) -> Option<Kind> {
        match t {
            VariableType::Number => Some(Kind::Num),
            VariableType::Bool => Some(Kind::Bool),
            VariableType::String | VariableType::Const(_) | VariableType::Enum(..) => {
                Some(Kind::Str)
            }
            VariableType::Nullable(inner) => Self::type_kind(inner),
            _ => None,
        }
    }

    fn node_kind(&self, node: &Node) -> Option<Kind> {
        self.types
            .and_then(|t| t.get_type(node))
            .and_then(|t| Self::type_kind(&t.kind))
    }

    fn element_kind(&self, node: &Node) -> Option<Kind> {
        let info = self.types?.get_type(node)?;
        match &info.kind {
            VariableType::Array(inner) => Self::type_kind(inner),
            VariableType::Nullable(inner) => match inner.as_ref() {
                VariableType::Array(inner) => Self::type_kind(inner),
                _ => None,
            },
            _ => None,
        }
    }

    fn pointer_key(&self, depth: u32) -> Option<String> {
        let level = self.stack.len() - 1;
        let target = level.checked_sub(depth as usize).filter(|t| *t >= 1)?;
        self.stack[target].element_key.clone()
    }

    fn node_key(&self, node: &Node) -> Option<String> {
        match node {
            Node::Identifier(v) => match self.lookup_alias(v) {
                Some(depth) => self.pointer_key(depth),
                None => Some(v.to_string()),
            },
            Node::Pointer => self.pointer_key(0),
            Node::Parenthesized(inner) => self.node_key(inner),
            Node::Member {
                node,
                property: Node::String(p),
            } if !p.contains('.') => self.node_key(node).map(|k| format!("{k}.{p}")),
            _ => None,
        }
    }

    fn release(&mut self, r: Reg) {
        self.top().release(r)
    }

    fn release_all(&mut self, regs: &[Reg]) {
        regs.iter().for_each(|r| self.release(*r));
    }

    fn emit(&mut self, op: Op) {
        self.top().emit(op)
    }

    fn under<T>(&mut self, mask: MaskId, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let saved = std::mem::replace(&mut self.top().mask, mask);
        let out = f(self);
        self.top().mask = saved;
        out
    }

    fn const_id(&mut self, value: Const) -> u16 {
        let consts = &mut self.top().program.consts;
        match consts.iter().position(|c| c.same(&value)) {
            Some(i) => i as u16,
            None => {
                consts.push(value);
                (consts.len() - 1) as u16
            }
        }
    }

    fn constant(&mut self, value: Const) -> Reg {
        let kind = match value {
            Const::Number(_) => Kind::Num,
            Const::Bool(_) => Kind::Bool,
            Const::String(_) => Kind::Str,
            Const::Date(_) => Kind::Date,
            _ => Kind::Dyn,
        };
        let id = self.const_id(value);
        let dst = match kind {
            Kind::Str | Kind::Dyn => self.top().pin(kind),
            _ => self.alloc_kind(kind),
        };
        self.emit(Op::Const { dst, id });
        dst
    }

    fn load_of(&self, node: &'a Node<'a>) -> Option<(Load, Option<String>)> {
        if self.dollar.is_some() && Self::dollar_rooted(node) {
            return None;
        }
        match node {
            Node::Identifier(v) if self.lookup_alias(v).is_none() => {
                Some((Load::Env(Arc::from(*v)), Some(v.to_string())))
            }
            Node::Member { .. } => self
                .member_fast(node)
                .filter(|p| !matches!(p.first(), Some(FetchFastTarget::Root)))
                .map(|p| {
                    let key = Self::path_key(&p);
                    (Load::Path(p.into()), key)
                }),
            _ => None,
        }
    }

    fn fail(&mut self, error: crate::vm::VMError) -> Reg {
        let dst = self.alloc();
        self.emit(Op::Fail { error });
        dst
    }

    fn unary(&mut self, a: Reg, kind: Kind, make: impl FnOnce(Reg, Reg) -> Op) -> Reg {
        self.release(a);
        let dst = self.alloc_kind(kind);
        self.emit(make(dst, a));
        dst
    }

    fn result_kind(op: Binary) -> Kind {
        match op {
            Binary::Add => Kind::Dyn,
            Binary::Subtract
            | Binary::Multiply
            | Binary::Divide
            | Binary::Modulo
            | Binary::Exponent => Kind::Num,
            Binary::Equal | Binary::In | Binary::Compare(_) => Kind::Bool,
        }
    }

    fn binary(&mut self, op: Binary, a: Reg, b: Reg) -> Reg {
        self.release(a);
        self.release(b);
        let dst = self.alloc_kind(Self::result_kind(op));
        self.emit(Op::Binary { dst, op, a, b });
        dst
    }

    fn num_operand(&mut self, node: &'a Node<'a>) -> Result<Operand> {
        match Const::number(node) {
            Some(n) => Ok(Operand::Num(n)),
            None => Ok(Operand::Reg(self.node(node)?)),
        }
    }

    fn numeric(&mut self, o: Operand) -> bool {
        match o {
            Operand::Num(_) => true,
            Operand::Reg(r) => self.kind(r) == Kind::Num,
        }
    }

    fn materialize(&mut self, o: Operand) -> Reg {
        match o {
            Operand::Reg(r) => r,
            Operand::Num(n) => self.constant(Const::Number(n)),
        }
    }

    fn release_operand(&mut self, o: Operand) {
        if let Operand::Reg(r) = o {
            self.release(r);
        }
    }

    fn typed_binary(&mut self, op: Binary, left: &'a Node<'a>, right: &'a Node<'a>) -> Result<Reg> {
        let num = match op {
            Binary::Add => Some(NumOp::Add),
            Binary::Subtract => Some(NumOp::Subtract),
            Binary::Multiply => Some(NumOp::Multiply),
            Binary::Divide => Some(NumOp::Divide),
            Binary::Modulo => Some(NumOp::Modulo),
            _ => None,
        };
        let cmp = match op {
            Binary::Compare(c) => Some(NumCmp::Order(c)),
            Binary::Equal => Some(NumCmp::Equal),
            _ => None,
        };

        if let (
            Binary::In,
            Node::Interval {
                left: lo,
                right: hi,
                left_bracket,
                right_bracket,
            },
        ) = (op, right)
        {
            if let (Some(lo), Some(hi)) = (Const::number(lo), Const::number(hi)) {
                let a = self.node(left)?;
                self.release(a);
                let dst = self.alloc_kind(Kind::Bool);
                self.emit(Op::InRange {
                    dst,
                    a,
                    lo,
                    hi,
                    left: *left_bracket,
                    right: *right_bracket,
                });
                return Ok(dst);
            }
        }
        if let (Binary::In, Node::Array(_)) = (op, right) {
            if let Some(value) = Const::of(right) {
                let a = self.node(left)?;
                let id = self.const_id(value);
                self.release(a);
                let dst = self.alloc_kind(Kind::Bool);
                self.emit(Op::InConst { dst, a, id });
                return Ok(dst);
            }
        }
        if op == Binary::In {
            if let Some((load, key)) = self.load_of(right) {
                let a = self.node(left)?;
                let site = self.site(key);
                self.release(a);
                let dst = self.alloc_kind(Kind::Bool);
                self.emit(Op::LoadIn { dst, a, load, site });
                return Ok(dst);
            }
        }
        if num.is_none() && cmp.is_none() {
            let a = self.node(left)?;
            let b = self.operand(op, right)?;
            return Ok(self.binary(op, a, b));
        }

        if op == Binary::Equal {
            if let Some(value) = Self::literal(right) {
                return self.compare_const(left, value, false);
            }
        }

        let a = self.num_operand(left)?;
        let b = self.num_operand(right)?;
        if let (Binary::Add, Operand::Reg(x), Operand::Reg(y)) = (op, a, b) {
            if self.kind(x) == Kind::Str && self.kind(y) == Kind::Str {
                self.release(x);
                self.release(y);
                let dst = self.alloc_kind(Kind::Str);
                self.emit(Op::Concat { dst, a: x, b: y });
                return Ok(dst);
            }
        }
        if self.numeric(a)
            && self.numeric(b)
            && !(matches!(a, Operand::Num(_)) && matches!(b, Operand::Num(_)))
        {
            self.release_operand(a);
            self.release_operand(b);
            return Ok(match (num, cmp) {
                (Some(op), _) => {
                    let dst = self.alloc_kind(Kind::Num);
                    self.emit(Op::Num { dst, op, a, b });
                    dst
                }
                (_, Some(op)) => {
                    let dst = self.alloc_kind(Kind::Bool);
                    self.emit(Op::Cmp { dst, op, a, b });
                    dst
                }
                _ => unreachable!(),
            });
        }

        let (a, b) = (self.materialize(a), self.materialize(b));
        Ok(self.binary(op, a, b))
    }

    fn literal(node: &Node) -> Option<Const> {
        if let Some(n) = Const::number(node) {
            return Some(Const::Number(n));
        }
        match node {
            Node::Null => Some(Const::Null),
            Node::Bool(b) => Some(Const::Bool(*b)),
            Node::String(s) => Some(Const::String(Arc::from(*s))),
            _ => None,
        }
    }

    fn compare_const(&mut self, left: &'a Node<'a>, value: Const, not: bool) -> Result<Reg> {
        let fused = match value {
            Const::String(_) => self.load_of(left),
            _ => None,
        };
        match fused {
            Some((load, key)) => {
                let memo = match (&key, &value) {
                    (Some(key), Const::String(text)) if !key.starts_with('$') => Some(format!("eq:{not}:{}:{key}:{text}", text.len())),
                    _ => None,
                };
                if let Some(reg) = self.cached_load(&memo) {
                    return Ok(reg);
                }
                let site = self.site(key);
                let dst = self.alloc_kind(Kind::Bool);
                let id = self.const_id(value);
                self.emit(Op::LoadEq {
                    dst,
                    load,
                    site,
                    id,
                    not,
                });
                self.remember_load(memo, dst);
                Ok(dst)
            }
            None => {
                let a = self.node(left)?;
                Ok(self.eq_const(a, value, not))
            }
        }
    }

    fn eq_const(&mut self, a: Reg, value: Const, not: bool) -> Reg {
        self.release(a);
        let dst = self.alloc_kind(Kind::Bool);
        let id = self.const_id(value.clone());
        self.emit(Op::EqConst {
            dst,
            a,
            value,
            id,
            not,
        });
        dst
    }

    fn lookup_alias(&self, name: &str) -> Option<u32> {
        self.aliases
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, &alias)| {
                alias.and_then(|a| (a == name).then_some((self.aliases.len() - 1 - index) as u32))
            })
    }

    fn pointer(&mut self, depth: u32) -> Reg {
        let level = self.stack.len() - 1;
        let Some(target) = level.checked_sub(depth as usize).filter(|t| *t >= 1) else {
            return self.fail(Ops::error(
                "Pointer",
                format!("Scope depth {} out of bounds", depth),
            ));
        };

        let mut reg = self.stack[target].program.element.unwrap_or_default();
        for j in target + 1..=level {
            let kind = self.stack[j - 1].kind(reg);
            let builder = &mut self.stack[j];
            reg = match builder.imports.iter().find(|(p, _)| *p == reg) {
                Some((_, c)) => *c,
                None => {
                    let c = builder.pin(kind);
                    builder.imports.push((reg, c));
                    c
                }
            };
        }
        reg
    }

    fn member_fast(&self, node: &'a Node<'a>) -> Option<Vec<FetchFastTarget>> {
        match node {
            Node::Root => Some(vec![FetchFastTarget::Root]),
            Node::Identifier("$") if self.dollar.as_ref().is_some_and(|d| !d.is_empty()) => None,
            Node::Identifier(v) => self.lookup_alias(v).is_none().then(|| {
                vec![
                    FetchFastTarget::Begin,
                    FetchFastTarget::String(Arc::from(*v)),
                ]
            }),
            Node::Member { node, property } => {
                let mut path = self.member_fast(node)?;
                match property {
                    Node::String(v) => {
                        path.push(FetchFastTarget::String(Arc::from(*v)));
                        Some(path)
                    }
                    Node::Number(v) => {
                        path.push(FetchFastTarget::Number(v.to_u32()?));
                        Some(path)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn arg(&mut self, node: &'a Node<'a>) -> Result<Input> {
        Ok(match Self::literal(node) {
            Some(value) => Input::Const(self.const_id(value)),
            None => Input::Reg(self.node(node)?),
        })
    }

    fn release_args(&mut self, args: &[Input]) {
        for a in args {
            if let Input::Reg(r) = a {
                self.release(*r);
            }
        }
    }

    fn load_call(&mut self, kind: &FunctionKind, arguments: &[&'a Node<'a>]) -> Option<Reg> {
        let (first, rest) = arguments.split_first()?;
        let literals: Vec<Const> = rest
            .iter()
            .map(|a| Self::literal(a))
            .collect::<Option<_>>()?;
        let (load, key) = self.load_of(first)?;
        let site = self.site(key);
        let args: Box<[u16]> = literals.into_iter().map(|c| self.const_id(c)).collect();
        let returns = FunctionRegistry::get_definition(kind)
            .map(|f| Self::returns(&f.return_type()))
            .unwrap_or(Kind::Dyn);
        let memo = self.top().program.memos;
        self.top().program.memos = memo.checked_add(1)?;
        let loaded = match &load {
            Load::Env(key) => self.hint(&[FetchFastTarget::String(key.clone())]),
            Load::Path(path) => self.hint(path),
        };
        let scratch = self.alloc_kind(self.node_kind(first).unwrap_or(loaded));
        let dst = self.alloc_kind(returns);
        self.release(scratch);
        let call = Op::Call {
            dst,
            kind: kind.clone(),
            args: std::iter::once(Input::Reg(scratch))
                .chain(args.iter().map(|id| Input::Const(*id)))
                .collect(),
        };
        self.emit(Op::LoadCall(Box::new(LoadCall {
            dst,
            load,
            site,
            kind: kind.clone(),
            args,
            memo,
            scratch,
            call,
        })));
        Some(dst)
    }

    fn extreme(
        &mut self,
        kind: &FunctionKind,
        largest: bool,
        items: &[&'a Node<'a>],
    ) -> Result<Reg> {
        let operands = items
            .iter()
            .map(|n| self.num_operand(n))
            .collect::<Result<Vec<_>>>()?;
        if operands.iter().all(|o| self.numeric(*o)) {
            operands.iter().for_each(|o| self.release_operand(*o));
            let dst = self.alloc_kind(Kind::Num);
            self.emit(Op::Extreme {
                dst,
                items: operands.into(),
                largest,
            });
            return Ok(dst);
        }
        let regs: Vec<Reg> = operands.into_iter().map(|o| self.materialize(o)).collect();
        self.release_all(&regs);
        let array = self.alloc();
        self.emit(Op::Array {
            dst: array,
            items: regs.into(),
        });
        Ok(self.call(kind.clone(), vec![Input::Reg(array)]))
    }

    fn call(&mut self, kind: FunctionKind, args: Vec<Input>) -> Reg {
        let listed =
            matches!(args.as_slice(), [Input::Reg(r)] if self.top().kind(*r) == Kind::List);
        self.release_args(&args);
        let returns = match &kind {
            FunctionKind::Internal(InternalFunction::Max | InternalFunction::Min) if listed => {
                Kind::Num
            }
            _ => FunctionRegistry::get_definition(&kind)
                .map(|f| Self::returns(&f.return_type()))
                .unwrap_or(Kind::Dyn),
        };
        let dst = self.alloc_kind(returns);
        self.emit(Op::Call {
            dst,
            kind,
            args: args.into(),
        });
        dst
    }

    fn string(&mut self, a: Reg) -> Reg {
        self.call(
            FunctionKind::Internal(InternalFunction::String),
            vec![Input::Reg(a)],
        )
    }

    fn branch(&mut self, cond: Reg, opcode: &'static str) -> (MaskId, MaskId) {
        let on_true = self.top().mask();
        let on_false = self.top().mask();
        self.emit(Op::Branch {
            cond,
            opcode,
            on_true,
            on_false,
        });
        (on_true, on_false)
    }

    fn merge(&mut self, mask: MaskId, a: Reg, b: Reg) -> Reg {
        self.release(a);
        self.release(b);
        let (ka, kb) = (self.kind(a), self.kind(b));
        let dst = self.alloc_kind(if ka == kb { ka } else { Kind::Dyn });
        self.emit(Op::Merge { dst, mask, a, b });
        dst
    }

    fn fold(&mut self, node: &'a Node<'a>) -> Option<Reg> {
        if self.folding || Self::literal(node).is_some() || !self.pure(node) {
            return None;
        }
        let program = Self::build(node, None, None, true).ok()?;
        let value = match crate::lane::LaneRunner::constant(&program)? {
            Variable::Null => Const::Null,
            Variable::Bool(b) => Const::Bool(b),
            Variable::Number(n) => Const::Number(n),
            Variable::String(s) => Const::String(Arc::from(s.as_str())),
            v @ Variable::Dynamic(_) if !Date::sourced(&v) => Const::Date(Date::of(&v)?.0),
            _ => return None,
        };
        Some(self.constant(value))
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn pure(&mut self, node: &'a Node<'a>) -> bool {
        let key = node as *const Node as usize;
        if let Some(known) = self.pure.get(&key) {
            return *known;
        }
        let pure = match node {
            Node::Null | Node::Bool(_) | Node::Number(_) | Node::String(_) => true,
            Node::Pointer
            | Node::Identifier(_)
            | Node::Root
            | Node::Closure { .. }
            | Node::Assignments { .. }
            | Node::Error { .. } => false,
            Node::Array(items) | Node::TemplateString(items) => {
                items.iter().all(|n| self.pure(n))
            }
            Node::Object(pairs) => pairs.iter().all(|(k, v)| self.pure(k) && self.pure(v)),
            Node::Parenthesized(n) | Node::Unary { node: n, .. } => self.pure(n),
            Node::Member { node, property } => self.pure(node) && self.pure(property),
            Node::Slice { node, from, to } => {
                self.pure(node)
                    && from.is_none_or(|n| self.pure(n))
                    && to.is_none_or(|n| self.pure(n))
            }
            Node::Interval { left, right, .. } | Node::Binary { left, right, .. } => {
                self.pure(left) && self.pure(right)
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => self.pure(condition) && self.pure(on_true) && self.pure(on_false),
            Node::FunctionCall { kind, arguments } => {
                Self::pure_call(kind, arguments) && arguments.iter().all(|n| self.pure(n))
            }
            Node::MethodCall {
                kind,
                this,
                arguments,
            } => {
                let crate::functions::MethodKind::DateMethod(method) = kind;
                let format = match (method, *arguments) {
                    (crate::functions::DateMethod::Format, [Node::String(pattern)]) => {
                        Self::formats(pattern)
                    }
                    (crate::functions::DateMethod::Format, []) => true,
                    (crate::functions::DateMethod::Format, _) => false,
                    _ => true,
                };
                format
                    && !matches!(
                    method,
                    crate::functions::DateMethod::IsToday
                        | crate::functions::DateMethod::IsYesterday
                        | crate::functions::DateMethod::IsTomorrow
                ) && self.pure(this)
                    && arguments.iter().all(|n| self.pure(n))
            }
        };
        self.pure.insert(key, pure);
        pure
    }

    fn formats(pattern: &str) -> bool {
        use chrono::TimeZone;
        let Ok(items) = chrono::format::StrftimeItems::new(pattern).parse_to_owned() else {
            return false;
        };
        let sample = chrono_tz::Tz::UTC.timestamp_opt(0, 0).earliest();
        sample.is_some_and(|date| {
            let mut out = String::new();
            std::fmt::Write::write_fmt(
                &mut out,
                format_args!("{}", date.format_with_items(items.iter())),
            )
            .is_ok()
        })
    }

    fn pure_call(kind: &FunctionKind, arguments: &[&Node]) -> bool {
        match kind {
            FunctionKind::Internal(InternalFunction::Rand) => false,
            FunctionKind::Internal(InternalFunction::Date) => match arguments {
                [Node::Number(_)] | [Node::Number(_), Node::String(_)] => true,
                [Node::String(text)] | [Node::String(text), Node::String(_)] => {
                    Date::parses(text)
                }
                _ => false,
            },
            FunctionKind::Internal(_) => true,
            FunctionKind::Deprecated(_) | FunctionKind::Closure(_) => false,
        }
    }

    fn assigns(node: &Node) -> bool {
        let found = std::cell::Cell::new(false);
        let found_ref = &found;
        node.walk(move |n| {
            if matches!(n, Node::Assignments { .. }) {
                found_ref.set(true);
            }
        });
        found.get()
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn node(&mut self, node: &'a Node<'a>) -> Result<Reg> {
        if self.dollar.is_some() && self.stack.len() == 1 {
            if let Some(resolved) = self.resolve_dollar(node) {
                return resolved;
            }
        }
        if let Some(folded) = self.fold(node) {
            return Ok(folded);
        }
        Ok(match node {
            Node::Null => self.constant(Const::Null),
            Node::Bool(v) => self.constant(Const::Bool(*v)),
            Node::Number(v) => self.constant(Const::Number(*v)),
            Node::String(v) => self.constant(Const::String(Arc::from(*v))),
            Node::Pointer => self.pointer(0),
            Node::Root => {
                let dst = self.alloc();
                self.emit(Op::RootEnv { dst });
                dst
            }
            Node::Array(items) if !items.is_empty() && Const::of(node).is_some() => {
                let id = self.const_id(Const::of(node).unwrap_or(Const::Null));
                let dst = self.alloc_kind(Kind::Dyn);
                self.emit(Op::Const { dst, id });
                dst
            }
            Node::Array(items) => {
                let regs = items
                    .iter()
                    .map(|n| self.node(n))
                    .collect::<Result<Vec<_>>>()?;
                self.release_all(&regs);
                let dst = self.alloc();
                self.emit(Op::Array {
                    dst,
                    items: regs.into(),
                });
                dst
            }
            Node::Object(pairs) => {
                let mut regs = Vec::with_capacity(pairs.len());
                for (key, value) in pairs.iter() {
                    let k = match key {
                        Node::String(k) => ObjectKey::Static(Arc::from(*k)),
                        _ => {
                            let k = self.node(key)?;
                            ObjectKey::Reg(self.string(k))
                        }
                    };
                    let v = self.node(value)?;
                    regs.push((k, v));
                }
                regs.iter().for_each(|(k, v)| {
                    if let ObjectKey::Reg(k) = k {
                        self.release(*k);
                    }
                    self.release(*v);
                });
                let dst = self.alloc();
                self.emit(Op::Object {
                    dst,
                    pairs: regs.into(),
                });
                dst
            }
            Node::Assignments { list, output } => {
                self.writes_env = true;
                let object = self.alloc();
                self.emit(Op::AssignBegin { dst: object });
                for (key, value) in list.iter() {
                    let k = self.node(key)?;
                    let v = self.node(value)?;
                    self.emit(Op::AssignStep {
                        object,
                        key: k,
                        value: v,
                    });
                    self.release(k);
                    self.release(v);
                }
                match output {
                    Some(output) => {
                        let out = self.node(output)?;
                        self.release(object);
                        out
                    }
                    None => object,
                }
            }
            Node::Identifier(v) => match self.lookup_alias(v) {
                Some(depth) => self.pointer(depth),
                None => {
                    let share = Some(format!("env:{v}"));
                    if let Some(reg) = self.cached_load(&share) {
                        return Ok(reg);
                    }
                    let hinted = self.hint(&[FetchFastTarget::String(Arc::from(*v))]);
                    let kind = self.node_kind(node).unwrap_or(hinted);
                    let dst = self.alloc_kind(kind);
                    let site = self.site(Some(v.to_string()));
                    self.emit(Op::Env {
                        dst,
                        key: Arc::from(*v),
                        site,
                    });
                    self.remember_load(share, dst);
                    dst
                }
            },
            Node::Closure { body, alias } => {
                self.aliases.push(*alias);
                let out = self.node(body);
                self.aliases.pop();
                out?
            }
            Node::Parenthesized(v) => self.node(v)?,
            Node::Member { node: n, property } => match self.member_fast(node) {
                Some(path) => {
                    let share = Self::path_key(&path).map(|k| format!("path:{k}"));
                    if let Some(reg) = self.cached_load(&share) {
                        return Ok(reg);
                    }
                    let hinted = self.hint(&path);
                    let kind = self.node_kind(node).unwrap_or(hinted);
                    let dst = self.alloc_kind(kind);
                    let site = self.site(Self::path_key(&path));
                    self.emit(Op::Path {
                        dst,
                        path: path.into(),
                        site,
                    });
                    self.remember_load(share, dst);
                    dst
                }
                None => match property {
                    Node::String(key) => {
                        let hinted = match self.node_key(node) {
                            Some(k) => self.hint_key(k),
                            None => Kind::Dyn,
                        };
                        let kind = self.node_kind(node).unwrap_or(hinted);
                        let src = self.node(n)?;
                        self.release(src);
                        let dst = self.alloc_kind(kind);
                        let site = self.site(None);
                        self.emit(Op::Field {
                            dst,
                            src,
                            key: Arc::from(*key),
                            site,
                        });
                        dst
                    }
                    _ => {
                        let a = self.node(n)?;
                        let b = self.node(property)?;
                        self.release(a);
                        self.release(b);
                        let dst = self.alloc();
                        self.emit(Op::Fetch { dst, a, b });
                        dst
                    }
                },
            },
            Node::TemplateString(parts) => {
                let mut regs = Vec::with_capacity(parts.len());
                for part in parts.iter() {
                    let arg = match part {
                        Node::String(text) => {
                            Input::Const(self.const_id(Const::String(Arc::from(*text))))
                        }
                        _ => {
                            let r = self.node(part)?;
                            Input::Reg(self.string(r))
                        }
                    };
                    regs.push(arg);
                }
                self.release_args(&regs);
                let dst = self.alloc_kind(Kind::Str);
                self.emit(Op::Join {
                    dst,
                    parts: regs.into(),
                });
                dst
            }
            Node::Slice { node, to, from } => {
                let a = self.node(node)?;
                let to = match to {
                    Some(t) => self.node(t)?,
                    None => {
                        let len = self.alloc();
                        self.emit(Op::Len { dst: len, a });
                        let one = self.constant(Const::Number(Decimal::ONE));
                        self.binary(Binary::Subtract, len, one)
                    }
                };
                let from = match from {
                    Some(f) => self.node(f)?,
                    None => self.constant(Const::Number(Decimal::ZERO)),
                };
                self.release_all(&[a, to, from]);
                let dst = self.alloc();
                self.emit(Op::Slice { dst, a, to, from });
                dst
            }
            Node::Interval {
                left,
                right,
                left_bracket,
                right_bracket,
            } => {
                let a = self.node(left)?;
                let b = self.node(right)?;
                self.release_all(&[a, b]);
                let dst = self.alloc();
                self.emit(Op::Interval {
                    dst,
                    a,
                    b,
                    left: *left_bracket,
                    right: *right_bracket,
                });
                dst
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } if Self::literal(on_true).is_some() && Self::literal(on_false).is_some() => {
                let cond = self.node(condition)?;
                let (a, b) = (Self::literal(on_true), Self::literal(on_false));
                let (a, b) = (a.unwrap_or(Const::Null), b.unwrap_or(Const::Null));
                let kind = match (&a, &b) {
                    (Const::Number(_), Const::Number(_)) => Kind::Num,
                    (Const::Bool(_), Const::Bool(_)) => Kind::Bool,
                    (Const::String(_), Const::String(_)) => Kind::Str,
                    _ => Kind::Dyn,
                };
                let (a, b) = (self.const_id(a), self.const_id(b));
                self.release(cond);
                let dst = self.alloc_kind(kind);
                self.emit(Op::SelectConst { dst, cond, a, b });
                dst
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                let cond = self.node(condition)?;
                let (t, f) = self.branch(cond, "JumpIfFalse");
                self.release(cond);
                let a = self.under(t, |c| c.node(on_true))?;
                let b = self.under(f, |c| c.node(on_false))?;
                self.merge(t, a, b)
            }
            Node::Unary { .. } if Const::number(node).is_some() => {
                let n = Const::number(node).unwrap_or_default();
                self.constant(Const::Number(n))
            }
            Node::Unary { node, operator } => {
                let a = self.node(node)?;
                match *operator {
                    Operator::Arithmetic(ArithmeticOperator::Add) => a,
                    Operator::Arithmetic(ArithmeticOperator::Subtract) => {
                        self.unary(a, Kind::Num, |dst, a| Op::Negate { dst, a })
                    }
                    Operator::Logical(LogicalOperator::Not) => {
                        self.unary(a, Kind::Bool, |dst, a| Op::Not { dst, a })
                    }
                    _ => {
                        return Err(CompilerError::UnknownUnaryOperator {
                            operator: operator.to_string(),
                        })
                    }
                }
            }
            Node::Binary {
                left,
                right,
                operator,
            } => self.binary_node(left, *operator, right)?,
            Node::FunctionCall { kind, arguments } => match kind {
                FunctionKind::Internal(_) | FunctionKind::Deprecated(_) => {
                    let function = FunctionRegistry::get_definition(kind).ok_or_else(|| {
                        CompilerError::UnknownFunction {
                            name: kind.to_string(),
                        }
                    })?;
                    let min = function.required_parameters();
                    let max = min + function.optional_parameters();
                    if arguments.len() < min || arguments.len() > max {
                        return Err(CompilerError::InvalidFunctionCall {
                            name: kind.to_string(),
                            message: "Invalid number of arguments".to_string(),
                        });
                    }
                    if let (
                        FunctionKind::Internal(f @ (InternalFunction::Max | InternalFunction::Min)),
                        [node],
                    ) = (kind, arguments)
                    {
                        if let Node::Array(items) = node {
                            if !items.is_empty() {
                                return self.extreme(kind, *f == InternalFunction::Max, items);
                            }
                        }
                    }
                    if let Some(dst) = self.load_call(kind, arguments) {
                        return Ok(dst);
                    }
                    let args = arguments
                        .iter()
                        .map(|a| self.arg(a))
                        .collect::<Result<Vec<_>>>()?;
                    self.call(kind.clone(), args)
                }
                FunctionKind::Closure(closure) => {
                    let argument = |index: usize| {
                        arguments.get(index).copied().ok_or_else(|| {
                            CompilerError::ArgumentNotFound {
                                index,
                                function: kind.to_string(),
                            }
                        })
                    };
                    let list_node = argument(0)?;
                    let element_key = self.node_key(list_node).map(|k| format!("{k}[]"));
                    let source = self
                        .load_of(list_node)
                        .map(|(load, key)| (load, self.site(key)));
                    let list = match source {
                        Some(_) => self.alloc(),
                        None => self.node(list_node)?,
                    };
                    let body = argument(1)?;
                    let (body, alias) = match body {
                        Node::Closure { body, alias } => (*body, *alias),
                        other => (other, None),
                    };
                    let sequential = Self::assigns(body);
                    let hinted = match element_key.clone() {
                        Some(key) => self.hint_key(key),
                        None => Kind::Dyn,
                    };
                    let element_kind = self.element_kind(list_node).unwrap_or(hinted);
                    let mut child = Builder::default();
                    child.program.element = Some(child.pin(element_kind));
                    child.element_key = element_key;
                    self.stack.push(child);
                    self.aliases.push(alias);
                    let out = self.node(body);
                    self.aliases.pop();
                    let mut child = self.stack.pop().unwrap_or_default();
                    child.program.out = out?;
                    let imports: Box<[(Reg, Reg)]> = std::mem::take(&mut child.imports).into();
                    let body = child.seal();
                    self.release(list);
                    let dst = self.alloc_kind(match closure {
                        ClosureFunction::Count => Kind::Num,
                        ClosureFunction::One
                        | ClosureFunction::Some
                        | ClosureFunction::All
                        | ClosureFunction::None => Kind::Bool,
                        ClosureFunction::Map | ClosureFunction::Filter => Kind::List,
                        _ => Kind::Dyn,
                    });
                    self.emit(Op::Closure(Box::new(ClosureOp {
                        kind: *closure,
                        dst,
                        list,
                        source,
                        body,
                        imports,
                        sequential,
                    })));
                    dst
                }
            },
            Node::MethodCall {
                kind,
                this,
                arguments,
            } => {
                let method = MethodRegistry::get_definition(kind).ok_or_else(|| {
                    CompilerError::UnknownFunction {
                        name: kind.to_string(),
                    }
                })?;
                let this = self.node(this)?;
                let min = method.required_parameters() - 1;
                let max = min + method.optional_parameters();
                if arguments.len() < min || arguments.len() > max {
                    return Err(CompilerError::InvalidMethodCall {
                        name: kind.to_string(),
                        message: "Invalid number of arguments".to_string(),
                    });
                }
                let mut args = vec![Input::Reg(this)];
                for a in arguments.iter() {
                    args.push(self.arg(a)?);
                }
                self.release_args(&args);
                let dst = self.alloc_kind(Self::returns(&method.return_type()));
                self.emit(Op::Method {
                    dst,
                    kind: kind.clone(),
                    args: args.into(),
                });
                dst
            }
            Node::Error { .. } => return Err(CompilerError::UnexpectedErrorNode),
        })
    }

    fn equalities(
        &self,
        left: &'a Node<'a>,
        right: &'a Node<'a>,
    ) -> Option<(&'a Node<'a>, Vec<Const>)> {
        let mut leaves = Vec::new();
        Self::or_leaves(left, &mut leaves);
        Self::or_leaves(right, &mut leaves);
        let mut subject: Option<(&'a Node<'a>, String)> = None;
        let mut values = Vec::with_capacity(leaves.len());
        for leaf in leaves {
            let Node::Binary {
                left: l,
                operator: Operator::Comparison(ComparisonOperator::Equal),
                right: r,
            } = leaf
            else {
                return None;
            };
            let key = self.node_key(l)?;
            match &subject {
                Some((_, k)) if *k != key => return None,
                Some(_) => {}
                None => subject = Some((*l, key)),
            }
            values.push(Self::literal(r)?);
        }
        subject.map(|(node, _)| (node, values))
    }

    fn or_leaves(node: &'a Node<'a>, out: &mut Vec<&'a Node<'a>>) {
        match node {
            Node::Binary {
                left,
                operator: Operator::Logical(LogicalOperator::Or),
                right,
            } => {
                Self::or_leaves(left, out);
                Self::or_leaves(right, out);
            }
            Node::Parenthesized(inner) => Self::or_leaves(inner, out),
            other => out.push(other),
        }
    }

    fn operand(&mut self, op: Binary, node: &'a Node<'a>) -> Result<Reg> {
        match (op, node) {
            (Binary::In, Node::Array(_)) => match Const::of(node) {
                Some(value) => Ok(self.constant(value)),
                None => self.node(node),
            },
            _ => self.node(node),
        }
    }

    fn binary_node(
        &mut self,
        left: &'a Node<'a>,
        operator: Operator,
        right: &'a Node<'a>,
    ) -> Result<Reg> {
        let simple = |op: Binary| Some(op);
        let op = match operator {
            Operator::Comparison(ComparisonOperator::Equal) => simple(Binary::Equal),
            Operator::Comparison(ComparisonOperator::In) => simple(Binary::In),
            Operator::Comparison(ComparisonOperator::LessThan) => {
                simple(Binary::Compare(Compare::Less))
            }
            Operator::Comparison(ComparisonOperator::LessThanOrEqual) => {
                simple(Binary::Compare(Compare::LessOrEqual))
            }
            Operator::Comparison(ComparisonOperator::GreaterThan) => {
                simple(Binary::Compare(Compare::More))
            }
            Operator::Comparison(ComparisonOperator::GreaterThanOrEqual) => {
                simple(Binary::Compare(Compare::MoreOrEqual))
            }
            Operator::Arithmetic(ArithmeticOperator::Add) => simple(Binary::Add),
            Operator::Arithmetic(ArithmeticOperator::Subtract) => simple(Binary::Subtract),
            Operator::Arithmetic(ArithmeticOperator::Multiply) => simple(Binary::Multiply),
            Operator::Arithmetic(ArithmeticOperator::Divide) => simple(Binary::Divide),
            Operator::Arithmetic(ArithmeticOperator::Modulus) => simple(Binary::Modulo),
            Operator::Arithmetic(ArithmeticOperator::Power) => simple(Binary::Exponent),
            _ => None,
        };
        if let Operator::Logical(LogicalOperator::Or) = operator {
            if let Some((subject, values)) = self.equalities(left, right) {
                let a = self.node(subject)?;
                let id = self.const_id(Const::Array(values));
                self.release(a);
                let dst = self.alloc_kind(Kind::Bool);
                self.emit(Op::EqAny { dst, a, id });
                return Ok(dst);
            }
        }
        if let Some(op) = op {
            return self.typed_binary(op, left, right);
        }

        Ok(match operator {
            Operator::Comparison(ComparisonOperator::NotEqual) => match Self::literal(right) {
                Some(value) => self.compare_const(left, value, true)?,
                None => {
                    let eq = self.typed_binary(Binary::Equal, left, right)?;
                    self.unary(eq, Kind::Bool, |dst, a| Op::Not { dst, a })
                }
            },
            Operator::Comparison(ComparisonOperator::NotIn) => {
                let is_in = self.typed_binary(Binary::In, left, right)?;
                self.unary(is_in, Kind::Bool, |dst, a| Op::Not { dst, a })
            }
            Operator::Logical(LogicalOperator::Or) => {
                let a = self.node(left)?;
                let (t, f) = self.branch(a, "JumpIfTrue");
                let b = self.under(f, |c| c.node(right))?;
                self.merge(t, a, b)
            }
            Operator::Logical(LogicalOperator::And) => {
                let a = self.node(left)?;
                let (t, f) = self.branch(a, "JumpIfFalse");
                let b = self.under(t, |c| c.node(right))?;
                self.merge(f, a, b)
            }
            Operator::Logical(LogicalOperator::NullishCoalescing) => {
                let a = self.node(left)?;
                if let Some(value) = Const::of(right) {
                    let kind = match (&value, self.kind(a)) {
                        (Const::Number(_), Kind::Num | Kind::Dyn) => Kind::Num,
                        (Const::Bool(_), Kind::Bool | Kind::Dyn) => Kind::Bool,
                        (Const::String(_), Kind::Str | Kind::Dyn) => Kind::Str,
                        (Const::Array(items), Kind::List) if items.is_empty() => Kind::List,
                        _ => Kind::Dyn,
                    };
                    let id = self.const_id(value);
                    self.release(a);
                    let dst = self.alloc_kind(kind);
                    self.emit(Op::Coalesce { dst, a, id });
                    return Ok(dst);
                }
                let null = self.top().mask();
                let other = self.top().mask();
                self.emit(Op::NullBranch { a, null, other });
                let b = self.under(null, |c| c.node(right))?;
                self.merge(other, a, b)
            }
            _ => {
                return Err(CompilerError::UnknownBinaryOperator {
                    operator: operator.to_string(),
                })
            }
        })
    }
}
