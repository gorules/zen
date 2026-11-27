use crate::context::EvaluationContext;
pub(crate) use crate::lane::date::duration::Duration;
pub(crate) use crate::lane::date::duration_unit::DurationUnit;
use crate::variable::DynamicVariable;
use crate::Variable;
use chrono::{DateTime, SecondsFormat};
use chrono_tz::Tz;
use serde_json::Value;
use std::any::Any;
use std::fmt::{Display, Formatter};
use std::rc::Rc;

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
    pub fn now(context: &EvaluationContext) -> Self {
        Self(Some(helper::now_tz(context, context.timezone())))
    }

    pub fn yesterday(context: &EvaluationContext) -> Self {
        Self::now(context).sub(Duration::day())
    }

    pub fn tomorrow(context: &EvaluationContext) -> Self {
        Self::now(context).add(Duration::day())
    }

    pub fn new(var: Variable, tz_opt: Option<Tz>, context: &EvaluationContext) -> Self {
        Self(helper::parse_date(var, tz_opt, context))
    }

    pub(crate) fn text(str: &str, tz_opt: Option<Tz>, context: &EvaluationContext) -> Self {
        Self(helper::parse_str(
            str,
            tz_opt.unwrap_or_else(|| context.timezone()),
            context,
        ))
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

#[derive(Clone, Copy)]
pub(crate) struct Remembered {
    zone: Option<Tz>,
    len: u8,
    text: [u8; Remembered::WIDTH],
    date: Option<DateTime<Tz>>,
}

impl Remembered {
    const WIDTH: usize = 36;
    const SLOTS: usize = 1 << 15;
    const EMPTY: Remembered = Remembered {
        zone: None,
        len: 0,
        text: [0; Remembered::WIDTH],
        date: None,
    };

    thread_local! {
        static SEEN: std::cell::RefCell<Vec<Remembered>> = const { std::cell::RefCell::new(Vec::new()) };
    }
}

pub(crate) struct Recall<'s>(&'s mut [Remembered]);

impl Recall<'_> {
    #[inline]
    pub(crate) fn date(&mut self, bytes: &[u8], tz: Tz) -> Option<Date> {
        if bytes.is_empty() || bytes.len() > Remembered::WIDTH {
            return std::str::from_utf8(bytes)
                .ok()
                .map(|text| Date::from_text(text, tz));
        }
        let mut hash = 0xCBF29CE484222325u64 ^ (tz as u64).wrapping_mul(0x9E3779B97F4A7C15);
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            hash = (hash ^ u64::from_le_bytes(word)).wrapping_mul(0x100000001B3);
            hash ^= hash >> 29;
        }
        let slot = &mut self.0[(hash as usize) & (Remembered::SLOTS - 1)];
        if slot.zone == Some(tz)
            && slot.len as usize == bytes.len()
            && slot.text[..bytes.len()] == *bytes
        {
            return Some(Date(slot.date));
        }
        let date = Date::from_text(std::str::from_utf8(bytes).ok()?, tz);
        slot.zone = Some(tz);
        slot.len = bytes.len() as u8;
        slot.text[..bytes.len()].copy_from_slice(bytes);
        slot.date = date.0;
        Some(date)
    }
}

mod helper {
    use crate::context::EvaluationContext;
    use crate::lane::date::{Duration, DurationUnit, DynamicVariableExt};
    use crate::Variable;
    use chrono::{
        DateTime, Datelike, Days, FixedOffset, LocalResult, Month, Months, NaiveDate,
        NaiveDateTime, Offset, TimeDelta, TimeZone, Timelike,
    };
    use chrono_tz::Tz;
    use rust_decimal::prelude::ToPrimitive;
    use std::ops::Deref;
    use std::str::FromStr;

    pub fn now_tz(context: &EvaluationContext, tz: Tz) -> DateTime<Tz> {
        context.now().with_timezone(&tz)
    }

