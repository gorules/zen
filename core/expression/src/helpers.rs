use chrono::{DateTime, NaiveDate, NaiveTime};
use chrono::{NaiveDateTime, Utc};
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
