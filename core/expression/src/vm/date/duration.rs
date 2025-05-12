use crate::vm::date::duration_parser::{DurationParseError, DurationParser};
use crate::vm::date::duration_unit::DurationUnit;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use std::ops::Neg;

#[derive(Debug, Clone, Default)]
pub(crate) struct Duration {
    pub seconds: i64,
    pub months: i32,
    pub years: i32,
}

impl Duration {
    pub fn parse(s: &str) -> Result<Self, DurationParseError> {
        DurationParser {
            iter: s.chars(),
            src: s,
            duration: Duration::default(),
        }
        .parse()
    }

    pub fn from_unit(n: Decimal, unit: DurationUnit) -> Option<Self> {
        if let Some(secs) = unit.as_secs() {
            return Some(Self {
                seconds: n.checked_mul(Decimal::from_u64(secs)?)?.to_i64()?,
                ..Default::default()
            });
        };

        match unit {
            DurationUnit::Month => Some(Duration {
                months: n.to_i32()?,
                ..Default::default()
            }),
            DurationUnit::Quarter => Some(Duration {
                months: n.to_i32()? * 3,
                ..Default::default()
            }),
            DurationUnit::Year => Some(Duration {
                years: n.to_i32()?,
                ..Default::default()
            }),
            _ => None,
        }
    }

    pub fn negate(self) -> Self {
        Self {
            years: self.years.neg(),
            months: self.months.neg(),
            seconds: self.seconds.neg(),
        }
    }

    pub fn day() -> Self {
        Self {
            seconds: DurationUnit::Day.as_secs().unwrap_or_default() as i64,
            ..Default::default()
        }
    }
}