    pub fn parse_date(
        var: Variable,
        tz_opt: Option<Tz>,
        context: &EvaluationContext,
    ) -> Option<DateTime<Tz>> {
        match var {
            Variable::Number(n) => {
                let tz = tz_opt.unwrap_or_else(|| context.timezone());
                let n_i64 = n.to_i64()?;
                let date_time = match tz.timestamp_millis_opt(n_i64) {
                    LocalResult::Single(date_time) => date_time,
                    LocalResult::Ambiguous(date_time, _) => date_time,
                    LocalResult::None => return None,
                };

                Some(date_time)
            }
            Variable::String(str) => parse_str(
                str.deref(),
                tz_opt.unwrap_or_else(|| context.timezone()),
                context,
            ),
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

    pub(crate) fn parse_str(
        str: &str,
        tz: Tz,
        context: &EvaluationContext,
    ) -> Option<DateTime<Tz>> {
        parse_text(str, tz).or_else(|| zone_now(str, context))
    }

    #[cold]
    #[inline(never)]
    fn zone_now(str: &str, context: &EvaluationContext) -> Option<DateTime<Tz>> {
        Tz::from_str(str).ok().map(|tz| now_tz(context, tz))
    }

    #[inline]
    pub(crate) fn parse_text(str: &str, tz: Tz) -> Option<DateTime<Tz>> {
        use crate::lane::date::helpers::IsoDate;
        match IsoDate::parse(str) {
            Some(naive) if IsoDate::utc(str) => Some(tz.from_utc_datetime(&naive)),
            Some(naive) => resolve_local(naive, tz),
            None => match IsoDate::zoneless(str) {
                Some(naive) => resolve_local(naive, tz),
                None => parse_slow(str, tz),
            },
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

    pub(crate) trait Wall: Datelike + Timelike + Sized {
        fn add_days(self, days: Days) -> Option<Self>;
        fn sub_days(self, days: Days) -> Option<Self>;
        fn plus(self, delta: TimeDelta) -> Option<Self>;
        fn add_months(self, months: Months) -> Option<Self>;
        fn sub_months(self, months: Months) -> Option<Self>;
        fn millis(&self) -> i64;
    }

    impl Wall for DateTime<Tz> {
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

        fn millis(&self) -> i64 {
            self.timestamp_millis()
        }
    }

    impl Wall for NaiveDateTime {
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

        fn millis(&self) -> i64 {
            self.and_utc().timestamp_millis()
        }
    }

    pub(crate) struct Calendar;

    impl Calendar {
        pub(crate) fn shift<T: Wall>(mut date_time: T, duration: Duration) -> Option<T> {
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

        pub(crate) fn start_of<T: Wall>(date_time: T, unit: DurationUnit) -> Option<T> {
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

        pub(crate) fn end_of<T: Wall>(mut date_time: T, unit: DurationUnit) -> Option<T> {
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
        match Calendar::fixed_utc(a.timezone()) && Calendar::fixed_utc(b.timezone()) {
            true => months_between(a.naive_utc(), b.naive_utc()),
            false => months_between(a, b),
        }
    }

    pub(crate) fn months_between<T: Wall + Copy>(a: T, b: T) -> f64 {
        if a.day() < b.day() {
            return -months_between(b, a);
        }

        let whole_month_diff = ((b.year() - a.year()) * 12) + (b.month() as i32 - a.month() as i32);
        let anchor = add_months_to_date(a, whole_month_diff).millis();
        let numerator = b.millis() - anchor;
        let c = numerator < 0;
        let anchor2 = add_months_to_date(a, whole_month_diff + if c { -1 } else { 1 }).millis();
        let denominator = if c {
            anchor - anchor2
        } else {
            anchor2 - anchor
        };

        let fractional = if denominator != 0 {
            numerator as f64 / denominator as f64
        } else {
            0.0
        };

        -((whole_month_diff as f64) + fractional)
    }

    fn add_months_to_date<T: Wall + Copy>(date: T, months: i32) -> T {
        if months >= 0 {
            date.add_months(Months::new(months as u32))
        } else {
            date.sub_months(Months::new((-months) as u32))
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

    pub(crate) fn coerce(v: &Variable, context: &EvaluationContext) -> Option<Date> {
        match v {
            Variable::Dynamic(d) => d.as_date(),
            Variable::String(text) => Some(Self::from_text(text, context.timezone())),
            _ => None,
        }
    }

    pub(crate) fn from_text(text: &str, tz: Tz) -> Date {
        Date(helper::parse_text(text, tz))
    }

    pub(crate) fn remembered(text: &str, tz: Tz) -> Date {
        Self::recalling(|recall| recall.date(text.as_bytes(), tz)).unwrap_or(Date(None))
    }

    pub(crate) fn recalling<R>(run: impl FnOnce(&mut Recall) -> R) -> R {
        Remembered::SEEN.with_borrow_mut(|seen| {
            if seen.is_empty() {
                seen.resize(Remembered::SLOTS, Remembered::EMPTY);
            }
            run(&mut Recall(seen))
        })
    }

    pub(crate) fn never(text: &str) -> bool {
        !Self::from_text(text, Tz::UTC).is_valid()
            && !Self::from_text(text, Tz::Etc__GMTMinus14).is_valid()
    }

    pub(crate) fn fixed(text: &str) -> Option<Date> {
        let utc = Self::from_text(text, Tz::UTC);
        let east = Self::from_text(text, Tz::Etc__GMTMinus14);
        (utc.is_valid() && utc == east).then_some(utc)
    }

    pub(crate) fn matches(&self, other: &Variable, context: &EvaluationContext) -> bool {
        self.0.is_some() && Self::coerce(other, context).is_some_and(|other| other == *self)
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
            let vm = Date::new(
                Variable::String(date.into()),
                None,
                &crate::context::EvaluationContext::default(),
            );
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
    use crate::lane::date::helpers::IsoDate;
    use chrono_tz::Tz;

    #[test]
    fn iso_fast_path_agrees_with_text_parsing() {
        let mut stamped = 0usize;
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
            assert_eq!(parse_text(&text, tz), parse_slow(&text, tz), "{text} {tz}");
            let digits = 1 + next(11) as usize;
            let fraction: String = (0..digits)
                .map(|_| char::from(b'0' + next(10) as u8))
                .collect();
            let fraction = match next(3) {
                0 => String::new(),
                1 => ".".to_string(),
                _ => format!(".{fraction}"),
            };
            let sign = if next(2) == 0 { '+' } else { '-' };
            let zone = match next(9) {
                0 => String::new(),
                1 => "Z".to_string(),
                2 => "z".to_string(),
                3 => format!("{sign}{:02}:{:02}", next(26), next(62)),
                4 => format!("{sign}{:02}{:02}", next(24), next(60)),
                5 => format!("{sign}{:02}", next(24)),
                6 => format!("{sign}{:02}:{:02}", next(15), 15 * next(4)),
                7 => "+00:00".to_string(),
                _ => "-00:00".to_string(),
            };
            let year = match next(40) {
                0 => 0,
                1 => 9999,
                _ => year,
            };
            let separator = match next(12) {
                0 => "t",
                1 => " ",
                _ => "T",
            };
            let text = format!(
                "{year:04}-{month:02}-{day:02}{separator}{h:02}:{m:02}:{sec:02}{fraction}{zone}"
            );
            assert_eq!(parse_text(&text, tz), parse_slow(&text, tz), "{text} {tz}");
            stamped += usize::from(IsoDate::zoneless(&text).is_some());
        }
        assert!(stamped > 2_000, "{stamped}");
    }
}

#[cfg(test)]
mod frozen_tests {
    use super::Date;
    use crate::vm::date::VmDate;
    use chrono_tz::Tz;

    fn texts() -> Vec<String> {
        let mut state = 0xA409_3822_299F_31D0u64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let mut out: Vec<String> = [
            "",
            " ",
            "now",
            "2024",
            "2024-02",
            "2024-02-30",
            "2024-02-29",
            "2023-02-29",
            "20240229",
            "2024/02/29",
            "2024/02/29 10:11",
            "2024/02/29 10:11:12.5",
            "20240229T101112",
            "20240229T1011",
            "2024-03-31T02:30:00",
            "2024-03-31 02:30:00",
            "2024-10-27 02:30:00",
            "2024-03-10 02:30:00",
            "2024-02-29T10:11:12.123456789Z",
            "2024-02-29T10:11:12+0530",
            "2024-02-29T10:11:12+05",
            "2024-02-29T10:11:12-05:30",
            "2024-02-29T10:11Z",
            "2024-02-29t10:11:12z",
            "2024-02-29T24:00:00Z",
            "2024-02-29T23:59:60Z",
            "0000-01-01",
            "9999-12-31",
            "9999-12-31T23:59:59Z",
            "10000-01-01",
            "-0001-01-01",
            "2024-1-5",
            "2024-01-05 ",
            " 2024-01-05",
            "2024-01-05T",
            "2024-01-05x",
            "Europe/Berlin",
            "12:30:00",
            "١٢٣٤-٠١-٠١",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for _ in 0..120_000 {
            let year = 1880 + next(300);
            let (month, day) = (next(14), next(33));
            let (h, m, sec) = (next(26), next(62), next(62));
            let milli = next(1000);
            out.push(match next(14) {
                0 => format!("{year:04}-{month:02}-{day:02}"),
                1 => format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{sec:02}"),
                2 => format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}Z"),
                3 => format!("{year:04}-03-{:02} 02:{m:02}:00", 25 + next(7)),
                4 => format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}+02:00"),
                5 => format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}"),
                6 => format!("{year:04}-{month}-{day}"),
                7 => format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}"),
                8 => format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}.{milli:03}Z"),
                9 => format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}.{milli:03}"),
                10 => format!("{year:04}{month:02}{day:02}"),
                11 => format!("{year:04}/{month:02}/{day:02}"),
                12 => format!(
                    "{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}.{milli:03}{:03}-0{}:30",
                    next(1000),
                    next(10)
                ),
                _ => format!("{year:04}-{month:02}"),
            });
        }
        out
    }

    #[test]
    fn column_parsing_agrees_with_the_row_engine() {
        let zones = [
            Tz::UTC,
            Tz::Europe__Berlin,
            Tz::America__New_York,
            Tz::Asia__Kolkata,
            Tz::Pacific__Apia,
            Tz::Etc__GMTMinus14,
        ];
        let mut valid = 0usize;
        for (at, text) in texts().iter().enumerate() {
            let tz = zones[at % zones.len()];
            let frozen = VmDate::from_text_in(text, tz).and_then(|date| date.0);
            assert_eq!(Date::from_text(text, tz).0, frozen, "{text} {tz}");
            assert_eq!(
                crate::DateValue::reads(text),
                crate::DateValue::is_text(text),
                "{text}"
            );
            valid += usize::from(frozen.is_some());
        }
        assert!(valid > 40_000, "{valid}");
    }
}

#[cfg(test)]
mod calendar_tests {
    use super::helper::{add_duration, end_of, months_between, start_of, Calendar};
    use super::DurationUnit;
    use chrono::{TimeZone, Utc};
    use chrono_tz::Tz;

    #[test]
    fn remembered_dates_equal_parsed_ones() {
        let zones = [
            Tz::UTC,
            Tz::America__New_York,
            Tz::Europe__Berlin,
            Tz::Australia__Lord_Howe,
        ];
        let mut texts: Vec<String> = vec![
            "".into(),
            "nope".into(),
            "2024-03-10T02:30:00".into(),
            "2024-11-03T01:30:00".into(),
            "2025-03-20T10:15:30+05:00".into(),
            "2024-03-31 02:30".into(),
            "2023-12-31T23:30:00Z".into(),
            "2024-02-30".into(),
            "20240229".into(),
            "2024-02-29T12:00:00.123456789+13:45 and far too long to be kept".into(),
        ];
        for day in 0..40_000u32 {
            texts.push(format!(
                "{:04}-{:02}-{:02}",
                1990 + day / 372,
                1 + day / 31 % 12,
                1 + day % 31
            ));
        }
        for round in 0..2 {
            for (at, text) in texts.iter().enumerate() {
                let tz = zones[(at + round) % zones.len()];
                let (kept, parsed) = (
                    super::Date::remembered(text, tz),
                    super::Date::from_text(text, tz),
                );
                assert_eq!(kept.0, parsed.0, "{text} in {tz}");
                assert_eq!(
                    kept.0.map(|at| at.timezone()),
                    parsed.0.map(|at| at.timezone()),
                    "{text} in {tz}"
                );
                assert_eq!(
                    super::Date::remembered(text, tz).0,
                    parsed.0,
                    "{text} in {tz} again"
                );
            }
        }
    }

    #[test]
    fn fixed_utc_month_differences_match_zoned_ones() {
        let mut state = 0x243F6A8885A308D3u64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let mut pairs = 0;
        while pairs < 200_000 {
            let seconds = next(8_000_000_000) as i64 - 2_000_000_000;
            let apart = match next(4) {
                0 => next(5_000_000) as i64 - 2_500_000,
                1 => (next(60) as i64 - 30) * 86_400 * (28 + next(4) as i64),
                _ => next(4_000_000_000) as i64 - 2_000_000_000,
            };
            let (Some(a), Some(b)) = (
                Utc.timestamp_opt(seconds, next(1_000) as u32 * 1_000_000)
                    .single(),
                Utc.timestamp_opt(seconds + apart, next(1_000) as u32 * 1_000_000)
                    .single(),
            ) else {
                continue;
            };
            pairs += 1;
            for tz in [Tz::UTC, Tz::Etc__UTC, Tz::GMT] {
                let (a, b) = (a.with_timezone(&tz), b.with_timezone(&tz));
                let zoned = months_between(a, b);
                let naive = months_between(a.naive_utc(), b.naive_utc());
                assert_eq!(zoned.to_bits(), naive.to_bits(), "{a} {b}");
            }
        }
    }

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
