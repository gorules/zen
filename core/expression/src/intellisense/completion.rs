use crate::functions::registry::FunctionRegistry;
use crate::functions::DateMethod;
use crate::functions::{
    ClosureFunction, DeprecatedFunction, FunctionKind, InternalFunction, MethodKind, MethodRegistry,
};
use crate::intellisense::IntelliSenseToken;
use crate::variable::VariableType;
use serde::Serialize;
use strum::IntoEnumIterator;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CompletionKind {
    Variable,
    Function,
    Method,
    Property,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: String,
    pub info: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boost: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method_for: Option<VariableType>,
}

pub struct Completions;

impl Completions {
    pub fn build(
        source: &str,
        pos: u32,
        data: &VariableType,
        tokens: &[IntelliSenseToken],
    ) -> Vec<Completion> {
        let before = source.get(..pos as usize).unwrap_or(source);
        let prefix = Self::extract_prefix(before);

        let completions = match Self::find_property_dot(before) {
            Some(dot) => {
                let target_type = tokens
                    .iter()
                    .filter(|t| t.span.1 <= dot as u32 && t.span.1 > 0)
                    .max_by(|a, b| {
                        a.span
                            .1
                            .cmp(&b.span.1)
                            .then_with(|| (a.span.1 - a.span.0).cmp(&(b.span.1 - b.span.0)))
                    })
                    .map(|t| t.kind.clone())
                    .unwrap_or(VariableType::Any);

                Self::build_property(&target_type)
            }
            None => Self::build_scope(data),
        };

        Self::filter(completions, prefix)
    }

    pub fn build_property(vt: &VariableType) -> Vec<Completion> {
        let mut completions = Vec::new();
        let resolved = match vt {
            VariableType::Nullable(inner) => inner.as_ref(),
            other => other,
        };

        if let VariableType::Object(obj) = resolved {
            let obj = obj.borrow();
            for (key, val) in obj.iter() {
                completions.push(Completion {
                    label: key.to_string(),
                    kind: CompletionKind::Property,
                    detail: val.to_string(),
                    info: String::new(),
                    boost: Some(10),
                    method_for: None,
                });
            }
        }

        for mk in DateMethod::iter().map(MethodKind::DateMethod) {
            let def = MethodRegistry::get_definition(&mk);
            let applies = def
                .as_ref()
                .and_then(|d| d.param_type(0))
                .map(|pt| vt.satisfies(&pt))
                .unwrap_or(false);

            if applies || matches!(vt, VariableType::Any) {
                completions.push(Self::method(mk));
            }
        }

        completions
    }

    pub fn build_scope(data: &VariableType) -> Vec<Completion> {
        let mut completions = Vec::new();

        let resolved_data = match data {
            VariableType::Nullable(inner) => inner.as_ref(),
            other => other,
        };

        if let VariableType::Object(obj) = resolved_data {
            let obj = obj.borrow();
            for (key, val) in obj.iter() {
                completions.push(Completion {
                    label: key.to_string(),
                    kind: CompletionKind::Variable,
                    detail: val.to_string(),
                    info: String::new(),
                    boost: Some(20),
                    method_for: None,
                });
            }
        }

        completions.push(Completion {
            label: "$root".to_string(),
            kind: CompletionKind::Variable,
            detail: "Root variable".to_string(),
            info: String::new(),
            boost: Some(-10),
            method_for: None,
        });

        completions.extend(
            InternalFunction::iter()
                .map(FunctionKind::Internal)
                .chain(ClosureFunction::iter().map(FunctionKind::Closure))
                .map(|fk| Self::function(fk, None)),
        );

        completions
    }

    fn function(fk: FunctionKind, boost_override: Option<i32>) -> Completion {
        let label = fk.to_string();
        let info = function_info(&fk);
        let detail = function_signature(&fk);
        let boost = boost_override.or(match &fk {
            FunctionKind::Internal(_) => Some(10),
            FunctionKind::Closure(_) => None,
            FunctionKind::Deprecated(_) => Some(-20),
        });

        Completion {
            label,
            kind: CompletionKind::Function,
            detail,
            info,
            boost,
            method_for: None,
        }
    }

    fn method(mk: MethodKind) -> Completion {
        let label = mk.to_string();
        let info = method_info(&mk);
        let (detail, method_for) = method_signature(&mk);

        Completion {
            label,
            kind: CompletionKind::Method,
            detail,
            info,
            boost: None,
            method_for,
        }
    }

