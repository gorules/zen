use crate::typed::hir::{Arith, Cmp, Fold, Func, Lit, NodeId, Range, Scope, Unsupported};
use crate::typed::num::Dec;
use std::fmt;

pub(crate) type R = u16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Rep {
    Fix(u8),
    Dyn,
    Bool,
    Code,
    Text,
    Null,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Src {
    Col(u16),
    Field(u16, u16),
    Class(u16),
}

#[derive(Clone, Debug)]
pub(crate) enum Arg {
    Src(Src),
    Lit(String),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum FixOp {
    Add,
    Sub,
    Mul,
}

#[derive(Clone, Debug)]
pub(crate) enum Guard {
    True(R),
    False(R),
    Null(R),
    Masks(R, Vec<u16>),
}

#[derive(Clone, Debug)]
pub(crate) enum Ins {
    Load { dst: R, src: Src, rep: Rep },
    Gather { dst: R, buf: u16, list: u16, rep: Rep },
    Fill { dst: R, buf: u16, rep: Rep },
    Spill { src: R, buf: u16, rep: Rep },
    Fix { op: FixOp, dst: R, a: R, b: R },
    Neg { dst: R, a: R, rep: Rep },
    Num { op: Arith, dst: R, a: R, ra: Rep, b: R, rb: Rep },
    DivPow { dst: R, a: R, ra: Rep, k: u32, by: u8 },
    CmpFix { op: Cmp, dst: R, a: R, b: R },
    CmpGen { op: Cmp, dst: R, a: R, ra: Rep, b: R, rb: Rep },
    CmpNum { op: Cmp, dst: R, a: R, ra: Rep, b: R, rb: Rep },
    CmpNumK { op: Cmp, dst: R, a: R, ra: Rep, k: Dec, tab: Vec<i64>, fits: u32 },
    PickTab { dst: R, sel: R, tab: Tab, rep: Rep },
    SelK { dst: R, c: R, t: i64, e: i64, rep: Rep },
    EqCode { ne: bool, dst: R, a: R, b: R },
    EqBool { ne: bool, dst: R, a: R, b: R },
    EqText { ne: bool, dst: R, a: Arg, b: Arg },
    InCode { dst: R, a: R, set: u64, null: bool },
    InGen { dst: R, a: R, ra: Rep, set: Vec<Lit> },
    InRangeFix { dst: R, a: R, lo: i64, span: u64, empty: bool },
    InRangeGen { dst: R, a: R, ra: Rep, range: Range },
    Not { dst: R, a: R },
    And { dst: R, a: R, b: R },
    Or { dst: R, a: R, b: R },
    Blend { dst: R, c: R, t: R, rt: Rep, e: R, re: Rep, rep: Rep },
    Coalesce { dst: R, a: R, ra: Rep, b: R, rb: Rep, rep: Rep },
    First { dst: R, conds: Vec<R>, sel: bool, strict: bool },
    Pick { dst: R, sel: R, arms: Vec<(R, Rep, Vec<u16>)>, rep: Rep },
    Skip { guard: Guard, len: u32 },
    Fold { fold: Fold, buf: u16, list: u16, filter: Option<R>, value: Option<(R, Rep)> },
    Text { func: Func, dst: R, a: Arg, b: Option<Arg> },
    Out { slot: u16, src: R, rep: Rep, scope: Scope },
}

#[derive(Clone)]
pub(crate) struct Tab {
    pub(crate) len: usize,
    pub(crate) mant: Box<[i64; 256]>,
    pub(crate) scale: Box<[u8; 256]>,
    pub(crate) codes: Box<[i32; 256]>,
    pub(crate) flag: [u64; 4],
    pub(crate) ok: [u64; 4],
}

#[derive(Clone, Debug)]
pub(crate) enum Arm {
    Node(NodeId),
    Tab,
    Mul(NodeId),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BufDef {
    pub(crate) scope: Scope,
    pub(crate) fold: Option<u16>,
}

impl Rep {
    pub(crate) fn of_lit(lit: &Lit) -> Rep {
        match lit {
            Lit::Null => Rep::Null,
            Lit::Num(d) => Rep::Fix(d.scale),
            Lit::Bool(_) => Rep::Bool,
            Lit::Code(..) => Rep::Code,
            Lit::Text(_) => Rep::Text,
        }
    }

    pub(crate) fn join(a: Rep, b: Rep) -> Result<Rep, Unsupported> {
        match (a, b) {
            (x, y) if x == y => Ok(x),
            (Rep::Null, x) | (x, Rep::Null) => Ok(x),
            (Rep::Fix(_) | Rep::Dyn, Rep::Fix(_) | Rep::Dyn) => Ok(Rep::Dyn),
            (x, y) => Err(Unsupported::new(format!("typed: cannot join {x:?} and {y:?}"))),
        }
    }

    pub(crate) fn num(self) -> bool {
        matches!(self, Rep::Fix(_) | Rep::Dyn)
    }
}

impl Tab {
    pub(crate) const MAX: usize = 255;

    pub(crate) fn new(entries: &[Option<Lit>]) -> Tab {
        let mut t =
            Tab { len: entries.len().min(256), mant: Box::new([0; 256]), scale: Box::new([0; 256]), codes: Box::new([0; 256]), flag: [0; 4], ok: [0; 4] };
        for (k, e) in entries.iter().enumerate().take(256) {
            let (mant, scale, code, flag, ok) = match e {
                Some(Lit::Num(d)) => (d.mant, d.scale, 0, false, true),
                Some(Lit::Code(_, c)) => (0, 0, *c, false, true),
                Some(Lit::Bool(b)) => (0, 0, 0, *b, true),
                _ => (0, 0, 0, false, false),
            };
            t.mant[k] = mant;
            t.scale[k] = scale;
            t.codes[k] = code;
            t.flag[k >> 6] |= (flag as u64) << (k & 63);
            t.ok[k >> 6] |= (ok as u64) << (k & 63);
        }
        t
    }

    #[inline(always)]
    pub(crate) fn bit(set: &[u64; 4], x: usize) -> bool {
        (set[(x >> 6) & 3] >> (x & 63)) & 1 != 0
    }
}

impl fmt::Debug for Tab {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let items: Vec<String> = (0..self.len)
            .map(|k| match Tab::bit(&self.ok, k) {
                false => "null".to_string(),
                true => format!("{}e-{}|c{}|{}", self.mant[k], self.scale[k], self.codes[k], Tab::bit(&self.flag, k)),
            })
            .collect();
        write!(f, "[{}]", items.join(", "))
    }
}

impl Ins {
    pub(crate) fn regs(&mut self, f: &mut impl FnMut(&mut R, bool)) {
        match self {
            Ins::Load { dst, .. } | Ins::Gather { dst, .. } | Ins::Fill { dst, .. } | Ins::EqText { dst, .. } | Ins::Text { dst, .. } => f(dst, true),
            Ins::Spill { src, .. } | Ins::Out { src, .. } => f(src, false),
            Ins::Fix { dst, a, b, .. }
            | Ins::Num { dst, a, b, .. }
            | Ins::CmpNum { dst, a, b, .. }
            | Ins::CmpFix { dst, a, b, .. }
            | Ins::CmpGen { dst, a, b, .. }
            | Ins::EqCode { dst, a, b, .. }
            | Ins::EqBool { dst, a, b, .. }
            | Ins::And { dst, a, b }
            | Ins::Or { dst, a, b }
            | Ins::Coalesce { dst, a, b, .. } => {
                f(a, false);
                f(b, false);
                f(dst, true);
            }
            Ins::Neg { dst, a, .. }
            | Ins::DivPow { dst, a, .. }
            | Ins::CmpNumK { dst, a, .. }
            | Ins::PickTab { dst, sel: a, .. }
            | Ins::SelK { dst, c: a, .. }
            | Ins::InCode { dst, a, .. }
            | Ins::InGen { dst, a, .. }
            | Ins::InRangeFix { dst, a, .. }
            | Ins::InRangeGen { dst, a, .. }
            | Ins::Not { dst, a } => {
                f(a, false);
                f(dst, true);
            }
            Ins::Blend { dst, c, t, e, .. } => {
                f(c, false);
                f(t, false);
                f(e, false);
                f(dst, true);
            }
            Ins::First { dst, conds, .. } => {
                for c in conds.iter_mut() {
                    f(c, false);
                }
                f(dst, true);
            }
            Ins::Pick { dst, sel, arms, .. } => {
                f(sel, false);
                for a in arms.iter_mut() {
                    f(&mut a.0, false);
                }
                f(dst, true);
            }
            Ins::Skip { guard, .. } => match guard {
                Guard::True(r) | Guard::False(r) | Guard::Null(r) | Guard::Masks(r, _) => f(r, false),
            },
            Ins::Fold { filter, value, .. } => {
                if let Some(r) = filter {
                    f(r, false);
                }
                if let Some((r, _)) = value {
                    f(r, false);
                }
            }
        }
    }

    pub(crate) fn dst(&self) -> Option<R> {
        let mut d = None;
        let mut copy = self.clone();
        copy.regs(&mut |r, def| {
            if def {
                d = Some(*r);
            }
        });
        d
    }
}
