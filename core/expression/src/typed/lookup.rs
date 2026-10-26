use crate::lane::Bits;
use crate::typed::hir::{Cmp, Hir, Kind, Lit, NodeId, Op, Range};
use crate::typed::ins::Rep;
use crate::typed::kernel::K;
use crate::typed::num::{Dec, N};
use std::cmp::Ordering;

#[derive(Clone, Debug)]
pub(crate) struct Lookup {
    pub(crate) keys: Vec<Key>,
    pub(crate) rules: usize,
    pub(crate) words: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Key {
    pub(crate) node: Option<NodeId>,
    pub(crate) domain: Domain,
    pub(crate) hit: Vec<u64>,
    pub(crate) bad: Vec<u64>,
    pub(crate) fixed: Option<(u8, Vec<i64>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Domain {
    Unit,
    Bools,
    Codes(usize),
    Points(Vec<Dec>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Val {
    Null,
    Num(N),
    Bool(bool),
    Code(i32),
    Fault,
}

struct Test {
    rule: usize,
    key: Option<NodeId>,
    node: NodeId,
}

impl Lookup {
    pub(crate) const RULES: usize = 256;
    pub(crate) const WORDS: usize = 4;
    const POINTS: usize = 512;

    pub(crate) fn plan(hir: &Hir, conds: &[NodeId], strict: bool, least: usize) -> Option<Lookup> {
        let rules = conds.len();
        if rules == 0 || rules > Self::RULES {
            return None;
        }
        let mut tests: Vec<Test> = Vec::new();
        for (rule, &cond) in conds.iter().enumerate() {
            let mut chain = Vec::new();
            Self::chain(hir, cond, &mut chain);
            for node in chain {
                let mut keys = Vec::new();
                Self::keys(hir, node, &mut keys)?;
                match keys.as_slice() {
                    [] => tests.push(Test {
                        rule,
                        key: None,
                        node,
                    }),
                    [key] => tests.push(Test {
                        rule,
                        key: Some(*key),
                        node,
                    }),
                    _ => return None,
                }
            }
        }
        if tests.len() < least {
            return None;
        }
        let order = Self::order(&tests, rules, strict)?;
        let words = rules.div_ceil(64);
        let mut keys = Vec::with_capacity(order.len());
        for key in order {
            let domain = Self::domain(hir, key, &tests)?;
            let values = Self::values(&domain)?;
            let mut hit = vec![0u64; values.len() * words];
            let mut bad = vec![0u64; values.len() * words];
            for (at, value) in values.iter().enumerate() {
                for rule in 0..rules {
                    let mut state = Val::Bool(true);
                    for test in tests.iter().filter(|t| t.rule == rule && t.key == key) {
                        if state != Val::Bool(true) {
                            break;
                        }
                        state = Self::eval(hir, test.node, key, *value)?;
                    }
                    match state {
                        Val::Bool(true) => {
                            Bits::set(&mut hit[at * words..(at + 1) * words], rule, true)
                        }
                        Val::Bool(false) => {}
                        _ if strict => {
                            Bits::set(&mut bad[at * words..(at + 1) * words], rule, true)
                        }
                        _ => {}
                    }
                }
            }
            keys.push(Key {
                node: key,
                domain,
                hit,
                bad,
                fixed: None,
            });
        }
        Some(Lookup { keys, rules, words })
    }

    pub(crate) fn fix(&mut self, reps: &[Rep]) {
        for (key, rep) in self.keys.iter_mut().zip(reps) {
            if let (Domain::Points(points), Rep::Fix(s)) = (&key.domain, rep) {
                key.fixed = points
                    .iter()
                    .map(|p| p.upscale(*s).map(|u| u.mant))
                    .collect::<Option<Vec<i64>>>()
                    .map(|m| (*s, m));
            }
        }
    }

    fn chain(hir: &Hir, node: NodeId, out: &mut Vec<NodeId>) {
        match hir.node(node).op {
            Op::And(a, b) => {
                Self::chain(hir, a, out);
                Self::chain(hir, b, out);
            }
            _ => out.push(node),
        }
    }

    fn keys(hir: &Hir, node: NodeId, out: &mut Vec<NodeId>) -> Option<()> {
        let n = hir.node(node);
        if matches!(n.ty.kind, Kind::Code(_)) && !matches!(n.op, Op::Const(_)) {
            if !out.contains(&node) {
                out.push(node);
            }
            return Some(());
        }
        match &n.op {
            Op::Const(Lit::Text(_)) => return None,
            Op::Const(_) => {}
            Op::Column(_) | Op::Field(..) if matches!(n.ty.kind, Kind::Num(_) | Kind::Bool) => {
                if !out.contains(&node) {
                    out.push(node);
                }
            }
            Op::Cmp(..)
            | Op::InSet(..)
            | Op::InRange(..)
            | Op::Not(_)
            | Op::And(..)
            | Op::Or(..)
            | Op::If(..)
            | Op::Coalesce(..) => {
                for input in n.op.inputs() {
                    Self::keys(hir, input, out)?;
                }
            }
            _ => return None,
        }
        Some(())
    }

    fn order(tests: &[Test], rules: usize, strict: bool) -> Option<Vec<Option<NodeId>>> {
        let mut keys: Vec<Option<NodeId>> = Vec::new();
        for t in tests {
            if !keys.contains(&t.key) {
                keys.push(t.key);
            }
        }
        if !strict {
            return Some(keys);
        }
        let mut before: Vec<(usize, usize)> = Vec::new();
        for rule in 0..rules {
            let mut seen: Vec<usize> = Vec::new();
            for t in tests.iter().filter(|t| t.rule == rule) {
                let k = keys.iter().position(|x| *x == t.key)?;
                match seen.last() {
                    Some(last) if *last == k => continue,
                    _ if seen.contains(&k) => return None,
                    _ => {}
                }
                for &s in &seen {
                    if !before.contains(&(s, k)) {
                        before.push((s, k));
                    }
                }
                seen.push(k);
            }
        }
        let mut left: Vec<usize> = (0..keys.len()).collect();
        let mut ordered = Vec::with_capacity(keys.len());
        while !left.is_empty() {
            let at = left
                .iter()
                .position(|k| !before.iter().any(|(a, b)| b == k && left.contains(a)))?;
            ordered.push(keys[left.remove(at)]);
        }
        Some(ordered)
    }

    fn domain(hir: &Hir, key: Option<NodeId>, tests: &[Test]) -> Option<Domain> {
        let Some(key) = key else {
            return Some(Domain::Unit);
        };
        let node = hir.node(key);
        match (&node.op, node.ty.kind) {
            (_, Kind::Code(d)) => Some(Domain::Codes(
                hir.schema.dicts.get(d as usize)?.values.len(),
            )),
            (_, Kind::Bool) => Some(Domain::Bools),
            (_, Kind::Num(_)) => {
                let mut points: Vec<Dec> = Vec::new();
                for t in tests.iter().filter(|t| t.key == Some(key)) {
                    Self::points(hir, t.node, &mut points);
                }
                points.sort_by(|a, b| a.compare(*b));
                points.dedup_by(|a, b| a.compare(*b) == Ordering::Equal);
                (points.len() <= Self::POINTS).then_some(Domain::Points(points))
            }
            _ => None,
        }
    }

    fn points(hir: &Hir, node: NodeId, out: &mut Vec<Dec>) {
        match &hir.node(node).op {
            Op::Const(Lit::Num(d)) => out.push(*d),
            Op::InRange(_, r) => out.extend([r.lo, r.hi]),
            Op::InSet(_, set) => out.extend(set.iter().filter_map(|l| match l {
                Lit::Num(d) => Some(*d),
                _ => None,
            })),
            _ => {}
        }
        for input in hir.node(node).op.inputs() {
            Self::points(hir, input, out);
        }
    }

    fn values(domain: &Domain) -> Option<Vec<Val>> {
        Some(match domain {
            Domain::Unit => vec![Val::Null],
            Domain::Bools => vec![Val::Bool(false), Val::Bool(true), Val::Null],
            Domain::Codes(k) => (0..=*k as i32)
                .map(Val::Code)
                .chain(std::iter::once(Val::Null))
                .collect(),
            Domain::Points(points) => {
                let mut values = Vec::with_capacity(2 * points.len() + 2);
                let one = N::Dec(Dec::int(1));
                let two = N::Dec(Dec::int(2));
                for (j, p) in points.iter().enumerate() {
                    let below = match j {
                        0 => N::Dec(*p)
                            .arith(crate::typed::hir::Arith::Sub, one)
                            .ok()??,
                        _ => {
                            let sum = N::Dec(points[j - 1])
                                .arith(crate::typed::hir::Arith::Add, N::Dec(*p))
                                .ok()??;
                            sum.arith(crate::typed::hir::Arith::Div, two).ok()??
                        }
                    };
                    values.push(Val::Num(below));
                    values.push(Val::Num(N::Dec(*p)));
                }
                let above = match points.last() {
                    Some(p) => N::Dec(*p)
                        .arith(crate::typed::hir::Arith::Add, one)
                        .ok()??,
                    None => N::Dec(Dec::ZERO),
                };
                values.push(Val::Num(above));
                values.push(Val::Null);
                values
            }
        })
    }

    fn eval(hir: &Hir, node: NodeId, key: Option<NodeId>, value: Val) -> Option<Val> {
        if Some(node) == key {
            return Some(value);
        }
        let ev = |n: NodeId| Self::eval(hir, n, key, value);
        Some(match &hir.node(node).op {
            Op::Const(Lit::Null) => Val::Null,
            Op::Const(Lit::Num(d)) => Val::Num(N::Dec(*d)),
            Op::Const(Lit::Bool(b)) => Val::Bool(*b),
            Op::Const(Lit::Code(_, c)) => Val::Code(*c),
            Op::Cmp(op, a, b) => {
                let (x, y) = (ev(*a)?, ev(*b)?);
                match (op, x, y) {
                    (_, Val::Fault, _) | (_, _, Val::Fault) => Val::Fault,
                    (Cmp::Eq | Cmp::Ne, x, y) => Val::Bool(Self::same(x, y) == (*op == Cmp::Eq)),
                    (_, Val::Num(x), Val::Num(y)) => Val::Bool(K::ord(*op, x.compare(y))),
                    _ => Val::Fault,
                }
            }
            Op::InSet(a, set) => match ev(*a)? {
                Val::Fault => Val::Fault,
                x => Val::Bool(set.iter().any(|l| Self::same(x, Self::lit(l)))),
            },
            Op::InRange(a, r) => match ev(*a)? {
                Val::Num(x) => Val::Bool(Self::within(x, r)),
                _ => Val::Fault,
            },
            Op::Not(a) => match ev(*a)? {
                Val::Bool(b) => Val::Bool(!b),
                _ => Val::Fault,
            },
            Op::And(a, b) => match ev(*a)? {
                Val::Bool(false) => Val::Bool(false),
                Val::Bool(true) => ev(*b)?,
                _ => Val::Fault,
            },
            Op::Or(a, b) => match ev(*a)? {
                Val::Bool(true) => Val::Bool(true),
                Val::Bool(false) => ev(*b)?,
                _ => Val::Fault,
            },
            Op::If(c, t, e) => match ev(*c)? {
                Val::Bool(true) => ev(*t)?,
                Val::Bool(false) => ev(*e)?,
                _ => Val::Fault,
            },
            Op::Coalesce(a, b) => match ev(*a)? {
                Val::Null => ev(*b)?,
                x => x,
            },
            _ => return None,
        })
    }

    fn lit(lit: &Lit) -> Val {
        match lit {
            Lit::Null => Val::Null,
            Lit::Num(d) => Val::Num(N::Dec(*d)),
            Lit::Bool(b) => Val::Bool(*b),
            Lit::Code(_, c) => Val::Code(*c),
            Lit::Text(_) | Lit::Date(_) => Val::Fault,
        }
    }

    fn same(x: Val, y: Val) -> bool {
        match (x, y) {
            (Val::Num(a), Val::Num(b)) => a.compare(b) == Ordering::Equal,
            (Val::Fault, _) | (_, Val::Fault) => false,
            (a, b) => a == b,
        }
    }

    fn within(x: N, r: &Range) -> bool {
        let (lo, hi) = (x.compare(N::Dec(r.lo)), x.compare(N::Dec(r.hi)));
        let above = lo == Ordering::Greater || (r.lo_closed && lo == Ordering::Equal);
        let below = hi == Ordering::Less || (r.hi_closed && hi == Ordering::Equal);
        above && below
    }
}

impl Key {
    #[inline]
    pub(crate) fn null(&self) -> usize {
        match &self.domain {
            Domain::Unit => 0,
            Domain::Bools => 2,
            Domain::Codes(k) => k + 1,
            Domain::Points(points) => 2 * points.len() + 1,
        }
    }

    #[inline]
    pub(crate) fn code(&self, code: i32) -> usize {
        match &self.domain {
            Domain::Codes(k) => usize::try_from(code).map_or(*k, |c| c.min(*k)),
            _ => 0,
        }
    }

    #[inline]
    pub(crate) fn mant(&self, m: i64) -> Option<usize> {
        let (_, mants) = self.fixed.as_ref()?;
        Some(match mants.binary_search(&m) {
            Ok(j) => 2 * j + 1,
            Err(j) => 2 * j,
        })
    }

    #[inline]
    pub(crate) fn num(&self, x: N) -> usize {
        match &self.domain {
            Domain::Points(points) => match points.binary_search_by(|p| N::Dec(*p).compare(x)) {
                Ok(j) => 2 * j + 1,
                Err(j) => 2 * j,
            },
            _ => 0,
        }
    }
}
