use crate::variable::DynamicVariable;
pub(crate) use crate::vm::date::duration::Duration;
pub(crate) use crate::vm::date::duration_unit::DurationUnit;
use crate::Variable;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::any::Any;
use std::fmt::{Display, Formatter};

// Duration is a modified copy of `humantime`
mod duration;
mod duration_parser;
mod duration_unit;

#[derive(Debug, Clone)]
pub(crate) struct VmDate(pub Option<DateTime<Utc>>);

impl DynamicVariable for VmDate {
    fn type_name(&self) -> &'static str {
        "date"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn to_value(&self) -> Value {
        match self.0 {
            None => Value::String(String::from("Invalid date")),
            Some(d) => Value::String(d.to_rfc3339_opts(SecondsFormat::Secs, true)),
        }
    }
}

impl Display for VmDate {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            None => write!(f, "Invalid date"),
            Some(d) => write!(f, "{}", d.to_rfc3339_opts(SecondsFormat::Secs, true)),
        }
    }
}

impl From<Option<DateTime<Utc>>> for VmDate {
    fn from(value: Option<DateTime<Utc>>) -> Self {
        Self(value)
    }
}

impl VmDate {
    pub fn now() -> Self {
        Self(Some(Utc::now()))
    }

    pub fn yesterday() -> Self {
        Self::now().sub(Duration::day())
    }

    pub fn tomorrow() -> Self {
        Self::now().add(Duration::day())
    }

    /// Create a new VmDate from the current time
    pub fn new(var: Variable) -> Self {
        Self(helper::parse_date(var))
    }

    pub fn is_valid(&self) -> bool {
        self.0.is_some()
    }

    pub fn format(&self, format: Option<&str>) -> String {
        let Some(date_time) = &self.0 else {
            return self.to_string();
        };

        match format {
            None => date_time.to_string(),
            Some(fmt) => date_time.format(fmt).to_string(),
        }
    }

    pub fn add(&self, duration: Duration) -> Self {
        let Some(date_time) = &self.0 else {
            return Self(None);
        };

        Self(helper::add_duration(date_time.clone(), duration))
    }

    pub fn sub(&self, duration: Duration) -> Self {
        let Some(date_time) = &self.0 else {
            return Self(None);
        };

        Self(helper::add_duration(date_time.clone(), duration.negate()))
    }

    pub fn start_of(&self, unit: DurationUnit) -> Self {
        let Some(date_time) = &self.0 else {
            return Self(None);
        };

        Self(helper::start_of(date_time.clone(), unit))
    }

    pub fn end_of(&self, unit: DurationUnit) -> Self {
        let Some(date_time) = &self.0 else {
            return Self(None);
        };

        Self(helper::end_of(date_time.clone(), unit))
    }

    pub fn diff(&self, date_time: &Self, unit: Option<DurationUnit>) -> Option<i64> {
        let (dt1, dt2) = match (self.0.clone(), date_time.0) {
            (Some(a), Some(b)) => (a, b),
            _ => return None,
        };

        helper::diff(dt1, dt2, unit)
    }

    pub fn set(&self, value: u32, unit: DurationUnit) -> Self {
        let Some(date_time) = self.0.clone() else {
            return Self(None);
        };

        Self(helper::set(date_time, value, unit))
    }

    pub fn is_same(&self, other: &Self, unit: Option<DurationUnit>) -> bool {
        let (dt1, dt2) = match (self.0.clone(), other.0) {
            (Some(a), Some(b)) => (a, b),
            _ => return false,
        };

        helper::is_same(dt1, dt2, unit).unwrap_or(false)
    }

    pub fn is_before(&self, other: &Self, unit: Option<DurationUnit>) -> bool {
        let (dt1, dt2) = match (self.0.clone(), other.0) {
            (Some(a), Some(b)) => (a, b),
            _ => return false,
        };

        helper::is_before(dt1, dt2, unit).unwrap_or(false)
    }

    pub fn is_after(&self, other: &Self, unit: Option<DurationUnit>) -> bool {
        let (dt1, dt2) = match (self.0.clone(), other.0) {
            (Some(a), Some(b)) => (a, b),
            _ => return false,
        };

        helper::is_after(dt1, dt2, unit).unwrap_or(false)
    }

    pub fn is_same_or_before(&self, other: &Self, unit: Option<DurationUnit>) -> bool {
        self.is_before(other, unit) || self.is_same(other, unit)
    }

