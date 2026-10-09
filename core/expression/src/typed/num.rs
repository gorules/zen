use crate::lane::scaled::Scaled;
use crate::typed::hir::{Arith, Func};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::{Decimal, MathematicalOps, RoundingStrategy};
use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Dec {
    pub mant: i64,
    pub scale: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum N {
    Dec(Dec),
    Big(Decimal),
}

pub(crate) struct Exact;

impl Dec {
    pub const MAX_SCALE: u8 = 28;
    pub const ZERO: Dec = Dec { mant: 0, scale: 0 };
    pub const POW10: [i64; 19] = Scaled::POW10;

    #[inline]
    pub const fn new(mant: i64, scale: u8) -> Self {
        Self { mant, scale }
    }

    #[inline]
    pub const fn int(mant: i64) -> Self {
        Self { mant, scale: 0 }
    }

    #[inline]
    pub fn is_zero(self) -> bool {
        self.mant == 0
    }

    #[inline]
    fn parts(self) -> (i64, u8) {
        (self.mant, self.scale)
    }

    #[inline]
    fn of(parts: (i64, u8)) -> Self {
        Self::new(parts.0, parts.1)
    }

    #[inline]
    pub fn checked_add(self, rhs: Self) -> Result<Self, Fault> {
        Scaled::add(self.parts(), rhs.parts(), false).map(Self::of).ok_or(Fault)
    }

    #[inline]
    pub fn checked_sub(self, rhs: Self) -> Result<Self, Fault> {
        Scaled::add(self.parts(), rhs.parts(), true).map(Self::of).ok_or(Fault)
    }

    #[inline]
    pub fn checked_mul(self, rhs: Self) -> Result<Self, Fault> {
        Scaled::multiply(self.parts(), rhs.parts()).map(Self::of).ok_or(Fault)
    }

    #[inline]
    pub fn checked_neg(self) -> Result<Self, Fault> {
        match self.mant {
            0 => Err(Fault),
            m => m.checked_neg().map(|m| Self::new(m, self.scale)).ok_or(Fault),
        }
    }

    #[inline]
    pub fn checked_div(self, rhs: Self) -> Result<Option<Self>, Fault> {
        if rhs.mant == 0 {
            return Ok(None);
        }
        let q = match Scaled::power(rhs.parts()) {
            Some(k) => Scaled::divide_power(self.parts(), k, rhs.scale),
            None => Scaled::divide(self.parts(), rhs.parts()),
        };
        q.map(|q| Some(Self::of(q))).ok_or(Fault)
    }

    #[inline]
    pub fn checked_rem(self, rhs: Self) -> Result<Option<Self>, Fault> {
        if rhs.mant == 0 {
            return Ok(None);
        }
        Scaled::remainder(self.parts(), rhs.parts()).map(|r| Some(Self::of(r))).ok_or(Fault)
    }

    #[inline]
    pub(crate) fn func(self, func: Func) -> Option<Self> {
        let (m, s) = self.parts();
        match func {
            Func::Abs => Scaled::abs(m, s),
            Func::Floor => Scaled::floor(m, s),
            Func::Ceil => Scaled::ceil(m, s),
            Func::Round(p) => Scaled::round(m, s, p),
            Func::Trunc(p) => Scaled::trunc(m, s, p),
            _ => None,
        }
        .map(Self::of)
    }

    #[inline]
    pub(crate) fn arith(self, op: Arith, rhs: Self) -> Result<Option<Self>, Fault> {
        match op {
            Arith::Add => self.checked_add(rhs).map(Some),
            Arith::Sub => self.checked_sub(rhs).map(Some),
            Arith::Mul => self.checked_mul(rhs).map(Some),
            Arith::Div => self.checked_div(rhs),
            Arith::Rem => self.checked_rem(rhs),
            Arith::Pow => Err(Fault),
        }
    }

    #[inline]
    pub fn div_power(self, k: u32, scale: u8) -> Result<Self, Fault> {
        Scaled::divide_power(self.parts(), k, scale).map(Self::of).ok_or(Fault)
    }

    pub fn power(self) -> Option<u32> {
        Scaled::power(self.parts())
    }

    #[inline]
    pub fn compare(self, rhs: Self) -> Ordering {
        match Scaled::compare(self.parts(), rhs.parts()) {
            Some(o) => o,
            None => self.decimal().cmp(&rhs.decimal()),
        }
    }

    #[inline]
    pub fn eq_value(self, rhs: Self) -> bool {
        self.compare(rhs) == Ordering::Equal
    }

    pub fn upscale(self, scale: u8) -> Option<Self> {
        (scale >= self.scale).then_some(())?;
        Scaled::rescale(self.mant, self.scale, scale).map(|m| Self::new(m, scale))
    }

    pub fn decimal(self) -> Decimal {
        Scaled::decimal(self.mant, self.scale)
    }

    pub fn from_decimal(d: &Decimal) -> Option<Self> {
        Scaled::parts(d).filter(|(_, s)| *s <= Self::MAX_SCALE).map(Self::of)
    }
}

impl Exact {
    pub(crate) fn arith(op: Arith, x: Decimal, y: Decimal) -> Result<Option<Decimal>, Fault> {
        match op {
            Arith::Add => x.checked_add(y).map(Some).ok_or(Fault),
            Arith::Sub => x.checked_sub(y).map(Some).ok_or(Fault),
            Arith::Mul => x.checked_mul(y).map(Some).ok_or(Fault),
            Arith::Div => Ok(x.checked_div(y)),
            Arith::Rem => Ok(x.checked_rem(y)),
            Arith::Pow => x.checked_powd(y).or_else(|| Decimal::from_f64(x.to_f64()?.powf(y.to_f64()?))).map(Some).ok_or(Fault),
        }
    }

    pub(crate) fn func(func: Func, x: Decimal) -> Decimal {
        match func {
            Func::Abs => x.abs(),
            Func::Floor => x.floor(),
            Func::Ceil => x.ceil(),
            Func::Round(p) => x.round_dp_with_strategy(u32::from(p), RoundingStrategy::MidpointAwayFromZero),
            Func::Trunc(p) => x.trunc_with_scale(u32::from(p)),
            _ => x,
        }
    }
}

impl N {
    #[inline]
    pub(crate) fn decimal(self) -> Decimal {
        match self {
            N::Dec(d) => d.decimal(),
            N::Big(d) => d,
        }
    }

    #[inline]
    pub(crate) fn of(d: Decimal) -> N {
        match Dec::from_decimal(&d) {
            Some(n) => N::Dec(n),
            None => N::Big(d),
        }
    }

    #[inline]
    pub(crate) fn compare(self, rhs: N) -> Ordering {
        match (self, rhs) {
            (N::Dec(x), N::Dec(y)) => x.compare(y),
            (x, y) => x.decimal().cmp(&y.decimal()),
        }
    }

    #[inline]
    pub(crate) fn arith(self, op: Arith, rhs: N) -> Result<Option<N>, Fault> {
        if let (N::Dec(x), N::Dec(y)) = (self, rhs) {
            if let Ok(r) = x.arith(op, y) {
                return Ok(r.map(N::Dec));
            }
        }
        Exact::arith(op, self.decimal(), rhs.decimal()).map(|r| r.map(N::of))
    }

    #[inline]
    pub(crate) fn func(self, func: Func) -> N {
        match self {
            N::Dec(d) => match d.func(func) {
                Some(r) => N::Dec(r),
                None => N::of(Exact::func(func, d.decimal())),
            },
            N::Big(d) => N::of(Exact::func(func, d)),
        }
    }

    #[inline]
    pub(crate) fn neg(self) -> N {
        match self {
            N::Dec(d) => match d.checked_neg() {
                Ok(r) => N::Dec(r),
                Err(_) => N::of(-d.decimal()),
            },
            N::Big(d) => N::of(-d),
        }
    }
}

impl fmt::Display for Dec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.decimal())
    }
}
