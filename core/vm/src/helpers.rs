use chrono::{NaiveDate, NaiveTime};
use chrono::{NaiveDateTime, Utc};
use once_cell::sync::Lazy;

use crate::vm::VMError;

#[allow(clippy::unwrap_used)]
static ZERO_TIME: Lazy<NaiveTime> = Lazy::new(|| NaiveTime::from_hms_opt(0, 0, 0).unwrap());

pub(crate) fn date_time(str: &str) -> Result<NaiveDateTime, VMError> {
    if str == "now" {
        return Ok(Utc::now().naive_utc());
    }

    let zero_time = ZERO_TIME.to_owned();

    let x = NaiveDateTime::parse_from_str(str, "%Y-%m-%d %H:%M:%S");
    let y = NaiveDate::parse_from_str(str, "%Y-%m-%d").map(|c| c.and_time(zero_time));

    x.or(y).map_err(|_| VMError::ParseTimeErr {
        timestamp: str.to_string(),
    })
}
