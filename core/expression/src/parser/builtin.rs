use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

#[derive(Debug, PartialEq)]
pub(crate) enum Arity {
    Single,
    Closure,
    Dual,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, EnumString, Display, IntoStaticStr, EnumIter)]
#[strum(serialize_all = "camelCase")]
pub enum BuiltInFunction {
    // General
    Len,
    Contains,
    Flatten,

    // String
    Upper,
    Lower,
    StartsWith,
    EndsWith,
    Matches,
    Extract,
    FuzzyMatch,
    Split,

    // Math
    Abs,
    Sum,
    Avg,
    Min,
    Max,
    Rand,
    Median,
    Mode,
    Floor,
    Ceil,
    Round,

    // Type
    IsNumeric,
    String,
    Number,
    Bool,
    Type,

    // Date + time
    Date,
    Time,
    Duration,
    Year,
    DayOfWeek,
    DayOfMonth,
    DayOfYear,
    WeekOfYear,
    MonthOfYear,
    MonthString,
    DateString,
    WeekdayString,
    StartOf,
    EndOf,

    // Map
    Keys,
    Values,

    // Closures
    All,
    Some,
    None,
    Filter,
    Map,
    Count,
    One,
    FlatMap,
}

impl BuiltInFunction {
    pub(crate) fn arity(&self) -> Arity {
        match &self {
            // General
            BuiltInFunction::Len => Arity::Single,
            BuiltInFunction::Contains => Arity::Dual,
            BuiltInFunction::Flatten => Arity::Single,

            // String
            BuiltInFunction::Upper => Arity::Single,
            BuiltInFunction::Lower => Arity::Single,
            BuiltInFunction::StartsWith => Arity::Dual,
            BuiltInFunction::EndsWith => Arity::Dual,
            BuiltInFunction::Matches => Arity::Dual,
            BuiltInFunction::Extract => Arity::Dual,
            BuiltInFunction::FuzzyMatch => Arity::Dual,
            BuiltInFunction::Split => Arity::Dual,

            // Math
            BuiltInFunction::Abs => Arity::Single,
            BuiltInFunction::Sum => Arity::Single,
            BuiltInFunction::Avg => Arity::Single,
            BuiltInFunction::Min => Arity::Single,
            BuiltInFunction::Max => Arity::Single,
            BuiltInFunction::Rand => Arity::Single,
            BuiltInFunction::Median => Arity::Single,
            BuiltInFunction::Mode => Arity::Single,
            BuiltInFunction::Floor => Arity::Single,
            BuiltInFunction::Ceil => Arity::Single,
            BuiltInFunction::Round => Arity::Single,

            // Date + time
            BuiltInFunction::Date => Arity::Single,
            BuiltInFunction::Time => Arity::Single,
            BuiltInFunction::Duration => Arity::Single,
            BuiltInFunction::Year => Arity::Single,
            BuiltInFunction::DayOfWeek => Arity::Single,
            BuiltInFunction::DayOfMonth => Arity::Single,
            BuiltInFunction::DayOfYear => Arity::Single,
            BuiltInFunction::WeekOfYear => Arity::Single,
            BuiltInFunction::MonthOfYear => Arity::Single,
            BuiltInFunction::MonthString => Arity::Single,
            BuiltInFunction::DateString => Arity::Single,
            BuiltInFunction::WeekdayString => Arity::Single,
            BuiltInFunction::StartOf => Arity::Dual,
            BuiltInFunction::EndOf => Arity::Dual,

            // Type
            BuiltInFunction::String => Arity::Single,
            BuiltInFunction::Number => Arity::Single,
            BuiltInFunction::Bool => Arity::Single,
            BuiltInFunction::IsNumeric => Arity::Single,
            BuiltInFunction::Type => Arity::Single,

            // Map
            BuiltInFunction::Keys => Arity::Single,
            BuiltInFunction::Values => Arity::Single,

            // Closure
            BuiltInFunction::All => Arity::Closure,
            BuiltInFunction::Some => Arity::Closure,
            BuiltInFunction::None => Arity::Closure,
            BuiltInFunction::Filter => Arity::Closure,
            BuiltInFunction::Map => Arity::Closure,
            BuiltInFunction::Count => Arity::Closure,
            BuiltInFunction::One => Arity::Closure,
            BuiltInFunction::FlatMap => Arity::Closure,
        }
    }
}
