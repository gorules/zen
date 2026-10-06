pub(crate) use crate::lane::date::duration::Duration;
pub(crate) use crate::lane::date::duration_unit::DurationUnit;
use crate::variable::DynamicVariable;
use crate::Variable;
use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;
use serde_json::Value;
use std::any::Any;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::OnceLock;

mod duration;
mod duration_parser;
mod duration_unit;
pub(crate) mod helpers;

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Ord, Eq)]
pub(crate) struct Date(pub Option<DateTime<Tz>>);

impl DynamicVariable for Date {
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

impl Display for Date {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            None => write!(f, "Invalid date"),
            Some(d) => write!(f, "{}", d.to_rfc3339_opts(SecondsFormat::Secs, true)),
        }
    }
}

impl From<Option<DateTime<Tz>>> for Date {
    fn from(value: Option<DateTime<Tz>>) -> Self {
        Self(value)
    }
}

struct Formats;

impl Formats {
    const CAPACITY: usize = 256;

    thread_local! {
        static CACHE: std::cell::RefCell<
            ahash::HashMap<String, Option<Vec<chrono::format::Item<'static>>>>,
        > = Default::default();
    }

    fn with<T>(format: &str, f: impl FnOnce(Option<&[chrono::format::Item<'static>]>) -> T) -> T {
        Self::CACHE.with_borrow_mut(|cache| {
            if !cache.contains_key(format) {
                if cache.len() >= Self::CAPACITY {
                    cache.clear();
                }
                let items = chrono::format::StrftimeItems::new(format)
                    .parse_to_owned()
                    .ok();
                cache.insert(format.to_string(), items);
            }
            f(cache.get(format).and_then(|items| items.as_deref()))
        })
    }
}

impl Date {
    pub fn now() -> Self {
        Self(Some(helper::now()))
    }

    pub fn yesterday() -> Self {
        Self::now().sub(Duration::day())
    }

    pub fn tomorrow() -> Self {
        Self::now().add(Duration::day())
    }

    pub fn new(var: Variable, tz_opt: Option<Tz>) -> Self {
        Self(helper::parse_date(var, tz_opt))
    }

    pub(crate) fn text(str: &str, tz_opt: Option<Tz>) -> Self {
        Self(helper::parse_str(str, tz_opt.unwrap_or_else(helper::tz)))
    }

    pub fn is_valid(&self) -> bool {
        self.0.is_some()
    }

    pub fn tz(&self, timezone: Tz) -> Self {
        let Some(date_time) = &self.0 else {
            return *self;
        };

        Self(Some(date_time.with_timezone(&timezone)))
    }

    pub fn format(&self, format: Option<&str>) -> String {
        let Some(date_time) = &self.0 else {
            return self.to_string();
        };

        match format {
            None => date_time.to_string(),
            Some(fmt) => Formats::with(fmt, |items| {
                let mut out = String::with_capacity(32);
                let written = items.map(|items| {
                    std::fmt::Write::write_fmt(
                        &mut out,
                        format_args!("{}", date_time.format_with_items(items.iter())),
                    )
                });
                match written {
                    Some(Ok(())) => out,
                    _ => date_time.format(fmt).to_string(),
                }
            }),
        }
    }

    pub fn add(&self, duration: Duration) -> Self {
        let Some(date_time) = &self.0 else {
            return Self(None);
        };

        Self(helper::add_duration(*date_time, duration))
    }

    pub fn sub(&self, duration: Duration) -> Self {
        let Some(date_time) = &self.0 else {
            return Self(None);
        };

        Self(helper::add_duration(*date_time, duration.negate()))
    }

    pub fn start_of(&self, unit: DurationUnit) -> Self {
        let Some(date_time) = &self.0 else {
            return Self(None);
        };

        Self(helper::start_of(*date_time, unit))
    }

    pub fn end_of(&self, unit: DurationUnit) -> Self {
        let Some(date_time) = &self.0 else {
            return Self(None);
        };

        Self(helper::end_of(*date_time, unit))
    }

    pub fn diff(&self, date_time: &Self, unit: Option<DurationUnit>) -> Option<i64> {
        let (dt1, dt2) = match (self.0, date_time.0) {
            (Some(a), Some(b)) => (a, b),
            _ => return None,
        };

        helper::diff(dt1, dt2, unit)
    }

    pub fn set(&self, value: u32, unit: DurationUnit) -> Self {
        let Some(date_time) = self.0 else {
            return Self(None);
        };

        Self(helper::set(date_time, value, unit))
    }

    pub fn is_same(&self, other: &Self, unit: Option<DurationUnit>) -> bool {
        let (dt1, dt2) = match (self.0, other.0) {
            (Some(a), Some(b)) => (a, b),
            _ => return false,
        };

        helper::is_same(dt1, dt2, unit).unwrap_or(false)
    }

    pub fn is_before(&self, other: &Self, unit: Option<DurationUnit>) -> bool {
        let (dt1, dt2) = match (self.0, other.0) {
            (Some(a), Some(b)) => (a, b),
            _ => return false,
        };

        helper::is_before(dt1, dt2, unit).unwrap_or(false)
    }

    pub fn is_after(&self, other: &Self, unit: Option<DurationUnit>) -> bool {
        let (dt1, dt2) = match (self.0, other.0) {
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
    use crate::lane::date::{utc_now, Duration, DurationUnit, DynamicVariableExt};
    use crate::Variable;
    use chrono::{
        DateTime, Datelike, Days, FixedOffset, LocalResult, Month, Months, NaiveDate,
        NaiveDateTime, Offset, TimeDelta, TimeZone, Timelike,
    };
    use chrono_tz::Tz;
    use rust_decimal::prelude::ToPrimitive;
    use std::ops::Deref;
    use std::str::FromStr;
    use std::sync::OnceLock;

    pub(super) fn tz() -> Tz {
        static CACHED_TZ: OnceLock<Tz> = OnceLock::new();

        *CACHED_TZ.get_or_init(|| {
            iana_time_zone::get_timezone()
                .ok()
                .and_then(|tz| Tz::from_str(&tz).ok())
                .unwrap_or(Tz::UTC)
        })
    }

    pub fn now() -> DateTime<Tz> {
        now_tz(tz())
    }

    pub fn now_tz(tz: Tz) -> DateTime<Tz> {
        utc_now().with_timezone(&tz)
    }

    pub fn parse_date(var: Variable, tz_opt: Option<Tz>) -> Option<DateTime<Tz>> {
        match var {
            Variable::Number(n) => {
                let tz = tz_opt.unwrap_or_else(tz);
                let n_i64 = n.to_i64()?;
                let date_time = match tz.timestamp_millis_opt(n_i64) {
                    LocalResult::Single(date_time) => date_time,
                    LocalResult::Ambiguous(date_time, _) => date_time,
                    LocalResult::None => return None,
                };

                Some(date_time)
            }
            Variable::String(str) => parse_str(str.deref(), tz_opt.unwrap_or_else(tz)),
            Variable::Dynamic(d) => {
                let date = d.as_date()?;
                let source = d
                    .as_any()
                    .downcast_ref::<crate::vm::VmDate>()
                    .and_then(|v| v.source());
                match (tz_opt, source) {
                    (Some(tz), Some(text)) => parse_text(text, tz),
                    (Some(tz), None) => date.0.map(|date_time| date_time.with_timezone(&tz)),
                    (None, _) => date.0,
                }
            }
            _ => None,
        }
    }

    pub(crate) fn parse_str(str: &str, tz: Tz) -> Option<DateTime<Tz>> {
        parse_text(str, tz).or_else(|| zone_now(str))
    }

    #[cold]
    #[inline(never)]
    fn zone_now(str: &str) -> Option<DateTime<Tz>> {
        Tz::from_str(str).ok().map(now_tz)
    }

    pub(crate) fn from_text(str: &str) -> Option<DateTime<Tz>> {
        parse_text(str, tz())
    }

    #[inline]
    pub(crate) fn parse_text(str: &str, tz: Tz) -> Option<DateTime<Tz>> {
        use crate::lane::date::helpers::IsoDate;
        match IsoDate::parse(str) {
            Some(naive) if IsoDate::utc(str) => Some(tz.from_utc_datetime(&naive)),
            Some(naive) => resolve_local(naive, tz),
            None => parse_slow(str, tz),
        }
    }

    const LENIENT: [&str; 2] = ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"];

    const SHAPED: [(&str, &str); 11] = [
        ("9999-99-99T99:99:99", "%Y-%m-%dT%H:%M:%S%.f"),
        ("9999-99-99T99:99", "%Y-%m-%dT%H:%M"),
        ("9999-99-99 99:99:99", "%Y-%m-%d %H:%M:%S%.f"),
        ("99999999T999999", "%Y%m%dT%H%M%S%.f"),
        ("99999999T9999", "%Y%m%dT%H%M"),
        ("9999/99/99 99:99:99", "%Y/%m/%d %H:%M:%S%.f"),
        ("9999/99/99 99:99", "%Y/%m/%d %H:%M"),
        ("99999999", "%Y%m%d"),
        ("9999/99/99", "%Y/%m/%d"),
        ("9999-99", "%Y-%m"),
        ("9999", "%Y"),
    ];

    fn shape(value: &str) -> String {
        let seconds = value
            .rfind('.')
            .filter(|&dot| {
                dot + 1 < value.len() && value[dot + 1..].bytes().all(|b| b.is_ascii_digit())
            })
            .map_or(value, |dot| &value[..dot]);
        seconds
            .chars()
            .map(|c| if c.is_ascii_digit() { '9' } else { c })
            .collect()
    }

    fn parse_shaped(value: &str) -> Option<NaiveDateTime> {
        let shape = shape(value);
        let (_, format) = SHAPED.iter().find(|(pattern, _)| *pattern == shape)?;
        match *format {
            "%Y" => NaiveDate::from_ymd_opt(value.parse().ok()?, 1, 1)?.and_hms_opt(0, 0, 0),
            "%Y-%m" => NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0),
            format if !format.contains("%H") => NaiveDate::parse_from_str(value, format)
                .ok()?
                .and_hms_opt(0, 0, 0),
            format => NaiveDateTime::parse_from_str(value, format).ok(),
        }
    }

    fn split_offset(value: &str) -> Option<(&str, FixedOffset)> {
        if let Some(local) = value.strip_suffix('Z') {
            return Some((local, FixedOffset::east_opt(0)?));
        }
        let time = value.find('T')?;
        let sign_at = value[time..].rfind(['+', '-'])? + time;
        let digits: String = value[sign_at + 1..].chars().filter(|c| *c != ':').collect();
        let valid = matches!(value.len() - sign_at - 1, 2 | 4 | 5)
            && matches!(digits.len(), 2 | 4)
            && digits.bytes().all(|b| b.is_ascii_digit());
        if !valid {
            return None;
        }
        let hours: i32 = digits[..2].parse().ok()?;
        let minutes: i32 = match &digits[2..] {
            "" => 0,
            minutes => minutes.parse().ok()?,
        };
        let sign = if value.as_bytes()[sign_at] == b'-' {
            -1
        } else {
            1
        };
        let seconds = sign * (hours * 3600 + minutes * 60);
        Some((&value[..sign_at], FixedOffset::east_opt(seconds)?))
    }

    #[inline]
    pub(crate) fn resolve_local(naive: NaiveDateTime, tz: Tz) -> Option<DateTime<Tz>> {
        tz.from_local_datetime(&naive)
            .earliest()
            .or_else(|| skip_gap(naive, tz))
    }

    #[cold]
    #[inline(never)]
    fn skip_gap(naive: NaiveDateTime, tz: Tz) -> Option<DateTime<Tz>> {
        let before = tz
            .from_local_datetime(&naive.checked_sub_signed(TimeDelta::hours(3))?)
            .earliest()?;
        Some(tz.from_utc_datetime(&naive.checked_sub_offset(before.offset().fix())?))
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn parse_slow(value: &str, tz: Tz) -> Option<DateTime<Tz>> {
        if let Ok(date_time) = DateTime::parse_from_rfc3339(value) {
            return Some(date_time.with_timezone(&tz));
        }
        if let Some(naive) = LENIENT
            .iter()
            .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
            .or_else(|| {
                NaiveDate::parse_from_str(value, "%Y-%m-%d")
                    .ok()?
                    .and_hms_opt(0, 0, 0)
            })
        {
            return resolve_local(naive, tz);
        }
        match split_offset(value) {
            Some((local, offset)) if local.contains('T') => {
                let naive = parse_shaped(local)?;
                let utc = naive.checked_sub_offset(offset)?;
                (utc.year().abs() <= 9999).then(|| tz.from_utc_datetime(&utc))
            }
            _ => {
                let naive = parse_shaped(value)?;
                (naive.year().abs() <= 9999).then(|| resolve_local(naive, tz))?
            }
        }
    }

    pub fn add_duration(date_time: DateTime<Tz>, duration: Duration) -> Option<DateTime<Tz>> {
        let tz = date_time.timezone();
        match Calendar::fixed_utc(tz) {
            true => Some(tz.from_utc_datetime(&Calendar::shift(date_time.naive_utc(), duration)?)),
            false => Calendar::shift(date_time, duration),
        }
    }

    pub fn start_of(date_time: DateTime<Tz>, unit: DurationUnit) -> Option<DateTime<Tz>> {
        let tz = date_time.timezone();
        match Calendar::fixed_utc(tz) {
            true => Some(tz.from_utc_datetime(&Calendar::start_of(date_time.naive_utc(), unit)?)),
            false => Calendar::start_of(date_time, unit),
        }
    }

    pub fn end_of(date_time: DateTime<Tz>, unit: DurationUnit) -> Option<DateTime<Tz>> {
        let tz = date_time.timezone();
        match Calendar::fixed_utc(tz) {
            true => Some(tz.from_utc_datetime(&Calendar::end_of(date_time.naive_utc(), unit)?)),
            false => Calendar::end_of(date_time, unit),
        }
    }

    pub(crate) trait Clock: Datelike + Timelike + Sized {
        fn add_days(self, days: Days) -> Option<Self>;
        fn sub_days(self, days: Days) -> Option<Self>;
        fn plus(self, delta: TimeDelta) -> Option<Self>;
        fn add_months(self, months: Months) -> Option<Self>;
        fn sub_months(self, months: Months) -> Option<Self>;
    }

    impl Clock for DateTime<Tz> {
        fn add_days(self, days: Days) -> Option<Self> {
            self.checked_add_days(days)
        }

        fn sub_days(self, days: Days) -> Option<Self> {
            self.checked_sub_days(days)
        }

        fn plus(self, delta: TimeDelta) -> Option<Self> {
            self.checked_add_signed(delta)
        }

        fn add_months(self, months: Months) -> Option<Self> {
            self.checked_add_months(months)
        }

        fn sub_months(self, months: Months) -> Option<Self> {
            self.checked_sub_months(months)
        }
    }

    impl Clock for NaiveDateTime {
        fn add_days(self, days: Days) -> Option<Self> {
            self.checked_add_days(days)
        }

        fn sub_days(self, days: Days) -> Option<Self> {
            self.checked_sub_days(days)
        }

        fn plus(self, delta: TimeDelta) -> Option<Self> {
            self.checked_add_signed(delta)
        }

        fn add_months(self, months: Months) -> Option<Self> {
            self.checked_add_months(months)
        }

        fn sub_months(self, months: Months) -> Option<Self> {
            self.checked_sub_months(months)
        }
    }

    pub(crate) struct Calendar;

    impl Calendar {
        pub(crate) fn shift<T: Clock>(mut date_time: T, duration: Duration) -> Option<T> {
            date_time = date_time.plus(TimeDelta::try_seconds(duration.seconds)?)?;
            date_time = match duration.months < 0 {
                true => date_time.sub_months(Months::new(duration.months.unsigned_abs()))?,
                false => date_time.add_months(Months::new(duration.months.unsigned_abs()))?,
            };
            date_time.with_year(date_time.year().checked_add(duration.years)?)
        }

        pub(crate) fn fixed_utc(tz: Tz) -> bool {
            matches!(
                tz,
                Tz::UTC
                    | Tz::Etc__UTC
                    | Tz::Etc__GMT
                    | Tz::GMT
                    | Tz::Etc__Universal
                    | Tz::Universal
                    | Tz::Zulu
                    | Tz::Etc__Zulu
                    | Tz::UCT
                    | Tz::Etc__UCT
            )
        }

        pub(crate) fn start_of<T: Clock>(date_time: T, unit: DurationUnit) -> Option<T> {
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
                        .sub_days(Days::new(weekday.to_u64()?))?
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

        pub(crate) fn end_of<T: Clock>(mut date_time: T, unit: DurationUnit) -> Option<T> {
            date_time = date_time.with_nanosecond(999_999_999)?;

            Some(match unit {
                DurationUnit::Second => date_time,
                DurationUnit::Minute => date_time.with_second(59)?,
                DurationUnit::Hour => date_time.with_minute(59)?.with_second(59)?,
                DurationUnit::Day => date_time.with_hour(23)?.with_minute(59)?.with_second(59)?,
                DurationUnit::Week => {
                    let days_until_sunday = 6 - date_time.weekday().num_days_from_monday();

                    date_time
                        .add_days(Days::new(days_until_sunday.to_u64()?))?
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
                    let month = Month::December;
                    let days_in_month = month.num_days(year)?.to_u32()?;

                    date_time
                        .with_month(month.number_from_month())?
                        .with_day(days_in_month)?
                        .with_hour(23)?
                        .with_minute(59)?
                        .with_second(59)?
                }
            })
        }
    }

    pub fn diff(a: DateTime<Tz>, b: DateTime<Tz>, maybe_unit: Option<DurationUnit>) -> Option<i64> {
        let zone_delta = (b.offset().fix().local_minus_utc() as i64
            - a.offset().fix().local_minus_utc() as i64)
            * 1000;

        let diff_ms = a.timestamp_millis() - b.timestamp_millis();
        let Some(unit) = maybe_unit else {
            return Some(diff_ms);
        };

        let result = match unit {
            DurationUnit::Year => month_diff(a, b) / 12.0,
            DurationUnit::Month => month_diff(a, b),
            DurationUnit::Quarter => month_diff(a, b) / 3.0,
            DurationUnit::Week => {
                (diff_ms - zone_delta) as f64 / DurationUnit::Week.as_millis().unwrap_or_default()
            }
            DurationUnit::Day => {
                (diff_ms - zone_delta) as f64 / DurationUnit::Day.as_millis().unwrap_or_default()
            }
            DurationUnit::Hour => {
                diff_ms as f64 / DurationUnit::Hour.as_millis().unwrap_or_default()
            }
            DurationUnit::Minute => {
                diff_ms as f64 / DurationUnit::Minute.as_millis().unwrap_or_default()
            }
            DurationUnit::Second => {
                diff_ms as f64 / DurationUnit::Second.as_millis().unwrap_or_default()
            }
        };

        Some(if result < 0.0 {
            result.ceil() as i64
        } else {
            result.floor() as i64
        })
    }

    pub fn set(date_time: DateTime<Tz>, value: u32, unit: DurationUnit) -> Option<DateTime<Tz>> {
        match unit {
            DurationUnit::Second => date_time.with_second(value),
            DurationUnit::Minute => date_time.with_minute(value),
            DurationUnit::Hour => date_time.with_hour(value),
            DurationUnit::Day => date_time.with_day(value),
            DurationUnit::Month => date_time.with_month(value),
            DurationUnit::Year => date_time.with_year(value.to_i32()?),
            DurationUnit::Week | DurationUnit::Quarter => Some(date_time),
        }
    }

    pub fn is_same(a: DateTime<Tz>, b: DateTime<Tz>, unit: Option<DurationUnit>) -> Option<bool> {
        match unit {
            Some(unit) => {
                let start_a = start_of(a, unit)?;
                let end_a = end_of(a, unit)?;

                Some(start_a <= b && b <= end_a)
            }
            None => Some(a.timestamp_millis() == b.timestamp_millis()),
        }
    }

    pub fn is_before(a: DateTime<Tz>, b: DateTime<Tz>, unit: Option<DurationUnit>) -> Option<bool> {
        match unit {
            Some(unit) => {
                let end_a = end_of(a, unit)?;
                Some(end_a < b)
            }
            None => Some(a < b),
        }
    }

    pub fn is_after(a: DateTime<Tz>, b: DateTime<Tz>, unit: Option<DurationUnit>) -> Option<bool> {
        match unit {
            Some(unit) => {
                let start_a = start_of(a, unit)?;
                Some(b < start_a)
            }
            None => Some(a > b),
        }
    }

    fn month_diff(a: DateTime<Tz>, b: DateTime<Tz>) -> f64 {
        if a.day() < b.day() {
            return -month_diff(b, a);
        }

        let whole_month_diff = ((b.year() - a.year()) * 12) + (b.month() as i32 - a.month() as i32);
        let anchor = add_months_to_date(a, whole_month_diff);
        let c = (b.timestamp_millis() - anchor.timestamp_millis()) < 0;
        let anchor2 = add_months_to_date(a, whole_month_diff + if c { -1 } else { 1 });

        let numerator = b.timestamp_millis() - anchor.timestamp_millis();
        let denominator = if c {
            anchor.timestamp_millis() - anchor2.timestamp_millis()
        } else {
            anchor2.timestamp_millis() - anchor.timestamp_millis()
        };

        let fractional = if denominator != 0 {
            numerator as f64 / denominator as f64
        } else {
            0.0
        };

        -((whole_month_diff as f64) + fractional)
    }

    fn add_months_to_date(date: DateTime<Tz>, months: i32) -> DateTime<Tz> {
        if months >= 0 {
            date.checked_add_months(Months::new(months as u32))
        } else {
            date.checked_sub_months(Months::new((-months) as u32))
        }
        .unwrap_or(date)
    }
}

pub(crate) trait DynamicVariableExt {
    fn as_date(&self) -> Option<Date>;
}

impl DynamicVariableExt for dyn DynamicVariable {
    fn as_date(&self) -> Option<Date> {
        let any = self.as_any();
        any.downcast_ref::<Date>()
            .copied()
            .or_else(|| any.downcast_ref::<crate::vm::VmDate>().map(|d| Date(d.0)))
    }
}

impl Date {
    pub(crate) fn of(v: &Variable) -> Option<Date> {
        match v {
            Variable::Dynamic(d) => d.as_date(),
            _ => None,
        }
    }

    pub(crate) fn variable(self) -> Variable {
        Variable::Dynamic(Rc::new(crate::vm::VmDate::from(self.0)))
    }

    pub(crate) fn coerce(v: &Variable) -> Option<Date> {
        match v {
            Variable::Dynamic(d) => d.as_date(),
            Variable::String(text) => Some(Self::from_text(text)),
            _ => None,
        }
    }

    pub(crate) fn parses(text: &str) -> bool {
        helper::parse_text(text, helper::tz()).is_some()
    }

    pub(crate) fn from_text(text: &str) -> Date {
        Date(helper::from_text(text))
    }

    pub(crate) fn matches(&self, other: &Variable) -> bool {
        self.0.is_some() && Self::coerce(other).is_some_and(|other| other == *self)
    }

    pub(crate) fn rendered(&self) -> Option<String> {
        self.0
            .map(|date_time| date_time.to_rfc3339_opts(SecondsFormat::Secs, true))
    }

    pub(crate) fn source(v: &Variable) -> Option<&str> {
        match v {
            Variable::Dynamic(d) => d.as_any().downcast_ref::<crate::vm::VmDate>()?.source(),
            _ => None,
        }
    }

    pub(crate) fn sourced(v: &Variable) -> bool {
        Self::source(v).is_some()
    }

    #[inline]
    pub(crate) fn textual(v: Variable) -> Variable {
        match &v {
            Variable::Dynamic(d) => match d.as_text() {
                Some(text) => Variable::String(text.into()),
                None => v,
            },
            _ => v,
        }
    }
}

pub(crate) fn utc_now() -> DateTime<Utc> {
    static CURRENT_DATE_VALUE: OnceLock<Option<DateTime<Utc>>> = OnceLock::new();

    CURRENT_DATE_VALUE
        .get_or_init(|| match std::env::var("__ZEN_MOCK_UTC_TIME") {
            Ok(v) => v.parse::<DateTime<Utc>>().ok(),
            Err(_) => None,
        })
        .unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod format_tests {
    use super::Date;
    use crate::Variable;

    #[test]
    fn cached_formats_match_chrono() {
        let formats = [
            "%Y-%m-%d",
            "%d/%m/%Y %H:%M:%S",
            "%A, %B %e %Y",
            "%j %U %W %u",
            "%s",
            "%+",
            "%.3f %Z %z",
            "YYYY-MM-DD",
            "%%",
            "",
            "%Y%m%d%H%M%S",
            "%-d.%-m.%y",
        ];
        let dates = [
            "2025-03-15T10:30:00Z",
            "1999-12-31 23:59:59",
            "2024-02-29",
            "2025-10-26 02:30:00",
        ];
        for date in dates {
            let vm = Date::new(Variable::String(date.into()), None);
            let Some(dt) = vm.0 else { continue };
            for format in formats {
                for _ in 0..2 {
                    assert_eq!(
                        vm.format(Some(format)),
                        dt.format(format).to_string(),
                        "{date} {format}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod parse_tests {
    use super::helper::{parse_slow, parse_text};
    use chrono_tz::Tz;

    #[test]
    fn iso_fast_path_agrees_with_text_parsing() {
        let mut state = 0x853C49E6748FEA9Bu64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let zones = [
            Tz::UTC,
            Tz::Europe__Berlin,
            Tz::America__New_York,
            Tz::Asia__Kolkata,
        ];
        for _ in 0..200_000 {
            let year = 1900 + next(250);
            let (month, day) = (next(14), next(33));
            let (h, m, sec) = (next(26), next(62), next(62));
            let text = match next(7) {
                0 => format!("{year:04}-{month:02}-{day:02}"),
                1 => format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{sec:02}"),
                2 => format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}Z"),
                3 => format!("{year:04}-03-{:02} 02:{m:02}:00", 25 + next(7)),
                4 => format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}+02:00"),
                5 => format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}"),
                _ => format!("{year:04}-{month}-{day}"),
            };
            let tz = zones[next(zones.len() as u64) as usize];
            assert_eq!(
                parse_text(&text, tz),
                parse_slow(&text, tz),
                "{text} {tz}"
            );
        }
    }
}

#[cfg(test)]
mod calendar_tests {
    use super::helper::{add_duration, end_of, start_of, Calendar};
    use super::DurationUnit;
    use chrono::{TimeZone, Utc};
    use chrono_tz::Tz;

    #[test]
    fn fixed_utc_shortcut_matches_chained_steps() {
        let units = [
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::Hour,
            DurationUnit::Day,
            DurationUnit::Week,
            DurationUnit::Month,
            DurationUnit::Quarter,
            DurationUnit::Year,
        ];
        let mut state = 0x6A09E667F3BCC909u64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        for _ in 0..50_000 {
            let seconds = next(8_000_000_000) as i64 - 2_000_000_000;
            let nanos = next(1_000_000_000) as u32;
            let Some(instant) = Utc.timestamp_opt(seconds, nanos).single() else {
                continue;
            };
            for tz in [Tz::UTC, Tz::Etc__UTC, Tz::GMT] {
                let date_time = instant.with_timezone(&tz);
                for unit in units {
                    assert_eq!(
                        start_of(date_time, unit),
                        Calendar::start_of(date_time, unit),
                        "{date_time} {unit:?}"
                    );
                    assert_eq!(
                        end_of(date_time, unit),
                        Calendar::end_of(date_time, unit),
                        "{date_time} {unit:?}"
                    );
                    let amount = rust_decimal::Decimal::from(next(80) as i64 - 40);
                    if let Some(duration) = super::Duration::from_unit(amount, unit) {
                        assert_eq!(
                            add_duration(date_time, duration.clone()),
                            Calendar::shift(date_time, duration),
                            "{date_time} {amount} {unit:?}"
                        );
                    }
                }
            }
        }
    }
}

