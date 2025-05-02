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
}
