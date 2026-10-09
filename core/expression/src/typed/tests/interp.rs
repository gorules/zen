use crate::lane::builtins::{Arg as Given, Builtins, Out};
use crate::lane::date::Date;
use crate::typed::batch::{Data, Feed, Feeds};
use crate::typed::ins::Pattern;
use crate::typed::kernel::K;
use crate::typed::num::Exact;
use crate::typed::tests::oracle::Val;
use crate::typed::{Clock, Cmp, Fold, Func, Hir, Kind, Lit, NodeId, Op, Range, Scope};
use crate::variable::Variable;
use rust_decimal::Decimal;
use std::borrow::Cow;
use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq)]
enum V<'a> {
    Null,
    Num(Decimal),
    Bool(bool),
    Code(i32),
    Text(Cow<'a, [u8]>),
    Date(Date),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Fault;

type R<'a> = Result<V<'a>, Fault>;

pub(crate) struct Interp<'a, 'x> {
    hir: &'a Hir,
    feeds: &'a Feeds<'x>,
    row: usize,
    elem: Vec<usize>,
    cache: Vec<Option<R<'a>>>,
}

impl<'a, 'x> Interp<'a, 'x> {
    pub(crate) fn run(hir: &'a Hir, feeds: &'a Feeds<'x>) -> Vec<Option<Vec<Val>>> {
        let n = hir.nodes.len();
        let mut walk = Interp { hir, feeds, row: 0, elem: vec![0; hir.schema.lists.len()], cache: vec![None; n] };
        let mut out = Vec::with_capacity(feeds.rows);
        for row in 0..feeds.rows {
            walk.row = row;
            walk.cache.fill(None);
            let mut values = Vec::with_capacity(hir.outputs.len());
            let mut faulted = false;
            for output in &hir.outputs {
                let owner = match output.collect {
                    Some(_) => hir.schema.owner(output.scope).unwrap_or(Scope::Row),
                    None => output.scope,
                };
                let value = match owner {
                    Scope::Row => walk.output(output),
                    Scope::Elem(l) => {
                        let mut items = Vec::new();
                        let mut fault = false;
                        for e in walk.elements(l) {
                            walk.enter(l, e);
                            match walk.output(output) {
                                Ok(v) => items.push(v),
                                Err(Fault) => fault = true,
                            }
                        }
                        match fault {
                            true => Err(Fault),
                            false => Ok(Val::List(items)),
                        }
                    }
                };
                match value {
                    Ok(v) => values.push(v),
                    Err(Fault) => faulted = true,
                }
            }
            out.push((!faulted).then_some(values));
        }
        out
    }

    fn owner(&self, list: u16) -> usize {
        match self.hir.schema.lists[list as usize].parent {
            None => self.row,
            Some(p) => self.elem[p as usize],
        }
    }

    fn elements(&self, list: u16) -> Vec<usize> {
        match self.hir.schema.lists[list as usize].parent {
            None => {
                let l = &self.feeds.lists[list as usize];
                (l.offsets[self.row] as usize..l.offsets[self.row + 1] as usize).collect()
            }
            Some(p) => {
                let l = &self.feeds.lists[list as usize];
                self.elements(p).into_iter().flat_map(|o| l.offsets[o] as usize..l.offsets[o + 1] as usize).collect()
            }
        }
    }

    fn enter(&mut self, list: u16, e: usize) {
        self.elem[list as usize] = e;
        let schema = &self.hir.schema;
        for (at, node) in self.hir.nodes.iter().enumerate() {
            if schema.within(node.scope, Scope::Elem(list)) {
                self.cache[at] = None;
            }
        }
    }

    fn range(&self, list: u16, empty: bool) -> Result<std::ops::Range<usize>, Fault> {
        let l = &self.feeds.lists[list as usize];
        let owner = self.owner(list);
        if l.faulted.as_ref().is_some_and(|f| K::get(f, owner)) {
            return Err(Fault);
        }
        match l.null.as_ref().is_some_and(|null| K::get(null, owner)) {
            true if empty => Ok(0..0),
            true => Err(Fault),
            false => Ok(l.offsets[owner] as usize..l.offsets[owner + 1] as usize),
        }
    }

    fn output(&mut self, output: &crate::typed::Output) -> Result<Val, Fault> {
        let Some(c) = output.collect else {
            let v = self.eval(output.node)?;
            return Ok(self.val(v, output.ty.kind));
        };
        let range = self.range(c.list, c.empty)?;
        let saved = self.elem[c.list as usize];
        let mut items = Vec::new();
        let mut result = Ok(());
        for e in range {
            self.enter(c.list, e);
            let keep = match c.keep {
                None => true,
                Some(k) => match self.eval(k) {
                    Ok(V::Bool(b)) => b,
                    _ => {
                        result = Err(Fault);
                        break;
                    }
                },
            };
            if !keep {
                continue;
            }
            match self.eval(output.node) {
                Ok(v) => items.push(self.val(v, output.ty.kind)),
                Err(Fault) => {
                    result = Err(Fault);
                    break;
                }
            }
        }
        self.enter(c.list, saved);
        result.map(|_| Val::List(items))
    }

    fn val(&self, v: V, kind: Kind) -> Val {
        match (v, kind) {
            (V::Null, _) => Val::Null,
            (V::Num(d), _) => Val::Num(d),
            (V::Bool(b), _) => Val::Bool(b),
            (V::Code(c), Kind::Code(d)) => self.hir.dict(d).text(c).map_or(Val::Other(format!("code {c}")), |t| Val::Str(t.to_string())),
            (V::Code(c), _) => Val::Other(format!("code {c}")),
            (V::Text(t), _) => Val::Str(String::from_utf8_lossy(&t).to_string()),
            (V::Date(d), _) => Val::Other(format!("{}", d.variable().to_value())),
        }
    }

    fn eval(&mut self, id: NodeId) -> R<'a> {
        let at = id as usize;
        if let Some(r) = &self.cache[at] {
            return r.clone();
        }
        let r = self.compute(id);
        self.cache[at] = Some(r.clone());
        r
    }

    fn read(feed: &'a Feed<'x>, i: usize) -> R<'a> {
        if K::get(feed.faults.as_deref().unwrap_or(&[]), i) {
            return Err(Fault);
        }
        if !feed.valid(i) {
            return Ok(V::Null);
        }
        if let Some((bits, values)) = &feed.wide {
            if K::get(bits, i) {
                return Ok(values.binary_search_by_key(&i, |(at, _)| *at).map_or(V::Null, |at| V::Num(values[at].1)));
            }
        }
        Ok(match &feed.data {
            Data::Num { .. } => feed.num(i).map_or(V::Null, |d| V::Num(d.decimal())),
            Data::Bool { bits, offset } => V::Bool(K::get(bits, offset + i)),
            Data::Codes(codes) => V::Code(codes[i]),
            Data::Text(text) => text.get(i).map_or(V::Null, |t| V::Text(Cow::Borrowed(t))),
        })
    }

    fn lit(lit: &'a Lit) -> V<'a> {
        match lit {
            Lit::Null => V::Null,
            Lit::Num(d) => V::Num(d.decimal()),
            Lit::Bool(b) => V::Bool(*b),
            Lit::Code(_, c) => V::Code(*c),
            Lit::Text(t) => V::Text(Cow::Borrowed(t.as_bytes())),
            Lit::Date(d) => V::Date(d.date()),
        }
    }

    fn equal(a: &V, b: &V) -> bool {
        a == b
    }

    fn within(x: Decimal, r: &Range) -> bool {
        let (lo, hi) = (x.cmp(&r.lo.decimal()), x.cmp(&r.hi.decimal()));
        let above = lo == Ordering::Greater || (r.lo_closed && lo == Ordering::Equal);
        let below = hi == Ordering::Less || (r.hi_closed && hi == Ordering::Equal);
        above && below
    }

    fn compute(&mut self, id: NodeId) -> R<'a> {
        let hir = self.hir;
        let node = &hir.nodes[id as usize];
        match &node.op {
            Op::Column(c) => Self::read(&self.feeds.columns[*c as usize], self.row),
            Op::Field(l, f) => Self::read(&self.feeds.fields[*l as usize][*f as usize], self.elem[*l as usize]),
            Op::Parent(a) => self.eval(*a),
            Op::Const(lit) => Ok(Self::lit(lit)),
            Op::Classify(a, d) => match self.eval(*a)? {
                V::Text(t) => {
                    let dict = hir.dict(*d);
                    Ok(V::Code(dict.values.iter().position(|v| v.as_bytes() == &*t).map_or(dict.values.len() as i32, |i| i as i32)))
                }
                _ => Ok(V::Null),
            },
            Op::Neg(a) => match self.eval(*a)? {
                V::Num(d) => Ok(V::Num(-d)),
                _ => Err(Fault),
            },
            Op::Not(a) => match self.eval(*a)? {
                V::Bool(b) => Ok(V::Bool(!b)),
                _ => Err(Fault),
            },
            Op::Arith(op, a, b) => {
                let (x, y) = (self.eval(*a)?, self.eval(*b)?);
                let (V::Num(x), V::Num(y)) = (x, y) else {
                    return Err(Fault);
                };
                match Exact::arith(*op, x, y) {
                    Ok(Some(q)) => Ok(V::Num(q)),
                    Ok(None) => Ok(V::Null),
                    Err(_) => Err(Fault),
                }
            }
            Op::Cmp(op, a, b) => {
                let (x, y) = (self.eval(*a)?, self.eval(*b)?);
                match op {
                    Cmp::Eq => Ok(V::Bool(Self::equal(&x, &y))),
                    Cmp::Ne => Ok(V::Bool(!Self::equal(&x, &y))),
                    _ => {
                        let o = match (x, y) {
                            (V::Num(x), V::Num(y)) => x.cmp(&y),
                            (V::Date(x), V::Date(y)) => x.cmp(&y),
                            _ => return Err(Fault),
                        };
                        Ok(V::Bool(match op {
                            Cmp::Lt => o == Ordering::Less,
                            Cmp::Le => o != Ordering::Greater,
                            Cmp::Gt => o == Ordering::Greater,
                            _ => o != Ordering::Less,
                        }))
                    }
                }
            }
            Op::And(a, b) => match self.eval(*a)? {
                V::Bool(false) => Ok(V::Bool(false)),
                V::Bool(true) => self.eval(*b),
                _ => Err(Fault),
            },
            Op::Or(a, b) => match self.eval(*a)? {
                V::Bool(true) => Ok(V::Bool(true)),
                V::Bool(false) => self.eval(*b),
                _ => Err(Fault),
            },
            Op::If(c, t, e) => match self.eval(*c)? {
                V::Bool(true) => self.eval(*t),
                V::Bool(false) => self.eval(*e),
                _ => Err(Fault),
            },
            Op::Coalesce(a, b) => match self.eval(*a)? {
                V::Null => self.eval(*b),
                v => Ok(v),
            },
            Op::InSet(a, set) => {
                let x = self.eval(*a)?;
                Ok(V::Bool(set.iter().any(|lit| Self::equal(&x, &Self::lit(lit)))))
            }
            Op::InRange(a, r) => match self.eval(*a)? {
                V::Num(x) => Ok(V::Bool(Self::within(x, r))),
                _ => Err(Fault),
            },
            Op::Fold { fold, list, filter, value, empty } => {
                let range = self.range(*list, *empty)?;
                let saved = self.elem[*list as usize];
                let mut kept = Vec::new();
                let mut result = Ok(());
                for e in range {
                    self.enter(*list, e);
                    if let Some(f) = filter {
                        match self.eval(*f) {
                            Ok(V::Bool(true)) => {}
                            Ok(V::Bool(false)) => continue,
                            _ => {
                                result = Err(Fault);
                                break;
                            }
                        }
                    }
                    kept.push(e);
                }
                let mut values = Vec::new();
                let mut dates = Vec::new();
                let mut decided: Option<bool> = None;
                let mut ones = 0;
                if result.is_ok() && *fold != Fold::Count {
                    for e in kept.iter().copied() {
                        self.enter(*list, e);
                        let Some(v) = value else {
                            result = Err(Fault);
                            break;
                        };
                        match (fold, self.eval(*v)) {
                            (Fold::Any | Fold::All | Fold::None | Fold::One, Ok(V::Bool(b))) => match fold {
                                Fold::Any if b => decided = Some(true),
                                Fold::All if !b => decided = Some(false),
                                Fold::None if b => decided = Some(false),
                                Fold::One => ones += i64::from(b),
                                _ => {}
                            },
                            (Fold::Sum | Fold::Min | Fold::Max | Fold::Avg, Ok(V::Num(d))) => values.push(d),
                            (Fold::Min | Fold::Max, Ok(V::Date(d))) => dates.push(d),
                            _ => {
                                result = Err(Fault);
                                break;
                            }
                        }
                        if decided.is_some() {
                            break;
                        }
                    }
                }
                self.enter(*list, saved);
                result?;
                let sum = || values.iter().try_fold(Decimal::ZERO, |acc, v| acc.checked_add(*v)).ok_or(Fault);
                if matches!(fold, Fold::Min | Fold::Max) && hir.nodes[id as usize].ty.kind == Kind::Date {
                    let best = match fold {
                        Fold::Min => dates.into_iter().min(),
                        _ => dates.into_iter().max(),
                    };
                    return best.map(V::Date).ok_or(Fault);
                }
                Ok(match fold {
                    Fold::Count => V::Num(Decimal::from(kept.len())),
                    Fold::Sum => V::Num(sum()?),
                    Fold::Any => V::Bool(decided.unwrap_or(false)),
                    Fold::All | Fold::None => V::Bool(decided.unwrap_or(true)),
                    Fold::One => V::Bool(ones == 1),
                    Fold::Min => V::Num(values.iter().copied().min().ok_or(Fault)?),
                    Fold::Max => V::Num(values.iter().copied().max().ok_or(Fault)?),
                    Fold::Avg => V::Num(sum()?.checked_div(Decimal::from(values.len())).ok_or(Fault)?),
                })
            }
            Op::FirstMatch { conds, strict } => {
                for (i, c) in conds.iter().enumerate() {
                    match (self.eval(*c), strict) {
                        (Ok(V::Bool(true)), _) => return Ok(V::Num(Decimal::from(i))),
                        (Ok(V::Bool(false)), _) | (Ok(_), false) | (Err(_), false) => {}
                        (Ok(_), true) | (Err(_), true) => return Err(Fault),
                    }
                }
                Ok(V::Num(Decimal::from(conds.len())))
            }
            Op::Pick(sel, arms) => match self.eval(*sel)? {
                V::Num(d) => match usize::try_from(d.mantissa()).ok().and_then(|i| arms.get(i)) {
                    Some(arm) => self.eval(*arm),
                    None => Ok(V::Null),
                },
                _ => Err(Fault),
            },
            Op::Call(func, args) => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(*a)?);
                }
                let text = |t: &[u8]| std::str::from_utf8(t).map(str::to_string).map_err(|_| Fault);
                match (func, vals.as_slice()) {
                    (Func::StartsWith, [V::Text(a), V::Text(b)]) => Ok(V::Bool(a.starts_with(&b[..]))),
                    (Func::EndsWith, [V::Text(a), V::Text(b)]) => Ok(V::Bool(a.ends_with(&b[..]))),
                    (Func::Contains, [V::Text(a), V::Text(b)]) => Ok(V::Bool(text(a)?.contains(text(b)?.as_str()))),
                    (Func::Matches, [V::Text(a), V::Text(b)]) => Ok(V::Bool(Pattern::new(&text(b)?).map_err(|_| Fault)?.is_match(&text(a)?))),
                    (Func::Upper, [V::Text(a)]) => Ok(V::Text(Cow::Owned(text(a)?.to_uppercase().into_bytes()))),
                    (Func::Lower, [V::Text(a)]) => Ok(V::Text(Cow::Owned(text(a)?.to_lowercase().into_bytes()))),
                    (Func::Trim, [V::Text(a)]) => Ok(V::Text(Cow::Owned(text(a)?.trim().as_bytes().to_vec()))),
                    (Func::Len, [V::Text(a)]) => Ok(V::Num(Decimal::from(a.len()))),
                    (Func::Min | Func::Max, items) if node.ty.kind == Kind::Date => {
                        let dates = items.iter().map(|v| if let V::Date(d) = v { Ok(*d) } else { Err(Fault) }).collect::<Result<Vec<_>, _>>()?;
                        let best = match func {
                            Func::Min => dates.into_iter().min(),
                            _ => dates.into_iter().max(),
                        };
                        best.map(V::Date).ok_or(Fault)
                    }
                    (Func::Min, items) => items.iter().map(|v| if let V::Num(d) = v { Ok(*d) } else { Err(Fault) }).collect::<Result<Vec<_>, _>>()?.into_iter().min().map(V::Num).ok_or(Fault),
                    (Func::Max, items) => items.iter().map(|v| if let V::Num(d) = v { Ok(*d) } else { Err(Fault) }).collect::<Result<Vec<_>, _>>()?.into_iter().max().map(V::Num).ok_or(Fault),
                    (func, [V::Num(d)]) => Ok(V::Num(Exact::func(*func, *d))),
                    _ => Err(Fault),
                }
            }
            Op::Str(a) => Ok(V::Text(Cow::Owned(match self.eval(*a)? {
                V::Null => b"null".to_vec(),
                V::Num(d) => d.to_string().into_bytes(),
                V::Bool(b) => b.to_string().into_bytes(),
                V::Text(t) => t.into_owned(),
                V::Date(d) => d.to_string().into_bytes(),
                V::Code(_) => return Err(Fault),
            }))),
            Op::Host(host, args) => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(*a)?);
                }
                let mut given = Vec::with_capacity(vals.len());
                for v in &vals {
                    given.push(match v {
                        V::Null => Given::Null,
                        V::Num(d) => Given::Num(*d),
                        V::Bool(b) => Given::Bool(*b),
                        V::Text(t) => Given::Str(std::str::from_utf8(t).map_err(|_| Fault)?),
                        V::Date(d) => Given::Date(*d),
                        V::Code(_) => return Err(Fault),
                    });
                }
                match Builtins::raw(host.builtin().ok_or(Fault)?, &given).map_err(|_| Fault)? {
                    Out::Num(n) => Ok(V::Num(n)),
                    Out::Bool(b) => Ok(V::Bool(b)),
                    Out::Str(s) => Ok(V::Text(Cow::Owned(s.as_bytes().to_vec()))),
                    Out::Date(d) => Ok(V::Date(d)),
                    Out::Var(Variable::Null) => Ok(V::Null),
                    Out::Var(_) => Err(Fault),
                }
            }
            Op::Clock(Clock::Now, _) => Ok(V::Date(self.feeds.now)),
            Op::Clock(clock, args) => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(*a)?);
                }
                let parsed = |t: &[u8]| std::str::from_utf8(t).map(Date::from_text).map_err(|_| Fault);
                match (clock, vals.as_slice()) {
                    (Clock::Coerce, [V::Text(t)]) => match parsed(t)? {
                        d if d.is_valid() => Ok(V::Date(d)),
                        _ => Err(Fault),
                    },
                    (Clock::Strict, [V::Date(d)]) if d.is_valid() => Ok(V::Date(*d)),
                    (Clock::Text, [V::Null]) => Ok(V::Null),
                    (Clock::Text, [V::Date(d)]) => d.rendered().map(|t| V::Text(Cow::Owned(t.into_bytes()))).ok_or(Fault),
                    (Clock::Matches, [V::Date(d), V::Text(t)]) => Ok(V::Bool(d.is_valid() && parsed(t)? == *d)),
                    (Clock::Matches, [V::Date(d), V::Date(e)]) => Ok(V::Bool(d.is_valid() && d == e)),
                    (Clock::Matches, [_, _]) => Ok(V::Bool(false)),
                    _ => Err(Fault),
                }
            }
            Op::Decode(a) => match (self.eval(*a)?, hir.nodes[*a as usize].ty.kind) {
                (V::Null, _) => Ok(V::Null),
                (V::Code(c), Kind::Code(d)) => hir.dict(d).text(c).map(|t| V::Text(Cow::Owned(t.as_bytes().to_vec()))).ok_or(Fault),
                _ => Err(Fault),
            },
            Op::Concat(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match self.eval(*p)? {
                        V::Text(t) => out.extend_from_slice(&t),
                        _ => return Err(Fault),
                    }
                }
                Ok(V::Text(Cow::Owned(out)))
            }
        }
    }
}