    fn extract_prefix(before_cursor: &str) -> &str {
        let boundary = before_cursor
            .rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != '$' && c != '#')
            .map(|i| i + 1)
            .unwrap_or(0);

        &before_cursor[boundary..]
    }

    fn filter(completions: Vec<Completion>, prefix: &str) -> Vec<Completion> {
        if prefix.is_empty() {
            return completions;
        }

        let prefix_lower = prefix.to_lowercase();
        completions
            .into_iter()
            .filter(|c| c.label.to_lowercase().starts_with(&prefix_lower))
            .collect()
    }

    fn find_property_dot(before_cursor: &str) -> Option<usize> {
        let trimmed = before_cursor.trim_end();
        if trimmed.ends_with('.') {
            return Some(trimmed.len() - 1);
        }

        let word_start = trimmed.rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != '#');
        match word_start {
            Some(i) if trimmed.as_bytes().get(i) == Some(&b'.') => Some(i),
            _ => None,
        }
    }
}

fn function_info(fk: &FunctionKind) -> String {
    let s = match fk {
        FunctionKind::Internal(i) => match i {
            InternalFunction::Len => "Returns the length of variable",
            InternalFunction::Contains => "Checks if variable contains a needle",
            InternalFunction::Flatten => "Flattens an array",
            InternalFunction::Upper => "Converts all characters in a string to uppercase",
            InternalFunction::Lower => "Converts all characters in a string to lowercase",
            InternalFunction::Trim => {
                "Returns the string with leading and trailing whitespace removed"
            }
            InternalFunction::StartsWith => {
                "Returns true if the string starts with the specified prefix"
            }
            InternalFunction::EndsWith => {
                "Returns true if the string ends with the specified suffix"
            }
            InternalFunction::Matches => "Returns true if the string matches the specified pattern",
            InternalFunction::Extract => "Extracts matching substrings according to a pattern",
            InternalFunction::FuzzyMatch => "Performs a fuzzy search of the needle in the haystack",
            InternalFunction::Split => {
                "Splits a string into an array of substrings using the specified delimiter"
            }
            InternalFunction::Abs => "Returns the absolute value of a number",
            InternalFunction::Sum => "Returns the sum of all elements in the input array",
            InternalFunction::Avg => "Calculates the average of all elements in the input array",
            InternalFunction::Min => "Returns the smallest of the elements in the input array",
            InternalFunction::Max => "Returns the largest of the elements in the input array",
            InternalFunction::Rand => {
                "Generates a random number between 0 (inclusive) and max (inclusive)"
            }
            InternalFunction::Median => {
                "Calculates the median value of all elements in the input array"
            }
            InternalFunction::Mode => "Finds the mode(s) of the input array",
            InternalFunction::Floor => "Rounds a number down to the nearest integer",
            InternalFunction::Ceil => "Rounds a number up to the nearest integer",
            InternalFunction::Round => "Rounds a number to a specified number of decimal places",
            InternalFunction::Trunc => "Truncates a number to a specified number of decimal places",
            InternalFunction::IsNumeric => "Checks if the given value is of a numeric type",
            InternalFunction::String => "Converts the given value to a string",
            InternalFunction::Number => "Converts the given value to a number",
            InternalFunction::Bool => "Converts the given value to a boolean",
            InternalFunction::Type => "Returns a string representing the data type of the value",
            InternalFunction::Keys => {
                "Returns an array of a given object's own enumerable property names"
            }
            InternalFunction::Values => {
                "Returns an array of a given object's own enumerable property values"
            }
            InternalFunction::Date => "Returns a new date time instance",
            InternalFunction::Merge => "Merges multiple objects into one",
            InternalFunction::MergeDeep => "Deeply merges multiple objects into one",
        },
        FunctionKind::Deprecated(d) => match d {
            DeprecatedFunction::Date => "Converts a numeric timestamp to a unix timestamp",
            DeprecatedFunction::Time => "Extracts the time from a numeric timestamp",
            DeprecatedFunction::Duration => "Parses a duration string (e.g. 1h30min)",
            DeprecatedFunction::Year => "Extracts the year from a given timestamp",
            DeprecatedFunction::DayOfWeek => "Gets the day of the week from a given timestamp",
            DeprecatedFunction::DayOfMonth => {
                "Extracts the day of the month from a given timestamp"
            }
            DeprecatedFunction::DayOfYear => "Gets the day of the year from a given timestamp",
            DeprecatedFunction::WeekOfYear => {
                "Calculates the week of the year from a given timestamp"
            }
            DeprecatedFunction::MonthOfYear => "Extracts the month from a given timestamp",
            DeprecatedFunction::MonthString => {
                "Converts the month from a given timestamp into its string representation"
            }
            DeprecatedFunction::DateString => {
                "Converts a timestamp to a human-readable date string"
            }
            DeprecatedFunction::WeekdayString => {
                "Converts the day of the week into its string representation"
            }
            DeprecatedFunction::StartOf => {
                "Returns the timestamp representing the start of a specified unit"
            }
            DeprecatedFunction::EndOf => {
                "Returns the timestamp representing the end of a specified unit"
            }
        },
        FunctionKind::Closure(c) => match c {
            ClosureFunction::All => "Checks if all elements in the array satisfy the condition",
            ClosureFunction::None => "Checks if no elements in the array satisfy the condition",
            ClosureFunction::Some => "Checks if at least one element satisfies the condition",
            ClosureFunction::One => "Checks if exactly one element satisfies the condition",
            ClosureFunction::Filter => {
                "Creates a new array with elements that satisfy the condition"
            }
            ClosureFunction::Map => "Creates a new array by transforming each element",
            ClosureFunction::FlatMap => "Maps each element then flattens the result",
            ClosureFunction::Count => "Counts elements that satisfy the condition",
        },
    };
    s.to_string()
}

