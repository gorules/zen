use crate::variable::VariableType;
use std::rc::Rc;

#[derive(Debug, Clone, Copy)]
pub(crate) enum DurationUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl DurationUnit {
    pub fn variable_type() -> VariableType {
        VariableType::Enum(
            Some(Rc::from("DurationUnit")),
            vec![
                "seconds", "second", "secs", "sec", "s", "minutes", "minute", "min", "mins", "m",
                "hours", "hour", "hr", "hrs", "h", "days", "day", "d", "weeks", "week", "w",
                "months", "month", "mo", "M", "quarters", "quarter", "qtr", "q", "years", "year",
                "y",
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
        )
    }

    pub fn parse(unit: &str) -> Option<Self> {
        match unit {
            "seconds" | "second" | "secs" | "sec" | "s" => Some(Self::Second),
            "minutes" | "minute" | "min" | "mins" | "m" => Some(Self::Minute),
            "hours" | "hour" | "hr" | "hrs" | "h" => Some(Self::Hour),
            "days" | "day" | "d" => Some(Self::Day),
            "weeks" | "week" | "w" => Some(Self::Week),
            "months" | "month" | "mo" | "M" => Some(Self::Month),
            "quarters" | "quarter" | "qtr" | "q" => Some(Self::Quarter),
            "years" | "year" | "y" => Some(Self::Year),
            _ => None,
        }
    }

    pub fn as_secs(&self) -> Option<u64> {
        match self {
            DurationUnit::Second => Some(1),
            DurationUnit::Minute => Some(60),
            DurationUnit::Hour => Some(3600),
            DurationUnit::Day => Some(86_400),
            DurationUnit::Week => Some(86_400 * 7),
            // Calendar units
            DurationUnit::Quarter => None,
            DurationUnit::Month => None,
            DurationUnit::Year => None,
        }
    }

    pub fn as_millis(&self) -> Option<f64> {
        self.as_secs().map(|s| s as f64 * 1000_f64)
    }
}
