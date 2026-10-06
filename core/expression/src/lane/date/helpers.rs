use crate::lane::date::utc_now;
use crate::vm::VMError;
use chrono::format::{parse, Item, ParseResult, Parsed, StrftimeItems};
use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};
use once_cell::sync::Lazy;

type VMResult<T> = Result<T, VMError>;

#[allow(clippy::unwrap_used)]
static ZERO_TIME: Lazy<NaiveTime> = Lazy::new(|| NaiveTime::from_hms_opt(0, 0, 0).unwrap());

static DATE_TIME: &str = "%Y-%m-%d %H:%M:%S";
static DATE: &str = "%Y-%m-%d";
static TIME_HMS: &str = "%H:%M:%S";
static TIME_HM: &str = "%H:%M";
static TIME_H: &str = "%H";

static DATE_TIME_ITEMS: Lazy<Vec<Item<'static>>> = Lazy::new(|| StrftimeItems::new(DATE_TIME).parse_to_owned().unwrap_or_default());
static DATE_ITEMS: Lazy<Vec<Item<'static>>> = Lazy::new(|| StrftimeItems::new(DATE).parse_to_owned().unwrap_or_default());

pub(crate) struct Formats;

impl Formats {
    fn date_time(s: &str) -> ParseResult<NaiveDateTime> {
        let mut parsed = Parsed::new();
        parse(&mut parsed, s, DATE_TIME_ITEMS.iter())?;
        parsed.to_naive_datetime_with_offset(0)
    }

    fn date(s: &str) -> ParseResult<NaiveDate> {
        let mut parsed = Parsed::new();
        parse(&mut parsed, s, DATE_ITEMS.iter())?;
        parsed.to_naive_date()
    }
}

pub(crate) struct IsoDate;

impl IsoDate {
    fn digits(bytes: &[u8]) -> Option<u32> {
        bytes.iter().try_fold(0u32, |acc, c| {
            c.is_ascii_digit().then(|| acc * 10 + (c - b'0') as u32)
        })
    }

    pub(crate) fn utc(s: &str) -> bool {
        s.as_bytes().get(10) == Some(&b'T')
    }

    pub(crate) fn parse(s: &str) -> Option<NaiveDateTime> {
        let b = s.as_bytes();
        if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let date = NaiveDate::from_ymd_opt(
            Self::digits(&b[0..4])? as i32,
            Self::digits(&b[5..7])?,
            Self::digits(&b[8..10])?,
        )?;
        let time = match &b[10..] {
            [] => (0, 0, 0),
            [sep, h1, h2, b':', m1, m2, b':', s1, s2, rest @ ..]
                if (*sep == b' ' && rest.is_empty()) || (*sep == b'T' && rest == b"Z") =>
            {
                (
                    Self::digits(&[*h1, *h2])?,
                    Self::digits(&[*m1, *m2])?,
                    Self::digits(&[*s1, *s2])?,
                )
            }
            _ => return None,
        };
        date.and_hms_opt(time.0, time.1, time.2)
    }
}

pub(crate) fn date_time(str: &str) -> VMResult<NaiveDateTime> {
    if str == "now" {
        return Ok(utc_now().naive_utc());
    }

    if let Some(parsed) = IsoDate::parse(str) {
        return Ok(parsed);
    }

    Formats::date_time(str)
        .or_else(|_| Formats::date(str).map(|c| c.and_time(*ZERO_TIME)))
        .or_else(|_| DateTime::parse_from_rfc3339(str).map(|dt| dt.naive_utc()))
        .map_err(|_| VMError::ParseDateTimeErr {
            timestamp: str.to_string(),
        })
}

pub(crate) fn time(str: &str) -> VMResult<NaiveTime> {
    let now = utc_now();

    if str == "now" {
        return Ok(now.naive_utc().time());
    }

    NaiveTime::parse_from_str(str, DATE_TIME)
        .or(NaiveTime::parse_from_str(str, TIME_HMS))
        .or(NaiveTime::parse_from_str(str, TIME_HM))
        .or(NaiveTime::parse_from_str(str, TIME_H))
        .or(DateTime::parse_from_rfc3339(str).map(|dt| dt.naive_utc().time()))
        .map_err(|_| VMError::ParseDateTimeErr {
            timestamp: str.to_string(),
        })
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

#[cfg(test)]
mod iso_tests {
    use super::{IsoDate, DATE, DATE_TIME, ZERO_TIME};
    use chrono::{DateTime, NaiveDate, NaiveDateTime};

    fn chrono_chain(s: &str) -> Option<NaiveDateTime> {
        NaiveDateTime::parse_from_str(s, DATE_TIME)
            .or_else(|_| NaiveDate::parse_from_str(s, DATE).map(|c| c.and_time(*ZERO_TIME)))
            .or_else(|_| DateTime::parse_from_rfc3339(s).map(|dt| dt.naive_utc()))
            .ok()
    }

    #[test]
    fn iso_fast_path_agrees_with_chrono() {
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let mut checked = 0;
        for _ in 0..200_000 {
            let year = 1900 + next(250);
            let month = next(14);
            let day = next(33);
            let (h, m, sec) = (next(26), next(62), next(62));
            let s = match next(8) {
                0 => format!("{year:04}-{month:02}-{day:02}"),
                1 => format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{sec:02}"),
                2 => format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}Z"),
                3 => format!("{year:04}-{month}-{day}"),
                4 => format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}+02:00"),
                5 => format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}"),
                6 => format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}.5Z"),
                _ => format!("{year:04}-{month:02}-{day:02}x"),
            };
            let fast = IsoDate::parse(&s);
            if fast.is_some() {
                checked += 1;
                assert_eq!(fast, chrono_chain(&s), "{s}");
            }
        }
        assert!(checked > 10_000);
    }
}
