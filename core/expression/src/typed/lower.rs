use crate::context::EvaluationContext;
use crate::functions::defs::FunctionDefinition;
use crate::functions::{
    ClosureFunction, DateMethod, DeprecatedFunction, FunctionKind, FunctionRegistry,
    InternalFunction, MethodKind, MethodRegistry,
};
use crate::lane::builtins::{Arg as Given, Builtins, Out, RowContext};
use crate::lane::date::Date;
use crate::lane::effect::{Effect, Literal};
use crate::lane::{Fusion, SourceInfo};
use crate::lexer::{
    ArithmeticOperator, Bracket, ComparisonOperator, Lexer, LogicalOperator, Operator,
};
use crate::parser::{Node as Ast, Parser};
use crate::typed::hir::{
    Arith, Clock, Cmp, Collect, ColumnDef, Dict, DictId, DictKind, Fold, Func, Hir, Host, Kind,
    ListDef, ListId, Lit, Node, NodeId, Op, Output, Range, Schema, Scope, Stamp, Ty, Unsupported,
};
use compose::Val;
use crate::typed::ins::Pattern;
use crate::typed::num::Dec;
use crate::variable::Variable;
use bumpalo::Bump;
use std::collections::HashMap;
use std::sync::Arc;

mod compose;

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
    Dated,
    Keyed(Class, Arc<str>),
    Output(usize),
    List {
        fields: Vec<(Arc<str>, Class)>,
    },
    DatedList {
        fields: Vec<(Arc<str>, Class)>,
        dated: Vec<Arc<str>>,
    },
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
    pub collect: bool,
}

pub trait Resolve {
    fn path(&mut self, path: &str) -> Resolved;
    fn alias(&mut self, field: &str) -> Resolved;
    fn nested(&mut self, chain: &[&str]) -> Resolved {
        let _ = chain;
        Resolved::Unsupported
    }
    fn novel(&mut self) -> bool {
        false
    }
}

pub struct Lowering {
    schema: Schema,
    nodes: Vec<Node>,
    index: HashMap<Node, NodeId>,
    outputs: Vec<Output>,
    parts: Vec<Output>,
    vals: Vec<Val>,
    names: Vec<(String, Named)>,
    absent: Vec<String>,
    literal: Option<DictId>,
    classes: Vec<(NodeId, DictId)>,
    member: Option<ListId>,
    units: usize,
    graph: bool,
    local: usize,
    novel: usize,
    lone: bool,
    dated: Vec<(Option<ListId>, String)>,
    outer: bool,
}

#[derive(Clone)]
struct Mark {
    nodes: usize,
    outputs: usize,
    parts: usize,
    vals: usize,
    names: usize,
    absent: usize,
    columns: usize,
    fields: Vec<usize>,
    dicts: Vec<usize>,
    literal: Option<DictId>,
    classes: usize,
    novel: usize,
}

#[derive(Clone, Copy)]
struct Ctx {
    scope: Scope,
    pointer: Option<ListId>,
    item: Option<NodeId>,
    dollar: Option<NodeId>,
    list: Option<Seq>,
    elem: Option<u32>,
    home: Option<ListId>,
    siblings: bool,
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
    Val(u32),
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
        self.values
            .get(at)
            .and_then(Option::as_deref)
            .filter(|v| !v.trim().is_empty())
    }
}

impl Lowering {
    const DEPTH: u32 = 3;
    const CROSSED: &'static str = "scope: two different lists";
    pub const OUTER: char = '^';

    pub fn new(member: Option<(&str, &str)>) -> Self {
        let mut schema = Schema::default();
        let member = member.map(|(alias, path)| {
            schema.lists.push(ListDef {
                path: path.to_string(),
                alias: Some(alias.to_string()),
                fields: Vec::new(),
                parent: None,
            });
            0
        });
        Self {
            schema,
            nodes: Vec::new(),
            index: HashMap::new(),
            outputs: Vec::new(),
            parts: Vec::new(),
            vals: Vec::new(),
            names: Vec::new(),
            absent: Vec::new(),
            literal: None,
            classes: Vec::new(),
            member,
            units: 0,
            graph: false,
            local: 0,
            novel: 0,
            lone: false,
            dated: Vec::new(),
            outer: false,
        }
    }

    pub fn graph() -> Self {
        Self {
            graph: true,
            ..Self::new(None)
        }
    }

    pub fn unit(
        &mut self,
        entries: &[Fusion],
        resolve: &mut dyn Resolve,
    ) -> Result<(), Unsupported> {
        let mark = self.mark();
        self.local = self.names.len();
        let mut result = entries
            .iter()
            .try_for_each(|entry| self.entry(entry, resolve));
        if self.crossed(&result) {
            self.rollback(mark.clone());
            self.outer = true;
            result = entries
                .iter()
                .try_for_each(|entry| self.entry(entry, resolve));
            self.outer = false;
        }
        self.settle(mark, result, resolve)
    }

    fn crossed(&self, result: &Result<(), Unsupported>) -> bool {
        self.member.is_some() && matches!(result, Err(error) if error.0 == Self::CROSSED)
    }

    pub fn table(
        &mut self,
        spec: &TableSpec,
        resolve: &mut dyn Resolve,
    ) -> Result<(), Unsupported> {
        let mark = self.mark();
        self.local = self.names.len();
        let mut result = self.tabled(spec, resolve);
        if self.crossed(&result) {
            self.rollback(mark.clone());
            self.outer = true;
            result = self.tabled(spec, resolve);
            self.outer = false;
        }
        self.settle(mark, result, resolve)
    }

    fn settle(
        &mut self,
        mark: Mark,
        result: Result<(), Unsupported>,
        resolve: &mut dyn Resolve,
    ) -> Result<(), Unsupported> {
        if resolve.novel() {
            self.novel += 1;
        }
        let fresh = self.novel > mark.novel;
        let result = match result {
            Ok(()) if self.units > 0 && (fresh || self.lone) => {
                Err(Unsupported::new("isolated: new typed coverage"))
            }
            other => other,
        };
        match result {
            Ok(()) => {
                self.units += 1;
                self.lone |= fresh;
            }
            Err(_) => self.rollback(mark),
        }
        result
    }

    fn widen(&mut self) {
        self.novel += 1;
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
        Hir {
            schema: self.schema,
            nodes: self.nodes,
            outputs: self.outputs,
            parts: self.parts,
        }
    }

    fn mark(&self) -> Mark {
        Mark {
            nodes: self.nodes.len(),
            outputs: self.outputs.len(),
            parts: self.parts.len(),
            vals: self.vals.len(),
            names: self.names.len(),
            absent: self.absent.len(),
            columns: self.schema.columns.len(),
            fields: self.schema.lists.iter().map(|l| l.fields.len()).collect(),
            dicts: self.schema.dicts.iter().map(|d| d.values.len()).collect(),
            literal: self.literal,
            classes: self.classes.len(),
            novel: self.novel,
        }
    }

    fn rollback(&mut self, mark: Mark) {
        self.nodes.truncate(mark.nodes);
        self.index.retain(|_, id| (*id as usize) < mark.nodes);
        self.outputs.truncate(mark.outputs);
        self.parts.truncate(mark.parts);
        self.vals.truncate(mark.vals);
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
        self.novel = mark.novel;
    }

