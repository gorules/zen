use crate::lane::scaled::Scaled;
use rust_decimal::Decimal;
use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Dec {
    pub mant: i64,
    pub scale: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault;

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

impl fmt::Display for Dec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.decimal())
    }
}
