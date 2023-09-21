use chrono::{
    DateTime, Datelike, Days, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc, Weekday,
};
use once_cell::sync::Lazy;

use crate::vm::VMError;

#[allow(clippy::unwrap_used)]
static ZERO_TIME: Lazy<NaiveTime> = Lazy::new(|| NaiveTime::from_hms_opt(0, 0, 0).unwrap());

static DATE_TIME: &str = "%Y-%m-%d %H:%M:%S";
static DATE: &str = "%Y-%m-%d";
static TIME_HMS: &str = "%H:%M:%S";
static TIME_HM: &str = "%H:%M";
static TIME_H: &str = "%H";

pub(crate) fn date_time(str: &str) -> Result<NaiveDateTime, VMError> {
    if str == "now" {
        return Ok(Utc::now().naive_utc());
    }

    let zero_time = ZERO_TIME.to_owned();

    NaiveDateTime::parse_from_str(str, DATE_TIME)
        .or(NaiveDate::parse_from_str(str, DATE).map(|c| c.and_time(zero_time)))
        .or(DateTime::parse_from_rfc3339(str).map(|dt| dt.naive_utc()))
        .map_err(|_| VMError::ParseDateTimeErr {
            timestamp: str.to_string(),
        })
}

pub(crate) fn time(str: &str) -> Result<NaiveTime, VMError> {
    let now = Utc::now();

    if str == "now" {
        return Ok(now.naive_utc().time());
    }

    return NaiveTime::parse_from_str(str, DATE_TIME)
        .or(NaiveTime::parse_from_str(str, TIME_HMS))
        .or(NaiveTime::parse_from_str(str, TIME_HM))
        .or(NaiveTime::parse_from_str(str, TIME_H))
        .or(DateTime::parse_from_rfc3339(str).map(|dt| dt.naive_utc().time()))
        .map_err(|_| VMError::ParseDateTimeErr {
            timestamp: str.to_string(),
        });
}

pub(crate) enum DateUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl TryFrom<&str> for DateUnit {
    type Error = VMError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "s" | "second" | "seconds" => Ok(Self::Second),
            "m" | "minute" | "minutes" => Ok(Self::Minute),
            "h" | "hour" | "hours" => Ok(Self::Hour),
            "d" | "day" | "days" => Ok(Self::Day),
            "w" | "week" | "weeks" => Ok(Self::Week),
            "M" | "month" | "months" => Ok(Self::Month),
            "y" | "year" | "years" => Ok(Self::Year),
            _ => Err(VMError::OpcodeErr {
                opcode: "DateUnit".into(),
                message: "Unknown date unit".into(),
            }),
        }
    }
}

pub(crate) fn date_time_start_of(date: NaiveDateTime, unit: DateUnit) -> Option<NaiveDateTime> {
    match unit {
        DateUnit::Second => Some(date),
        DateUnit::Minute => date.with_second(0),
        DateUnit::Hour => date.with_second(0)?.with_minute(0),
        DateUnit::Day => date.with_second(0)?.with_minute(0)?.with_hour(0),
        DateUnit::Week => date
            .with_second(0)?
            .with_minute(0)?
            .with_hour(0)?
            .checked_sub_days(Days::new(date.weekday().num_days_from_monday() as u64)),
        DateUnit::Month => date
            .with_second(0)?
            .with_minute(0)?
            .with_hour(0)?
            .with_day0(0),
        DateUnit::Year => date
            .with_second(0)?
            .with_minute(0)?
            .with_hour(0)?
            .with_day0(0)?
            .with_month0(0),
    }
}

pub(crate) fn date_time_end_of(date: NaiveDateTime, unit: DateUnit) -> Option<NaiveDateTime> {
    match unit {
        DateUnit::Second => Some(date),
        DateUnit::Minute => date.with_second(59),
        DateUnit::Hour => date.with_second(59)?.with_minute(59),
        DateUnit::Day => date.with_second(59)?.with_minute(59)?.with_hour(23),
        DateUnit::Week => date
            .with_second(59)?
            .with_minute(59)?
            .with_hour(23)?
            .checked_add_days(Days::new(Weekday::Sun as u64 - date.weekday() as u64)),
        DateUnit::Month => date
            .with_second(59)?
            .with_minute(59)?
            .with_hour(23)?
            .with_day(get_month_days(&date)? as u32),
        DateUnit::Year => date
            .with_second(59)?
            .with_minute(59)?
            .with_hour(23)?
            .with_day(get_month_days(&date)? as u32)?
            .with_month0(11),
    }
}

fn get_month_days(date: &NaiveDateTime) -> Option<i64> {
    Some(
        NaiveDate::from_ymd_opt(
            match date.month() {
                12 => date.year() + 1,
                _ => date.year(),
            },
            match date.month() {
                12 => 1,
                _ => date.month() + 1,
            },
            1,
        )?
        .signed_duration_since(NaiveDate::from_ymd_opt(date.year(), date.month(), 1)?)
        .num_days(),
    )
}
