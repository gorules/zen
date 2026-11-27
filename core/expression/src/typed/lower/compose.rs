use super::{Base, Ctx, Lowered, Lowering, Named, Resolve, Resolved, Seq};
use crate::functions::{ClosureFunction, FunctionKind, InternalFunction};
use crate::parser::Node as Ast;
use crate::typed::hir::{
    Arith, Cmp, Fold, Func, Item, Kind, Lit, NodeId, Op, Output, Scope, Shape, Ty, Unsupported,
};
use crate::typed::num::Dec;
use rust_decimal::prelude::ToPrimitive;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Val {
    Node(NodeId),
    Seq(Seq),
    List(Vec<Elem>),
    Object(Vec<(String, Val)>),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Elem {
    keep: Option<NodeId>,
    val: Val,
    splice: bool,
}

impl Elem {
    fn plain(val: Val) -> Self {
        Self {
            keep: None,
            val,
            splice: false,
        }
    }
}

impl Lowering {
    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    pub(super) fn built(ast: &Ast) -> bool {
        match ast {
            Ast::Parenthesized(inner) => Self::built(inner),
            Ast::Object(_) | Ast::Array(_) => true,
            _ => false,
        }
    }

    pub(super) fn emit(&mut self, name: &str, val: Val, scope: Scope) -> Result<(), Unsupported> {
        match val {
            Val::Node(id) => self.output(name, id, scope, true),
            Val::Seq(seq) => self.collect(name, seq, scope),
            val => self.composite(name, val, scope),
        }
    }

    fn composite(&mut self, name: &str, val: Val, scope: Scope) -> Result<(), Unsupported> {
        if scope != Scope::Row {
            return Err(Unsupported::new(format!(
                "composite output {name} in a list"
            )));
        }
        let shape = self.shaped(&val, scope)?;
        let null = self.konst(Lit::Null);
        let at = u32::try_from(self.vals.len()).map_err(|_| Unsupported::new("too many values"))?;
        self.vals.push(val);
        if !name.is_empty() {
            self.names.push((name.to_string(), Named::Val(at)));
        }
        self.outputs.push(Output {
            name: name.to_string(),
            node: null,
            scope,
            ty: Ty::new(Kind::Null, true),
            collect: None,
            shape: Some(Arc::new(shape)),
        });
        Ok(())
    }

    fn part(&mut self, output: Output) -> Result<u16, Unsupported> {
        let at = u16::try_from(self.parts.len()).map_err(|_| Unsupported::new("too many parts"))?;
        self.parts.push(output);
        Ok(at)
    }

    fn scalar_part(&mut self, id: NodeId, scope: Scope) -> Result<u16, Unsupported> {
        let id = self.exportable("", id, scope)?;
        let ty = self.ty(id);
        self.part(Output {
            name: String::new(),
            node: id,
            scope,
            ty,
            collect: None,
            shape: None,
        })
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn shaped(&mut self, val: &Val, scope: Scope) -> Result<Shape, Unsupported> {
        match val {
            Val::Node(id) => Ok(Shape::Part(self.scalar_part(*id, scope)?)),
            Val::Seq(seq) => {
                let (output, _) = self.gathered("", *seq, scope)?;
                Ok(Shape::Part(self.part(output)?))
            }
            Val::List(elems) => {
                let mut items = Vec::with_capacity(elems.len());
                for elem in elems {
                    let keep = match elem.keep {
                        Some(keep) => Some(self.scalar_part(keep, scope)?),
                        None => None,
                    };
                    items.push(Item {
                        keep,
                        shape: self.shaped(&elem.val, scope)?,
                        splice: elem.splice,
                    });
                }
                Ok(Shape::List(items))
            }
            Val::Object(members) => {
                let mut out = Vec::with_capacity(members.len());
                for (key, member) in members {
                    out.push((Arc::from(key.as_str()), self.shaped(member, scope)?));
                }
                Ok(Shape::Object(out))
            }
        }
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn revive(&self, shape: &Shape) -> Option<Val> {
        match shape {
            Shape::Part(p) => {
                let part = self.parts.get(*p as usize)?;
                Some(match part.collect {
                    None => Val::Node(part.node),
                    Some(c) => Val::Seq(Seq {
                        list: c.list,
                        keep: c.keep,
                        value: Some(c.item),
                        empty: c.empty,
                        pure: true,
                    }),
                })
            }
            Shape::List(items) => {
                let mut elems = Vec::with_capacity(items.len());
                for item in items {
                    let keep = match item.keep {
                        Some(k) => Some(self.parts.get(k as usize)?.node),
                        None => None,
                    };
                    elems.push(Elem {
                        keep,
                        val: self.revive(&item.shape)?,
                        splice: item.splice,
                    });
                }
                Some(Val::List(elems))
            }
            Shape::Object(members) => {
                let mut out = Vec::with_capacity(members.len());
                for (key, member) in members {
                    out.push((key.to_string(), self.revive(member)?));
                }
                Some(Val::Object(out))
            }
        }
    }

    fn named_val(&self, named: Named) -> Option<Val> {
        match named {
            Named::Node(id) => Some(Val::Node(id)),
            Named::Seq(seq) => Some(Val::Seq(seq)),
            Named::Val(at) => self.vals.get(at as usize).cloned(),
        }
    }

    fn descend(&mut self, mut val: Val, segments: &[&str]) -> Option<Val> {
        for segment in segments {
            val = match val {
                Val::Object(members) => match members.into_iter().find(|(k, _)| k == segment) {
                    Some((_, member)) => member,
                    None => Val::Node(self.konst(Lit::Null)),
                },
                _ => return None,
            };
        }
        Some(val)
    }

    pub(super) fn reach(&mut self, path: &str) -> Option<Val> {
        let from = match self.graph {
            true => self.local.min(self.names.len()),
            false => 0,
        };
        let (name, named) = self.names[from..]
            .iter()
            .rev()
            .find(|(name, _)| {
                path == name
                    || (path.len() > name.len()
                        && path.starts_with(name.as_str())
                        && path.as_bytes()[name.len()] == b'.')
            })
            .map(|(name, named)| (name.clone(), *named))?;
        let val = self.named_val(named)?;
        let rest: Vec<&str> = match path.len() > name.len() {
            true => path[name.len() + 1..].split('.').collect(),
            false => Vec::new(),
        };
        self.descend(val, &rest)
    }

    pub(super) fn descend_elem(&mut self, ctx: Ctx, segments: &[&str]) -> Option<Val> {
        let val = self.vals.get(ctx.elem? as usize)?.clone();
        self.descend(val, segments)
    }

    pub(super) fn computed(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Lowered {
        let mark = self.mark();
        match self.value(ast, ctx, r) {
            Ok(Val::Node(id)) => Ok(id),
            _ => {
                self.rollback(mark);
                Err(Unsupported::new("computed member access"))
            }
        }
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    pub(super) fn value(
        &mut self,
        ast: &Ast,
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Result<Val, Unsupported> {
        match ast {
            Ast::Parenthesized(inner) => self.value(inner, ctx, r),
            Ast::Object(entries) => {
                let mut members: Vec<(String, Val)> = Vec::with_capacity(entries.len());
                for (key, value) in entries.iter() {
                    let Ast::String(key) = key else {
                        return Err(Unsupported::new("computed object key"));
                    };
                    if members.iter().any(|(k, _)| k == key) {
                        return Err(Unsupported::new("duplicate object key"));
                    }
                    let value = self.value(value, ctx, r)?;
                    members.push((key.to_string(), value));
                }
                members.reverse();
                Ok(Val::Object(members))
            }
            Ast::Array(items) => {
                let mut elems = Vec::with_capacity(items.len());
                for item in items.iter() {
                    elems.push(Elem::plain(self.value(item, ctx, r)?));
                }
                Ok(Val::List(elems))
            }
            Ast::FunctionCall {
                kind: FunctionKind::Closure(c @ (ClosureFunction::Filter | ClosureFunction::Map)),
                arguments: [source, body],
            } => match self.value(source, ctx, r)? {
                Val::List(elems) => self.unrolled(*c, elems, body, ctx, r),
                Val::Seq(_) => self.seq(ast, ctx, r).map(Val::Seq),
                _ => Err(Unsupported::new("closure over a value that is not a list")),
            },
            Ast::FunctionCall {
                kind: FunctionKind::Internal(InternalFunction::Values),
                arguments: [arg],
            } => match self.value(arg, ctx, r)? {
                Val::Object(members) => Ok(Val::List(
                    members
                        .into_iter()
                        .map(|(_, val)| Elem::plain(val))
                        .collect(),
                )),
                _ => Err(Unsupported::new(
                    "values of a value that is not a static object",
                )),
            },
            Ast::FunctionCall {
                kind: FunctionKind::Internal(InternalFunction::Keys),
                arguments: [arg],
            } => {
                let base = self.value(arg, ctx, r)?;
                let keys: Vec<Lit> = match &base {
                    Val::Object(members) => members
                        .iter()
                        .map(|(key, _)| Lit::Text(key.clone()))
                        .collect(),
                    Val::List(elems) if elems.iter().all(|e| e.keep.is_none() && !e.splice) => (0
                        ..elems.len())
                        .map(|at| Lit::Num(Dec::int(at as i64)))
                        .collect(),
                    _ => return Err(Unsupported::new("keys of a value that is not static")),
                };
                let mut leaves = Vec::new();
                Self::leaves(&base, &mut leaves)?;
                if leaves.iter().any(|leaf| !self.settled(*leaf)) {
                    return Err(Unsupported::new("keys of a value that may fail"));
                }
                Ok(Val::List(
                    keys.into_iter()
                        .map(|lit| Elem::plain(Val::Node(self.konst(lit))))
                        .collect(),
                ))
            }
            Ast::FunctionCall {
                kind: FunctionKind::Internal(InternalFunction::Flatten),
                arguments: [arg],
            } => match self.value(arg, ctx, r)? {
                Val::List(elems) => {
                    let mut out = Vec::with_capacity(elems.len());
                    for elem in elems {
                        match elem.val {
                            _ if elem.splice => {
                                return Err(Unsupported::new("flatten of a spliced list"))
                            }
                            Val::List(inner) => {
                                for e in inner {
                                    let keep = self.both(elem.keep, e.keep)?;
                                    out.push(Elem { keep, ..e });
                                }
                            }
                            Val::Seq(seq) => out.push(Elem {
                                keep: elem.keep,
                                val: Val::Seq(seq),
                                splice: true,
                            }),
                            _ => out.push(elem),
                        }
                    }
                    Ok(Val::List(out))
                }
                _ => Err(Unsupported::new(
                    "flatten of a value that is not a static list",
                )),
            },
            Ast::Member { node, property } => {
                let mut segments = Vec::new();
                if Self::segments(ast, &mut segments).is_ok() {
                    return self.named_path(ast, ctx, r);
                }
                if let Some(part) = self.split_part(node, property, ctx, r)? {
                    return Ok(Val::Node(part));
                }
                if let Some(element) = self.indexed(ast, ctx, r)? {
                    return Ok(Val::Node(element));
                }
                self.member_of(node, property, ctx, r)
            }
            Ast::Identifier(_) | Ast::Pointer => self.named_path(ast, ctx, r),
            _ => self.either(ast, ctx, r),
        }
    }

    fn either(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Result<Val, Unsupported> {
        let mark = self.mark();
        match self.expr(ast, ctx, r) {
            Ok(id) => Ok(Val::Node(id)),
            Err(e) => {
                self.rollback(mark.clone());
                match self.seq(ast, ctx, r) {
                    Ok(seq) => Ok(Val::Seq(seq)),
                    Err(_) => {
                        self.rollback(mark);
                        Err(e)
                    }
                }
            }
        }
    }

    fn named_path(&mut self, ast: &Ast, ctx: Ctx, r: &mut dyn Resolve) -> Result<Val, Unsupported> {
        let mut segments = Vec::new();
        let base = Self::segments(ast, &mut segments)?;
        match base {
            Base::Dollar if ctx.dollar.is_none() && !segments.is_empty() => {
                let rest = segments.join(".");
                self.reach(&rest)
                    .ok_or_else(|| Unsupported::new(format!("unknown $.{rest}")))
            }
            Base::Pointer if ctx.elem.is_some() => self
                .descend_elem(ctx, &segments)
                .ok_or_else(|| Unsupported::new("member of a composite element")),
            Base::Ident(name) if self.graph => {
                let path = match segments.is_empty() {
                    true => name.to_string(),
                    false => format!("{name}.{}", segments.join(".")),
                };
                if let Resolved::Output(at) = r.path(&path) {
                    if let Some(shape) = self.outputs.get(at).and_then(|o| o.shape.clone()) {
                        return self
                            .revive(&shape)
                            .ok_or_else(|| Unsupported::new(format!("composite {path}")));
                    }
                }
                self.either(ast, ctx, r)
            }
            Base::Ident(name) => {
                let path = match segments.is_empty() {
                    true => name.to_string(),
                    false => format!("{name}.{}", segments.join(".")),
                };
                match self.named(&path) {
                    Some(Named::Val(at)) => self
                        .vals
                        .get(at as usize)
                        .cloned()
                        .ok_or_else(|| Unsupported::new(format!("composite {path}"))),
                    _ => self.either(ast, ctx, r),
                }
            }
            _ => self.either(ast, ctx, r),
        }
    }

    fn split_part(
        &mut self,
        node: &Ast,
        property: &Ast,
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Result<Option<NodeId>, Unsupported> {
        let mut node = node;
        while let Ast::Parenthesized(inner) = node {
            node = inner;
        }
        let (
            Ast::FunctionCall {
                kind: FunctionKind::Internal(InternalFunction::Split),
                arguments: [text, sep],
            },
            Ast::Number(n),
        ) = (node, property)
        else {
            return Ok(None);
        };
        let Some(at) = n
            .is_integer()
            .then(|| n.to_u16())
            .flatten()
            .filter(|_| !n.is_sign_negative())
        else {
            return Ok(None);
        };
        let text = self.expr(text, ctx, r)?;
        let sep = self.expr(sep, ctx, r)?;
        let (text, sep) = (self.textual(text)?, self.textual(sep)?);
        if let (Some(Lit::Text(a)), Some(Lit::Text(b))) = (self.lit(text), self.lit(sep)) {
            let part = a.split(b.as_str()).nth(usize::from(at)).map(str::to_string);
            return Ok(Some(self.konst(match part {
                Some(part) => Lit::Text(part),
                None => Lit::Null,
            })));
        }
        let (scope, ids) = self.scoped(&[text, sep])?;
        Ok(Some(self.add(
            Op::Call(Func::Part(at), ids),
            Ty::new(Kind::Text, true),
            scope,
        )))
    }

    fn indexed(
        &mut self,
        ast: &Ast,
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Result<Option<NodeId>, Unsupported> {
        let mut path: Vec<&str> = Vec::new();
        let mut at = ast;
        let (list, n) = loop {
            match at {
                Ast::Member {
                    node,
                    property: Ast::String(key),
                } if !key.is_empty() && !key.contains('.') => {
                    path.push(key);
                    at = node;
                }
                Ast::Member {
                    node,
                    property: Ast::Number(n),
                } => break (*node, *n),
                Ast::Parenthesized(inner) => at = inner,
                _ => return Ok(None),
            }
        };
        path.reverse();
        let Some(index) = n
            .is_integer()
            .then(|| n.to_u32())
            .flatten()
            .filter(|_| !n.is_sign_negative())
        else {
            return Ok(None);
        };
        let mark = self.mark();
        let seq = match self.seq(list, ctx, r) {
            Ok(seq) if path.is_empty() || seq.value.is_none() => Self::pure(seq)?,
            _ => {
                self.rollback(mark);
                return Ok(None);
            }
        };
        let value = match path.is_empty() {
            true => self.item(seq)?,
            false => self.field(seq.list, &path.join("."), None, r)?,
        };
        let value = match self.kind(value) {
            Kind::Code(_) => self.decode(value)?,
            _ => value,
        };
        let value = self.lifted(value, seq.list)?;
        let filter = seq.keep.map(|k| self.lifted(k, seq.list)).transpose()?;
        let scope = self
            .schema
            .owner(Scope::Elem(seq.list))
            .ok_or_else(|| Unsupported::new("element owner"))?;
        let ty = Ty::new(self.kind(value), true);
        Ok(Some(self.add(
            Op::Fold {
                fold: Fold::At(index),
                list: seq.list,
                filter,
                value: Some(value),
                empty: seq.empty || seq.keep.is_none(),
            },
            ty,
            scope,
        )))
    }

    fn member_of(
        &mut self,
        node: &Ast,
        property: &Ast,
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Result<Val, Unsupported> {
        let base = self.value(node, ctx, r)?;
        let picked = match (&base, property) {
            (Val::Object(members), Ast::String(key)) => {
                members.iter().position(|(k, _)| k == key).map(Some)
            }
            (Val::List(elems), Ast::Number(n))
                if elems.iter().all(|e| e.keep.is_none() && !e.splice)
                    && n.is_integer()
                    && !n.is_sign_negative() =>
            {
                n.to_usize().map(|at| (at < elems.len()).then_some(at))
            }
            _ => None,
        };
        let Some(picked) = picked else {
            return Err(Unsupported::new(format!(
                "member access on {}",
                <&'static str>::from(node)
            )));
        };
        let (chosen, rest): (Val, Vec<Val>) = match base {
            Val::Object(members) => {
                let mut rest = Vec::new();
                let mut chosen = None;
                for (at, (_, val)) in members.into_iter().enumerate() {
                    match Some(at) == picked {
                        true => chosen = Some(val),
                        false => rest.push(val),
                    }
                }
                (
                    chosen.unwrap_or_else(|| Val::Node(self.konst(Lit::Null))),
                    rest,
                )
            }
            Val::List(elems) => {
                let mut rest = Vec::new();
                let mut chosen = None;
                for (at, elem) in elems.into_iter().enumerate() {
                    match Some(at) == picked {
                        true => chosen = Some(elem.val),
                        false => rest.push(elem.val),
                    }
                }
                (
                    chosen.unwrap_or_else(|| Val::Node(self.konst(Lit::Null))),
                    rest,
                )
            }
            _ => return Err(Unsupported::new("member access")),
        };
        let mut leaves = Vec::new();
        for val in &rest {
            Self::leaves(val, &mut leaves)?;
        }
        match chosen {
            Val::Node(id) => Ok(Val::Node(self.guard_all(&leaves, id)?)),
            other if leaves.iter().all(|leaf| self.settled(*leaf)) => Ok(other),
            _ => Err(Unsupported::new("member of a composite that may fail")),
        }
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn leaves(val: &Val, out: &mut Vec<NodeId>) -> Result<(), Unsupported> {
        match val {
            Val::Node(id) => out.push(*id),
            Val::Seq(_) => return Err(Unsupported::new("list inside a static list")),
            Val::List(elems) => {
                for elem in elems {
                    out.extend(elem.keep);
                    Self::leaves(&elem.val, out)?;
                }
            }
            Val::Object(members) => {
                for (_, member) in members {
                    Self::leaves(member, out)?;
                }
            }
        }
        Ok(())
    }

    fn guard(&mut self, x: NodeId, y: NodeId) -> Lowered {
        if x == y || self.settled(x) {
            return Ok(y);
        }
        if self.ty(y) == Ty::boolean() {
            return self.checked(x, y);
        }
        let scope = self.nodes[x as usize].scope;
        let ok = self.add(Op::Checked(x), Ty::boolean(), scope);
        let ty = self.ty(y);
        let (scope, ids) = self.scoped(&[ok, y, y])?;
        Ok(self.add(Op::If(ids[0], ids[1], ids[2]), ty, scope))
    }

    fn guard_all(&mut self, leaves: &[NodeId], mut y: NodeId) -> Lowered {
        for leaf in leaves {
            y = self.guard(*leaf, y)?;
        }
        Ok(y)
    }

    fn both(
        &mut self,
        a: Option<NodeId>,
        b: Option<NodeId>,
    ) -> Result<Option<NodeId>, Unsupported> {
        match (a, b) {
            (Some(a), Some(b)) => Ok(Some(self.logic(true, a, b)?)),
            (a, b) => Ok(a.or(b)),
        }
    }

    fn bind(&mut self, ctx: Ctx, val: &Val) -> Result<Ctx, Unsupported> {
        match val {
            Val::Node(id) => Ok(Ctx {
                item: Some(*id),
                pointer: None,
                elem: None,
                ..ctx
            }),
            Val::Seq(_) => Err(Unsupported::new("closure over a list of lists")),
            other => {
                let at = u32::try_from(self.vals.len())
                    .map_err(|_| Unsupported::new("too many values"))?;
                self.vals.push(other.clone());
                Ok(Ctx {
                    item: None,
                    pointer: None,
                    elem: Some(at),
                    ..ctx
                })
            }
        }
    }

    fn statics(elems: &[Elem]) -> Result<(), Unsupported> {
        match elems.iter().any(|e| e.splice) {
            true => Err(Unsupported::new("closure over a spliced list")),
            false => Ok(()),
        }
    }

    fn tests(
        &mut self,
        elems: &[Elem],
        body: &Ast,
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Result<Vec<NodeId>, Unsupported> {
        Self::statics(elems)?;
        let body = Self::closure(body)?;
        let mut tests = Vec::with_capacity(elems.len());
        for elem in elems {
            let inner = self.bind(ctx, &elem.val)?;
            let test = self.expr(body, inner, r)?;
            if self.ty(test) != Ty::boolean() {
                return Err(Unsupported::new(format!(
                    "predicate: unexpected type {}",
                    self.ty(test)
                )));
            }
            tests.push(test);
        }
        Ok(tests)
    }

    fn unrolled(
        &mut self,
        closure: ClosureFunction,
        elems: Vec<Elem>,
        body: &Ast,
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Result<Val, Unsupported> {
        match closure {
            ClosureFunction::Filter => {
                let tests = self.tests(&elems, body, ctx, r)?;
                let mut out = Vec::with_capacity(elems.len());
                for (elem, test) in elems.into_iter().zip(tests) {
                    let keep = match self.lit(test) {
                        Some(Lit::Bool(true)) => elem.keep,
                        Some(Lit::Bool(false)) if elem.keep.is_none() => {
                            let mut leaves = Vec::new();
                            Self::leaves(&elem.val, &mut leaves)?;
                            if leaves.iter().any(|leaf| !self.settled(*leaf)) {
                                Some(self.konst(Lit::Bool(false)))
                            } else {
                                continue;
                            }
                        }
                        _ => self.both(elem.keep, Some(test))?,
                    };
                    out.push(Elem { keep, ..elem });
                }
                Ok(Val::List(out))
            }
            _ => {
                Self::statics(&elems)?;
                let body = Self::closure(body)?;
                let mut out = Vec::with_capacity(elems.len());
                for elem in elems {
                    let inner = self.bind(ctx, &elem.val)?;
                    let value = self.value(body, inner, r)?;
                    let mut leaves = Vec::new();
                    Self::leaves(&elem.val, &mut leaves)?;
                    let value = match value {
                        Val::Node(id) => Val::Node(self.guard_all(&leaves, id)?),
                        other if leaves.iter().all(|leaf| self.settled(*leaf)) => other,
                        _ => return Err(Unsupported::new("map to a composite that may fail")),
                    };
                    out.push(Elem {
                        keep: elem.keep,
                        val: value,
                        splice: false,
                    });
                }
                Ok(Val::List(out))
            }
        }
    }

    pub(super) fn statically(
        &mut self,
        kind: &FunctionKind,
        args: &[&Ast],
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Lowered {
        let (source, rest) = match args {
            [source, rest @ ..] => (*source, rest),
            [] => return Err(Unsupported::new("call without arguments")),
        };
        let Val::List(elems) = self.value(source, ctx, r)? else {
            return Err(Unsupported::new(
                "fold over a value that is not a static list",
            ));
        };
        Self::statics(&elems)?;
        let mut leaves = Vec::new();
        for elem in &elems {
            out_leaves(elem, &mut leaves)?;
        }
        let result = match (kind, rest) {
            (FunctionKind::Internal(InternalFunction::Len), []) => {
                let ins: Vec<Option<NodeId>> = elems.iter().map(|e| e.keep).collect();
                self.counted(&ins)?
            }
            (FunctionKind::Internal(InternalFunction::Contains), [x]) => {
                let x = self.expr(x, ctx, r)?;
                let found = self.found(x, &elems)?;
                self.checked(x, found)?
            }
            (
                FunctionKind::Closure(
                    c @ (ClosureFunction::Count
                    | ClosureFunction::Some
                    | ClosureFunction::All
                    | ClosureFunction::None
                    | ClosureFunction::One),
                ),
                [body],
            ) => {
                let tests = self.tests(&elems, body, ctx, r)?;
                let mut ins = Vec::with_capacity(tests.len());
                for (elem, test) in elems.iter().zip(&tests) {
                    ins.push(self.both(elem.keep, Some(*test))?);
                }
                match c {
                    ClosureFunction::Count => self.counted(&ins)?,
                    ClosureFunction::One => {
                        let count = self.counted(&ins)?;
                        let one = self.konst(Lit::Num(Dec::int(1)));
                        self.compare(Cmp::Eq, count, one)?
                    }
                    ClosureFunction::All => {
                        let mut all = Vec::with_capacity(tests.len());
                        for (elem, test) in elems.iter().zip(&tests) {
                            all.push(match elem.keep {
                                None => *test,
                                Some(keep) => {
                                    let skip = self.not(keep)?;
                                    self.logic(false, skip, *test)?
                                }
                            });
                        }
                        self.chain(true, all)?
                    }
                    _ => {
                        let mut any = Vec::with_capacity(ins.len());
                        for at in ins {
                            any.push(match at {
                                Some(at) => at,
                                None => self.konst(Lit::Bool(true)),
                            });
                        }
                        let hit = self.chain(false, any)?;
                        match c {
                            ClosureFunction::None => self.not(hit)?,
                            _ => hit,
                        }
                    }
                }
            }
            (
                FunctionKind::Internal(
                    f @ (InternalFunction::Sum
                    | InternalFunction::Min
                    | InternalFunction::Max
                    | InternalFunction::Avg),
                ),
                [],
            ) => self.aggregated(*f, &elems)?,
            _ => return Err(Unsupported::new("fold over a static list")),
        };
        self.guard_all(&leaves, result)
    }

    fn counted(&mut self, ins: &[Option<NodeId>]) -> Lowered {
        let one = self.konst(Lit::Num(Dec::int(1)));
        let zero = self.konst(Lit::Num(Dec::ZERO));
        let fixed = ins.iter().filter(|at| at.is_none()).count();
        let mut acc = self.konst(Lit::Num(Dec::int(fixed as i64)));
        for keep in ins.iter().flatten() {
            let (scope, ids) = self.scoped(&[*keep, one, zero])?;
            let bit = self.add(Op::If(ids[0], ids[1], ids[2]), Ty::num(Some(0)), scope);
            acc = self.arith(Arith::Add, acc, bit)?;
        }
        Ok(acc)
    }

    fn found(&mut self, x: NodeId, elems: &[Elem]) -> Lowered {
        let mut hits = Vec::with_capacity(elems.len());
        for elem in elems {
            let Val::Node(item) = elem.val else {
                return Err(Unsupported::new("membership in a list of composites"));
            };
            let hit = self.belongs(x, item)?;
            hits.push(match elem.keep {
                Some(keep) => self.logic(true, keep, hit)?,
                None => hit,
            });
        }
        self.chain(false, hits)
    }

    fn aggregated(&mut self, f: InternalFunction, elems: &[Elem]) -> Lowered {
        if elems.is_empty() {
            return Err(Unsupported::new("aggregate of an empty list"));
        }
        let mut args = Vec::with_capacity(elems.len());
        for elem in elems {
            let Val::Node(id) = elem.val else {
                return Err(Unsupported::new("aggregate of composites"));
            };
            self.require(id, |k| matches!(k, Kind::Num(_)), "aggregate item")?;
            args.push((elem.keep, id));
        }
        let kept = args.iter().any(|(keep, _)| keep.is_some());
        match f {
            InternalFunction::Sum => {
                let zero = self.konst(Lit::Num(Dec::ZERO));
                let mut acc = zero;
                for (keep, id) in args {
                    let term = match keep {
                        None => id,
                        Some(keep) => {
                            let (arms, ty) = self.unify(vec![id, zero])?;
                            let (scope, ids) = self.scoped(&[keep, arms[0], arms[1]])?;
                            self.add(Op::If(ids[0], ids[1], ids[2]), ty, scope)
                        }
                    };
                    acc = self.arith(Arith::Add, acc, term)?;
                }
                Ok(acc)
            }
            _ if kept => Err(Unsupported::new("aggregate of a filtered static list")),
            InternalFunction::Avg => {
                let zero = self.konst(Lit::Num(Dec::ZERO));
                let mut acc = zero;
                for (_, id) in &args {
                    acc = self.arith(Arith::Add, acc, *id)?;
                }
                let count = self.konst(Lit::Num(Dec::int(args.len() as i64)));
                self.arith(Arith::Div, acc, count)
            }
            _ => {
                let ids: Vec<NodeId> = args.iter().map(|(_, id)| *id).collect();
                let first = self.kind(ids[0]);
                let kind = match ids.iter().all(|a| self.kind(*a) == first) {
                    true => first,
                    false => Kind::Num(None),
                };
                let func = match f {
                    InternalFunction::Min => Func::Min,
                    _ => Func::Max,
                };
                self.math(func, ids, Ty::new(kind, false))
            }
        }
    }

    pub(super) fn unequal(
        &mut self,
        ne: bool,
        left: &Ast,
        right: &Ast,
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Lowered {
        let a = self.value(left, ctx, r)?;
        let b = self.value(right, ctx, r)?;
        let composite = |val: &Val| matches!(val, Val::List(_) | Val::Object(_) | Val::Seq(_));
        if !composite(&a) && !composite(&b) {
            return Err(Unsupported::new("equality without a composite"));
        }
        let mut leaves = Vec::new();
        let mut nulls = Vec::with_capacity(2);
        for val in [&a, &b] {
            nulls.push(match val {
                Val::Seq(seq) => self.missing(*seq)?,
                Val::Node(id) => {
                    leaves.push(*id);
                    let null = self.konst(Lit::Null);
                    self.compare(Cmp::Eq, *id, null)?
                }
                other => {
                    Self::leaves(other, &mut leaves)?;
                    self.konst(Lit::Bool(false))
                }
            });
        }
        let both = match nulls
            .iter()
            .any(|n| self.lit(*n) == Some(&Lit::Bool(false)))
        {
            true => self.konst(Lit::Bool(false)),
            false => self.logic(true, nulls[0], nulls[1])?,
        };
        let answer = match ne {
            true => self.not(both)?,
            false => both,
        };
        self.guard_all(&leaves, answer)
    }

    fn missing(&mut self, seq: Seq) -> Lowered {
        if seq.keep.is_some() || seq.value.is_some() {
            return Err(Unsupported::new("equality with a computed list"));
        }
        if seq.empty {
            return Ok(self.konst(Lit::Bool(false)));
        }
        let scope = self
            .schema
            .owner(Scope::Elem(seq.list))
            .ok_or_else(|| Unsupported::new("list owner"))?;
        Ok(self.add(
            Op::Fold {
                fold: Fold::Missing,
                list: seq.list,
                filter: None,
                value: None,
                empty: true,
            },
            Ty::boolean(),
            scope,
        ))
    }

    pub(super) fn within(
        &mut self,
        x: NodeId,
        list: &Ast,
        ctx: Ctx,
        r: &mut dyn Resolve,
    ) -> Lowered {
        let Val::List(elems) = self.value(list, ctx, r)? else {
            return Err(Unsupported::new(
                "membership in a value that is not a static list",
            ));
        };
        Self::statics(&elems)?;
        let mut leaves = Vec::new();
        for elem in &elems {
            out_leaves(elem, &mut leaves)?;
        }
        let found = self.found(x, &elems)?;
        let found = self.checked(x, found)?;
        self.guard_all(&leaves, found)
    }
}

fn out_leaves(elem: &Elem, out: &mut Vec<NodeId>) -> Result<(), Unsupported> {
    out.extend(elem.keep);
    Lowering::leaves(&elem.val, out)
}
