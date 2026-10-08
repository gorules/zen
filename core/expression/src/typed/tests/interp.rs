use crate::typed::batch::{Data, Feed, Feeds};
use crate::typed::kernel::K;
use crate::typed::tests::oracle::Val;
use crate::typed::{Arith, Cmp, Dec, Fold, Func, Hir, Kind, Lit, NodeId, Op, Scope};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq)]
enum V<'a> {
    Null,
    Num(Dec),
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
            (V::Num(d), _) => Val::Num(d.decimal()),
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
        Ok(match &feed.data {
            Data::Num { .. } => feed.num(i).map_or(V::Null, V::Num),
            Data::Bool { bits, offset } => V::Bool(K::get(bits, offset + i)),
            Data::Codes(codes) => V::Code(codes[i]),
            Data::Text(text) => text.get(i).map_or(V::Null, V::Text),
        })
    }

    fn lit(lit: &'a Lit) -> V<'a> {
        match lit {
            Lit::Null => V::Null,
            Lit::Num(d) => V::Num(*d),
            Lit::Bool(b) => V::Bool(*b),
            Lit::Code(_, c) => V::Code(*c),
            Lit::Text(t) => V::Text(t.as_bytes()),
        }
    }

    fn equal(a: V, b: V) -> bool {
        match (a, b) {
            (V::Num(x), V::Num(y)) => x.eq_value(y),
            (x, y) => x == y,
        }
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
                V::Num(d) => d.checked_neg().map(V::Num).map_err(|_| Fault),
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
                let r = match op {
                    Arith::Add => x.checked_add(y).map(Some),
                    Arith::Sub => x.checked_sub(y).map(Some),
                    Arith::Mul => x.checked_mul(y).map(Some),
                    Arith::Div => x.checked_div(y),
                };
                match r {
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
                        let o = x.compare(y);
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
                V::Num(x) => {
                    let lo = x.compare(r.lo);
                    let hi = x.compare(r.hi);
                    let above = lo == Ordering::Greater || (r.lo_closed && lo == Ordering::Equal);
                    let below = hi == Ordering::Less || (r.hi_closed && hi == Ordering::Equal);
                    Ok(V::Bool(above && below))
                }
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
                let mut acc = Dec::ZERO;
                let mut count = 0i64;
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
                    count += 1;
                    if let (Fold::Sum, Some(v)) = (fold, value) {
                        match self.eval(*v) {
                            Ok(V::Num(d)) => match acc.checked_add(d) {
                                Ok(next) => acc = next,
                                Err(_) => {
                                    result = Err(Fault);
                                    break;
                                }
                            },
                            _ => {
                                result = Err(Fault);
                                break;
                            }
                        }
                    }
                }
                self.elem = saved.0;
                self.elems = saved.1;
                result?;
                Ok(V::Num(match fold {
                    Fold::Sum => acc,
                    Fold::Count => Dec::int(count),
                }))
            }
            Op::FirstMatch { conds, strict } => {
                for (i, c) in conds.iter().enumerate() {
                    match (self.eval(*c), strict) {
                        (Ok(V::Bool(true)), _) => return Ok(V::Num(Dec::int(i as i64))),
                        (Ok(V::Bool(false)), _) | (_, false) => {}
                        (_, true) => return Err(Fault),
                    }
                }
                Ok(V::Num(Dec::int(conds.len() as i64)))
            }
            Op::Pick(sel, arms) => match self.eval(*sel)? {
                V::Num(d) => match usize::try_from(d.mant).ok().and_then(|i| arms.get(i)) {
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
                    (Func::Len, [V::Text(a)]) => Ok(V::Num(Dec::int(a.len() as i64))),
                    _ => Err(Fault),
                }
            }
        }
    }
}
