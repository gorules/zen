use crate::functions::{ClosureFunction, FunctionKind, InternalFunction};
use crate::lane::Fusion;
use crate::lexer::{ArithmeticOperator, Bracket, ComparisonOperator, Lexer, LogicalOperator, Operator};
use crate::parser::{Node as Ast, Parser};
use crate::typed::hir::{
    Arith, Cmp, Collect, ColumnDef, Dict, DictId, DictKind, Fold, Func, Hir, Kind, ListDef, ListId, Lit, Node, NodeId, Op, Output, Range, Schema, Scope, Ty,
    Unsupported,
};
use crate::typed::num::Dec;
use bumpalo::Bump;
use std::collections::HashMap;
use std::sync::Arc;

type Lowered = Result<NodeId, Unsupported>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Num,
    Bool,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    Column(Class),
    Keyed(Class, Arc<str>),
    Output(usize),
    List { fields: Vec<(Arc<str>, Class)> },
    KeyedList(Arc<str>, Vec<(Arc<str>, Class)>),
    Absent,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Strict,
    Lenient,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRule {
    pub cells: Vec<Option<String>>,
    pub values: Vec<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableSpec {
    pub inputs: Vec<Option<String>>,
    pub outputs: Vec<String>,
    pub rules: Vec<TableRule>,
    pub hit: Hit,
    pub selector: bool,
}

pub trait Resolve {
    fn path(&mut self, path: &str) -> Resolved;
    fn alias(&mut self, field: &str) -> Resolved;
    fn nested(&mut self, chain: &[&str]) -> Resolved {
        let _ = chain;
        Resolved::Unsupported
    }
}

pub struct Lowering {
    schema: Schema,
    nodes: Vec<Node>,
    index: HashMap<Node, NodeId>,
    outputs: Vec<Output>,
    names: Vec<(String, Named)>,
    absent: Vec<String>,
    literal: Option<DictId>,
    classes: Vec<(NodeId, DictId)>,
    member: Option<ListId>,
    units: usize,
    graph: bool,
    local: usize,
}

struct Mark {
    nodes: usize,
    outputs: usize,
    names: usize,
    absent: usize,
    columns: usize,
    fields: Vec<usize>,
    dicts: Vec<usize>,
    literal: Option<DictId>,
    classes: usize,
}

#[derive(Clone, Copy)]
struct Ctx {
    scope: Scope,
    pointer: Option<ListId>,
    item: Option<NodeId>,
    dollar: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seq {
    list: ListId,
    keep: Option<NodeId>,
    value: Option<NodeId>,
    empty: bool,
    pure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Named {
    Node(NodeId),
    Seq(Seq),
}

enum Base<'a> {
    Ident(&'a str),
    Pointer,
    Dollar,
}

impl Class {
    fn ty(self) -> Ty {
        match self {
            Class::Num => Ty::new(Kind::Num(None), true),
            Class::Bool => Ty::new(Kind::Bool, true),
            Class::Text => Ty::new(Kind::Text, true),
        }
    }
}

impl TableRule {
    fn value(&self, at: usize) -> Option<&str> {
        self.values.get(at).and_then(Option::as_deref).filter(|v| !v.trim().is_empty())
    }
}

impl Lowering {
    const DEPTH: u32 = 3;

    pub fn new(member: Option<(&str, &str)>) -> Self {
        let mut schema = Schema::default();
        let member = member.map(|(alias, path)| {
            schema.lists.push(ListDef { path: path.to_string(), alias: Some(alias.to_string()), fields: Vec::new(), parent: None });
            0
        });
        Self {
            schema,
            nodes: Vec::new(),
            index: HashMap::new(),
            outputs: Vec::new(),
            names: Vec::new(),
            absent: Vec::new(),
            literal: None,
            classes: Vec::new(),
            member,
            units: 0,
            graph: false,
            local: 0,
        }
    }

    pub fn graph() -> Self {
        Self { graph: true, ..Self::new(None) }
    }

    pub fn unit(&mut self, entries: &[Fusion], resolve: &mut dyn Resolve) -> Result<(), Unsupported> {
        let mark = self.mark();
        self.local = self.names.len();
        let result = entries.iter().try_for_each(|entry| self.entry(entry, resolve));
        self.settle(mark, result)
    }

    pub fn table(&mut self, spec: &TableSpec, resolve: &mut dyn Resolve) -> Result<(), Unsupported> {
        let mark = self.mark();
        self.local = self.names.len();
        let result = self.tabled(spec, resolve);
        self.settle(mark, result)
    }

    fn settle(&mut self, mark: Mark, result: Result<(), Unsupported>) -> Result<(), Unsupported> {
        match result {
            Ok(()) => self.units += 1,
            Err(_) => self.rollback(mark),
        }
        result
    }

    pub fn units(&self) -> usize {
        self.units
    }

    pub fn outputs(&self) -> usize {
        self.outputs.len()
    }

    pub fn retain(&mut self, mut keep: impl FnMut(usize) -> bool) {
        let mut at = 0;
        self.outputs.retain(|_| {
            at += 1;
            keep(at - 1)
        });
    }

    pub fn finish(mut self) -> Hir {
        self.prune();
        Hir { schema: self.schema, nodes: self.nodes, outputs: self.outputs }
    }

    fn mark(&self) -> Mark {
        Mark {
            nodes: self.nodes.len(),
            outputs: self.outputs.len(),
            names: self.names.len(),
            absent: self.absent.len(),
            columns: self.schema.columns.len(),
            fields: self.schema.lists.iter().map(|l| l.fields.len()).collect(),
            dicts: self.schema.dicts.iter().map(|d| d.values.len()).collect(),
            literal: self.literal,
            classes: self.classes.len(),
        }
    }

    fn rollback(&mut self, mark: Mark) {
        self.nodes.truncate(mark.nodes);
        self.index.retain(|_, id| (*id as usize) < mark.nodes);
        self.outputs.truncate(mark.outputs);
        self.names.truncate(mark.names);
        self.absent.truncate(mark.absent);
        self.schema.columns.truncate(mark.columns);
        self.schema.lists.truncate(mark.fields.len());
        for (list, len) in self.schema.lists.iter_mut().zip(&mark.fields) {
            list.fields.truncate(*len);
        }
        self.schema.dicts.truncate(mark.dicts.len());
        for (dict, len) in self.schema.dicts.iter_mut().zip(&mark.dicts) {
            dict.values.truncate(*len);
        }
        self.literal = mark.literal;
        self.classes.truncate(mark.classes);
    }

    fn prune(&mut self) {
        let mut live = vec![false; self.nodes.len()];
        for o in &self.outputs {
            live[o.node as usize] = true;
            if let Some(c) = o.collect {
                live[c.item as usize] = true;
                if let Some(k) = c.keep {
                    live[k as usize] = true;
                }
            }
        }
        for id in (0..self.nodes.len()).rev() {
            if live[id] {
                for input in self.nodes[id].op.inputs() {
                    live[input as usize] = true;
                }
            }
        }
        let mut remap = vec![0 as NodeId; self.nodes.len()];
        let mut kept = Vec::with_capacity(self.nodes.len());
        for (id, node) in std::mem::take(&mut self.nodes).into_iter().enumerate() {
            if live[id] {
                remap[id] = kept.len() as NodeId;
                kept.push(Node { op: node.op.remap(|x| remap[x as usize]), ty: node.ty, scope: node.scope });
            }
        }
        self.nodes = kept;
        self.index.clear();
        for o in &mut self.outputs {
            o.node = remap[o.node as usize];
            if let Some(c) = &mut o.collect {
                c.item = remap[c.item as usize];
                c.keep = c.keep.map(|k| remap[k as usize]);
            }
        }
    }

    fn ctx(&self) -> Ctx {
        match self.member {
            Some(list) => Ctx { scope: Scope::Elem(list), pointer: None, item: None, dollar: None },
            None => Ctx { scope: Scope::Row, pointer: None, item: None, dollar: None },
        }
    }

    fn entry(&mut self, entry: &Fusion, r: &mut dyn Resolve) -> Result<(), Unsupported> {
        let ctx = self.ctx();
        match entry {
            Fusion::Output { key, source } => self.parsed(source, false, ctx, r, |this, ast, ctx, r| match Self::listy(ast) {
                true => {
                    let seq = this.seq(ast, ctx, r)?;
                    this.collect(key, seq, ctx.scope)
                }
                false => match this.expr(ast, ctx, r) {
                    Ok(id) => this.output(key, id, ctx.scope, true),
                    Err(e) if Self::passable(ast) => match this.seq(ast, ctx, r) {
                        Ok(seq) => this.collect(key, seq, ctx.scope),
                        Err(_) => Err(e),
                    },
                    Err(e) => Err(e),
                },
            }),
            Fusion::Hidden { key, source } => {
                let id = self.parse(source, false, ctx, r)?;
                self.output(key, id, ctx.scope, false)
            }
            Fusion::Rules { .. } => Err(Unsupported::new("rules outside a table")),
        }
    }

    fn tabled(&mut self, spec: &TableSpec, r: &mut dyn Resolve) -> Result<(), Unsupported> {
        if spec.rules.is_empty() || spec.outputs.is_empty() {
            return Err(Unsupported::new("empty table"));
        }
        let ctx = self.ctx();
        let mut targets: Vec<Option<NodeId>> = vec![None; spec.inputs.len()];
        let mut conds = Vec::with_capacity(spec.rules.len());
        for rule in &spec.rules {
            let mut cond: Option<NodeId> = None;
            for (at, cell) in rule.cells.iter().enumerate() {
                let Some(cell) = cell.as_deref().filter(|c| !c.is_empty()) else {
                    continue;
                };
                if cell.trim().is_empty() {
                    return Err(Unsupported::new("blank table cell"));
                }
                let test = match (spec.inputs.get(at).and_then(Option::as_deref), targets.get(at).copied().flatten()) {
                    (Some(_), Some(target)) => self.parse(cell, true, Ctx { dollar: Some(target), ..ctx }, r)?,
                    (Some(field), None) => {
                        let target = self.parse(field, false, ctx, r)?;
                        targets[at] = Some(target);
                        self.parse(cell, true, Ctx { dollar: Some(target), ..ctx }, r)?
                    }
                    (None, _) => self.parse(cell, false, ctx, r)?,
                };
                self.require(test, |k| k == Kind::Bool, "table cell")?;
                let test = match spec.hit {
                    Hit::Lenient => self.guarded(test)?,
                    Hit::Strict => test,
                };
                cond = Some(match cond {
                    None => test,
                    Some(left) => self.logic(true, left, test)?,
                });
            }
            conds.push(match cond {
                Some(c) => c,
                None => self.konst(Lit::Bool(true)),
            });
        }
        let total = conds.iter().any(|c| self.nodes[*c as usize].op == Op::Const(Lit::Bool(true)));
        let (scope, conds) = match self.member {
            Some(_) => (ctx.scope, conds.iter().map(|c| self.parent(*c, ctx.scope)).collect::<Result<Vec<_>, _>>()?),
            None => self.scoped(&conds)?,
        };
        let sel = self.add(Op::FirstMatch { conds, strict: spec.hit == Hit::Strict }, Ty::num(Some(0)), scope);
        let mut picks = Vec::with_capacity(spec.outputs.len());
        for at in 0..spec.outputs.len() {
            let mut arms = Vec::with_capacity(spec.rules.len());
            for rule in &spec.rules {
                arms.push(match rule.value(at) {
                    Some(source) => self.parse(source, false, ctx, r)?,
                    None => self.konst(Lit::Null),
                });
            }
            picks.push(self.picked(sel, arms, total)?);
        }
        if spec.selector {
            picks.push(sel);
        }
        let names = spec.outputs.iter().map(String::as_str).chain(std::iter::repeat(""));
        for (name, pick) in names.zip(picks) {
            self.output(name, pick, ctx.scope, true)?;
        }
        Ok(())
    }

    fn leftmost(&self, id: NodeId) -> Vec<NodeId> {
        match &self.nodes[id as usize].op {
            Op::And(a, _) | Op::Or(a, _) | Op::Not(a) => self.leftmost(*a),
            Op::Cmp(Cmp::Lt | Cmp::Le | Cmp::Gt | Cmp::Ge, a, b) => vec![*a, *b],
            Op::InRange(a, _) => vec![*a],
            _ => Vec::new(),
        }
    }

    fn guarded(&mut self, id: NodeId) -> Lowered {
        if let Op::And(a, b) = self.nodes[id as usize].op {
            let (a, b) = (self.guarded(a)?, self.guarded(b)?);
            return self.logic(true, a, b);
        }
        let operands = self.leftmost(id);
        let null = self.konst(Lit::Null);
        let mut valid: Option<NodeId> = None;
        let nullable: Vec<NodeId> = operands.into_iter().filter(|x| self.ty(*x).nullable).collect();
        for operand in nullable {
            let present = self.compare(Cmp::Ne, operand, null)?;
            valid = Some(match valid {
                None => present,
                Some(left) => self.logic(true, left, present)?,
            });
        }
        let Some(valid) = valid else {
            return Ok(id);
        };
        let no = self.konst(Lit::Bool(false));
        let (arms, ty) = self.unify(vec![id, no])?;
        let (scope, ids) = self.scoped(&[valid, arms[0], arms[1]])?;
        Ok(self.add(Op::If(ids[0], ids[1], ids[2]), ty, scope))
    }

    fn picked(&mut self, sel: NodeId, arms: Vec<NodeId>, total: bool) -> Lowered {
        let (arms, mut ty) = self.unify(arms)?;
        ty.nullable |= !total;
        let ids: Vec<NodeId> = std::iter::once(sel).chain(arms).collect();
        let (scope, ids) = self.scoped(&ids)?;
        Ok(self.add(Op::Pick(ids[0], ids[1..].to_vec()), ty, scope))
    }

    fn parse(&mut self, source: &str, unary: bool, ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        self.parsed(source, unary, ctx, r, |this, ast, ctx, r| this.expr(ast, ctx, r))
    }

    fn parsed<T>(
        &mut self,
        source: &str,
        unary: bool,
        ctx: Ctx,
        r: &mut dyn Resolve,
        lower: impl FnOnce(&mut Self, &Ast, Ctx, &mut dyn Resolve) -> Result<T, Unsupported>,
    ) -> Result<T, Unsupported> {
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let tokens = lexer.tokenize(&bump, source).map_err(|e| Unsupported::new(format!("lex {source:?}: {e}")))?;
        let parser = Parser::try_new(&tokens, &bump).map_err(|e| Unsupported::new(format!("parse {source:?}: {e}")))?;
        let parsed = match unary {
            true => parser.unary().parse(),
            false => parser.standard().parse(),
        };
        parsed.error().map_err(|e| Unsupported::new(format!("parse {source:?}: {e}")))?;
        lower(self, parsed.root, ctx, r)
    }

    fn output(&mut self, name: &str, id: NodeId, scope: Scope, export: bool) -> Result<(), Unsupported> {
        if !export {
            self.names.push((name.to_string(), Named::Node(id)));
            return Ok(());
        }
        let id = match self.kind(id) {
            Kind::Text => match self.lit(id) {
                Some(Lit::Text(text)) => {
                    let text = text.clone();
                    let dict = self.literals();
                    let code = self.code(dict, &text);
                    self.konst(Lit::Code(dict, code))
                }
                _ => return Err(Unsupported::new(format!("text output {name}"))),
            },
            Kind::Code(d) if self.schema.dicts[d as usize].kind == DictKind::Classes => return Err(Unsupported::new(format!("class output {name}"))),
            _ => id,
        };
        let id = match (scope, self.nodes[id as usize].scope) {
            (Scope::Elem(_), Scope::Row) => self.parent(id, scope)?,
            (a, b) if a == b => id,
            _ => return Err(Unsupported::new(format!("output {name} scope mismatch"))),
        };
        if !name.is_empty() {
            self.names.push((name.to_string(), Named::Node(id)));
        }
        let ty = self.nodes[id as usize].ty;
        self.outputs.push(Output { name: name.to_string(), node: id, scope, ty, collect: None });
        Ok(())
    }

    fn listy(ast: &Ast) -> bool {
        match ast {
            Ast::Parenthesized(inner) => Self::listy(inner),
            Ast::FunctionCall { kind: FunctionKind::Closure(ClosureFunction::Map | ClosureFunction::Filter), .. } => true,
            _ => false,
        }
    }

    fn passable(ast: &Ast) -> bool {
        match ast {
            Ast::Parenthesized(inner) => Self::passable(inner),
            Ast::Identifier(_) | Ast::Member { .. } => true,
            Ast::Binary { left, operator: Operator::Logical(LogicalOperator::NullishCoalescing), right: Ast::Array([]) } => Self::passable(left),
            _ => false,
        }
    }

    fn collect(&mut self, name: &str, seq: Seq, scope: Scope) -> Result<(), Unsupported> {
        let inner = Scope::Elem(seq.list);
        if self.schema.owner(inner) != Some(scope) {
            return Err(Unsupported::new(format!("list output {name} outside its owner")));
        }
        let item = self.item(seq)?;
        let item = match self.kind(item) {
            Kind::Text => match self.lit(item) {
                Some(Lit::Text(text)) => {
                    let text = text.clone();
                    let dict = self.literals();
                    let code = self.code(dict, &text);
                    self.konst(Lit::Code(dict, code))
                }
                _ => return Err(Unsupported::new(format!("text list output {name}"))),
            },
            Kind::Code(d) if self.schema.dicts[d as usize].kind == DictKind::Classes => return Err(Unsupported::new(format!("class list output {name}"))),
            _ => item,
        };
        let item = self.parent(item, inner)?;
        let node = match seq.keep {
            None => item,
            Some(keep) => {
                let null = self.konst(Lit::Null);
                let (arms, ty) = self.unify(vec![item, null])?;
                let ids = [self.parent(keep, inner)?, self.parent(arms[0], inner)?, self.parent(arms[1], inner)?];
                self.add(Op::If(ids[0], ids[1], ids[2]), ty, inner)
            }
        };
        let keep = seq.keep.map(|k| self.parent(k, inner)).transpose()?;
        if !name.is_empty() {
            self.names.push((name.to_string(), Named::Seq(Seq { value: Some(item), keep, pure: true, ..seq })));
        }
        let ty = self.nodes[node as usize].ty;
        let collect = Collect { list: seq.list, keep, item, empty: seq.empty };
        self.outputs.push(Output { name: name.to_string(), node, scope: inner, ty, collect: Some(collect) });
        Ok(())
    }

    fn add(&mut self, op: Op, ty: Ty, scope: Scope) -> NodeId {
        let node = Node { op, ty, scope };
        if let Some(id) = self.index.get(&node) {
            return *id;
        }
        let id = self.nodes.len() as NodeId;
        self.index.insert(node.clone(), id);
        self.nodes.push(node);
        id
    }

    fn konst(&mut self, lit: Lit) -> NodeId {
        let ty = match &lit {
            Lit::Null => Ty::new(Kind::Null, true),
            Lit::Num(d) => Ty::num(Some(d.scale)),
            Lit::Bool(_) => Ty::boolean(),
            Lit::Code(d, _) => Ty::new(Kind::Code(*d), false),
            Lit::Text(_) => Ty::new(Kind::Text, false),
        };
        self.add(Op::Const(lit), ty, Scope::Row)
    }

    fn dict(&mut self, kind: DictKind) -> Result<DictId, Unsupported> {
        let d = DictId::try_from(self.schema.dicts.len()).map_err(|_| Unsupported::new("too many dictionaries"))?;
        self.schema.dicts.push(Dict { kind, values: Vec::new() });
        Ok(d)
    }

    fn literals(&mut self) -> DictId {
        if let Some(d) = self.literal {
            return d;
        }
        let d = self.schema.dicts.len() as DictId;
        self.schema.dicts.push(Dict { kind: DictKind::Literals, values: Vec::new() });
        self.literal = Some(d);
        d
    }

    fn code(&mut self, dict: DictId, text: &str) -> i32 {
        let values = &mut self.schema.dicts[dict as usize].values;
        match values.iter().position(|v| v.as_ref() == text) {
            Some(i) => i as i32,
            None => {
                values.push(text.into());
                (values.len() - 1) as i32
            }
        }
    }

    fn lit(&self, id: NodeId) -> Option<&Lit> {
        match &self.nodes[id as usize].op {
            Op::Const(lit) => Some(lit),
            _ => None,
        }
    }

    fn kind(&self, id: NodeId) -> Kind {
        self.nodes[id as usize].ty.kind
    }

    fn ty(&self, id: NodeId) -> Ty {
        self.nodes[id as usize].ty
    }

    fn require(&self, id: NodeId, ok: impl Fn(Kind) -> bool, what: &str) -> Result<(), Unsupported> {
        match ok(self.kind(id)) {
            true => Ok(()),
            false => Err(Unsupported::new(format!("{what}: unexpected type {}", self.ty(id)))),
        }
    }

    fn parent(&mut self, id: NodeId, scope: Scope) -> Lowered {
        let node = &self.nodes[id as usize];
        let (from, ty) = (node.scope, node.ty);
        match self.schema.owner(scope) {
            _ if from == scope || matches!(node.op, Op::Const(_)) => Ok(id),
            Some(up) if self.schema.within(up, from) => {
                let lifted = self.parent(id, up)?;
                Ok(self.add(Op::Parent(lifted), ty, scope))
            }
            _ => Err(Unsupported::new("scope: element value used outside its list")),
        }
    }

    fn scoped(&mut self, ids: &[NodeId]) -> Result<(Scope, Vec<NodeId>), Unsupported> {
        let mut scope = Scope::Row;
        for id in ids {
            let node = &self.nodes[*id as usize];
            if matches!(node.op, Op::Const(_)) {
                continue;
            }
            match (self.schema.within(node.scope, scope), self.schema.within(scope, node.scope)) {
                (true, _) => scope = node.scope,
                (false, true) => {}
                (false, false) => return Err(Unsupported::new("scope: two different lists")),
            }
        }
        let mut lifted = Vec::with_capacity(ids.len());
        for id in ids {
            lifted.push(self.parent(*id, scope)?);
        }
        Ok((scope, lifted))
    }

    fn expr(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        match ast {
            Ast::Null => Ok(self.konst(Lit::Null)),
            Ast::Bool(b) => Ok(self.konst(Lit::Bool(*b))),
            Ast::Number(n) => {
                let d = Dec::from_decimal(n).ok_or_else(|| Unsupported::new(format!("number literal {n} exceeds i64")))?;
                Ok(self.konst(Lit::Num(d)))
            }
            Ast::String(s) => Ok(self.konst(Lit::Text(s.to_string()))),
            Ast::Parenthesized(inner) => self.expr(inner, ctx, r),
            Ast::Identifier(_) | Ast::Member { .. } | Ast::Pointer => self.path(ast, ctx, r),
            Ast::Unary { node, operator } => {
                let a = self.expr(node, ctx, r)?;
                match operator {
                    Operator::Logical(LogicalOperator::Not) => self.not(a),
                    Operator::Arithmetic(ArithmeticOperator::Subtract) => self.neg(a),
                    Operator::Arithmetic(ArithmeticOperator::Add) => {
                        self.require(a, |k| matches!(k, Kind::Num(_)), "unary +")?;
                        Ok(a)
                    }
                    other => Err(Unsupported::new(format!("unary operator {other}"))),
                }
            }
            Ast::Binary { left, operator, right } => self.binary(left, *operator, right, ctx, r),
            Ast::Conditional { condition, on_true, on_false } => {
                let c = self.expr(condition, ctx, r)?;
                self.require(c, |k| k == Kind::Bool, "condition")?;
                let t = self.expr(on_true, ctx, r)?;
                let e = self.expr(on_false, ctx, r)?;
                let (arms, ty) = self.unify(vec![t, e])?;
                let (scope, ids) = self.scoped(&[c, arms[0], arms[1]])?;
                Ok(self.add(Op::If(ids[0], ids[1], ids[2]), ty, scope))
            }
            Ast::FunctionCall { kind, arguments } => self.call(kind, arguments, ctx, r),
            other => Err(Unsupported::new(format!("expression node {}", <&'static str>::from(other)))),
        }
    }

    fn not(&mut self, a: NodeId) -> Lowered {
        self.require(a, |k| k == Kind::Bool, "not")?;
        if let Some(Lit::Bool(b)) = self.lit(a) {
            let b = !*b;
            return Ok(self.konst(Lit::Bool(b)));
        }
        let scope = self.nodes[a as usize].scope;
        Ok(self.add(Op::Not(a), Ty::boolean(), scope))
    }

    fn neg(&mut self, a: NodeId) -> Lowered {
        self.require(a, |k| matches!(k, Kind::Num(_)), "negation")?;
        if let Some(Lit::Num(d)) = self.lit(a) {
            if let Ok(n) = d.checked_neg() {
                return Ok(self.konst(Lit::Num(n)));
            }
        }
        let node = &self.nodes[a as usize];
        let (ty, scope) = (Ty::new(node.ty.kind, false), node.scope);
        Ok(self.add(Op::Neg(a), ty, scope))
    }

    fn segments<'a>(ast: &'a Ast<'a>, out: &mut Vec<&'a str>) -> Result<Base<'a>, Unsupported> {
        match ast {
            Ast::Identifier("$") => Ok(Base::Dollar),
            Ast::Identifier(name) => Ok(Base::Ident(name)),
            Ast::Pointer => Ok(Base::Pointer),
            Ast::Member { node, property: Ast::String(segment) } => {
                let base = Self::segments(node, out)?;
                out.push(segment);
                Ok(base)
            }
            other => Err(Unsupported::new(format!("member access on {}", <&'static str>::from(other)))),
        }
    }

    fn named(&self, path: &str) -> Option<Named> {
        self.names.iter().rev().find(|(name, _)| name == path).map(|(_, named)| *named)
    }

    fn own(&self, path: &str) -> Option<Named> {
        let from = match self.graph {
            true => self.local.min(self.names.len()),
            false => 0,
        };
        self.names[from..].iter().rev().find(|(name, _)| name == path).map(|(_, named)| *named)
    }

    fn scalar(named: Named, path: &str) -> Lowered {
        match named {
            Named::Node(id) => Ok(id),
            Named::Seq(_) => Err(Unsupported::new(format!("list {path} used as a value"))),
        }
    }

    fn overlap(&self, path: &str) -> Result<(), Unsupported> {
        let inside = |outer: &str, inner: &str| inner.len() > outer.len() && inner.starts_with(outer) && inner.as_bytes()[outer.len()] == b'.';
        match self.names.iter().any(|(name, _)| inside(name, path) || inside(path, name)) {
            true => Err(Unsupported::new(format!("{path} overlaps a written path"))),
            false => Ok(()),
        }
    }

    fn path(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        let mut segments = Vec::new();
        let base = Self::segments(ast, &mut segments)?;
        let rest = segments.join(".");
        match base {
            Base::Pointer => {
                let list = ctx.pointer.ok_or_else(|| Unsupported::new("# outside a closure"))?;
                match (ctx.item, rest.is_empty()) {
                    (Some(item), true) => Ok(item),
                    (Some(_), false) => Err(Unsupported::new("member of a mapped element")),
                    (None, _) => self.field(list, &rest, None, r),
                }
            }
            Base::Dollar if segments.is_empty() => ctx.dollar.ok_or_else(|| Unsupported::new("$ outside a table cell")),
            Base::Dollar if ctx.dollar.is_some() => Err(Unsupported::new("member of $ in a table cell")),
            Base::Dollar => Self::scalar(self.own(&rest).ok_or_else(|| Unsupported::new(format!("unknown $.{rest}")))?, &rest),
            Base::Ident(name) => {
                let path = match rest.is_empty() {
                    true => name.to_string(),
                    false => format!("{name}.{rest}"),
                };
                if self.graph {
                    return self.column(&path, r);
                }
                self.overlap(&path)?;
                if let Some(named) = self.named(&path) {
                    return Self::scalar(named, &path);
                }
                if let Some(list) = self.member {
                    if self.schema.lists[list as usize].alias.as_deref() == Some(name) {
                        return match rest.is_empty() {
                            true => Err(Unsupported::new(format!("whole element {name}"))),
                            false => self.field(list, &rest, Some(&path), r),
                        };
                    }
                }
                self.column(&path, r)
            }
        }
    }

    fn column(&mut self, path: &str, r: &mut dyn Resolve) -> Lowered {
        if self.graph {
            return match r.path(path) {
                Resolved::Keyed(class, key) => self.keyed(class, &key),
                Resolved::Output(at) => match self.outputs.get(at) {
                    Some(Output { collect: None, node, .. }) => Ok(*node),
                    Some(_) => Err(Unsupported::new(format!("list {path} used as a value"))),
                    None => Err(Unsupported::new(format!("output {at} of {path}"))),
                },
                Resolved::Absent => Ok(self.konst(Lit::Null)),
                _ => Err(Unsupported::new(format!("path {path}"))),
            };
        }
        if let Some(c) = self.schema.column(path) {
            let ty = self.schema.columns[c as usize].ty;
            return Ok(self.add(Op::Column(c), ty, Scope::Row));
        }
        if self.absent.iter().any(|a| a == path) {
            return Ok(self.konst(Lit::Null));
        }
        if self.schema.list(path).is_some() {
            return Err(Unsupported::new(format!("list {path} used as a value")));
        }
        match r.path(path) {
            Resolved::Column(class) => {
                let c = u16::try_from(self.schema.columns.len()).map_err(|_| Unsupported::new("too many columns"))?;
                self.schema.columns.push(ColumnDef { path: path.to_string(), ty: class.ty() });
                Ok(self.add(Op::Column(c), class.ty(), Scope::Row))
            }
            Resolved::Absent => {
                self.absent.push(path.to_string());
                Ok(self.konst(Lit::Null))
            }
            Resolved::Keyed(class, key) => self.keyed(class, &key),
            Resolved::List { .. } | Resolved::KeyedList(..) => Err(Unsupported::new(format!("list {path} used as a value"))),
            Resolved::Output(_) | Resolved::Unsupported => Err(Unsupported::new(format!("path {path}"))),
        }
    }

    fn keyed(&mut self, class: Class, key: &str) -> Lowered {
        let c = match self.schema.column(key) {
            Some(c) => c,
            None => {
                let c = u16::try_from(self.schema.columns.len()).map_err(|_| Unsupported::new("too many columns"))?;
                self.schema.columns.push(ColumnDef { path: key.to_string(), ty: class.ty() });
                c
            }
        };
        let ty = self.schema.columns[c as usize].ty;
        match ty == class.ty() {
            true => Ok(self.add(Op::Column(c), ty, Scope::Row)),
            false => Err(Unsupported::new(format!("column {key} read as two classes"))),
        }
    }

    fn field(&mut self, list: ListId, name: &str, member: Option<&str>, r: &mut dyn Resolve) -> Lowered {
        if let Some(f) = self.schema.field(list, name) {
            let ty = self.schema.lists[list as usize].fields[f as usize].ty;
            return Ok(self.add(Op::Field(list, f), ty, Scope::Elem(list)));
        }
        let Some(path) = member else {
            return Err(Unsupported::new(format!("unknown field {name}")));
        };
        if self.absent.iter().any(|a| a == path) {
            return Ok(self.konst(Lit::Null));
        }
        match r.alias(name) {
            Resolved::Column(class) => {
                let fields = &mut self.schema.lists[list as usize].fields;
                let f = u16::try_from(fields.len()).map_err(|_| Unsupported::new("too many fields"))?;
                fields.push(ColumnDef { path: name.to_string(), ty: class.ty() });
                Ok(self.add(Op::Field(list, f), class.ty(), Scope::Elem(list)))
            }
            Resolved::Absent => {
                self.absent.push(path.to_string());
                Ok(self.konst(Lit::Null))
            }
            _ => Err(Unsupported::new(format!("member field {name}"))),
        }
    }

    fn binary(&mut self, left: &Ast, operator: Operator, right: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        match operator {
            Operator::Arithmetic(op) => {
                let op = match op {
                    ArithmeticOperator::Add => Arith::Add,
                    ArithmeticOperator::Subtract => Arith::Sub,
                    ArithmeticOperator::Multiply => Arith::Mul,
                    ArithmeticOperator::Divide => Arith::Div,
                    ArithmeticOperator::Modulus => Arith::Rem,
                    ArithmeticOperator::Power => Arith::Pow,
                };
                let a = self.expr(left, ctx, r)?;
                let b = self.expr(right, ctx, r)?;
                self.arith(op, a, b)
            }
            Operator::Comparison(ComparisonOperator::In | ComparisonOperator::NotIn) => {
                let a = self.expr(left, ctx, r)?;
                let test = match right {
                    Ast::Array(items) => self.inset(a, items, ctx, r)?,
                    Ast::Interval { left: lo, right: hi, left_bracket, right_bracket } => self.inrange(a, (lo, hi), (*left_bracket, *right_bracket), ctx, r)?,
                    other => {
                        let seq = self.seq(other, ctx, r)?;
                        self.member(a, seq)?
                    }
                };
                match operator {
                    Operator::Comparison(ComparisonOperator::NotIn) => self.not(test),
                    _ => Ok(test),
                }
            }
            Operator::Comparison(op) => {
                let a = self.expr(left, ctx, r)?;
                let b = self.expr(right, ctx, r)?;
                let op = match op {
                    ComparisonOperator::Equal => Cmp::Eq,
                    ComparisonOperator::NotEqual => Cmp::Ne,
                    ComparisonOperator::LessThan => Cmp::Lt,
                    ComparisonOperator::LessThanOrEqual => Cmp::Le,
                    ComparisonOperator::GreaterThan => Cmp::Gt,
                    ComparisonOperator::GreaterThanOrEqual => Cmp::Ge,
                    other => return Err(Unsupported::new(format!("comparison {other}"))),
                };
                self.compare(op, a, b)
            }
            Operator::Logical(LogicalOperator::And | LogicalOperator::Or) => {
                let a = self.expr(left, ctx, r)?;
                let b = self.expr(right, ctx, r)?;
                self.logic(operator == Operator::Logical(LogicalOperator::And), a, b)
            }
            Operator::Logical(LogicalOperator::NullishCoalescing) => {
                let a = self.expr(left, ctx, r)?;
                let b = self.expr(right, ctx, r)?;
                let nullable = self.ty(a).nullable && self.ty(b).nullable;
                let (arms, ty) = self.unify(vec![a, b])?;
                let (scope, ids) = self.scoped(&arms)?;
                Ok(self.add(Op::Coalesce(ids[0], ids[1]), Ty::new(ty.kind, nullable), scope))
            }
            other => Err(Unsupported::new(format!("operator {other}"))),
        }
    }

    fn arith(&mut self, op: Arith, a: NodeId, b: NodeId) -> Lowered {
        let (Kind::Num(sa), Kind::Num(sb)) = (self.kind(a), self.kind(b)) else {
            return Err(Unsupported::new(format!("arithmetic between {} and {}", self.ty(a), self.ty(b))));
        };
        let divisor = match self.lit(b) {
            Some(Lit::Num(d)) => Some(*d),
            _ => None,
        };
        let ty = match op {
            Arith::Add | Arith::Sub => Ty::num(sa.filter(|x| sb == Some(*x))),
            Arith::Mul => Ty::num((sa == Some(0) && sb == Some(0)).then_some(0)),
            Arith::Div | Arith::Rem => Ty::new(Kind::Num(None), divisor.is_none_or(|d| d.is_zero())),
            Arith::Pow if divisor.is_some_and(|d| d.scale == 0) => Ty::num(None),
            Arith::Pow => return Err(Unsupported::new("exponent must be an integer literal")),
        };
        let (scope, ids) = self.scoped(&[a, b])?;
        Ok(self.add(Op::Arith(op, ids[0], ids[1]), ty, scope))
    }

    fn classify(&mut self, id: NodeId) -> Lowered {
        if !matches!(self.nodes[id as usize].op, Op::Column(_) | Op::Field(..)) {
            return Err(Unsupported::new("computed text"));
        }
        let dict = match self.classes.iter().find(|(n, _)| *n == id) {
            Some((_, d)) => *d,
            None => {
                let d = self.dict(DictKind::Classes)?;
                self.classes.push((id, d));
                d
            }
        };
        let node = &self.nodes[id as usize];
        let (nullable, scope) = (node.ty.nullable, node.scope);
        Ok(self.add(Op::Classify(id, dict), Ty::new(Kind::Code(dict), nullable), scope))
    }

    fn compare(&mut self, op: Cmp, a: NodeId, b: NodeId) -> Lowered {
        let (ka, kb) = (self.kind(a), self.kind(b));
        let eq = matches!(op, Cmp::Eq | Cmp::Ne);
        let plain = |x: NodeId, y: NodeId| match (self.lit(y), self.ty(x)) {
            (Some(Lit::Bool(b)), Ty { kind: Kind::Bool, nullable: false }) if self.lit(x).is_none() => Some((x, *b)),
            _ => None,
        };
        if let (true, Some((x, b))) = (eq, plain(a, b).or_else(|| plain(b, a))) {
            return match b == (op == Cmp::Eq) {
                true => Ok(x),
                false => self.not(x),
            };
        }
        let (a, b) = match (ka, kb) {
            (Kind::Text, Kind::Text) if eq => match (self.lit(a).is_some(), self.lit(b).is_some()) {
                (true, true) => {
                    let equal = self.lit(a) == self.lit(b);
                    return Ok(self.konst(Lit::Bool(equal == (op == Cmp::Eq))));
                }
                (false, true) => {
                    let x = self.classify(a)?;
                    let Kind::Code(d) = self.kind(x) else {
                        return Err(Unsupported::new("classify"));
                    };
                    (x, self.coerce(b, d)?)
                }
                (true, false) => {
                    let y = self.classify(b)?;
                    let Kind::Code(d) = self.kind(y) else {
                        return Err(Unsupported::new("classify"));
                    };
                    (self.coerce(a, d)?, y)
                }
                (false, false) => return Err(Unsupported::new("text compared to text")),
            },
            (Kind::Text, Kind::Null) if eq => (self.classify(a)?, b),
            (Kind::Null, Kind::Text) if eq => (a, self.classify(b)?),
            (Kind::Code(d), Kind::Text) if eq => (a, self.coerce(b, d)?),
            (Kind::Text, Kind::Code(d)) if eq => (self.coerce(a, d)?, b),
            (Kind::Code(x), Kind::Code(y)) if eq && x == y => (a, b),
            (Kind::Num(_), Kind::Num(_)) | (Kind::Bool, Kind::Bool) if eq => (a, b),
            (Kind::Null, _) | (_, Kind::Null) if eq => (a, b),
            (Kind::Num(_), Kind::Num(_)) => (a, b),
            _ => return Err(Unsupported::new(format!("compare {op:?} between {ka} and {kb}"))),
        };
        let (scope, ids) = self.scoped(&[a, b])?;
        Ok(self.add(Op::Cmp(op, ids[0], ids[1]), Ty::boolean(), scope))
    }

    fn coerce(&mut self, id: NodeId, dict: DictId) -> Lowered {
        match self.lit(id) {
            Some(Lit::Text(text)) => {
                let text = text.clone();
                let code = self.code(dict, &text);
                Ok(self.konst(Lit::Code(dict, code)))
            }
            _ if self.kind(id) == Kind::Code(dict) || self.kind(id) == Kind::Null => Ok(id),
            _ => Err(Unsupported::new(format!("cannot code {} as dict {dict}", self.ty(id)))),
        }
    }

    fn logic(&mut self, and: bool, a: NodeId, b: NodeId) -> Lowered {
        self.require(a, |k| k == Kind::Bool, "logical operand")?;
        self.require(b, |k| k == Kind::Bool, "logical operand")?;
        let nullable = self.ty(b).nullable;
        let (scope, ids) = self.scoped(&[a, b])?;
        let op = match and {
            true => Op::And(ids[0], ids[1]),
            false => Op::Or(ids[0], ids[1]),
        };
        Ok(self.add(op, Ty::new(Kind::Bool, nullable), scope))
    }

    fn inset(&mut self, a: NodeId, items: &[&Ast], ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        let a = match self.kind(a) {
            Kind::Text => self.classify(a)?,
            _ => a,
        };
        let kind = self.kind(a);
        let mut set: Vec<Lit> = Vec::new();
        for item in items {
            let id = self.expr(item, ctx, r)?;
            let lit = self.lit(id).cloned().ok_or_else(|| Unsupported::new("in: non-literal set item"))?;
            let lit = match (kind, lit) {
                (_, Lit::Null) => Some(Lit::Null),
                (Kind::Code(d), Lit::Text(t)) => Some(Lit::Code(d, self.code(d, &t))),
                (Kind::Num(_), lit @ Lit::Num(_)) | (Kind::Bool, lit @ Lit::Bool(_)) => Some(lit),
                _ => None,
            };
            if let Some(lit) = lit {
                if !set.contains(&lit) {
                    set.push(lit);
                }
            }
        }
        let scope = self.nodes[a as usize].scope;
        Ok(self.add(Op::InSet(a, set), Ty::boolean(), scope))
    }

    fn inrange(&mut self, a: NodeId, (lo, hi): (&Ast, &Ast), (left, right): (Bracket, Bracket), ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        self.require(a, |k| matches!(k, Kind::Num(_)), "interval operand")?;
        let lo = self.expr(lo, ctx, r)?;
        let hi = self.expr(hi, ctx, r)?;
        let (Some(Lit::Num(lo)), Some(Lit::Num(hi))) = (self.lit(lo).cloned(), self.lit(hi).cloned()) else {
            return Err(Unsupported::new("interval bounds must be numeric literals"));
        };
        let lo_closed = match left {
            Bracket::LeftSquareBracket => true,
            Bracket::LeftParenthesis => false,
            other => return Err(Unsupported::new(format!("interval bracket {other}"))),
        };
        let hi_closed = match right {
            Bracket::RightSquareBracket => true,
            Bracket::RightParenthesis => false,
            other => return Err(Unsupported::new(format!("interval bracket {other}"))),
        };
        let scope = self.nodes[a as usize].scope;
        Ok(self.add(Op::InRange(a, Range { lo, lo_closed, hi, hi_closed }), Ty::boolean(), scope))
    }

    fn unify(&mut self, ids: Vec<NodeId>) -> Result<(Vec<NodeId>, Ty), Unsupported> {
        let mut dict: Option<DictId> = None;
        for id in &ids {
            match self.kind(*id) {
                Kind::Code(d) if dict.is_some_and(|x| x != d) => return Err(Unsupported::new("branches code different dictionaries")),
                Kind::Code(d) => dict = Some(d),
                Kind::Text if self.lit(*id).is_none() => return Err(Unsupported::new("computed text branch")),
                _ => {}
            }
        }
        let texts = ids.iter().any(|id| self.kind(*id) == Kind::Text);
        if texts {
            let d = match dict {
                Some(d) => d,
                None => self.literals(),
            };
            if self.schema.dicts[d as usize].kind != DictKind::Literals {
                return Err(Unsupported::new("dictionary mixing"));
            }
            dict = Some(d);
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(match (self.kind(id), dict) {
                (Kind::Text, Some(d)) => self.coerce(id, d)?,
                _ => id,
            });
        }
        let mut kind: Option<Kind> = None;
        let mut nullable = false;
        for id in &out {
            let ty = self.ty(*id);
            nullable |= ty.nullable;
            kind = match (kind, ty.kind) {
                (k, Kind::Null) => k,
                (None, k) => Some(k),
                (Some(Kind::Num(x)), Kind::Num(y)) => Some(Kind::Num(if x == y { x } else { None })),
                (Some(x), y) if x == y => Some(x),
                (Some(x), y) => return Err(Unsupported::new(format!("branches of type {x} and {y}"))),
            };
        }
        Ok((out, Ty::new(kind.unwrap_or(Kind::Null), nullable || kind.is_none())))
    }

    fn call(&mut self, kind: &FunctionKind, args: &[&Ast], ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        match (kind, args) {
            (FunctionKind::Internal(f @ (InternalFunction::Sum | InternalFunction::Min | InternalFunction::Max | InternalFunction::Avg)), [arg])
                if !matches!(arg, Ast::Array(_)) =>
            {
                let seq = self.seq(arg, ctx, r)?;
                let value = self.item(seq)?;
                let fold = match f {
                    InternalFunction::Sum => Fold::Sum,
                    InternalFunction::Min => Fold::Min,
                    InternalFunction::Max => Fold::Max,
                    _ => Fold::Avg,
                };
                self.fold(fold, seq, seq.keep, Some(value))
            }
            (FunctionKind::Closure(ClosureFunction::Count), [source, body]) => {
                let seq = Self::pure(self.seq(source, ctx, r)?)?;
                let test = self.predicate(seq, body, ctx, r)?;
                let filter = match seq.keep {
                    None => test,
                    Some(keep) => self.logic(true, keep, test)?,
                };
                self.fold(Fold::Count, seq, Some(filter), None)
            }
            (FunctionKind::Internal(InternalFunction::Contains), [list, x]) => {
                let seq = self.seq(list, ctx, r)?;
                let x = self.expr(x, ctx, r)?;
                self.member(x, seq)
            }
            (FunctionKind::Internal(InternalFunction::Len), [arg]) => match self.seq(arg, ctx, r).and_then(Self::pure) {
                Ok(seq) => self.fold(Fold::Count, seq, seq.keep, None),
                Err(_) => {
                    let a = self.expr(arg, ctx, r)?;
                    self.require(a, |k| k == Kind::Text, "len")?;
                    let scope = self.nodes[a as usize].scope;
                    Ok(self.add(Op::Call(Func::Len, vec![a]), Ty::num(Some(0)), scope))
                }
            },
            (FunctionKind::Internal(f @ (InternalFunction::StartsWith | InternalFunction::EndsWith)), [a, b]) => {
                let a = self.expr(a, ctx, r)?;
                let b = self.expr(b, ctx, r)?;
                self.require(a, |k| k == Kind::Text, "string function")?;
                self.require(b, |k| k == Kind::Text, "string function")?;
                let func = match f {
                    InternalFunction::StartsWith => Func::StartsWith,
                    _ => Func::EndsWith,
                };
                let (scope, ids) = self.scoped(&[a, b])?;
                Ok(self.add(Op::Call(func, ids), Ty::boolean(), scope))
            }
            (FunctionKind::Internal(InternalFunction::Bool), [arg]) => {
                let a = self.expr(arg, ctx, r)?;
                match self.ty(a) {
                    Ty { kind: Kind::Bool, nullable: false } => Ok(a),
                    Ty { kind: Kind::Bool, nullable: true } => {
                        let no = self.konst(Lit::Bool(false));
                        let (scope, ids) = self.scoped(&[a, no])?;
                        Ok(self.add(Op::Coalesce(ids[0], ids[1]), Ty::boolean(), scope))
                    }
                    Ty { kind: Kind::Null, .. } => Ok(self.konst(Lit::Bool(false))),
                    ty => Err(Unsupported::new(format!("bool of {ty}"))),
                }
            }
            (FunctionKind::Internal(f @ (InternalFunction::Abs | InternalFunction::Floor | InternalFunction::Ceil)), [arg]) => {
                let a = self.expr(arg, ctx, r)?;
                let Kind::Num(scale) = self.kind(a) else {
                    return Err(Unsupported::new(format!("{f:?} of {}", self.ty(a))));
                };
                let (func, ty) = match f {
                    InternalFunction::Abs => (Func::Abs, Ty::num(scale)),
                    InternalFunction::Floor => (Func::Floor, Ty::num(Some(0))),
                    _ => (Func::Ceil, Ty::num(Some(0))),
                };
                self.math(func, vec![a], ty)
            }
            (FunctionKind::Internal(f @ (InternalFunction::Round | InternalFunction::Trunc)), [arg, rest @ ..]) => {
                let a = self.expr(arg, ctx, r)?;
                let Kind::Num(scale) = self.kind(a) else {
                    return Err(Unsupported::new(format!("{f:?} of {}", self.ty(a))));
                };
                let places = match rest {
                    [] => 0,
                    [places] => {
                        let p = self.expr(places, ctx, r)?;
                        match self.lit(p) {
                            Some(Lit::Num(d)) if d.scale == 0 && (0..=i64::from(Dec::MAX_SCALE)).contains(&d.mant) => d.mant as u8,
                            _ => return Err(Unsupported::new("decimal places must be a small integer literal")),
                        }
                    }
                    _ => return Err(Unsupported::new(format!("{f:?} arity"))),
                };
                let (func, ty) = match f {
                    InternalFunction::Round => (Func::Round(places), Ty::num(scale.map(|s| s.min(places)))),
                    _ => (Func::Trunc(places), Ty::num(Some(places))),
                };
                self.math(func, vec![a], ty)
            }
            (FunctionKind::Internal(f @ (InternalFunction::Min | InternalFunction::Max | InternalFunction::Avg | InternalFunction::Sum)), [Ast::Array(items)]) => {
                if items.is_empty() {
                    return Err(Unsupported::new("aggregate of an empty list"));
                }
                let mut args = Vec::with_capacity(items.len());
                for item in items.iter() {
                    let a = self.expr(item, ctx, r)?;
                    self.require(a, |k| matches!(k, Kind::Num(_)), "aggregate item")?;
                    args.push(a);
                }
                match f {
                    InternalFunction::Min | InternalFunction::Max => {
                        let first = self.kind(args[0]);
                        let kind = match args.iter().all(|a| self.kind(*a) == first) {
                            true => first,
                            false => Kind::Num(None),
                        };
                        let func = if *f == InternalFunction::Min { Func::Min } else { Func::Max };
                        self.math(func, args, Ty::new(kind, false))
                    }
                    _ => {
                        let zero = self.konst(Lit::Num(Dec::ZERO));
                        let mut acc = zero;
                        for a in &args {
                            acc = self.arith(Arith::Add, acc, *a)?;
                        }
                        match f {
                            InternalFunction::Sum => Ok(acc),
                            _ => {
                                let count = self.konst(Lit::Num(Dec::int(args.len() as i64)));
                                self.arith(Arith::Div, acc, count)
                            }
                        }
                    }
                }
            }
            (FunctionKind::Closure(c @ (ClosureFunction::Some | ClosureFunction::All | ClosureFunction::None | ClosureFunction::One)), [source, body]) => {
                let seq = Self::pure(self.seq(source, ctx, r)?)?;
                let test = self.predicate(seq, body, ctx, r)?;
                let fold = match c {
                    ClosureFunction::Some => Fold::Any,
                    ClosureFunction::All => Fold::All,
                    ClosureFunction::None => Fold::None,
                    _ => Fold::One,
                };
                self.fold(fold, seq, seq.keep, Some(test))
            }
            (kind, _) => Err(Unsupported::new(format!("function {kind:?}"))),
        }
    }

    fn math(&mut self, func: Func, args: Vec<NodeId>, ty: Ty) -> Lowered {
        let (scope, ids) = self.scoped(&args)?;
        Ok(self.add(Op::Call(func, ids), ty, scope))
    }

    fn closure<'a>(ast: &'a Ast<'a>) -> Result<&'a Ast<'a>, Unsupported> {
        match ast {
            Ast::Closure { body, alias: None } => Ok(body),
            _ => Err(Unsupported::new("expected an unaliased closure")),
        }
    }

    fn inner(ctx: Ctx, seq: Seq) -> Ctx {
        Ctx { scope: Scope::Elem(seq.list), pointer: Some(seq.list), item: seq.value, dollar: ctx.dollar }
    }

    fn lifted(&mut self, id: NodeId, list: ListId) -> Lowered {
        self.parent(id, Scope::Elem(list))
    }

    fn pure(seq: Seq) -> Result<Seq, Unsupported> {
        match seq.pure {
            true => Ok(seq),
            false => Err(Unsupported::new("closure over a computed map")),
        }
    }

    fn item(&mut self, seq: Seq) -> Lowered {
        match seq.value {
            Some(value) => Ok(value),
            None => match self.schema.field(seq.list, "") {
                Some(f) => {
                    let ty = self.schema.lists[seq.list as usize].fields[f as usize].ty;
                    Ok(self.add(Op::Field(seq.list, f), ty, Scope::Elem(seq.list)))
                }
                None => Err(Unsupported::new("element of a list of objects")),
            },
        }
    }

    fn seq(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Result<Seq, Unsupported> {
        match ast {
            Ast::Parenthesized(inner) => self.seq(inner, ctx, r),
            Ast::FunctionCall { kind: FunctionKind::Closure(ClosureFunction::Filter), arguments: [source, body] } => {
                let seq = Self::pure(self.seq(source, ctx, r)?)?;
                let body = Self::closure(body)?;
                let test = self.expr(body, Self::inner(ctx, seq), r)?;
                self.require(test, |k| k == Kind::Bool, "filter")?;
                let test = self.lifted(test, seq.list)?;
                let keep = match seq.keep {
                    None => test,
                    Some(keep) => self.logic(true, keep, test)?,
                };
                Ok(Seq { keep: Some(keep), ..seq })
            }
            Ast::FunctionCall { kind: FunctionKind::Closure(ClosureFunction::Map), arguments: [source, body] } => {
                let seq = Self::pure(self.seq(source, ctx, r)?)?;
                let body = Self::closure(body)?;
                let value = self.expr(body, Self::inner(ctx, seq), r)?;
                let value = self.lifted(value, seq.list)?;
                Ok(Seq { value: Some(value), pure: false, ..seq })
            }
            Ast::Binary { left, operator: Operator::Logical(LogicalOperator::NullishCoalescing), right: Ast::Array([]) } => {
                let seq = self.seq(left, ctx, r)?;
                Ok(Seq { empty: true, ..seq })
            }
            other => self.source(other, ctx, r),
        }
    }

    fn plain(list: ListId) -> Seq {
        Seq { list, keep: None, value: None, empty: false, pure: true }
    }

    fn produced(&self, at: usize, path: &str) -> Result<Seq, Unsupported> {
        match self.outputs.get(at).and_then(|o| o.collect) {
            Some(c) => Ok(Seq { list: c.list, keep: c.keep, value: Some(c.item), empty: c.empty, pure: true }),
            None => Err(Unsupported::new(format!("{path} is not a list"))),
        }
    }

    fn source(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Result<Seq, Unsupported> {
        let mut segments = Vec::new();
        let base = Self::segments(ast, &mut segments)?;
        let rest = segments.join(".");
        match base {
            Base::Pointer => {
                let parent = ctx.pointer.ok_or_else(|| Unsupported::new("# outside a closure"))?;
                if ctx.item.is_some() || rest.is_empty() {
                    return Err(Unsupported::new("list of lists"));
                }
                Ok(Self::plain(self.nested(parent, &rest, r)?))
            }
            Base::Dollar if ctx.dollar.is_some() || rest.is_empty() => Err(Unsupported::new("list source $")),
            Base::Dollar => match self.own(&rest) {
                Some(Named::Seq(seq)) => Ok(seq),
                _ => Err(Unsupported::new(format!("$.{rest} is not a list"))),
            },
            Base::Ident(name) => {
                let path = match rest.is_empty() {
                    true => name.to_string(),
                    false => format!("{name}.{rest}"),
                };
                match self.graph {
                    true => self.keyed_list(&path, r),
                    false => self.root_list(name, &rest, &path, r),
                }
            }
        }
    }

    fn keyed_list(&mut self, path: &str, r: &mut dyn Resolve) -> Result<Seq, Unsupported> {
        match r.path(path) {
            Resolved::KeyedList(key, fields) => {
                if let Some(list) = self.schema.list(&key) {
                    return Ok(Self::plain(list));
                }
                Ok(Self::plain(self.declare(key.to_string(), fields, None)?))
            }
            Resolved::Output(at) => self.produced(at, path),
            _ => Err(Unsupported::new(format!("{path} is not a list"))),
        }
    }

    fn root_list(&mut self, name: &str, rest: &str, path: &str, r: &mut dyn Resolve) -> Result<Seq, Unsupported> {
        self.overlap(path)?;
        match self.named(path) {
            Some(Named::Seq(seq)) => return Ok(seq),
            Some(Named::Node(_)) => return Err(Unsupported::new(format!("{path} is not a list"))),
            None => {}
        }
        if let Some(member) = self.member {
            if self.schema.lists[member as usize].alias.as_deref() == Some(name) {
                return match rest.is_empty() {
                    true => Err(Unsupported::new(format!("whole element {name}"))),
                    false => Ok(Self::plain(self.nested(member, rest, r)?)),
                };
            }
        }
        if let Some(list) = self.schema.list(path) {
            return Ok(Self::plain(list));
        }
        if self.schema.column(path).is_some() {
            return Err(Unsupported::new(format!("{path} is not a list")));
        }
        match r.path(path) {
            Resolved::List { fields } => Ok(Self::plain(self.declare(path.to_string(), fields, None)?)),
            _ => Err(Unsupported::new(format!("{path} is not a list"))),
        }
    }

    fn nested(&mut self, parent: ListId, field: &str, r: &mut dyn Resolve) -> Result<ListId, Unsupported> {
        if let Some(list) = self.schema.nested(parent, field) {
            return Ok(list);
        }
        if self.schema.depth(Scope::Elem(parent)) >= Self::DEPTH {
            return Err(Unsupported::new(format!("list {field} nested too deep")));
        }
        if self.schema.field(parent, field).is_some() {
            return Err(Unsupported::new(format!("{field} is not a list")));
        }
        let mut chain = self.schema.chain(parent);
        chain.push(field);
        match r.nested(&chain) {
            Resolved::List { fields } => self.declare(field.to_string(), fields, Some(parent)),
            _ => Err(Unsupported::new(format!("nested list {field}"))),
        }
    }

    fn declare(&mut self, path: String, fields: Vec<(Arc<str>, Class)>, parent: Option<ListId>) -> Result<ListId, Unsupported> {
        let l = ListId::try_from(self.schema.lists.len()).map_err(|_| Unsupported::new("too many lists"))?;
        let fields = fields.into_iter().map(|(name, class)| ColumnDef { path: name.to_string(), ty: class.ty() }).collect();
        self.schema.lists.push(ListDef { path, alias: None, fields, parent });
        Ok(l)
    }

    fn predicate(&mut self, seq: Seq, body: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        let test = self.expr(Self::closure(body)?, Self::inner(ctx, seq), r)?;
        self.require(test, |k| k == Kind::Bool, "predicate")?;
        self.lifted(test, seq.list)
    }

    fn member(&mut self, x: NodeId, seq: Seq) -> Lowered {
        let seq = Self::pure(seq)?;
        let item = self.item(seq)?;
        let test = self.compare(Cmp::Eq, x, item)?;
        let test = self.lifted(test, seq.list)?;
        self.fold(Fold::Any, seq, seq.keep, Some(test))
    }

    fn fold(&mut self, fold: Fold, seq: Seq, filter: Option<NodeId>, value: Option<NodeId>) -> Lowered {
        let list = seq.list;
        let filter = filter.map(|f| self.lifted(f, list)).transpose()?;
        let value = match value {
            Some(id) => {
                match fold.quantifier() {
                    true => self.require(id, |k| k == Kind::Bool, "predicate")?,
                    false => self.require(id, |k| matches!(k, Kind::Num(_)), "fold value")?,
                }
                Some(self.lifted(id, list)?)
            }
            None => None,
        };
        let ty = match (fold, value) {
            (Fold::Sum, Some(v)) => Ty::num((self.kind(v) == Kind::Num(Some(0))).then_some(0)),
            (Fold::Min | Fold::Max, Some(v)) => Ty::new(self.kind(v), false),
            (Fold::Avg, _) => Ty::num(None),
            (Fold::Any | Fold::All | Fold::None | Fold::One, _) => Ty::boolean(),
            _ => Ty::num(Some(0)),
        };
        let scope = self.schema.owner(Scope::Elem(list)).ok_or_else(|| Unsupported::new("fold owner"))?;
        Ok(self.add(Op::Fold { fold, list, filter, value, empty: seq.empty }, ty, scope))
    }
}
