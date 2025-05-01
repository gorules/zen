use crate::variable::DynamicVariable;
pub(crate) use crate::vm::date::duration::Duration;
pub(crate) use crate::vm::date::duration_unit::DurationUnit;
use crate::Variable;
use chrono::{
    DateTime, Datelike, LocalResult, Months, NaiveDate, NaiveDateTime, SecondsFormat, TimeDelta,
    TimeZone, Utc,
};
use rust_decimal::prelude::ToPrimitive;
use serde_json::Value;
use std::any::Any;
use std::fmt::{Display, Formatter};
use std::ops::{Add, Deref};

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

fn parse_date(var: Variable) -> Option<DateTime<Utc>> {
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
                    .or_else(|| NaiveDateTime::parse_from_str(str.deref(), "%Y-%m-%d %H:%M").ok())
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

fn add_duration(mut date_time: DateTime<Utc>, duration: Duration) -> Option<DateTime<Utc>> {
    date_time = date_time.add(TimeDelta::seconds(duration.seconds));
    date_time = match duration.months < 0 {
        true => date_time.checked_sub_months(Months::new(duration.months.unsigned_abs()))?,
        false => date_time.checked_add_months(Months::new(duration.months.unsigned_abs()))?,
    };

    date_time.with_year(date_time.year() + duration.years)
}

impl VmDate {
    pub fn now() -> Self {
        Self(Some(Utc::now()))
    }

    pub fn invalid() -> Self {
        Self(None)
    }

    /// Create a new VmDate from the current time
    pub fn new(var: Variable) -> Self {
        Self(parse_date(var))
    }

    pub fn add(&self, duration: Duration) -> Self {
        let Some(date_time) = &self.0 else {
            return Self(None);
        };

        Self(add_duration(date_time.clone(), duration))
    }

    pub fn sub(&self, duration: Duration) -> Self {
        let Some(date_time) = &self.0 else {
            return Self(None);
        };

        Self(add_duration(date_time.clone(), duration.negate()))
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

    // pub fn is_after(&self, other: &VmDate, unit: Option<DateUnit>) -> bool {
    //     self.0 > other.0
    // }
    //
    // /// Check if this date is before another date
    // pub fn is_before(&self, other: &VmDate) -> bool {
    //     self.0 < other.0
    // }
    //
    // /// Check if this date is the same as another date, optionally to a specific unit of precision
    // pub fn is_same(&self, other: &VmDate, unit: Option<DateUnit>) -> bool {
    //     match unit {
    //         None => self.0 == other.0,
    //         Some(DateUnit::Year) => self.0.year() == other.0.year(),
    //         Some(DateUnit::Month) => {
    //             self.0.year() == other.0.year() && self.0.month() == other.0.month()
    //         }
    //         Some(DateUnit::Day) => {
    //             self.0.year() == other.0.year()
    //                 && self.0.month() == other.0.month()
    //                 && self.0.day() == other.0.day()
    //         }
    //         Some(DateUnit::Hour) => {
    //             self.0.year() == other.0.year()
    //                 && self.0.month() == other.0.month()
    //                 && self.0.day() == other.0.day()
    //                 && self.0.hour() == other.0.hour()
    //         }
    //         Some(DateUnit::Minute) => {
    //             self.0.year() == other.0.year()
    //                 && self.0.month() == other.0.month()
    //                 && self.0.day() == other.0.day()
    //                 && self.0.hour() == other.0.hour()
    //                 && self.0.minute() == other.0.minute()
    //         }
    //         Some(DateUnit::Second) => {
    //             self.0.year() == other.0.year()
    //                 && self.0.month() == other.0.month()
    //                 && self.0.day() == other.0.day()
    //                 && self.0.hour() == other.0.hour()
    //                 && self.0.minute() == other.0.minute()
    //                 && self.0.second() == other.0.second()
    //         }
    //         Some(DateUnit::Millisecond) => self.0 == other.0,
    //         Some(DateUnit::Week) => {
    //             // ISO week number
    //             self.0.year() == other.0.year()
    //                 && self.0.iso_week().week() == other.0.iso_week().week()
    //         }
    //         Some(DateUnit::Quarter) => {
    //             self.0.year() == other.0.year()
    //                 && (self.0.month() - 1) / 3 == (other.0.month() - 1) / 3
    //         }
    //     }
    // }
    //
    // /// Calculate the difference between two dates in the specified unit
    // pub fn diff(&self, other: &VmDate, unit: DateUnit) -> i64 {
    //     match unit {
    //         DateUnit::Millisecond => {
    //             let duration = self.0 - other.0;
    //             duration.num_milliseconds()
    //         }
    //         DateUnit::Second => {
    //             let duration = self.0 - other.0;
    //             duration.num_seconds()
    //         }
    //         DateUnit::Minute => {
    //             let duration = self.0 - other.0;
    //             duration.num_minutes()
    //         }
    //         DateUnit::Hour => {
    //             let duration = self.0 - other.0;
    //             duration.num_hours()
    //         }
    //         DateUnit::Day => {
    //             let duration = self.0 - other.0;
    //             duration.num_days()
    //         }
    //         DateUnit::Week => {
    //             let duration = self.0 - other.0;
    //             duration.num_days() / 7
    //         }
    //         DateUnit::Month => {
    //             let year_diff = self.0.year() - other.0.year();
    //             let month_diff = self.0.month() as i32 - other.0.month() as i32;
    //             (year_diff * 12 + month_diff) as i64
    //         }
    //         DateUnit::Quarter => {
    //             let year_diff = self.0.year() - other.0.year();
    //             let quarter_diff = (self.0.month() - 1) / 3 - (other.0.month() - 1) / 3;
    //             (year_diff * 4 + quarter_diff as i32) as i64
    //         }
    //         DateUnit::Year => (self.0.year() - other.0.year()) as i64,
    //     }
    // }
    //
    //
    // /// Format the date according to the specified format string
    // pub fn format(&self, fmt: &str) -> String {
    //     self.0.format(fmt).to_string()
    // }
    //
    // /// Get the start of the time unit (beginning of day, month, etc.)
    // pub fn start_of(&self, unit: DateUnit) -> Self {
    //     let naive = self.0.naive_local();
    //
    //     match unit {
    //         DateUnit::Millisecond => Self(self.0.clone()),
    //         DateUnit::Second => {
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 naive.date(),
    //                 chrono::NaiveTime::from_hms_opt(naive.hour(), naive.minute(), naive.second())
    //                     .unwrap(),
    //             );
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Minute => {
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 naive.date(),
    //                 chrono::NaiveTime::from_hms_opt(naive.hour(), naive.minute(), 0).unwrap(),
    //             );
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Hour => {
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 naive.date(),
    //                 chrono::NaiveTime::from_hms_opt(naive.hour(), 0, 0).unwrap(),
    //             );
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Day => {
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 naive.date(),
    //                 chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
    //             );
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Week => {
    //             // Start of the ISO week (Monday)
    //             let day_of_week = naive.weekday().num_days_from_monday();
    //             let days_to_subtract = day_of_week as i64;
    //
    //             let start_of_week = naive
    //                 .date()
    //                 .checked_sub_signed(chrono::Duration::days(days_to_subtract))
    //                 .unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 start_of_week,
    //                 chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
    //             );
    //
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Month => {
    //             let new_date =
    //                 chrono::NaiveDate::from_ymd_opt(naive.year(), naive.month(), 1).unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 new_date,
    //                 chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
    //             );
    //
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Quarter => {
    //             let quarter_month = (naive.month() - 1) / 3 * 3 + 1;
    //
    //             let new_date =
    //                 chrono::NaiveDate::from_ymd_opt(naive.year(), quarter_month, 1).unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 new_date,
    //                 chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
    //             );
    //
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Year => {
    //             let new_date = chrono::NaiveDate::from_ymd_opt(naive.year(), 1, 1).unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 new_date,
    //                 chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
    //             );
    //
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //     }
    // }
    //
    // /// Get the end of the time unit (end of day, month, etc.)
    // pub fn end_of(&self, unit: DateUnit) -> Self {
    //     match unit {
    //         DateUnit::Millisecond => Self(self.0.clone()),
    //         DateUnit::Second => {
    //             let naive = self.0.naive_local();
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 naive.date(),
    //                 chrono::NaiveTime::from_hms_milli_opt(
    //                     naive.hour(),
    //                     naive.minute(),
    //                     naive.second(),
    //                     999,
    //                 )
    //                     .unwrap(),
    //             );
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Minute => {
    //             let naive = self.0.naive_local();
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 naive.date(),
    //                 chrono::NaiveTime::from_hms_milli_opt(naive.hour(), naive.minute(), 59, 999)
    //                     .unwrap(),
    //             );
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Hour => {
    //             let naive = self.0.naive_local();
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 naive.date(),
    //                 chrono::NaiveTime::from_hms_milli_opt(naive.hour(), 59, 59, 999).unwrap(),
    //             );
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Day => {
    //             let naive = self.0.naive_local();
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 naive.date(),
    //                 chrono::NaiveTime::from_hms_milli_opt(23, 59, 59, 999).unwrap(),
    //             );
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Week => {
    //             // End of the ISO week (Sunday)
    //             let day_of_week = naive.weekday().num_days_from_monday();
    //             let days_to_add = 6 - day_of_week as i64;
    //
    //             let end_of_week = naive
    //                 .date()
    //                 .checked_add_signed(chrono::Duration::days(days_to_add))
    //                 .unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 end_of_week,
    //                 chrono::NaiveTime::from_hms_milli_opt(23, 59, 59, 999).unwrap(),
    //             );
    //
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Month => {
    //             let naive = self.0.naive_local();
    //             let days_in_month = get_days_in_month(naive.year(), naive.month());
    //
    //             let new_date =
    //                 chrono::NaiveDate::from_ymd_opt(naive.year(), naive.month(), days_in_month)
    //                     .unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 new_date,
    //                 chrono::NaiveTime::from_hms_milli_opt(23, 59, 59, 999).unwrap(),
    //             );
    //
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Quarter => {
    //             let naive = self.0.naive_local();
    //             let quarter_month = (naive.month() - 1) / 3 * 3 + 3;
    //
    //             let days_in_month = get_days_in_month(naive.year(), quarter_month);
    //
    //             let new_date =
    //                 chrono::NaiveDate::from_ymd_opt(naive.year(), quarter_month, days_in_month)
    //                     .unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 new_date,
    //                 chrono::NaiveTime::from_hms_milli_opt(23, 59, 59, 999).unwrap(),
    //             );
    //
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Year => {
    //             let naive = self.0.naive_local();
    //             let new_date = chrono::NaiveDate::from_ymd_opt(naive.year(), 12, 31).unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(
    //                 new_date,
    //                 chrono::NaiveTime::from_hms_milli_opt(23, 59, 59, 999).unwrap(),
    //             );
    //
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //     }
    // }
    //
    // /// Get a specific component of the date
    // pub fn get(&self, unit: DateUnit) -> i64 {
    //     match unit {
    //         DateUnit::Second => self.0.second() as i64,
    //         DateUnit::Minute => self.0.minute() as i64,
    //         DateUnit::Hour => self.0.hour() as i64,
    //         DateUnit::Day => self.0.day() as i64,
    //         DateUnit::Week => self.0.iso_week().week() as i64,
    //         DateUnit::Month => self.0.month() as i64,
    //         DateUnit::Year => self.0.year() as i64,
    //     }
    // }
    //
    // /// Set a specific component of the date
    // pub fn set(&self, unit: DateUnit, value: i64) -> Self {
    //     let naive = self.0.naive_local();
    //
    //     match unit {
    //         DateUnit::Millisecond => {
    //             let new_time = chrono::NaiveTime::from_hms_milli_opt(
    //                 naive.hour(),
    //                 naive.minute(),
    //                 naive.second(),
    //                 value as u32,
    //             )
    //                 .unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(naive.date(), new_time);
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Second => {
    //             let new_time = chrono::NaiveTime::from_hms_milli_opt(
    //                 naive.hour(),
    //                 naive.minute(),
    //                 value as u32,
    //                 naive.timestamp_subsec_millis(),
    //             )
    //                 .unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(naive.date(), new_time);
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Minute => {
    //             let new_time = chrono::NaiveTime::from_hms_milli_opt(
    //                 naive.hour(),
    //                 value as u32,
    //                 naive.second(),
    //                 naive.timestamp_subsec_millis(),
    //             )
    //                 .unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(naive.date(), new_time);
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Hour => {
    //             let new_time = chrono::NaiveTime::from_hms_milli_opt(
    //                 value as u32,
    //                 naive.minute(),
    //                 naive.second(),
    //                 naive.timestamp_subsec_millis(),
    //             )
    //                 .unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(naive.date(), new_time);
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Day => {
    //             // Ensure the day is valid for the month
    //             let max_day = get_days_in_month(naive.year(), naive.month());
    //             let day = std::cmp::min(value as u32, max_day);
    //
    //             let new_date =
    //                 chrono::NaiveDate::from_ymd_opt(naive.year(), naive.month(), day).unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(new_date, naive.time());
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Week => {
    //             // Setting week is not straightforward in chrono
    //             // This is a simplified approach assuming ISO week
    //             let current_week = naive.iso_week().week() as i64;
    //             let diff_weeks = value - current_week;
    //             self.add(diff_weeks * 7, DateUnit::Day)
    //         }
    //         DateUnit::Month => {
    //             let month = std::cmp::min(std::cmp::max(value, 1), 12) as u32;
    //             let max_day = get_days_in_month(naive.year(), month);
    //             let day = std::cmp::min(naive.day(), max_day);
    //
    //             let new_date = chrono::NaiveDate::from_ymd_opt(naive.year(), month, day).unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(new_date, naive.time());
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //         DateUnit::Quarter => {
    //             let quarter = std::cmp::min(std::cmp::max(value, 1), 4) as u32;
    //             let month = (quarter - 1) * 3 + 1;
    //             self.set(DateUnit::Month, month as i64)
    //         }
    //         DateUnit::Year => {
    //             let year = value as i32;
    //             let month = naive.month();
    //
    //             // Handle Feb 29 in non-leap years
    //             let mut day = naive.day();
    //             if month == 2 && day == 29 && !is_leap_year(year) {
    //                 day = 28;
    //             }
    //
    //             let new_date = chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap();
    //
    //             let new_datetime = chrono::NaiveDateTime::new(new_date, naive.time());
    //             Self(DateTime::from_naive_local_datetime(new_datetime).unwrap())
    //         }
    //     }
    // }
}

// Helper function to determine if a year is a leap year
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

// Helper function to get the number of days in a month
fn get_days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => panic!("Invalid month: {}", month),
    }
}