    fn prune(&mut self) {
        let mut live = vec![false; self.nodes.len()];
        for o in self.outputs.iter().chain(&self.parts) {
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
                kept.push(Node {
                    op: node.op.remap(|x| remap[x as usize]),
                    ty: node.ty,
                    scope: node.scope,
                });
            }
        }
        self.nodes = kept;
        self.index.clear();
        for o in self.outputs.iter_mut().chain(self.parts.iter_mut()) {
            o.node = remap[o.node as usize];
            if let Some(c) = &mut o.collect {
                c.item = remap[c.item as usize];
                c.keep = c.keep.map(|k| remap[k as usize]);
            }
        }
    }

    fn ctx(&self) -> Ctx {
        match self.member {
            Some(list) => Ctx {
                scope: Scope::Elem(list),
                pointer: None,
                item: None,
                dollar: None,
                list: None,
                elem: None,
                home: self.member,
                siblings: false,
            },
            None => Ctx {
                scope: Scope::Row,
                pointer: None,
                item: None,
                dollar: None,
                list: None,
                elem: None,
                home: self.member,
                siblings: false,
            },
        }
    }

    fn entry(&mut self, entry: &Fusion, r: &mut dyn Resolve) -> Result<(), Unsupported> {
        let ctx = self.ctx();
        match entry {
            Fusion::Output { key, source } => {
                self.parsed(source, false, ctx, r, |this, ast, ctx, r| {
                    if Self::built(ast) {
                        let val = this.value(ast, ctx, r)?;
                        return this.emit(key, val, ctx.scope);
                    }
                    let mark = this.mark();
                    let first = match Self::listy(ast) {
                        true => this
                            .seq(ast, ctx, r)
                            .and_then(|seq| this.collect(key, seq, ctx.scope)),
                        false => match this.expr(ast, ctx, r) {
                            Ok(id) => this.output(key, id, ctx.scope, true),
                            Err(e) if Self::passable(ast) => match this.seq(ast, ctx, r) {
                                Ok(seq) => this.collect(key, seq, ctx.scope),
                                Err(_) => Err(e),
                            },
                            Err(e) => Err(e),
                        },
                    };
                    let Err(e) = first else {
                        return Ok(());
                    };
                    this.rollback(mark.clone());
                    match this
                        .value(ast, ctx, r)
                        .and_then(|val| this.emit(key, val, ctx.scope))
                    {
                        Ok(()) => Ok(()),
                        Err(_) => {
                            this.rollback(mark);
                            Err(e)
                        }
                    }
                })
            }
            Fusion::Hidden { key, source } => {
                let id = self.parse(source, false, ctx, r)?;
                self.output(key, id, ctx.scope, false)
            }
            Fusion::Rules { .. } => Err(Unsupported::new("rules outside a table")),
        }
    }

    fn cell(&mut self, cell: &str, target: Ctx, r: &mut dyn Resolve) -> Lowered {
        let test = self.parse(cell, true, target, r)?;
        match target.dollar {
            Some(dollar) if !self.bound(test, dollar) => self.checked(dollar, test),
            _ => Ok(test),
        }
    }

    fn bound(&self, test: NodeId, on: NodeId) -> bool {
        match &self.nodes[test as usize].op {
            Op::Cmp(_, a, b) => *a == on || *b == on,
            Op::InSet(a, _) | Op::InRange(a, _) => *a == on,
            Op::Not(a) | Op::And(a, _) | Op::Or(a, _) => self.bound(*a, on),
            _ => false,
        }
    }

    fn checked(&mut self, x: NodeId, y: NodeId) -> Lowered {
        if self.settled(x) {
            return Ok(y);
        }
        let scope = self.nodes[x as usize].scope;
        let ok = self.add(Op::Checked(x), Ty::boolean(), scope);
        self.logic(true, ok, y)
    }

    fn tabled(&mut self, spec: &TableSpec, r: &mut dyn Resolve) -> Result<(), Unsupported> {
        if spec.rules.is_empty() || (spec.outputs.is_empty() && !spec.collect) {
            return Err(Unsupported::new("empty table"));
        }
        let ctx = self.ctx();
        let mut targets: Vec<Option<Ctx>> = vec![None; spec.inputs.len()];
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
                let lenient = spec.hit == Hit::Lenient;
                let test = match (
                    spec.inputs.get(at).and_then(Option::as_deref),
                    targets.get(at).copied().flatten(),
                ) {
                    (Some(_), Some(target)) => self.cell(cell, target, r),
                    (Some(field), None) => {
                        let target = self.target(field, ctx, r)?;
                        targets[at] = Some(target);
                        self.cell(cell, target, r)
                    }
                    (None, _) => self.parse(cell, false, ctx, r),
                };
                let test = match test {
                    Err(e) if lenient => {
                        let target = targets.get(at).copied().flatten();
                        match target {
                            Some(target) if self.doomed(cell, true, target, r) => {
                                let no = self.konst(Lit::Bool(false));
                                match target.dollar {
                                    Some(dollar) => self.checked(dollar, no)?,
                                    None => no,
                                }
                            }
                            None if !SourceInfo::parses(cell, false)
                                || self.doomed(cell, false, ctx, r) =>
                            {
                                self.konst(Lit::Bool(false))
                            }
                            _ => return Err(e),
                        }
                    }
                    other => other?,
                };
                self.require(test, |k| k == Kind::Bool, "table cell")?;
                let test = match spec.hit {
                    Hit::Lenient => self.guarded(test)?,
                    Hit::Strict => test,
                };
                if spec.hit == Hit::Lenient && self.lit(test) == Some(&Lit::Bool(false)) {
                    self.widen();
                    cond = Some(test);
                    break;
                }
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
        let mut dead = vec![false; spec.rules.len()];
        if spec.hit == Hit::Lenient && !spec.collect {
            for (at, rule) in spec.rules.iter().enumerate() {
                for column in 0..spec.outputs.len() {
                    let Some(source) = rule.value(column) else {
                        continue;
                    };
                    let mark = self.mark();
                    let lowered = self.parse(source, false, ctx, r).is_ok();
                    self.rollback(mark);
                    if !lowered && self.doomed(source, false, ctx, r) {
                        self.widen();
                        dead[at] = true;
                        conds[at] = self.konst(Lit::Bool(false));
                    }
                }
            }
        }
        let total = conds
            .iter()
            .any(|c| self.nodes[*c as usize].op == Op::Const(Lit::Bool(true)));
        let (scope, conds) = match self.member {
            Some(_) => (
                ctx.scope,
                conds
                    .iter()
                    .map(|c| self.parent(*c, ctx.scope))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => self.scoped(&conds)?,
        };
        if spec.collect {
            for cond in &conds {
                self.output("", *cond, ctx.scope, true)?;
            }
            for (rule, cond) in spec.rules.iter().zip(&conds) {
                for source in rule.values.iter().flatten() {
                    let id = self.parse(source, false, ctx, r)?;
                    let null = self.konst(Lit::Null);
                    let (arms, ty) = self.unify(vec![id, null])?;
                    let (scope, ids) = self.scoped(&[*cond, arms[0], arms[1]])?;
                    let guarded = self.add(Op::If(ids[0], ids[1], ids[2]), ty, scope);
                    self.output("", guarded, ctx.scope, true)?;
                }
            }
            return Ok(());
        }
        let sel = self.add(
            Op::FirstMatch {
                conds: conds.clone(),
                strict: spec.hit == Hit::Strict,
                blind: Vec::new(),
            },
            Ty::num(Some(0)),
            scope,
        );
        let unfilled = spec.hit == Hit::Strict
            && !spec.selector
            && spec
                .rules
                .iter()
                .any(|rule| (0..spec.outputs.len()).any(|at| rule.value(at).is_none()));
        let mut picks = Vec::with_capacity(spec.outputs.len());
        for at in 0..spec.outputs.len() {
            let blind: Vec<bool> = match unfilled {
                true => spec.rules.iter().map(|rule| rule.value(at).is_none()).collect(),
                false => Vec::new(),
            };
            let (sel, total) = match unfilled {
                false => (sel, total),
                true => (
                    self.add(
                        Op::FirstMatch {
                            conds: conds.clone(),
                            strict: true,
                            blind: blind.clone(),
                        },
                        Ty::num(Some(0)),
                        scope,
                    ),
                    conds.iter().zip(&blind).any(|(c, blind)| {
                        !blind && self.nodes[*c as usize].op == Op::Const(Lit::Bool(true))
                    }),
                ),
            };
            let mut arms = Vec::with_capacity(spec.rules.len());
            for (rule, dead) in spec.rules.iter().zip(&dead) {
                arms.push(match rule.value(at) {
                    Some(source) if !dead => self.parse(source, false, ctx, r)?,
                    _ => self.konst(Lit::Null),
                });
            }
            picks.push(self.picked(sel, arms, total)?);
        }
        if spec.selector {
            picks.push(sel);
        }
        let names = spec
            .outputs
            .iter()
            .map(String::as_str)
            .chain(std::iter::repeat(""));
        for (name, pick) in names.zip(picks) {
            self.output(name, pick, ctx.scope, true)?;
        }
        Ok(())
    }

    fn doomed(&mut self, source: &str, unary: bool, ctx: Ctx, r: &mut dyn Resolve) -> bool {
        let mark = self.mark();
        let doomed = self
            .parsed(source, unary, ctx, r, |this, ast, ctx, r| {
                Ok(this.fails(ast, ctx, r))
            })
            .unwrap_or(false);
        self.rollback(mark);
        doomed
    }

    fn fails(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> bool {
        let Ast::Binary {
            left,
            operator,
            right,
        } = ast
        else {
            return false;
        };
        let (Ok(a), Ok(b)) = (self.expr(left, ctx, r), self.expr(right, ctx, r)) else {
            return false;
        };
        let (a, b) = (self.kind(a), self.kind(b));
        match operator {
            Operator::Arithmetic(_) => a == Kind::Null || b == Kind::Null,
            Operator::Comparison(ComparisonOperator::In | ComparisonOperator::NotIn) => {
                b == Kind::Null
            }
            Operator::Comparison(
                ComparisonOperator::GreaterThan
                | ComparisonOperator::GreaterThanOrEqual
                | ComparisonOperator::LessThan
                | ComparisonOperator::LessThanOrEqual,
            ) => !matches!(
                (a, b),
                (Kind::Num(_), Kind::Num(_))
                    | (Kind::Date, Kind::Date)
                    | (Kind::Date, Kind::Text | Kind::Code(_))
                    | (Kind::Text | Kind::Code(_), Kind::Date)
            ),
            _ => false,
        }
    }

    fn target(&mut self, field: &str, ctx: Ctx, r: &mut dyn Resolve) -> Result<Ctx, Unsupported> {
        match self.parse(field, false, ctx, r) {
            Ok(target) => Ok(Ctx {
                dollar: Some(target),
                ..ctx
            }),
            Err(e) => {
                self.widen();
                match self.parsed(field, false, ctx, r, |this, ast, ctx, r| {
                    this.seq(ast, ctx, r)
                }) {
                    Ok(seq) => Ok(Ctx {
                        list: Some(seq),
                        ..ctx
                    }),
                    Err(_) => Err(e),
                }
            }
        }
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn leftmost(&self, id: NodeId) -> Vec<NodeId> {
        match &self.nodes[id as usize].op {
            Op::And(a, _) | Op::Or(a, _) | Op::Not(a) => self.leftmost(*a),
            Op::Cmp(Cmp::Lt | Cmp::Le | Cmp::Gt | Cmp::Ge, a, b) => vec![*a, *b],
            Op::InRange(a, _) => vec![*a],
            _ => Vec::new(),
        }
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn guarded(&mut self, id: NodeId) -> Lowered {
        if let Op::And(a, b) = self.nodes[id as usize].op {
            let (a, b) = (self.guarded(a)?, self.guarded(b)?);
            return self.logic(true, a, b);
        }
        let operands = self.leftmost(id);
        if operands.iter().any(|x| self.lit(*x) == Some(&Lit::Null)) {
            self.widen();
            return Ok(self.konst(Lit::Bool(false)));
        }
        let null = self.konst(Lit::Null);
        let mut valid: Option<NodeId> = None;
        let nullable: Vec<NodeId> = operands
            .into_iter()
            .filter(|x| self.ty(*x).nullable)
            .collect();
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
        self.parsed(source, unary, ctx, r, |this, ast, ctx, r| {
            this.expr(ast, ctx, r)
        })
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
        let tokens = lexer
            .tokenize(&bump, source)
            .map_err(|e| Unsupported::new(format!("lex {source:?}: {e}")))?;
        let parser = Parser::try_new(&tokens, &bump)
            .map_err(|e| Unsupported::new(format!("parse {source:?}: {e}")))?;
        let parsed = match unary {
            true => parser.unary().parse(),
            false => parser.standard().parse(),
        };
        parsed
            .error()
            .map_err(|e| Unsupported::new(format!("parse {source:?}: {e}")))?;
        lower(self, parsed.root, ctx, r)
    }

    fn output(
        &mut self,
        name: &str,
        id: NodeId,
        scope: Scope,
        export: bool,
    ) -> Result<(), Unsupported> {
        if !export {
            self.names.push((name.to_string(), Named::Node(id)));
            return Ok(());
        }
        let id = self.exportable(name, id, scope)?;
        if !name.is_empty() {
            self.names.push((name.to_string(), Named::Node(id)));
        }
        let ty = self.nodes[id as usize].ty;
        self.outputs.push(Output {
            name: name.to_string(),
            node: id,
            scope,
            ty,
            collect: None,
            shape: None,
        });
        Ok(())
    }

    fn exportable(&mut self, name: &str, id: NodeId, scope: Scope) -> Lowered {
        let id = match self.kind(id) {
            Kind::Text => match self.lit(id) {
                Some(Lit::Text(text)) => {
                    let text = text.clone();
                    let dict = self.literals();
                    let code = self.code(dict, &text);
                    self.konst(Lit::Code(dict, code))
                }
                _ => id,
            },
            Kind::Code(d) if self.schema.dicts[d as usize].kind == DictKind::Classes => {
                return Err(Unsupported::new(format!("class output {name}")))
            }
            _ => id,
        };
        match (scope, self.nodes[id as usize].scope) {
            (Scope::Elem(_), Scope::Row) => self.parent(id, scope),
            (a, b) if a == b => Ok(id),
            _ => Err(Unsupported::new(format!("output {name} scope mismatch"))),
        }
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn listy(ast: &Ast) -> bool {
        match ast {
            Ast::Parenthesized(inner) => Self::listy(inner),
            Ast::FunctionCall {
                kind: FunctionKind::Closure(ClosureFunction::Map | ClosureFunction::Filter),
                ..
            } => true,
            _ => false,
        }
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn passable(ast: &Ast) -> bool {
        match ast {
            Ast::Parenthesized(inner) => Self::passable(inner),
            Ast::Identifier(_) | Ast::Member { .. } => true,
            Ast::Binary {
                left,
                operator: Operator::Logical(LogicalOperator::NullishCoalescing),
                right: Ast::Array([]),
            } => Self::passable(left),
            _ => false,
        }
    }

    fn collect(&mut self, name: &str, seq: Seq, scope: Scope) -> Result<(), Unsupported> {
        let (output, named) = self.gathered(name, seq, scope)?;
        if !name.is_empty() {
            self.names.push((name.to_string(), Named::Seq(named)));
        }
        self.outputs.push(output);
        Ok(())
    }

    fn gathered(
        &mut self,
        name: &str,
        seq: Seq,
        scope: Scope,
    ) -> Result<(Output, Seq), Unsupported> {
        let inner = Scope::Elem(seq.list);
        if self.schema.owner(inner) != Some(scope) {
            return Err(Unsupported::new(format!(
                "list output {name} outside its owner"
            )));
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
                _ => item,
            },
            Kind::Code(d) if self.schema.dicts[d as usize].kind == DictKind::Classes => {
                return Err(Unsupported::new(format!("class list output {name}")))
            }
            _ => item,
        };
        let item = self.parent(item, inner)?;
        let node = match seq.keep {
            None => item,
            Some(keep) => {
                let null = self.konst(Lit::Null);
                let (arms, ty) = self.unify(vec![item, null])?;
                let ids = [
                    self.parent(keep, inner)?,
                    self.parent(arms[0], inner)?,
                    self.parent(arms[1], inner)?,
                ];
                self.add(Op::If(ids[0], ids[1], ids[2]), ty, inner)
            }
        };
        let keep = seq.keep.map(|k| self.parent(k, inner)).transpose()?;
        let named = Seq {
            value: Some(item),
            keep,
            pure: true,
            ..seq
        };
        let ty = self.nodes[node as usize].ty;
        let collect = Collect {
            list: seq.list,
            keep,
            item,
            empty: seq.empty,
        };
        let output = Output {
            name: name.to_string(),
            node,
            scope: inner,
            ty,
            collect: Some(collect),
            shape: None,
        };
        Ok((output, named))
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
            Lit::Date(_) => Ty::new(Kind::Date, false),
        };
        self.add(Op::Const(lit), ty, Scope::Row)
    }

    fn dict(&mut self, kind: DictKind) -> Result<DictId, Unsupported> {
        let d = DictId::try_from(self.schema.dicts.len())
            .map_err(|_| Unsupported::new("too many dictionaries"))?;
        self.schema.dicts.push(Dict {
            kind,
            values: Vec::new(),
        });
        Ok(d)
    }

    fn literals(&mut self) -> DictId {
        if let Some(d) = self.literal {
            return d;
        }
        let d = self.schema.dicts.len() as DictId;
        self.schema.dicts.push(Dict {
            kind: DictKind::Literals,
            values: Vec::new(),
        });
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

    fn require(
        &self,
        id: NodeId,
        ok: impl Fn(Kind) -> bool,
        what: &str,
    ) -> Result<(), Unsupported> {
        match ok(self.kind(id)) {
            true => Ok(()),
            false => Err(Unsupported::new(format!(
                "{what}: unexpected type {}",
                self.ty(id)
            ))),
        }
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn parent(&mut self, id: NodeId, scope: Scope) -> Lowered {
        let node = &self.nodes[id as usize];
        let (from, ty) = (node.scope, node.ty);
        match self.schema.owner(scope) {
            _ if from == scope || matches!(node.op, Op::Const(_)) => Ok(id),
            Some(up) if self.schema.within(up, from) => {
                let lifted = self.parent(id, up)?;
                Ok(self.add(Op::Parent(lifted), ty, scope))
            }
            _ => Err(Unsupported::new(
                "scope: element value used outside its list",
            )),
        }
    }

    fn scoped(&mut self, ids: &[NodeId]) -> Result<(Scope, Vec<NodeId>), Unsupported> {
        let mut scope = Scope::Row;
        for id in ids {
            let node = &self.nodes[*id as usize];
            if matches!(node.op, Op::Const(_)) {
                continue;
            }
            match (
                self.schema.within(node.scope, scope),
                self.schema.within(scope, node.scope),
            ) {
                (true, _) => scope = node.scope,
                (false, true) => {}
                (false, false) => return Err(Unsupported::new(Self::CROSSED)),
            }
        }
        let mut lifted = Vec::with_capacity(ids.len());
        for id in ids {
            lifted.push(self.parent(*id, scope)?);
        }
        Ok((scope, lifted))
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn expr(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        match ast {
            Ast::Null => Ok(self.konst(Lit::Null)),
            Ast::Bool(b) => Ok(self.konst(Lit::Bool(*b))),
            Ast::Number(n) => {
                let d = Dec::from_decimal(n)
                    .ok_or_else(|| Unsupported::new(format!("number literal {n} exceeds i64")))?;
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
            Ast::Binary {
                left,
                operator,
                right,
            } => self.binary(left, *operator, right, ctx, r),
            Ast::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                let c = self.expr(condition, ctx, r)?;
                self.require(c, |k| k == Kind::Bool, "condition")?;
                let t = self.expr(on_true, ctx, r)?;
                let e = self.expr(on_false, ctx, r)?;
                let (arms, ty) = self.unify(vec![t, e])?;
                let (scope, ids) = self.scoped(&[c, arms[0], arms[1]])?;
                Ok(self.add(Op::If(ids[0], ids[1], ids[2]), ty, scope))
            }
            Ast::FunctionCall { kind, arguments } => self.call(kind, arguments, ctx, r),
            Ast::TemplateString(parts) => self.template(parts, ctx, r),
            Ast::MethodCall {
                kind: MethodKind::DateMethod(method),
                this,
                arguments,
            } => self.method(*method, this, arguments, ctx, r),
            other => Err(Unsupported::new(format!(
                "expression node {}",
                <&'static str>::from(other)
            ))),
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

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn segments<'a>(ast: &'a Ast<'a>, out: &mut Vec<&'a str>) -> Result<Base<'a>, Unsupported> {
        match ast {
            Ast::Identifier("$") => Ok(Base::Dollar),
            Ast::Identifier(name) => Ok(Base::Ident(name)),
            Ast::Pointer => Ok(Base::Pointer),
            Ast::Member {
                node,
                property: Ast::String(segment),
            } if !segment.is_empty() && !segment.contains('.') => {
                let base = Self::segments(node, out)?;
                out.push(segment);
                Ok(base)
            }
            other => Err(Unsupported::new(format!(
                "member access on {}",
                <&'static str>::from(other)
            ))),
        }
    }

    fn named(&self, path: &str) -> Option<Named> {
        self.names
            .iter()
            .rev()
            .find(|(name, _)| name == path)
            .map(|(_, named)| *named)
    }

    fn own(&self, path: &str) -> Option<Named> {
        let from = match self.graph {
            true => self.local.min(self.names.len()),
            false => 0,
        };
        self.names[from..]
            .iter()
            .rev()
            .find(|(name, _)| name == path)
            .map(|(_, named)| *named)
    }

    fn scalar(named: Named, path: &str) -> Lowered {
        match named {
            Named::Node(id) => Ok(id),
            Named::Seq(_) => Err(Unsupported::new(format!("list {path} used as a value"))),
            Named::Val(_) => Err(Unsupported::new(format!(
                "composite {path} used as a value"
            ))),
        }
    }

    fn overlap(&self, path: &str) -> Result<(), Unsupported> {
        let inside = |outer: &str, inner: &str| {
            inner.len() > outer.len()
                && inner.starts_with(outer)
                && inner.as_bytes()[outer.len()] == b'.'
        };
        match self
            .names
            .iter()
            .any(|(name, _)| inside(name, path) || inside(path, name))
        {
            true => Err(Unsupported::new(format!("{path} overlaps a written path"))),
            false => Ok(()),
        }
    }

    fn path(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        let mut segments = Vec::new();
        let base = match Self::segments(ast, &mut segments) {
            Ok(base) => base,
            Err(e) => return self.computed(ast, ctx, r).map_err(|_| e),
        };
        let rest = segments.join(".");
        match base {
            Base::Pointer if ctx.elem.is_some() => {
                match self.descend_elem(ctx, &segments) {
                    Some(Val::Node(id)) => Ok(id),
                    _ => Err(Unsupported::new("member of a composite element")),
                }
            }
            Base::Pointer => match (ctx.item, ctx.pointer, rest.is_empty()) {
                (Some(item), _, true) => Ok(item),
                (Some(_), _, false) => Err(Unsupported::new("member of a mapped element")),
                (None, Some(list), _) => self.field(list, &rest, None, r),
                (None, None, _) => Err(Unsupported::new("# outside a closure")),
            },
            Base::Dollar if segments.is_empty() => ctx
                .dollar
                .ok_or_else(|| Unsupported::new("$ outside a table cell")),
            Base::Dollar if ctx.dollar.is_some() => {
                Err(Unsupported::new("member of $ in a table cell"))
            }
            Base::Dollar => match self.reach(&rest) {
                Some(Val::Node(id)) => Ok(id),
                Some(Val::Seq(_)) => Err(Unsupported::new(format!(
                    "list $.{rest} used as a value"
                ))),
                Some(_) => Err(Unsupported::new(format!(
                    "composite $.{rest} used as a value"
                ))),
                None => Err(Unsupported::new(format!("unknown $.{rest}"))),
            },
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
                        return match (rest.is_empty(), ctx.siblings) {
                            (true, _) => Err(Unsupported::new(format!("whole element {name}"))),
                            (false, true) => Err(Unsupported::new(Self::CROSSED)),
                            (false, false) => self.field(list, &rest, Some(&path), r),
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
                    Some(Output {
                        collect: None,
                        shape: None,
                        node,
                        ..
                    }) => Ok(*node),
                    Some(_) => Err(Unsupported::new(format!("list {path} used as a value"))),
                    None => Err(Unsupported::new(format!("output {at} of {path}"))),
                },
                Resolved::Absent => Ok(self.konst(Lit::Null)),
                _ => Err(Unsupported::new(format!("path {path}"))),
            };
        }
        if let Some(c) = self.schema.column(path) {
            let ty = self.schema.columns[c as usize].ty;
            let read = self.add(Op::Column(c), ty, Scope::Row);
            return Ok(self.declared(None, path, read, Scope::Row));
        }
        if self.absent.iter().any(|a| a == path) {
            return Ok(self.konst(Lit::Null));
        }
        if self.schema.list(path).is_some() {
            return Err(Unsupported::new(format!("list {path} used as a value")));
        }
        match r.path(path) {
            Resolved::Column(class) => {
                let c = u16::try_from(self.schema.columns.len())
                    .map_err(|_| Unsupported::new("too many columns"))?;
                self.schema.columns.push(ColumnDef {
                    path: path.to_string(),
                    ty: class.ty(),
                });
                Ok(self.add(Op::Column(c), class.ty(), Scope::Row))
            }
            Resolved::Dated => {
                let c = u16::try_from(self.schema.columns.len())
                    .map_err(|_| Unsupported::new("too many columns"))?;
                self.schema.columns.push(ColumnDef {
                    path: path.to_string(),
                    ty: Class::Text.ty(),
                });
                let text = self.add(Op::Column(c), Class::Text.ty(), Scope::Row);
                self.dated.push((None, path.to_string()));
                Ok(self.declared(None, path, text, Scope::Row))
            }
            Resolved::Absent => {
                self.absent.push(path.to_string());
                Ok(self.konst(Lit::Null))
            }
            Resolved::Keyed(class, key) => self.keyed(class, &key),
            Resolved::List { .. } | Resolved::DatedList { .. } | Resolved::KeyedList(..) => {
                Err(Unsupported::new(format!("list {path} used as a value")))
            }
            Resolved::Output(_) | Resolved::Unsupported => {
                Err(Unsupported::new(format!("path {path}")))
            }
        }
    }

    fn declared(&mut self, list: Option<ListId>, path: &str, read: NodeId, scope: Scope) -> NodeId {
        match self.dated.iter().any(|(l, p)| *l == list && p == path) {
            true => self.add(
                Op::Clock(Clock::Declared, vec![read]),
                Ty::new(Kind::Date, true),
                scope,
            ),
            false => read,
        }
    }

    fn keyed(&mut self, class: Class, key: &str) -> Lowered {
        let c = match self.schema.column(key) {
            Some(c) => c,
            None => {
                let c = u16::try_from(self.schema.columns.len())
                    .map_err(|_| Unsupported::new("too many columns"))?;
                self.schema.columns.push(ColumnDef {
                    path: key.to_string(),
                    ty: class.ty(),
                });
                c
            }
        };
        let ty = self.schema.columns[c as usize].ty;
        match ty == class.ty() {
            true => Ok(self.add(Op::Column(c), ty, Scope::Row)),
            false => Err(Unsupported::new(format!(
                "column {key} read as two classes"
            ))),
        }
    }

    fn field(
        &mut self,
        list: ListId,
        name: &str,
        member: Option<&str>,
        r: &mut dyn Resolve,
    ) -> Lowered {
        if let Some(f) = self.schema.field(list, name) {
            let ty = self.schema.lists[list as usize].fields[f as usize].ty;
            let read = self.add(Op::Field(list, f), ty, Scope::Elem(list));
            return Ok(self.declared(Some(list), name, read, Scope::Elem(list)));
        }
        let own = match (member, self.member == Some(list)) {
            (None, true) => self.schema.lists[list as usize]
                .alias
                .as_ref()
                .map(|alias| format!("{alias}.{name}")),
            _ => None,
        };
        let Some(path) = member.or(own.as_deref()) else {
            return Err(Unsupported::new(format!("unknown field {name}")));
        };
        if self.absent.iter().any(|a| a == path) {
            return Ok(self.konst(Lit::Null));
        }
        match r.alias(name) {
            Resolved::Column(class) => {
                let fields = &mut self.schema.lists[list as usize].fields;
                let f =
                    u16::try_from(fields.len()).map_err(|_| Unsupported::new("too many fields"))?;
                fields.push(ColumnDef {
                    path: name.to_string(),
                    ty: class.ty(),
                });
                Ok(self.add(Op::Field(list, f), class.ty(), Scope::Elem(list)))
            }
            Resolved::Dated => {
                let fields = &mut self.schema.lists[list as usize].fields;
                let f =
                    u16::try_from(fields.len()).map_err(|_| Unsupported::new("too many fields"))?;
                fields.push(ColumnDef {
                    path: name.to_string(),
                    ty: Class::Text.ty(),
                });
                let text = self.add(Op::Field(list, f), Class::Text.ty(), Scope::Elem(list));
                self.dated.push((Some(list), name.to_string()));
                Ok(self.declared(Some(list), name, text, Scope::Elem(list)))
            }
            Resolved::Absent => {
                self.absent.push(path.to_string());
                Ok(self.konst(Lit::Null))
            }
            _ => Err(Unsupported::new(format!("member field {name}"))),
        }
    }

    fn binary(
        &mut self,
        left: &Ast,
        operator: Operator,
        right: &Ast,
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Lowered {
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
                let dated = |this: &Self, x: NodeId| this.kind(x) == Kind::Date;
                match op == Arith::Add
                    && (self.texty(a) || self.texty(b) || dated(self, a) || dated(self, b))
                {
                    true => {
                        let (a, b) = (self.rendered(a)?, self.rendered(b)?);
                        self.concat(vec![a, b])
                    }
                    false => self.arith(op, a, b),
                }
            }
            Operator::Comparison(ComparisonOperator::In | ComparisonOperator::NotIn) => {
                let a = self.expr(left, ctx, r)?;
                let test = match right {
                    Ast::Array(items) => {
                        let mark = self.mark();
                        match self.inset(a, items, ctx, r) {
                            Ok(test) => test,
                            Err(e) => {
                                self.rollback(mark);
                                self.within(a, right, ctx, r).map_err(|_| e)?
                            }
                        }
                    }
                    Ast::Interval {
                        left: lo,
                        right: hi,
                        left_bracket,
                        right_bracket,
                    } => self.inrange(a, (lo, hi), (*left_bracket, *right_bracket), ctx, r)?,
                    other => {
                        let mark = self.mark();
                        match self.seq(other, ctx, r).and_then(|seq| self.member(a, seq)) {
                            Ok(test) => test,
                            Err(e) => {
                                self.rollback(mark);
                                self.within(a, other, ctx, r).map_err(|_| e)?
                            }
                        }
                    }
                };
                match operator {
                    Operator::Comparison(ComparisonOperator::NotIn) => self.not(test),
                    _ => Ok(test),
                }
            }
            Operator::Comparison(
                op @ (ComparisonOperator::Equal | ComparisonOperator::NotEqual),
            ) => {
                let mark = self.mark();
                let direct = self.expr(left, ctx, r).and_then(|a| {
                    let b = self.expr(right, ctx, r)?;
                    let op = match op {
                        ComparisonOperator::Equal => Cmp::Eq,
                        _ => Cmp::Ne,
                    };
                    self.compare(op, a, b)
                });
                match direct {
                    Ok(id) => Ok(id),
                    Err(e) => {
                        self.rollback(mark.clone());
                        self.unequal(op == ComparisonOperator::NotEqual, left, right, ctx, r)
                            .map_err(|_| {
                                self.rollback(mark);
                                e
                            })
                    }
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
                Ok(self.add(
                    Op::Coalesce(ids[0], ids[1]),
                    Ty::new(ty.kind, nullable),
                    scope,
                ))
            }
            other => Err(Unsupported::new(format!("operator {other}"))),
        }
    }

    fn arith(&mut self, op: Arith, a: NodeId, b: NodeId) -> Lowered {
        let (Kind::Num(sa), Kind::Num(sb)) = (self.kind(a), self.kind(b)) else {
            return Err(Unsupported::new(format!(
                "arithmetic between {} and {}",
                self.ty(a),
                self.ty(b)
            )));
        };
        let divisor = match self.lit(b) {
            Some(Lit::Num(d)) => Some(*d),
            _ => None,
        };
        let ty = match op {
            Arith::Add | Arith::Sub => Ty::num(sa.filter(|x| sb == Some(*x))),
            Arith::Mul => Ty::num((sa == Some(0) && sb == Some(0)).then_some(0)),
            Arith::Div => Ty::new(
                Kind::Num(None),
                divisor.is_none_or(|d| d.is_zero() || Self::fractional(d)),
            ),
            Arith::Rem => Ty::new(Kind::Num(None), divisor.is_none_or(|d| d.is_zero())),
            Arith::Pow if divisor.is_some_and(|d| !d.decimal().fract().is_zero()) => {
                return Err(Unsupported::new("exponent must be an integer"))
            }
            Arith::Pow => Ty::num(None),
        };
        let (scope, ids) = self.scoped(&[a, b])?;
        Ok(self.add(Op::Arith(op, ids[0], ids[1]), ty, scope))
    }

    fn fractional(d: Dec) -> bool {
        u128::from(d.mant.unsigned_abs()) < 10u128.pow(u32::from(d.scale))
    }

    fn classify(&mut self, id: NodeId) -> Lowered {
        self.require(id, |k| k == Kind::Text, "classify")?;
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
        Ok(self.add(
            Op::Classify(id, dict),
            Ty::new(Kind::Code(dict), nullable),
            scope,
        ))
    }

    fn compare(&mut self, op: Cmp, a: NodeId, b: NodeId) -> Lowered {
        let (ka, kb) = (self.kind(a), self.kind(b));
        let eq = matches!(op, Cmp::Eq | Cmp::Ne);
        let plain = |x: NodeId, y: NodeId| match (self.lit(y), self.ty(x)) {
            (
                Some(Lit::Bool(b)),
                Ty {
                    kind: Kind::Bool,
                    nullable: false,
                },
            ) if self.lit(x).is_none() => Some((x, *b)),
            _ => None,
        };
        if let (true, Some((x, b))) = (eq, plain(a, b).or_else(|| plain(b, a))) {
            return match b == (op == Cmp::Eq) {
                true => Ok(x),
                false => self.not(x),
            };
        }
        if ka == Kind::Date || kb == Kind::Date {
            return self.dated(op, a, b);
        }
        let (a, b) = match (ka, kb) {
            (Kind::Text, Kind::Text) if eq => {
                match (self.lit(a).is_some(), self.lit(b).is_some()) {
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
                    (false, false) => (a, b),
                }
            }
            (Kind::Text, Kind::Null) if eq && self.direct(a) => (self.classify(a)?, b),
            (Kind::Null, Kind::Text) if eq && self.direct(b) => (a, self.classify(b)?),
            (Kind::Text, Kind::Null) | (Kind::Null, Kind::Text) if eq => (a, b),
            (Kind::Code(d), Kind::Text) if eq && self.lit(b).is_some() => (a, self.coerce(b, d)?),
            (Kind::Text, Kind::Code(d)) if eq && self.lit(a).is_some() => (self.coerce(a, d)?, b),
            (Kind::Code(_), Kind::Text) if eq => (self.decode(a)?, b),
            (Kind::Text, Kind::Code(_)) if eq => (a, self.decode(b)?),
            (Kind::Code(x), Kind::Code(y)) if eq && x == y => (a, b),
            (Kind::Num(_), Kind::Num(_)) | (Kind::Bool, Kind::Bool) if eq => (a, b),
            (Kind::Null, _) | (_, Kind::Null) if eq => (a, b),
            (Kind::Num(_), Kind::Num(_)) => (a, b),
            (Kind::Null, Kind::Num(_) | Kind::Null) | (Kind::Num(_), Kind::Null) => {
                self.widen();
                (a, b)
            }
            (x, y)
                if eq
                    && Self::family(x)
                        .zip(Self::family(y))
                        .is_some_and(|(p, q)| p != q) =>
            {
                return self.mismatch(op, a, b)
            }
            _ => {
                return Err(Unsupported::new(format!(
                    "compare {op:?} between {ka} and {kb}"
                )))
            }
        };
        let (scope, ids) = self.scoped(&[a, b])?;
        Ok(self.add(Op::Cmp(op, ids[0], ids[1]), Ty::boolean(), scope))
    }

    fn family(kind: Kind) -> Option<u8> {
        match kind {
            Kind::Num(_) => Some(0),
            Kind::Bool => Some(1),
            Kind::Text | Kind::Code(_) => Some(2),
            Kind::Date => Some(3),
            Kind::Null => None,
        }
    }

    fn settled(&self, id: NodeId) -> bool {
        match &self.nodes[id as usize].op {
            Op::Column(_) | Op::Field(..) | Op::Const(_) => true,
            Op::Parent(inner) => self.settled(*inner),
            _ => {
                self.names
                    .iter()
                    .any(|(_, named)| *named == Named::Node(id))
                    || self
                        .parts
                        .iter()
                        .any(|part| part.collect.is_none() && part.node == id)
            }
        }
    }

    fn mismatch(&mut self, op: Cmp, a: NodeId, b: NodeId) -> Lowered {
        let test = match self.ty(a).nullable && self.ty(b).nullable {
            false => self.konst(Lit::Bool(op == Cmp::Ne)),
            true => {
                let null = self.konst(Lit::Null);
                let x = self.compare(Cmp::Eq, a, null)?;
                let y = self.compare(Cmp::Eq, b, null)?;
                let both = self.logic(true, x, y)?;
                match op {
                    Cmp::Eq => both,
                    _ => self.not(both)?,
                }
            }
        };
        let test = self.checked(b, test)?;
        self.checked(a, test)
    }

    fn coerce(&mut self, id: NodeId, dict: DictId) -> Lowered {
        match self.lit(id) {
            Some(Lit::Text(text)) => {
                let text = text.clone();
                let code = self.code(dict, &text);
                Ok(self.konst(Lit::Code(dict, code)))
            }
            _ if self.kind(id) == Kind::Code(dict) || self.kind(id) == Kind::Null => Ok(id),
            _ => Err(Unsupported::new(format!(
                "cannot code {} as dict {dict}",
                self.ty(id)
            ))),
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

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn inset(&mut self, a: NodeId, items: &[&Ast], ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        if self.kind(a) == Kind::Date {
            return self.dates_in(a, items, ctx, r);
        }
        if matches!(self.kind(a), Kind::Text | Kind::Code(_)) {
            let mut dates = Vec::new();
            let mut rest = Vec::with_capacity(items.len());
            for item in items {
                let id = self.expr(item, ctx, r)?;
                match self.lit(id) {
                    Some(Lit::Date(_)) => dates.push(id),
                    _ => rest.push(*item),
                }
            }
            if !dates.is_empty() {
                let mut hit = self.inset(a, &rest, ctx, r)?;
                for date in dates {
                    let found = self.belongs(date, a)?;
                    hit = self.logic(false, hit, found)?;
                }
                return Ok(hit);
            }
        }
        let a = match self.kind(a) {
            Kind::Text => self.classify(a)?,
            _ => a,
        };
        let kind = self.kind(a);
        let mut set: Vec<Lit> = Vec::new();
        for item in items {
            let id = self.expr(item, ctx, r)?;
            let lit = self
                .lit(id)
                .cloned()
                .ok_or_else(|| Unsupported::new("in: non-literal set item"))?;
            let lit = match lit {
                Lit::Code(d, c) if self.schema.dicts[d as usize].kind == DictKind::Literals => {
                    let text = self.schema.dicts[d as usize]
                        .text(c)
                        .ok_or_else(|| Unsupported::new("in: unknown literal code"))?;
                    Lit::Text(text.to_string())
                }
                Lit::Code(..) => return Err(Unsupported::new("in: coded set item")),
                other => other,
            };
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

    fn inrange(
        &mut self,
        a: NodeId,
        (lo, hi): (&Ast, &Ast),
        (left, right): (Bracket, Bracket),
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Lowered {
        if self.kind(a) == Kind::Date {
            return self.date_range(a, (lo, hi), (left, right), ctx, r);
        }
        self.require(a, |k| matches!(k, Kind::Num(_)), "interval operand")?;
        let lo = self.expr(lo, ctx, r)?;
        let hi = self.expr(hi, ctx, r)?;
        let (Some(Lit::Num(lo)), Some(Lit::Num(hi))) =
            (self.lit(lo).cloned(), self.lit(hi).cloned())
        else {
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
        Ok(self.add(
            Op::InRange(
                a,
                Range {
                    lo,
                    lo_closed,
                    hi,
                    hi_closed,
                },
            ),
            Ty::boolean(),
            scope,
        ))
    }

    fn unify(&mut self, ids: Vec<NodeId>) -> Result<(Vec<NodeId>, Ty), Unsupported> {
        let computed = ids
            .iter()
            .any(|id| self.kind(*id) == Kind::Text && self.lit(*id).is_none());
        let ids = match computed {
            true => ids
                .into_iter()
                .map(|id| match self.kind(id) {
                    Kind::Code(_) => self.decode(id),
                    _ => Ok(id),
                })
                .collect::<Result<Vec<_>, _>>()?,
            false => ids,
        };
        let mut dict: Option<DictId> = None;
        for id in &ids {
            match self.kind(*id) {
                Kind::Code(d) if dict.is_some_and(|x| x != d) => {
                    return Err(Unsupported::new("branches code different dictionaries"))
                }
                Kind::Code(d) => dict = Some(d),
                _ => {}
            }
        }
        let texts = !computed && ids.iter().any(|id| self.kind(*id) == Kind::Text);
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
                (Some(Kind::Num(x)), Kind::Num(y)) => {
                    Some(Kind::Num(if x == y { x } else { None }))
                }
                (Some(x), y) if x == y => Some(x),
                (Some(x), y) => {
                    return Err(Unsupported::new(format!("branches of type {x} and {y}")))
                }
            };
        }
        Ok((
            out,
            Ty::new(kind.unwrap_or(Kind::Null), nullable || kind.is_none()),
        ))
    }

    fn call(
        &mut self,
        kind: &FunctionKind,
        args: &[&Ast],
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Lowered {
        let mark = self.mark();
        match self.called(kind, args, ctx, r) {
            Ok(id) => Ok(id),
            Err(e) => {
                self.rollback(mark.clone());
                match self.statically(kind, args, ctx, r) {
                    Ok(id) => Ok(id),
                    Err(_) => {
                        self.rollback(mark);
                        Err(e)
                    }
                }
            }
        }
    }

    fn called(
        &mut self,
        kind: &FunctionKind,
        args: &[&Ast],
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Lowered {
        match (kind, args) {
            (
                FunctionKind::Internal(
                    f @ (InternalFunction::Sum
                    | InternalFunction::Min
                    | InternalFunction::Max
                    | InternalFunction::Avg),
                ),
                [arg],
            ) if !matches!(arg, Ast::Array(_)) => {
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
            (FunctionKind::Internal(InternalFunction::Contains), [Ast::Array(items), x]) => {
                self.widen();
                let x = self.expr(x, ctx, r)?;
                let items = self.literals_of(items, ctx, r)?;
                let mut hits = Vec::with_capacity(items.len());
                for item in items {
                    hits.push(self.belongs(x, item)?);
                }
                let found = self.chain(false, hits)?;
                self.checked(x, found)
            }
            (
                FunctionKind::Closure(
                    c @ (ClosureFunction::Some | ClosureFunction::All | ClosureFunction::None),
                ),
                [Ast::Array(items), body],
            ) => {
                self.widen();
                let body = Self::closure(body)?;
                let items = self.literals_of(items, ctx, r)?;
                let mut tests = Vec::with_capacity(items.len());
                for item in items {
                    let test = self.expr(
                        body,
                        Ctx {
                            item: Some(item),
                            ..ctx
                        },
                        r,
                    )?;
                    if self.ty(test) != Ty::boolean() {
                        return Err(Unsupported::new(format!(
                            "predicate: unexpected type {}",
                            self.ty(test)
                        )));
                    }
                    tests.push(test);
                }
                match c {
                    ClosureFunction::Some => self.chain(false, tests),
                    ClosureFunction::All => self.chain(true, tests),
                    _ => {
                        let any = self.chain(false, tests)?;
                        self.not(any)
                    }
                }
            }
            (FunctionKind::Internal(InternalFunction::Contains), [list, x]) => {
                match self.seq(list, ctx, r) {
                    Ok(seq) => {
                        let x = self.expr(x, ctx, r)?;
                        self.member(x, seq)
                    }
                    Err(e) => {
                        let a = self.expr(list, ctx, r).map_err(|_| e)?;
                        let b = self.expr(x, ctx, r)?;
                        self.test(Func::Contains, a, b)
                    }
                }
            }
            (FunctionKind::Internal(InternalFunction::Len), [arg]) => {
                match self.seq(arg, ctx, r).and_then(Self::pure) {
                    Ok(seq) => self.fold(Fold::Count, seq, seq.keep, None),
                    Err(_) => {
                        let a = self.expr(arg, ctx, r)?;
                        let a = self.textual(a)?;
                        if let Some(Lit::Text(t)) = self.lit(a) {
                            let n = Dec::int(t.len() as i64);
                            return Ok(self.konst(Lit::Num(n)));
                        }
                        let scope = self.nodes[a as usize].scope;
                        Ok(self.add(Op::Call(Func::Len, vec![a]), Ty::num(Some(0)), scope))
                    }
                }
            }
            (
                FunctionKind::Internal(
                    f @ (InternalFunction::StartsWith
                    | InternalFunction::EndsWith
                    | InternalFunction::Matches),
                ),
                [a, b],
            ) => {
                let a = self.expr(a, ctx, r)?;
                let b = self.expr(b, ctx, r)?;
                let func = match f {
                    InternalFunction::StartsWith => Func::StartsWith,
                    InternalFunction::EndsWith => Func::EndsWith,
                    _ => Func::Matches,
                };
                self.test(func, a, b)
            }
            (FunctionKind::Internal(InternalFunction::String), [arg]) => {
                let a = self.expr(arg, ctx, r)?;
                self.stringify(a)
            }
            (
                FunctionKind::Internal(
                    f
                    @ (InternalFunction::Upper | InternalFunction::Lower | InternalFunction::Trim),
                ),
                [arg],
            ) => {
                let a = self.expr(arg, ctx, r)?;
                let func = match f {
                    InternalFunction::Upper => Func::Upper,
                    InternalFunction::Lower => Func::Lower,
                    _ => Func::Trim,
                };
                self.mapped(func, a)
            }
            (FunctionKind::Internal(InternalFunction::Bool), [arg]) => {
                let a = self.expr(arg, ctx, r)?;
                match self.ty(a) {
                    Ty {
                        kind: Kind::Bool,
                        nullable: false,
                    } => Ok(a),
                    Ty {
                        kind: Kind::Bool,
                        nullable: true,
                    } => {
                        let no = self.konst(Lit::Bool(false));
                        let (scope, ids) = self.scoped(&[a, no])?;
                        Ok(self.add(Op::Coalesce(ids[0], ids[1]), Ty::boolean(), scope))
                    }
                    Ty {
                        kind: Kind::Null, ..
                    } => {
                        let no = self.konst(Lit::Bool(false));
                        self.checked(a, no)
                    }
                    ty => Err(Unsupported::new(format!("bool of {ty}"))),
                }
            }
            (
                FunctionKind::Internal(
                    f @ (InternalFunction::Abs | InternalFunction::Floor | InternalFunction::Ceil),
                ),
                [arg],
            ) => {
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
            (
                FunctionKind::Internal(f @ (InternalFunction::Round | InternalFunction::Trunc)),
                [arg, rest @ ..],
            ) => {
                let a = self.expr(arg, ctx, r)?;
                let Kind::Num(scale) = self.kind(a) else {
                    return Err(Unsupported::new(format!("{f:?} of {}", self.ty(a))));
                };
                let places = match rest {
                    [] => 0,
                    [places] => {
                        let p = self.expr(places, ctx, r)?;
                        match self.lit(p) {
                            Some(Lit::Num(d))
                                if d.scale == 0
                                    && (0..=i64::from(Dec::MAX_SCALE)).contains(&d.mant) =>
                            {
                                d.mant as u8
                            }
                            _ => {
                                return Err(Unsupported::new(
                                    "decimal places must be a small integer literal",
                                ))
                            }
                        }
                    }
                    _ => return Err(Unsupported::new(format!("{f:?} arity"))),
                };
                let (func, ty) = match f {
                    InternalFunction::Round => {
                        (Func::Round(places), Ty::num(scale.map(|s| s.min(places))))
                    }
                    _ => (Func::Trunc(places), Ty::num(Some(places))),
                };
                self.math(func, vec![a], ty)
            }
            (
                FunctionKind::Internal(
                    f @ (InternalFunction::Min
                    | InternalFunction::Max
                    | InternalFunction::Avg
                    | InternalFunction::Sum),
                ),
                [Ast::Array(items)],
            ) => {
                if items.is_empty() {
                    return Err(Unsupported::new("aggregate of an empty list"));
                }
                let mut args = Vec::with_capacity(items.len());
                for item in items.iter() {
                    args.push(self.expr(item, ctx, r)?);
                }
                let dates = matches!(f, InternalFunction::Min | InternalFunction::Max)
                    && args.iter().all(|a| self.kind(*a) == Kind::Date);
                if !dates {
                    for a in &args {
                        self.require(*a, |k| matches!(k, Kind::Num(_)), "aggregate item")?;
                    }
                }
                match f {
                    InternalFunction::Min | InternalFunction::Max => {
                        let first = self.kind(args[0]);
                        let kind = match args.iter().all(|a| self.kind(*a) == first) {
                            true => first,
                            false => Kind::Num(None),
                        };
                        let func = if *f == InternalFunction::Min {
                            Func::Min
                        } else {
                            Func::Max
                        };
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
            (
                FunctionKind::Closure(
                    c @ (ClosureFunction::Some
                    | ClosureFunction::All
                    | ClosureFunction::None
                    | ClosureFunction::One),
                ),
                [source, body],
            ) => {
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
            (
                FunctionKind::Internal(f @ (InternalFunction::Number | InternalFunction::Type)),
                [arg],
            ) => {
                let a = self.expr(arg, ctx, r)?;
                self.convert(*f, a)
            }
            (FunctionKind::Internal(InternalFunction::Date), args) => self.construct(args, ctx, r),
            (FunctionKind::Deprecated(f), args) => self.legacy(*f, args, ctx, r),
            (kind, _) => Err(Unsupported::new(format!("function {kind:?}"))),
        }
    }

    fn literals_of(
        &mut self,
        items: &[&Ast],
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Result<Vec<NodeId>, Unsupported> {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let id = self.expr(item, ctx, r)?;
            match self.lit(id) {
                Some(Lit::Num(_) | Lit::Bool(_) | Lit::Text(_) | Lit::Null) => out.push(id),
                _ => return Err(Unsupported::new("list literal: non-literal item")),
            }
        }
        Ok(out)
    }

    fn chain(&mut self, and: bool, tests: Vec<NodeId>) -> Lowered {
        let mut tests = tests.into_iter();
        let Some(mut acc) = tests.next() else {
            return Ok(self.konst(Lit::Bool(and)));
        };
        for test in tests {
            acc = self.logic(and, acc, test)?;
        }
        Ok(acc)
    }

    fn direct(&self, id: NodeId) -> bool {
        matches!(self.nodes[id as usize].op, Op::Column(_) | Op::Field(..))
    }

    fn texty(&self, id: NodeId) -> bool {
        match self.kind(id) {
            Kind::Text => true,
            Kind::Code(d) => self.schema.dicts[d as usize].kind == DictKind::Literals,
            _ => false,
        }
    }

    fn decode(&mut self, id: NodeId) -> Lowered {
        let Kind::Code(d) = self.kind(id) else {
            return Err(Unsupported::new(format!("decode of {}", self.ty(id))));
        };
        let dict = &self.schema.dicts[d as usize];
        if dict.kind != DictKind::Literals {
            return Err(Unsupported::new("dictionary mixing"));
        }
        if let Some(Lit::Code(_, c)) = self.lit(id) {
            let text = dict
                .text(*c)
                .ok_or_else(|| Unsupported::new("decode of an unknown code"))?
                .to_string();
            return Ok(self.konst(Lit::Text(text)));
        }
        let node = &self.nodes[id as usize];
        let (nullable, scope) = (node.ty.nullable, node.scope);
        Ok(self.add(Op::Decode(id), Ty::new(Kind::Text, nullable), scope))
    }

    fn textual(&mut self, id: NodeId) -> Lowered {
        match self.kind(id) {
            Kind::Text => Ok(id),
            Kind::Code(_) => self.decode(id),
            _ => Err(Unsupported::new(format!(
                "string function on {}",
                self.ty(id)
            ))),
        }
    }

    fn stringify(&mut self, a: NodeId) -> Lowered {
        let text = match self.lit(a) {
            Some(Lit::Text(_)) => return Ok(a),
            Some(Lit::Null) => Some("null".to_string()),
            Some(Lit::Bool(b)) => Some(b.to_string()),
            Some(Lit::Num(d)) => Some(d.decimal().to_string()),
            Some(Lit::Code(..)) => return self.decode(a),
            Some(Lit::Date(d)) => Some(d.to_string()),
            None => None,
        };
        if let Some(text) = text {
            return Ok(self.konst(Lit::Text(text)));
        }
        let a = match self.ty(a) {
            Ty {
                kind: Kind::Text,
                nullable: false,
            } => return Ok(a),
            Ty {
                kind: Kind::Code(_),
                ..
            } => self.decode(a)?,
            Ty {
                kind: Kind::Text | Kind::Num(_) | Kind::Bool | Kind::Null | Kind::Date,
                ..
            } => a,
        };
        let scope = self.nodes[a as usize].scope;
        Ok(self.add(Op::Str(a), Ty::new(Kind::Text, false), scope))
    }

    fn concat(&mut self, ids: Vec<NodeId>) -> Lowered {
        let mut parts: Vec<NodeId> = Vec::with_capacity(ids.len());
        for id in ids {
            let id = self.textual(id)?;
            let nested = match &self.nodes[id as usize].op {
                Op::Concat(inner) => inner.clone(),
                _ => vec![id],
            };
            for part in nested {
                let joined = match (parts.last().and_then(|p| self.lit(*p)), self.lit(part)) {
                    (Some(Lit::Text(x)), Some(Lit::Text(y))) => Some(format!("{x}{y}")),
                    _ => None,
                };
                match joined {
                    Some(text) => {
                        parts.pop();
                        parts.push(self.konst(Lit::Text(text)));
                    }
                    None => parts.push(part),
                }
            }
        }
        if let [only] = parts.as_slice() {
            if self.lit(*only).is_some() || !self.ty(*only).nullable {
                return Ok(*only);
            }
        }
        let (scope, ids) = self.scoped(&parts)?;
        Ok(self.add(Op::Concat(ids), Ty::new(Kind::Text, false), scope))
    }

    fn template(&mut self, parts: &[&Ast], ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        if parts.is_empty() {
            return Err(Unsupported::new("empty template"));
        }
        let mut ids = Vec::with_capacity(parts.len());
        for part in parts {
            let id = self.expr(part, ctx, r)?;
            ids.push(self.stringify(id)?);
        }
        self.concat(ids)
    }

    fn mapped(&mut self, func: Func, a: NodeId) -> Lowered {
        let a = self.textual(a)?;
        if let Some(Lit::Text(t)) = self.lit(a) {
            let text = match func {
                Func::Upper => t.to_uppercase(),
                Func::Lower => t.to_lowercase(),
                _ => t.trim().to_string(),
            };
            return Ok(self.konst(Lit::Text(text)));
        }
        let scope = self.nodes[a as usize].scope;
        Ok(self.add(Op::Call(func, vec![a]), Ty::new(Kind::Text, false), scope))
    }

    fn test(&mut self, func: Func, a: NodeId, b: NodeId) -> Lowered {
        let failing = |this: &mut Self, x: NodeId| {
            let null = func != Func::Matches && this.kind(x) == Kind::Null;
            if null {
                this.widen();
            }
            null
        };
        let a = if failing(self, a) {
            a
        } else {
            self.textual(a)?
        };
        let b = if failing(self, b) {
            b
        } else {
            self.textual(b)?
        };
        if func == Func::Matches {
            let Some(Lit::Text(pattern)) = self.lit(b) else {
                return Err(Unsupported::new(
                    "matches: pattern must be a string literal",
                ));
            };
            Pattern::new(pattern).map_err(|_| Unsupported::new("matches: invalid pattern"))?;
        }
        if let (Some(Lit::Text(x)), Some(Lit::Text(y))) = (self.lit(a), self.lit(b)) {
            let hit = match func {
                Func::StartsWith => Some(x.starts_with(y.as_str())),
                Func::EndsWith => Some(x.ends_with(y.as_str())),
                Func::Contains => Some(x.contains(y.as_str())),
                _ => None,
            };
            if let Some(hit) = hit {
                return Ok(self.konst(Lit::Bool(hit)));
            }
        }
        let (scope, ids) = self.scoped(&[a, b])?;
        Ok(self.add(Op::Call(func, ids), Ty::boolean(), scope))
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
        Ctx {
            scope: Scope::Elem(seq.list),
            pointer: Some(seq.list),
            item: seq.value,
            dollar: ctx.dollar,
            list: ctx.list,
            elem: None,
            home: ctx.home,
            siblings: ctx.siblings || ctx.home == Some(seq.list),
        }
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

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn seq(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Result<Seq, Unsupported> {
        match ast {
            Ast::Parenthesized(inner) => self.seq(inner, ctx, r),
            Ast::FunctionCall {
                kind: FunctionKind::Closure(ClosureFunction::Filter),
                arguments: [source, body],
            } => {
                let seq = Self::pure(self.seq(source, ctx, r)?)?;
                let body = Self::closure(body)?;
                let test = self.expr(body, Self::inner(ctx, seq), r)?;
                self.require(test, |k| k == Kind::Bool, "filter")?;
                let test = self.lifted(test, seq.list)?;
                let keep = match seq.keep {
                    None => test,
                    Some(keep) => self.logic(true, keep, test)?,
                };
                Ok(Seq {
                    keep: Some(keep),
                    ..seq
                })
            }
            Ast::FunctionCall {
                kind: FunctionKind::Closure(ClosureFunction::Map),
                arguments: [source, body],
            } => {
                let seq = Self::pure(self.seq(source, ctx, r)?)?;
                let body = Self::closure(body)?;
                let value = self.expr(body, Self::inner(ctx, seq), r)?;
                let value = self.lifted(value, seq.list)?;
                Ok(Seq {
                    value: Some(value),
                    pure: false,
                    ..seq
                })
            }
            Ast::Binary {
                left,
                operator: Operator::Logical(LogicalOperator::NullishCoalescing),
                right: Ast::Array([]),
            } => {
                let seq = self.seq(left, ctx, r)?;
                if seq.keep.is_some() || seq.value.is_some() {
                    return Err(Unsupported::new("?? [] after a closure"));
                }
                Ok(Seq { empty: true, ..seq })
            }
            other => self.source(other, ctx, r),
        }
    }

    fn plain(list: ListId) -> Seq {
        Seq {
            list,
            keep: None,
            value: None,
            empty: false,
            pure: true,
        }
    }

    fn produced(&self, at: usize, path: &str) -> Result<Seq, Unsupported> {
        match self.outputs.get(at).and_then(|o| o.collect) {
            Some(c) => Ok(Seq {
                list: c.list,
                keep: c.keep,
                value: Some(c.item),
                empty: c.empty,
                pure: true,
            }),
            None => Err(Unsupported::new(format!("{path} is not a list"))),
        }
    }

    fn source(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Result<Seq, Unsupported> {
        let mut segments = Vec::new();
        let base = Self::segments(ast, &mut segments)?;
        let rest = segments.join(".");
        match base {
            Base::Pointer => {
                let parent = ctx
                    .pointer
                    .ok_or_else(|| Unsupported::new("# outside a closure"))?;
                if ctx.item.is_some() || rest.is_empty() {
                    return Err(Unsupported::new("list of lists"));
                }
                Ok(Self::plain(self.nested(parent, &rest, r)?))
            }
            Base::Dollar if rest.is_empty() && ctx.list.is_some() => {
                ctx.list.ok_or_else(|| Unsupported::new("list source $"))
            }
            Base::Dollar if ctx.dollar.is_some() || rest.is_empty() => {
                Err(Unsupported::new("list source $"))
            }
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
                    false => self.root_list(name, &rest, &path, ctx, r),
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
                Ok(Self::plain(self.declare(
                    key.to_string(),
                    fields,
                    Vec::new(),
                    None,
                )?))
            }
            Resolved::Output(at) => self.produced(at, path),
            _ => Err(Unsupported::new(format!("{path} is not a list"))),
        }
    }

    fn root_list(
        &mut self,
        name: &str,
        rest: &str,
        path: &str,
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Result<Seq, Unsupported> {
        self.overlap(path)?;
        let across = match ctx.scope {
            Scope::Elem(list) if self.member == Some(list) && !ctx.siblings => Some(list),
            _ => None,
        };
        match self.named(path) {
            Some(Named::Seq(_)) if self.outer && across.is_some() => {
                return Err(Unsupported::new(format!("list {path} made in the same pass")))
            }
            Some(Named::Seq(seq)) => return Ok(seq),
            Some(Named::Node(_) | Named::Val(_)) => {
                return Err(Unsupported::new(format!("{path} is not a list")))
            }
            None => {}
        }
        if let Some(member) = self.member {
            if self.schema.lists[member as usize].alias.as_deref() == Some(name) {
                return match (rest.is_empty(), ctx.siblings) {
                    (true, _) => Err(Unsupported::new(format!("whole element {name}"))),
                    (false, true) => Err(Unsupported::new(Self::CROSSED)),
                    (false, false) => Ok(Self::plain(self.nested(member, rest, r)?)),
                };
            }
        }
        if let (true, Some(list)) = (self.outer, across) {
            let outer = format!("{}{path}", Self::OUTER);
            return Ok(Self::plain(self.nested(list, &outer, r)?));
        }
        if let Some(list) = self.schema.list(path) {
            return Ok(Self::plain(list));
        }
        if self.schema.column(path).is_some() {
            return Err(Unsupported::new(format!("{path} is not a list")));
        }
        match r.path(path) {
            Resolved::List { fields } => Ok(Self::plain(self.declare(
                path.to_string(),
                fields,
                Vec::new(),
                None,
            )?)),
            Resolved::DatedList { fields, dated } => Ok(Self::plain(self.declare(
                path.to_string(),
                fields,
                dated,
                None,
            )?)),
            _ => Err(Unsupported::new(format!("{path} is not a list"))),
        }
    }

    fn nested(
        &mut self,
        parent: ListId,
        field: &str,
        r: &mut dyn Resolve,
    ) -> Result<ListId, Unsupported> {
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
            Resolved::List { fields } => {
                self.declare(field.to_string(), fields, Vec::new(), Some(parent))
            }
            Resolved::DatedList { fields, dated } => {
                self.declare(field.to_string(), fields, dated, Some(parent))
            }
            _ => Err(Unsupported::new(format!("nested list {field}"))),
        }
    }

    fn declare(
        &mut self,
        path: String,
        fields: Vec<(Arc<str>, Class)>,
        dated: Vec<Arc<str>>,
        parent: Option<ListId>,
    ) -> Result<ListId, Unsupported> {
        let l = ListId::try_from(self.schema.lists.len())
            .map_err(|_| Unsupported::new("too many lists"))?;
        let texts = dated.iter().map(|name| (name.clone(), Class::Text));
        let fields = fields
            .into_iter()
            .chain(texts)
            .map(|(name, class)| ColumnDef {
                path: name.to_string(),
                ty: class.ty(),
            })
            .collect();
        self.dated
            .extend(dated.iter().map(|name| (Some(l), name.to_string())));
        self.schema.lists.push(ListDef {
            path,
            alias: None,
            fields,
            parent,
        });
        Ok(l)
    }

    fn predicate(&mut self, seq: Seq, body: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        let test = self.expr(Self::closure(body)?, Self::inner(ctx, seq), r)?;
        self.require(test, |k| k == Kind::Bool, "predicate")?;
        self.lifted(test, seq.list)
    }

    fn member(&mut self, x: NodeId, seq: Seq) -> Lowered {
        let seq = Self::pure(seq)?;
        if self
            .schema
            .within(self.nodes[x as usize].scope, Scope::Elem(seq.list))
        {
            return self.in_row(x, seq);
        }
        if let Some(found) = self.in_root(x, seq) {
            return found;
        }
        let item = self.item(seq)?;
        let test = self.belongs(x, item)?;
        let test = self.lifted(test, seq.list)?;
        let found = self.fold(Fold::Any, seq, seq.keep, Some(test))?;
        self.checked(x, found)
    }

    fn in_root(&mut self, x: NodeId, seq: Seq) -> Option<Lowered> {
        let Scope::Elem(of) = self.nodes[x as usize].scope else {
            return None;
        };
        if of == seq.list
            || seq.keep.is_some()
            || seq.value.is_some()
            || self.schema.lists[seq.list as usize].parent.is_some()
        {
            return None;
        }
        let field = self.schema.field(seq.list, "")?;
        let ty = self.schema.lists[seq.list as usize].fields[field as usize].ty;
        match (self.kind(x), ty.kind) {
            (Kind::Num(_), Kind::Num(_)) | (Kind::Bool, Kind::Bool) | (Kind::Text, Kind::Text) => {}
            _ => return None,
        }
        let found = self.add(
            Op::InRow(x, seq.list, field, Some(of)),
            Ty::boolean(),
            Scope::Elem(of),
        );
        Some(self.checked(x, found))
    }

    fn in_row(&mut self, x: NodeId, seq: Seq) -> Lowered {
        let refused = || Unsupported::new("membership in the enclosing list");
        if seq.keep.is_some()
            || seq.value.is_some()
            || self.nodes[x as usize].scope != Scope::Elem(seq.list)
        {
            return Err(refused());
        }
        let field = self.schema.field(seq.list, "").ok_or_else(refused)?;
        let ty = self.schema.lists[seq.list as usize].fields[field as usize].ty;
        match (self.kind(x), ty.kind) {
            (Kind::Num(_), Kind::Num(_)) | (Kind::Bool, Kind::Bool) | (Kind::Text, Kind::Text) => {}
            _ => return Err(refused()),
        }
        let found = self.add(
            Op::InRow(x, seq.list, field, None),
            Ty::boolean(),
            Scope::Elem(seq.list),
        );
        self.checked(x, found)
    }

    fn fold(
        &mut self,
        fold: Fold,
        seq: Seq,
        filter: Option<NodeId>,
        value: Option<NodeId>,
    ) -> Lowered {
        let list = seq.list;
        let filter = filter.map(|f| self.lifted(f, list)).transpose()?;
        let value = match value {
            Some(id) => {
                match (fold.quantifier(), fold) {
                    (true, _) => self.require(id, |k| k == Kind::Bool, "predicate")?,
                    (false, Fold::Min | Fold::Max) => {
                        self.require(id, |k| matches!(k, Kind::Num(_) | Kind::Date), "fold value")?
                    }
                    (false, _) => self.require(id, |k| matches!(k, Kind::Num(_)), "fold value")?,
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
        let scope = self
            .schema
            .owner(Scope::Elem(list))
            .ok_or_else(|| Unsupported::new("fold owner"))?;
        Ok(self.add(
            Op::Fold {
                fold,
                list,
                filter,
                value,
                empty: seq.empty,
            },
            ty,
            scope,
        ))
    }
}

impl Lowering {
    fn returns(method: DateMethod) -> Ty {
        match method {
            DateMethod::Add
            | DateMethod::Sub
            | DateMethod::Set
            | DateMethod::StartOf
            | DateMethod::EndOf
            | DateMethod::Tz => Ty::new(Kind::Date, false),
            DateMethod::Format => Ty::new(Kind::Text, false),
            DateMethod::OffsetName => Ty::new(Kind::Text, true),
            DateMethod::IsSame
            | DateMethod::IsBefore
            | DateMethod::IsAfter
            | DateMethod::IsSameOrBefore
            | DateMethod::IsSameOrAfter
            | DateMethod::IsValid => Ty::boolean(),
            DateMethod::IsYesterday
            | DateMethod::IsToday
            | DateMethod::IsTomorrow
            | DateMethod::IsLeapYear => Ty::new(Kind::Bool, true),
            DateMethod::Diff
            | DateMethod::Second
            | DateMethod::Minute
            | DateMethod::Hour
            | DateMethod::Day
            | DateMethod::DayOfYear
            | DateMethod::Week
            | DateMethod::Weekday
            | DateMethod::Month
            | DateMethod::Quarter
            | DateMethod::Year
            | DateMethod::Timestamp => Ty::new(Kind::Num(Some(0)), true),
        }
    }

    fn arity(def: &dyn FunctionDefinition, skip: usize, given: usize) -> Result<(), Unsupported> {
        let least = def.required_parameters().saturating_sub(skip);
        match (least..=least + def.optional_parameters()).contains(&given) {
            true => Ok(()),
            false => Err(Unsupported::new("call arity")),
        }
    }

    fn loose(&mut self, id: NodeId) -> Lowered {
        match self.kind(id) {
            Kind::Code(_) => self.decode(id),
            _ => Ok(id),
        }
    }

    fn arguments(
        &mut self,
        arguments: &[&Ast],
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Result<Vec<NodeId>, Unsupported> {
        let mut args = Vec::with_capacity(arguments.len());
        for a in arguments {
            let id = self.expr(a, ctx, r)?;
            args.push(self.loose(id)?);
        }
        Ok(args)
    }

    fn method(
        &mut self,
        method: DateMethod,
        this: &Ast,
        arguments: &[&Ast],
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Lowered {
        let def = MethodRegistry::get_definition(&MethodKind::DateMethod(method))
            .ok_or_else(|| Unsupported::new(format!("method {method}")))?;
        Self::arity(def.as_ref(), 1, arguments.len())?;
        let this = self.expr(this, ctx, r)?;
        self.require(this, |k| k == Kind::Date, "method receiver")?;
        let mut args = vec![this];
        args.extend(self.arguments(arguments, ctx, r)?);
        if method == DateMethod::Format {
            match args.get(1).map(|a| self.lit(*a)) {
                None => {}
                Some(Some(Lit::Text(pattern))) if Effect::formats(pattern) => {}
                _ => return Err(Unsupported::new("format pattern")),
            }
        }
        let pure = Effect::method(&MethodKind::DateMethod(method)) == Effect::Pure;
        self.host(Host::Method(method), args, Self::returns(method), pure)
    }

    fn construct(&mut self, arguments: &[&Ast], ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        let def = FunctionRegistry::get_definition(&FunctionKind::Internal(InternalFunction::Date))
            .ok_or_else(|| Unsupported::new("function d"))?;
        Self::arity(def.as_ref(), 0, arguments.len())?;
        if arguments.is_empty() {
            return Ok(self.add(
                Op::Clock(Clock::Now, Vec::new()),
                Ty::new(Kind::Date, false),
                ctx.scope,
            ));
        }
        let args = self.arguments(arguments, ctx, r)?;
        if let [only] = args.as_slice() {
            if self.kind(*only) == Kind::Date {
                return Ok(*only);
            }
        }
        let pure =
            self.effect(&FunctionKind::Internal(InternalFunction::Date), &args) == Effect::Pure;
        self.host(Host::Construct, args, Ty::new(Kind::Date, false), pure)
    }

    fn legacy(
        &mut self,
        function: DeprecatedFunction,
        arguments: &[&Ast],
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Lowered {
        let def = FunctionRegistry::get_definition(&FunctionKind::Deprecated(function))
            .ok_or_else(|| Unsupported::new(format!("function {function}")))?;
        Self::arity(def.as_ref(), 0, arguments.len())?;
        let args = self.arguments(arguments, ctx, r)?;
        let ty = match function {
            DeprecatedFunction::MonthString
            | DeprecatedFunction::DateString
            | DeprecatedFunction::WeekdayString => Ty::new(Kind::Text, true),
            _ => Ty::new(Kind::Num(None), true),
        };
        let pure = self.effect(&FunctionKind::Deprecated(function), &args) == Effect::Pure;
        self.host(Host::Legacy(function), args, ty, pure)
    }

    fn effect(&self, kind: &FunctionKind, args: &[NodeId]) -> Effect {
        let literals = args.iter().map(|a| match self.lit(*a) {
            Some(Lit::Text(text)) => Literal::Text(text),
            Some(_) => Literal::Constant,
            None => Literal::Computed,
        });
        Effect::call(kind, literals)
    }

    fn convert(&mut self, function: InternalFunction, a: NodeId) -> Lowered {
        let a = match self.kind(a) {
            Kind::Code(_) => self.decode(a)?,
            _ => a,
        };
        let Ty { kind, nullable } = self.ty(a);
        let named = match kind {
            _ if !self.settled(a) => None,
            Kind::Null => Some("null"),
            _ if nullable => None,
            Kind::Num(_) => Some("number"),
            Kind::Bool => Some("bool"),
            Kind::Text | Kind::Code(_) => Some("string"),
            Kind::Date => Some("date"),
        };
        let known = match kind {
            Kind::Num(_) => Some("number"),
            Kind::Bool => Some("bool"),
            Kind::Text => Some("string"),
            _ => None,
        };
        let ty = match function {
            InternalFunction::Type => match (named, known) {
                (Some(name), _) => return Ok(self.konst(Lit::Text(name.to_string()))),
                (None, Some(name)) if self.settled(a) => {
                    let null = self.konst(Lit::Null);
                    let missing = self.compare(Cmp::Eq, a, null)?;
                    let (named, none) = (
                        self.konst(Lit::Text(name.to_string())),
                        self.konst(Lit::Text("null".to_string())),
                    );
                    let (scope, ids) = self.scoped(&[missing, none, named])?;
                    return Ok(self.add(
                        Op::If(ids[0], ids[1], ids[2]),
                        Ty::new(Kind::Text, false),
                        scope,
                    ));
                }
                _ => Ty::new(Kind::Text, false),
            },
            _ => match (kind, nullable) {
                (Kind::Num(_), false) => return Ok(a),
                _ => Ty::new(Kind::Num(None), false),
            },
        };
        let pure = self.effect(&FunctionKind::Internal(function), &[a]) == Effect::Pure;
        self.host(Host::Internal(function), vec![a], ty, pure)
    }

    fn host(&mut self, host: Host, args: Vec<NodeId>, ty: Ty, pure: bool) -> Lowered {
        if let Some(lit) = pure.then(|| self.folded(host, &args)).flatten() {
            return Ok(self.konst(lit));
        }
        let (scope, ids) = self.scoped(&args)?;
        Ok(self.add(Op::Host(host, ids), ty, scope))
    }

    fn folded(&self, host: Host, args: &[NodeId]) -> Option<Lit> {
        let lits = args
            .iter()
            .map(|a| self.lit(*a))
            .collect::<Option<Vec<&Lit>>>()?;
        let given = lits
            .iter()
            .map(|lit| match lit {
                Lit::Null => Some(Given::Null),
                Lit::Num(d) => Some(Given::Num(d.decimal())),
                Lit::Bool(b) => Some(Given::Bool(*b)),
                Lit::Text(t) => Some(Given::Str(t.as_str())),
                Lit::Date(d) => Some(Given::Date(d.date())),
                Lit::Code(..) => None,
            })
            .collect::<Option<Vec<Given>>>()?;
        let context = EvaluationContext::default();
        match Builtins::raw(host.builtin()?, &given, &RowContext::new(&context, 0)).ok()? {
            Out::Num(n) => Dec::from_decimal(&n).map(Lit::Num),
            Out::Bool(b) => Some(Lit::Bool(b)),
            Out::Str(s) => Some(Lit::Text(s.to_string())),
            Out::Date(d) => Some(Lit::Date(Stamp::of(d))),
            Out::Var(Variable::Null) => Some(Lit::Null),
            Out::Var(_) => None,
        }
    }

    fn rendered(&mut self, id: NodeId) -> Lowered {
        if self.kind(id) != Kind::Date {
            return Ok(id);
        }
        if let Some(Lit::Date(d)) = self.lit(id) {
            if d.is_valid() {
                let text = d.to_string();
                return Ok(self.konst(Lit::Text(text)));
            }
        }
        let node = &self.nodes[id as usize];
        let (nullable, scope) = (node.ty.nullable, node.scope);
        Ok(self.add(
            Op::Clock(Clock::Text, vec![id]),
            Ty::new(Kind::Text, nullable),
            scope,
        ))
    }

    fn strict(&mut self, id: NodeId) -> NodeId {
        if let Some(Lit::Date(d)) = self.lit(id) {
            if d.is_valid() {
                return id;
            }
        }
        let scope = self.nodes[id as usize].scope;
        self.add(
            Op::Clock(Clock::Strict, vec![id]),
            Ty::new(Kind::Date, false),
            scope,
        )
    }

    fn coerced(&mut self, id: NodeId) -> NodeId {
        if let Some(Lit::Text(text)) = self.lit(id) {
            if let Some(date) = Date::fixed(text) {
                return self.konst(Lit::Date(Stamp::of(date)));
            }
        }
        let scope = self.nodes[id as usize].scope;
        self.add(
            Op::Clock(Clock::Coerce, vec![id]),
            Ty::new(Kind::Date, false),
            scope,
        )
    }

    fn dated(&mut self, op: Cmp, a: NodeId, b: NodeId) -> Lowered {
        let (a, b) = (self.loose(a)?, self.loose(b)?);
        let eq = matches!(op, Cmp::Eq | Cmp::Ne);
        match (self.kind(a), self.kind(b)) {
            (Kind::Date, Kind::Date) | (Kind::Date, Kind::Null) | (Kind::Null, Kind::Date) => {
                let (scope, ids) = self.scoped(&[a, b])?;
                Ok(self.add(Op::Cmp(op, ids[0], ids[1]), Ty::boolean(), scope))
            }
            (Kind::Date, Kind::Text) | (Kind::Text, Kind::Date) => {
                let flip = self.kind(a) == Kind::Text;
                let (date, text) = if flip { (b, a) } else { (a, b) };
                if !eq {
                    let (x, y) = (self.strict(date), self.coerced(text));
                    let (x, y) = if flip { (y, x) } else { (x, y) };
                    let (scope, ids) = self.scoped(&[x, y])?;
                    return Ok(self.add(Op::Cmp(op, ids[0], ids[1]), Ty::boolean(), scope));
                }
                let fixed = match self.lit(text) {
                    Some(Lit::Text(t)) => Date::fixed(t),
                    _ => None,
                };
                let hit = match fixed {
                    Some(parsed) => {
                        let other = self.konst(Lit::Date(Stamp::of(parsed)));
                        self.dated(Cmp::Eq, date, other)?
                    }
                    None => {
                        let (scope, ids) = self.scoped(&[date, text])?;
                        let matches =
                            self.add(Op::Clock(Clock::Matches, ids), Ty::boolean(), scope);
                        match self.ty(date).nullable && self.ty(text).nullable {
                            true => {
                                let null = self.konst(Lit::Null);
                                let x = self.compare(Cmp::Eq, date, null)?;
                                let y = self.compare(Cmp::Eq, text, null)?;
                                let both = self.logic(true, x, y)?;
                                self.logic(false, both, matches)?
                            }
                            false => matches,
                        }
                    }
                };
                match op {
                    Cmp::Eq => Ok(hit),
                    _ => self.not(hit),
                }
            }
            (x, y)
                if eq
                    && Self::family(x)
                        .zip(Self::family(y))
                        .is_some_and(|(p, q)| p != q) =>
            {
                self.mismatch(op, a, b)
            }
            (x, y) => Err(Unsupported::new(format!(
                "compare {op:?} between {x} and {y}"
            ))),
        }
    }

    fn belongs(&mut self, x: NodeId, item: NodeId) -> Lowered {
        let (x, item) = (self.loose(x)?, self.loose(item)?);
        let (date, other) = match (self.kind(x), self.kind(item)) {
            (Kind::Date, Kind::Date | Kind::Text) => (x, item),
            (Kind::Text, Kind::Date) => (item, x),
            _ => return self.compare(Cmp::Eq, x, item),
        };
        let (scope, ids) = self.scoped(&[date, other])?;
        let hit = self.add(Op::Clock(Clock::Matches, ids), Ty::boolean(), scope);
        if !(self.ty(x).nullable && self.ty(item).nullable) {
            return Ok(hit);
        }
        let null = self.konst(Lit::Null);
        let (p, q) = (
            self.compare(Cmp::Eq, x, null)?,
            self.compare(Cmp::Eq, item, null)?,
        );
        let both = self.logic(true, p, q)?;
        self.logic(false, both, hit)
    }

    fn dates_in(&mut self, a: NodeId, items: &[&Ast], ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        let mut set: Vec<Lit> = Vec::new();
        let mut zoned: Vec<NodeId> = Vec::new();
        for item in items {
            let id = self.expr(item, ctx, r)?;
            let lit = match self.lit(id).cloned() {
                Some(Lit::Null) => Some(Lit::Null),
                Some(Lit::Date(d)) => d.is_valid().then_some(Lit::Date(d)),
                Some(Lit::Text(t)) => match Date::fixed(&t) {
                    Some(date) => Some(Lit::Date(Stamp::of(date))),
                    None if Date::never(&t) => None,
                    None => {
                        zoned.push(id);
                        None
                    }
                },
                Some(_) => None,
                None if self.constructed(id) => {
                    zoned.push(id);
                    None
                }
                None => return Err(Unsupported::new("in: non-literal set item")),
            };
            if let Some(lit) = lit {
                if !set.contains(&lit) {
                    set.push(lit);
                }
            }
        }
        let scope = self.nodes[a as usize].scope;
        let mut hit = self.add(Op::InSet(a, set), Ty::boolean(), scope);
        for id in &zoned {
            let one = self.belongs(a, *id)?;
            hit = self.logic(false, hit, one)?;
        }
        for id in zoned {
            hit = self.checked(id, hit)?;
        }
        Ok(hit)
    }

    fn constructed(&self, id: NodeId) -> bool {
        match &self.nodes[id as usize].op {
            Op::Host(Host::Construct, args) => args
                .iter()
                .all(|arg| matches!(self.lit(*arg), Some(Lit::Text(_) | Lit::Num(_)))),
            _ => false,
        }
    }

    fn date_range(
        &mut self,
        a: NodeId,
        (lo, hi): (&Ast, &Ast),
        (left, right): (Bracket, Bracket),
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Lowered {
        let lo = self.expr(lo, ctx, r)?;
        let hi = self.expr(hi, ctx, r)?;
        for bound in [lo, hi] {
            if self.ty(bound) != Ty::new(Kind::Date, false) {
                return Err(Unsupported::new("interval bounds must be dates"));
            }
        }
        let low = match left {
            Bracket::LeftSquareBracket => Cmp::Le,
            Bracket::LeftParenthesis => Cmp::Lt,
            other => return Err(Unsupported::new(format!("interval bracket {other}"))),
        };
        let high = match right {
            Bracket::RightSquareBracket => Cmp::Le,
            Bracket::RightParenthesis => Cmp::Lt,
            other => return Err(Unsupported::new(format!("interval bracket {other}"))),
        };
        let first = self.dated(low, lo, a)?;
        let second = self.dated(high, a, hi)?;
        let both = self.compare(Cmp::Eq, first, second)?;
        self.logic(true, both, first)
    }
}