    pub fn is_same_or_after(&self, other: &Self, unit: Option<DurationUnit>) -> bool {
        self.is_after(other, unit) || self.is_same(other, unit)
    }
}

mod helper {
    use crate::vm::date::{Duration, DurationUnit};
    use crate::vm::VmDate;
    use crate::Variable;
    use chrono::{
        DateTime, Datelike, Days, LocalResult, Month, Months, NaiveDate, NaiveDateTime, TimeDelta,
        TimeZone, Timelike, Utc,
    };
    use rust_decimal::prelude::ToPrimitive;
    use std::ops::{Add, Deref};

    pub fn parse_date(var: Variable) -> Option<DateTime<Utc>> {
        match var {
            Variable::Null => Some(Utc::now()),
            Variable::Number(n) => {
                let n_i64 = n.to_i64()?;
                let date_time = match Utc.timestamp_millis_opt(n_i64) {
                    LocalResult::Single(date_time) => date_time,
                    LocalResult::Ambiguous(date_time, _) => date_time,
                    LocalResult::None => return None,
                };

                Some(date_time)
            }
            Variable::String(str) => DateTime::parse_from_rfc3339(str.deref())
                .ok()
                .map(|date_time| date_time.to_utc())
                .or_else(|| {
                    NaiveDateTime::parse_from_str(str.deref(), "%Y-%m-%d %H:%M:%S")
                        .ok()
                        .or_else(|| {
                            NaiveDateTime::parse_from_str(str.deref(), "%Y-%m-%d %H:%M").ok()
                        })
                        .or_else(|| {
                            NaiveDate::parse_from_str(str.deref(), "%Y-%m-%d")
                                .ok()?
                                .and_hms_opt(0, 0, 0)
                        })
                        .map(|dt| dt.and_utc())
                }),
            Variable::Dynamic(d) => match d.as_any().downcast_ref::<VmDate>() {
                Some(d) => d.0.clone(),
                None => None,
            },
            _ => None,
        }
    }

    pub fn add_duration(mut date_time: DateTime<Utc>, duration: Duration) -> Option<DateTime<Utc>> {
        date_time = date_time.add(TimeDelta::seconds(duration.seconds));
        date_time = match duration.months < 0 {
            true => date_time.checked_sub_months(Months::new(duration.months.unsigned_abs()))?,
            false => date_time.checked_add_months(Months::new(duration.months.unsigned_abs()))?,
        };

        date_time.with_year(date_time.year() + duration.years)
    }

    pub fn start_of(date_time: DateTime<Utc>, unit: DurationUnit) -> Option<DateTime<Utc>> {
        Some(match unit {
            DurationUnit::Second => date_time.with_nanosecond(0)?,
            DurationUnit::Minute => date_time.with_second(0)?.with_nanosecond(0)?,
            DurationUnit::Hour => date_time
                .with_minute(0)?
                .with_second(0)?
                .with_nanosecond(0)?,
            DurationUnit::Day => date_time
                .with_hour(0)?
                .with_minute(0)?
                .with_second(0)?
                .with_nanosecond(0)?,
            DurationUnit::Week => {
                let weekday = date_time.weekday().num_days_from_monday();

                date_time
                    .checked_sub_days(Days::new(weekday.to_u64()?))?
                    .with_hour(0)?
                    .with_minute(0)?
                    .with_second(0)?
                    .with_nanosecond(0)?
            }
            DurationUnit::Month => date_time
                .with_day0(0)?
                .with_hour(0)?
                .with_minute(0)?
                .with_second(0)?
                .with_nanosecond(0)?,
            DurationUnit::Quarter => date_time
                .with_month0((date_time.quarter() - 1) * 3)?
                .with_day0(0)?
                .with_hour(0)?
                .with_minute(0)?
                .with_second(0)?
                .with_nanosecond(0)?,
            DurationUnit::Year => date_time
                .with_month0(0)?
                .with_day0(0)?
                .with_hour(0)?
                .with_minute(0)?
                .with_second(0)?
                .with_nanosecond(0)?,
        })
    }

