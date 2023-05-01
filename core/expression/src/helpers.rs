use chrono::{NaiveDate, NaiveTime};
use chrono::{NaiveDateTime, Utc};
use once_cell::sync::Lazy;

use crate::vm::VMError;

#[allow(clippy::unwrap_used)]
static ZERO_TIME: Lazy<NaiveTime> = Lazy::new(|| NaiveTime::from_hms_opt(0, 0, 0).unwrap());

static DT_RFC: &str = "%Y-%m-%d %H:%M:%S";
static DT_ISO: &str = "%Y-%m-%dT%H:%M:%S";
static DT_ISO_Z: &str = "%Y-%m-%dT%H:%M:%S%z";
static DATE: &str = "%Y-%m-%d";
static TIME: &str = "%H:%M:%S";

pub(crate) fn date_time(str: &str) -> Result<NaiveDateTime, VMError> {
    if str == "now" {
        return Ok(Utc::now().naive_utc());
    }

    let zero_time = ZERO_TIME.to_owned();

    NaiveDateTime::parse_from_str(str, DT_RFC)
        .or(NaiveDateTime::parse_from_str(str, DT_ISO))
        .or(NaiveDateTime::parse_from_str(str, DT_ISO_Z))
        .or(NaiveDate::parse_from_str(str, DATE).map(|c| c.and_time(zero_time)))
        .map_err(|_| VMError::ParseDateTimeErr {
            timestamp: str.to_string(),
        })
}

pub(crate) fn time(str: &str) -> Result<i64, VMError> {
    let today_midnight = Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap();
    let now = Utc::now().naive_utc();

    if str == "now" {
        return Ok(now.signed_duration_since(today_midnight).num_seconds());
    }

    return NaiveTime::parse_from_str(str, DT_RFC)
        .or(NaiveTime::parse_from_str(str, DT_ISO))
        .or(NaiveTime::parse_from_str(str, DT_ISO_Z))
        .or(NaiveTime::parse_from_str(str, TIME))
        .map_err(|_| VMError::ParseDateTimeErr {
            timestamp: str.to_string(),
        })
        .and_then(|time| {
            Ok(Utc::now()
                .date_naive()
                .and_time(time)
                .signed_duration_since(today_midnight)
                .num_seconds())
        });
}