fn function_param_names(fk: &FunctionKind) -> Vec<&'static str> {
    match fk {
        FunctionKind::Internal(i) => match i {
            InternalFunction::Len => vec!["var"],
            InternalFunction::Contains => vec!["haystack", "needle"],
            InternalFunction::Flatten => vec!["arr"],
            InternalFunction::Upper | InternalFunction::Lower | InternalFunction::Trim => {
                vec!["str"]
            }
            InternalFunction::StartsWith => vec!["str", "prefix"],
            InternalFunction::EndsWith => vec!["str", "suffix"],
            InternalFunction::Matches | InternalFunction::Extract => vec!["str", "pattern"],
            InternalFunction::FuzzyMatch => vec!["haystack", "needle"],
            InternalFunction::Split => vec!["str", "delimiter"],
            InternalFunction::Abs | InternalFunction::Floor | InternalFunction::Ceil => {
                vec!["num"]
            }
            InternalFunction::Sum
            | InternalFunction::Avg
            | InternalFunction::Min
            | InternalFunction::Max
            | InternalFunction::Median
            | InternalFunction::Mode => vec!["arr"],
            InternalFunction::Rand => vec!["max"],
            InternalFunction::Round | InternalFunction::Trunc => vec!["num", "digits"],
            InternalFunction::IsNumeric
            | InternalFunction::String
            | InternalFunction::Number
            | InternalFunction::Bool
            | InternalFunction::Type => vec!["value"],
            InternalFunction::Keys | InternalFunction::Values => vec!["obj"],
            InternalFunction::Date => vec!["dateOrTimezone", "timezone"],
            InternalFunction::Merge | InternalFunction::MergeDeep => vec!["objects"],
        },
        FunctionKind::Deprecated(d) => match d {
            DeprecatedFunction::Date
            | DeprecatedFunction::Time
            | DeprecatedFunction::Year
            | DeprecatedFunction::DayOfWeek
            | DeprecatedFunction::DayOfMonth
            | DeprecatedFunction::DayOfYear
            | DeprecatedFunction::WeekOfYear
            | DeprecatedFunction::MonthOfYear
            | DeprecatedFunction::MonthString
            | DeprecatedFunction::DateString
            | DeprecatedFunction::WeekdayString => vec!["timestamp"],
            DeprecatedFunction::Duration => vec!["duration"],
            DeprecatedFunction::StartOf | DeprecatedFunction::EndOf => vec!["timestamp", "unit"],
        },
        FunctionKind::Closure(_) => vec![],
    }
}