    pub fn end_of(mut date_time: DateTime<Utc>, unit: DurationUnit) -> Option<DateTime<Utc>> {
        date_time = date_time.with_nanosecond(999_999_999)?;

        Some(match unit {
            DurationUnit::Second => date_time,
            DurationUnit::Minute => date_time.with_second(59)?,
            DurationUnit::Hour => date_time.with_minute(59)?.with_second(59)?,
            DurationUnit::Day => date_time.with_hour(23)?.with_minute(59)?.with_second(59)?,
            DurationUnit::Week => {
                let weekday = date_time.weekday().num_days_from_sunday();

                date_time
                    .checked_add_days(Days::new(weekday.to_u64()?))?
                    .with_hour(23)?
                    .with_minute(59)?
                    .with_second(59)?
            }
            DurationUnit::Month => {
                let month = Month::try_from(date_time.month().to_u8()?).ok()?;
                let days_in_month = month.num_days(date_time.year())?.to_u32()?;

                date_time
                    .with_day(days_in_month)?
                    .with_hour(23)?
                    .with_minute(59)?
                    .with_second(59)?
            }
            DurationUnit::Quarter => {
                let new_month_index = date_time.quarter() * 3;
                let month = Month::try_from(new_month_index.to_u8()?).ok()?;
                let days_in_month = month.num_days(date_time.year())?.to_u32()?;

                date_time
                    .with_month(month.number_from_month())?
                    .with_day(days_in_month)?
                    .with_hour(23)?
                    .with_minute(59)?
                    .with_second(59)?
            }
            DurationUnit::Year => {
                let year = date_time.year();
                let month = Month::try_from(date_time.month().to_u8()?).ok()?;
                let days_in_month = month.num_days(year)?.to_u32()?;

                date_time
                    .with_month(12)?
                    .with_day(days_in_month)?
                    .with_hour(23)?
                    .with_minute(59)?
                    .with_second(59)?
            }
        })
    }

    pub fn diff(
        a: DateTime<Utc>,
        b: DateTime<Utc>,
        maybe_unit: Option<DurationUnit>,
    ) -> Option<i64> {
        let Some(unit) = maybe_unit else {
            return Some(a.timestamp_millis() - b.timestamp_millis());
        };

        if let Some(unit_secs) = unit.as_secs() {
            let secs = (a.timestamp_millis() - b.timestamp_millis()) / 1_000;
            return secs.checked_mul(unit_secs.to_i64()?);
        }

        let year_diff = a.year().to_i64()?.checked_sub(b.year().to_i64()?)?;

        match unit {
            DurationUnit::Month => {
                let month_diff = a.month().to_i64()?.checked_sub(b.month().to_i64()?)?;
                Some(year_diff * 12 + month_diff)
            }
            DurationUnit::Year => Some(year_diff),
            _ => None,
        }
    }

    pub fn set(date_time: DateTime<Utc>, value: u32, unit: DurationUnit) -> Option<DateTime<Utc>> {
        match unit {
            DurationUnit::Second => date_time.with_second(value),
            DurationUnit::Minute => date_time.with_minute(value),
            DurationUnit::Hour => date_time.with_hour(value),
            DurationUnit::Day => date_time.with_day(value),
            DurationUnit::Month => date_time.with_month(value),
            DurationUnit::Year => date_time.with_year(value.to_i32()?),
            // Noops
            DurationUnit::Week | DurationUnit::Quarter => Some(date_time),
        }
    }

    pub fn is_same(a: DateTime<Utc>, b: DateTime<Utc>, unit: Option<DurationUnit>) -> Option<bool> {
        match unit {
            Some(unit) => {
                let start_a = start_of(a, unit.clone())?;
                let end_a = end_of(a, unit.clone())?;

                Some(start_a <= b && b <= end_a)
            }
            None => Some(a.timestamp_millis() == b.timestamp_millis()),
        }
    }

    pub fn is_before(
        a: DateTime<Utc>,
        b: DateTime<Utc>,
        unit: Option<DurationUnit>,
    ) -> Option<bool> {
        match unit {
            Some(unit) => {
                let end_a = end_of(a, unit)?;
                Some(end_a < b)
            }
            None => Some(a < b),
        }
    }

    pub fn is_after(
        a: DateTime<Utc>,
        b: DateTime<Utc>,
        unit: Option<DurationUnit>,
    ) -> Option<bool> {
        match unit {
            Some(unit) => {
                let start_b = start_of(b, unit)?;
                Some(a > start_b)
            }
            None => Some(a > b),
        }
    }
}
