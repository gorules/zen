use crate::typed::batch::{Data, Feed, Feeds};
use crate::typed::kernel::K;
use crate::typed::num::Exact;
use crate::typed::tests::oracle::Val;
use crate::typed::{Cmp, Fold, Func, Hir, Kind, Lit, NodeId, Op, Range, Scope};
use rust_decimal::Decimal;
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq)]
enum V<'a> {
    Null,
    Num(Decimal),
    Bool(bool),
    Code(i32),
    Text(&'a [u8]),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Fault;

type R<'a> = Result<V<'a>, Fault>;

pub(crate) struct Interp<'a, 'x> {
    hir: &'a Hir,
    feeds: &'a Feeds<'x>,
    row: usize,
    elem: usize,
    rows: Vec<Option<R<'a>>>,
    elems: Vec<Option<R<'a>>>,
}

impl<'a, 'x> Interp<'a, 'x> {
    pub(crate) fn run(hir: &'a Hir, feeds: &'a Feeds<'x>) -> Vec<Option<Vec<Val>>> {
        let n = hir.nodes.len();
        let mut walk = Interp { hir, feeds, row: 0, elem: 0, rows: vec![None; n], elems: vec![None; n] };
        let mut out = Vec::with_capacity(feeds.rows);
        for row in 0..feeds.rows {
            walk.row = row;
            walk.rows.fill(None);
            let mut values = Vec::with_capacity(hir.outputs.len());
            let mut faulted = false;
            for output in &hir.outputs {
                match output.scope {
                    Scope::Row => match walk.eval(output.node) {
                        Ok(v) => values.push(walk.val(v, output.ty.kind)),
                        Err(Fault) => faulted = true,
                    },
                    Scope::Elem(l) => {
                        let list = &feeds.lists[l as usize];
                        let range = list.offsets[row] as usize..list.offsets[row + 1] as usize;
                        let mut items = Vec::new();
                        for e in range {
                            walk.elem = e;
                            walk.elems.fill(None);
                            match walk.eval(output.node) {
                                Ok(v) => items.push(walk.val(v, output.ty.kind)),
                                Err(Fault) => faulted = true,
                            }
                        }
                        values.push(Val::List(items));
                    }
                }
            }
            out.push((!faulted).then_some(values));
        }
        out
    }

    fn val(&self, v: V, kind: Kind) -> Val {
        match (v, kind) {
            (V::Null, _) => Val::Null,
            (V::Num(d), _) => Val::Num(d),
            (V::Bool(b), _) => Val::Bool(b),
            (V::Code(c), Kind::Code(d)) => self.hir.dict(d).text(c).map_or(Val::Other(format!("code {c}")), |t| Val::Str(t.to_string())),
            (V::Code(c), _) => Val::Other(format!("code {c}")),
            (V::Text(t), _) => Val::Str(String::from_utf8_lossy(t).to_string()),
        }
    }

    fn eval(&mut self, id: NodeId) -> R<'a> {
        let at = id as usize;
        let elem = matches!(self.hir.nodes[at].scope, Scope::Elem(_));
        let cached = match elem {
            true => self.elems[at],
            false => self.rows[at],
        };
        if let Some(r) = cached {
            return r;
        }
        let r = self.compute(id);
        match elem {
            true => self.elems[at] = Some(r),
            false => self.rows[at] = Some(r),
        }
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
            Data::Text(text) => text.get(i).map_or(V::Null, V::Text),
        })
    }

    fn lit(lit: &'a Lit) -> V<'a> {
        match lit {
            Lit::Null => V::Null,
            Lit::Num(d) => V::Num(d.decimal()),
            Lit::Bool(b) => V::Bool(*b),
            Lit::Code(_, c) => V::Code(*c),
            Lit::Text(t) => V::Text(t.as_bytes()),
        }
    }

    fn equal(a: V, b: V) -> bool {
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
            Op::Field(l, f) => Self::read(&self.feeds.fields[*l as usize][*f as usize], self.elem),
            Op::Parent(a) => self.eval(*a),
            Op::Const(lit) => Ok(Self::lit(lit)),
            Op::Classify(a, d) => match self.eval(*a)? {
                V::Text(t) => {
                    let dict = hir.dict(*d);
                    Ok(V::Code(dict.values.iter().position(|v| v.as_bytes() == t).map_or(dict.values.len() as i32, |i| i as i32)))
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
                    Cmp::Eq => Ok(V::Bool(Self::equal(x, y))),
                    Cmp::Ne => Ok(V::Bool(!Self::equal(x, y))),
                    _ => {
                        let (V::Num(x), V::Num(y)) = (x, y) else {
                            return Err(Fault);
                        };
                        let o = x.cmp(&y);
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
                Ok(V::Bool(set.iter().any(|lit| Self::equal(x, Self::lit(lit)))))
            }
            Op::InRange(a, r) => match self.eval(*a)? {
                V::Num(x) => Ok(V::Bool(Self::within(x, r))),
                _ => Err(Fault),
            },
            Op::Fold { fold, list, filter, value } => {
                let l = &self.feeds.lists[*list as usize];
                if l.null.as_ref().is_some_and(|null| K::get(null, self.row)) {
                    return Err(Fault);
                }
                let range = l.offsets[self.row] as usize..l.offsets[self.row + 1] as usize;
                let saved = (self.elem, std::mem::take(&mut self.elems));
                self.elems = vec![None; hir.nodes.len()];
                let mut kept = Vec::new();
                let mut result = Ok(());
                for e in range {
                    self.elem = e;
                    self.elems.fill(None);
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
                let mut decided: Option<bool> = None;
                let mut ones = 0;
                if result.is_ok() && *fold != Fold::Count {
                    for e in kept.iter().copied() {
                        self.elem = e;
                        self.elems.fill(None);
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
                self.elem = saved.0;
                self.elems = saved.1;
                result?;
                let sum = || values.iter().try_fold(Decimal::ZERO, |acc, v| acc.checked_add(*v)).ok_or(Fault);
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
                match (func, vals.as_slice()) {
                    (Func::StartsWith, [V::Text(a), V::Text(b)]) => Ok(V::Bool(a.starts_with(b))),
                    (Func::EndsWith, [V::Text(a), V::Text(b)]) => Ok(V::Bool(a.ends_with(b))),
                    (Func::Len, [V::Text(a)]) => Ok(V::Num(Decimal::from(a.len()))),
                    (Func::Min, items) => items.iter().map(|v| if let V::Num(d) = v { Ok(*d) } else { Err(Fault) }).collect::<Result<Vec<_>, _>>()?.into_iter().min().map(V::Num).ok_or(Fault),
                    (Func::Max, items) => items.iter().map(|v| if let V::Num(d) = v { Ok(*d) } else { Err(Fault) }).collect::<Result<Vec<_>, _>>()?.into_iter().max().map(V::Num).ok_or(Fault),
                    (func, [V::Num(d)]) => Ok(V::Num(Exact::func(*func, *d))),
                    _ => Err(Fault),
                }
            }
        }
    }
}