fn function_signature(fk: &FunctionKind) -> String {
    match fk {
        FunctionKind::Internal(_) | FunctionKind::Deprecated(_) => {
            let param_names = function_param_names(fk);
            let Some(definition) = FunctionRegistry::get_definition(fk) else {
                return String::new();
            };

            let required = definition.required_parameters();
            let total = required + definition.optional_parameters();
            let params: Vec<String> = (0..total)
                .map(|i| {
                    let name = param_names.get(i).copied().unwrap_or("var");
                    let optional = if i >= required { "?" } else { "" };
                    let typ = definition.param_type_str(i);
                    format!("{name}{optional}: {typ}")
                })
                .collect();

            format!(
                "({}) -> {}",
                params.join(", "),
                definition.return_type_str()
            )
        }
        FunctionKind::Closure(c) => match c {
            ClosureFunction::All
            | ClosureFunction::None
            | ClosureFunction::Some
            | ClosureFunction::One => {
                "<T>(array: T[], callback: Callback<T, boolean>) -> boolean".to_string()
            }
            ClosureFunction::Filter => {
                "<T>(array: T[], callback: Callback<T, boolean>) -> T[]".to_string()
            }
            ClosureFunction::Map => {
                "<T, U>(array: T[], callback: Callback<T, U>) -> U[]".to_string()
            }
            ClosureFunction::FlatMap => {
                "<T, U>(array: T[], callback: Callback<T, U[]>) -> U[]".to_string()
            }
            ClosureFunction::Count => {
                "<T>(array: T[], callback: Callback<T, boolean>) -> number".to_string()
            }
        },
    }
}

fn method_info(mk: &MethodKind) -> String {
    let s = match mk {
        MethodKind::DateMethod(dm) => match dm {
            DateMethod::Add => "Adds time to a date",
            DateMethod::Sub => "Subtracts time from a date",
            DateMethod::Set => "Sets a specific unit of time on a date",
            DateMethod::Format => "Formats a date into a string representation",
            DateMethod::StartOf => "Returns the start of a specified time unit",
            DateMethod::EndOf => "Returns the end of a specified time unit",
            DateMethod::Diff => "Calculates the difference between two dates",
            DateMethod::Tz => "Converts a date to a different timezone",
            DateMethod::IsSame => "Checks if two dates are the same",
            DateMethod::IsBefore => "Checks if a date is before another date",
            DateMethod::IsAfter => "Checks if a date is after another date",
            DateMethod::IsSameOrBefore => "Checks if a date is the same as or before another",
            DateMethod::IsSameOrAfter => "Checks if a date is the same as or after another",
            DateMethod::Second => "Gets the seconds of a date",
            DateMethod::Minute => "Gets the minutes of a date",
            DateMethod::Hour => "Gets the hours of a date",
            DateMethod::Day => "Gets the day of the month",
            DateMethod::DayOfYear => "Gets the day of the year",
            DateMethod::Week => "Gets the week of the year",
            DateMethod::Weekday => "Gets the day of the week",
            DateMethod::Month => "Gets the month",
            DateMethod::Quarter => "Gets the quarter",
            DateMethod::Year => "Gets the year",
            DateMethod::Timestamp => "Gets the Unix timestamp",
            DateMethod::OffsetName => "Gets the timezone offset name",
            DateMethod::IsValid => "Checks if a date is valid",
            DateMethod::IsYesterday => "Checks if a date is yesterday",
            DateMethod::IsToday => "Checks if a date is today",
            DateMethod::IsTomorrow => "Checks if a date is tomorrow",
            DateMethod::IsLeapYear => "Checks if the year is a leap year",
        },
    };
    s.to_string()
}

fn method_param_names(mk: &MethodKind) -> Vec<&'static str> {
    match mk {
        MethodKind::DateMethod(dm) => match dm {
            DateMethod::Add | DateMethod::Sub => vec!["amount", "unit"],
            DateMethod::Set => vec!["value", "unit"],
            DateMethod::Format => vec!["format"],
            DateMethod::StartOf | DateMethod::EndOf => vec!["unit"],
            DateMethod::Diff => vec!["otherDate", "unit"],
            DateMethod::Tz => vec!["timezone"],
            DateMethod::IsSame
            | DateMethod::IsBefore
            | DateMethod::IsAfter
            | DateMethod::IsSameOrBefore
            | DateMethod::IsSameOrAfter => vec!["otherDate", "unit"],
            _ => vec![],
        },
    }
}

fn method_signature(mk: &MethodKind) -> (String, Option<VariableType>) {
    let Some(definition) = MethodRegistry::get_definition(mk) else {
        return (String::new(), None);
    };

    let param_names = method_param_names(mk);
    let method_for = definition.param_type(0);
    let required = definition.required_parameters();
    let total = required + definition.optional_parameters();

    let params: Vec<String> = (1..total)
        .map(|i| {
            let name = param_names.get(i - 1).copied().unwrap_or("var");
            let optional = if i >= required { "?" } else { "" };
            let typ = definition.param_type_str(i);
            format!("{name}{optional}: {typ}")
        })
        .collect();

    (
        format!(
            "({}) -> {}",
            params.join(", "),
            definition.return_type_str()
        ),
        method_for,
    )
}
