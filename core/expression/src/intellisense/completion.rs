use crate::functions::registry::FunctionRegistry;
use crate::functions::DateMethod;
use crate::functions::{
    ClosureFunction, DeprecatedFunction, FunctionKind, InternalFunction, MethodKind, MethodRegistry,
};
use crate::intellisense::IntelliSenseToken;
use crate::lexer::codes::is_token_type;
use crate::variable::VariableType;
use ahash::HashMap;
use serde::Serialize;
use std::rc::Rc;
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
    #[serde(skip)]
    pub var_type: Option<VariableType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow: Option<&'static str>,
}

pub struct Completions;

impl Completions {
    pub fn from_slot(
        source: &str,
        pos: u32,
        data: &VariableType,
        slot: &crate::slot::Slot,
    ) -> Vec<Completion> {
        use crate::slot::SlotState;
        if slot.suppress_completions
            || matches!(
                slot.state,
                SlotState::Operator | SlotState::Logical | SlotState::InString
            )
        {
            return Vec::new();
        }
        let locals = slot
            .locals
            .iter()
            .map(|local| (Rc::from(local.name.as_str()), local.kind.shallow_clone()))
            .collect::<Vec<_>>();
        let mut items = match slot.state {
            SlotState::Member => {
                Self::build_property(slot.operand.as_ref().unwrap_or(&VariableType::Any))
            }
            SlotState::Path if slot.operand.is_some() => {
                let mut items = Self::build_property(slot.operand.as_ref().unwrap());
                items.retain(|item| item.kind != CompletionKind::Method);
                items
            }
            SlotState::Path => {
                let mut items = Self::build_scope(data, &locals);
                items.retain(|item| {
                    item.kind != CompletionKind::Function && !item.label.starts_with('$')
                });
                items
            }
            _ => Self::build_scope(data, &locals),
        };
        for item in &mut items {
            if let Some(t) = &item.var_type {
                item.follow = match t.unwrap_nullable().0 {
                    VariableType::Object(_) => Some("."),
                    _ if slot.can_chain => Some(" "),
                    _ => None,
                };
            }
        }
        let before = source.get(..pos as usize).unwrap_or(source);
        Self::filter(items, Self::extract_prefix(before))
    }

    pub fn build(
        source: &str,
        pos: u32,
        data: &VariableType,
        tokens: &[IntelliSenseToken],
        locals: &[(Rc<str>, VariableType)],
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
            None => Self::build_scope(data, locals),
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
            for (key, val) in Self::sorted_fields(&obj.borrow()) {
                completions.push(Completion {
                    label: key.to_string(),
                    kind: CompletionKind::Property,
                    detail: val.to_string(),
                    info: String::new(),
                    boost: Some(10),
                    method_for: None,
                    var_type: Some(val.shallow_clone()),
                    follow: None,
                });
            }
        }

        for mk in DateMethod::iter().map(MethodKind::DateMethod) {
            let def = MethodRegistry::get_definition(&mk);
            let applies = def
                .as_ref()
                .and_then(|d| d.param_type(0))
                .map(|pt| match pt {
                    VariableType::Date => {
                        matches!(resolved, VariableType::Date | VariableType::Any)
                    }
                    _ => vt.satisfies(&pt),
                })
                .unwrap_or(false);

            if applies || matches!(vt, VariableType::Any) {
                completions.push(Self::method(mk));
            }
        }

        completions
    }

    pub fn build_scope(data: &VariableType, locals: &[(Rc<str>, VariableType)]) -> Vec<Completion> {
        let mut completions = Vec::new();

        for (name, kind) in locals {
            completions.push(Completion {
                label: name.to_string(),
                kind: CompletionKind::Variable,
                detail: kind.to_string(),
                info: String::new(),
                boost: Some(30),
                method_for: None,
                var_type: Some(kind.shallow_clone()),
                follow: None,
            });
        }

        let resolved_data = match data {
            VariableType::Nullable(inner) => inner.as_ref(),
            other => other,
        };

        if let VariableType::Object(obj) = resolved_data {
            for (key, val) in Self::sorted_fields(&obj.borrow()) {
                completions.push(Completion {
                    label: key.to_string(),
                    kind: CompletionKind::Variable,
                    detail: val.to_string(),
                    info: String::new(),
                    boost: Some(20),
                    method_for: None,
                    var_type: Some(val.shallow_clone()),
                    follow: None,
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
            var_type: None,
            follow: None,
        });

        completions.extend(
            InternalFunction::iter()
                .filter(|f| !f.is_hidden())
                .map(FunctionKind::Internal)
                .chain(ClosureFunction::iter().map(FunctionKind::Closure))
                .chain(
                    FunctionRegistry::host_functions()
                        .into_iter()
                        .map(|(name, _)| FunctionKind::Host(name)),
                )
                .map(|fk| Self::function(fk, None)),
        );

        completions
    }

    fn sorted_fields(fields: &HashMap<Rc<str>, VariableType>) -> Vec<(&Rc<str>, &VariableType)> {
        let mut out: Vec<_> = fields.iter().collect();
        out.sort_by_cached_key(|(key, _)| {
            (key.starts_with('$'), key.to_lowercase(), key.to_string())
        });
        out
    }

    pub(crate) fn function_named(name: &str) -> Option<Completion> {
        FunctionKind::try_from(name)
            .ok()
            .map(|fk| Self::function(fk, None))
    }

    pub(crate) fn method_named(name: &str) -> Option<Completion> {
        MethodKind::try_from(name).ok().map(Self::method)
    }

    fn function(fk: FunctionKind, boost_override: Option<i32>) -> Completion {
        let label = fk.to_string();
        let info = function_info(&fk);
        let detail = function_signature(&fk);
        let boost = boost_override.or(match &fk {
            FunctionKind::Internal(_) => Some(10),
            FunctionKind::Closure(_) => None,
            FunctionKind::Deprecated(_) => Some(-20),
            FunctionKind::Host(_) => Some(5),
        });

        Completion {
            label,
            kind: CompletionKind::Function,
            detail,
            info,
            boost,
            method_for: None,
            var_type: None,
            follow: None,
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
            var_type: None,
            follow: None,
        }
    }

    fn extract_prefix(before_cursor: &str) -> &str {
        let boundary = before_cursor
            .char_indices()
            .rev()
            .find(|(_, c)| !is_token_type!(*c, "alphanumeric"))
            .map(|(i, c)| i + c.len_utf8())
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

        let word_start = trimmed.rfind(|c: char| !is_token_type!(c, "alphanumeric"));
        match word_start {
            Some(i) if trimmed.as_bytes().get(i) == Some(&b'.') => Some(i),
            _ => None,
        }
    }
}

fn function_info(fk: &FunctionKind) -> String {
    let s = match fk {
        FunctionKind::Host(name) => {
            return FunctionRegistry::host_description(name).unwrap_or_default();
        }
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
            InternalFunction::Join => {
                "Joins the items into a string with a separator (`,` by default); null items are skipped"
            }
            InternalFunction::Replace => {
                "Replaces the first match of a regular expression; $1 in the replacement refers to a group"
            }
            InternalFunction::ReplaceAll => {
                "Replaces every match of a regular expression; $1 in the replacement refers to a group"
            }
            InternalFunction::Unaccent => "Removes diacritics: Miletić becomes Miletic",
            InternalFunction::Squish => {
                "Trims the string and collapses every run of whitespace to one space"
            }
            InternalFunction::TokenSort => {
                "Lower-cases the words, drops punctuation and sorts them: the same for any word order"
            }
            InternalFunction::NameKey => {
                "One key for a name: lower-cased, diacritics removed, punctuation dropped, words sorted"
            }
            InternalFunction::Soundex => "American Soundex code of how the text sounds (R163)",
            InternalFunction::JaroWinkler => {
                "Jaro-Winkler similarity from 0 to 1, favouring a shared beginning; also of each string of a list"
            }
            InternalFunction::TokenSetSimilarity => {
                "Share of words two texts have in common, from 0 to 1, ignoring order, case and punctuation; also of each string of a list"
            }
            InternalFunction::Abs => "Returns the absolute value of a number",
            InternalFunction::Sum | InternalFunction::AggregateSum => {
                "Returns the sum of all elements in the input array; sum(items, #.amount, #.ok) adds a projection of each item, optionally filtered, skipping nulls (0 when nothing remains)"
            }
            InternalFunction::Avg | InternalFunction::AggregateAvg => {
                "Calculates the average of all elements in the input array; avg(items, #.amount, #.ok) averages a projection of each item, optionally filtered, skipping nulls (null when nothing remains)"
            }
            InternalFunction::Min | InternalFunction::AggregateMin => {
                "Returns the smallest of the elements in the input array; min(items, #.amount, #.ok) takes a projection of each item, optionally filtered, skipping nulls (null when nothing remains)"
            }
            InternalFunction::Max | InternalFunction::AggregateMax => {
                "Returns the largest of the elements in the input array; max(items, #.amount, #.ok) takes a projection of each item, optionally filtered, skipping nulls (null when nothing remains)"
            }
            InternalFunction::Rand => {
                "Generates a random number between 0 (inclusive) and max (inclusive); every call, and every item of a closure, draws anew from the evaluation's seeded stream"
            }
            InternalFunction::Median | InternalFunction::AggregateMedian => {
                "Calculates the median value of all elements in the input array; median(items, #.amount, #.ok) takes a projection of each item, optionally filtered, skipping nulls"
            }
            InternalFunction::Mode | InternalFunction::AggregateMode => {
                "Finds the most common value of the input array; mode(items, #.status, #.ok) counts a projection of each item (numbers or strings), optionally filtered, skipping nulls"
            }
            InternalFunction::Stddev => {
                "Sample standard deviation of the numbers, nulls skipped (null below two values); stddev(items, #.amount, #.ok) projects and filters items"
            }
            InternalFunction::Variance => {
                "Sample variance of the numbers, nulls skipped (null below two values); variance(items, #.amount, #.ok) projects and filters items"
            }
            InternalFunction::StddevPop => {
                "Population standard deviation of the numbers, nulls skipped (null when empty)"
            }
            InternalFunction::VariancePop => {
                "Population variance of the numbers, nulls skipped (null when empty)"
            }
            InternalFunction::Skew => {
                "Skewness of the numbers, nulls skipped (null below two distinct values)"
            }
            InternalFunction::Kurtosis => {
                "Excess kurtosis of the numbers, nulls skipped (null below two distinct values)"
            }
            InternalFunction::Percentile => {
                "The value at a fraction (0 to 1) of the sorted numbers, interpolated linearly; percentile(items, #.amount, 0.9, #.ok) projects and filters items"
            }
            InternalFunction::PercentileApprox => {
                "The t-digest estimate of the value at a fraction (0 to 1) of the numbers; percentileApprox(items, #.amount, 0.9, #.ok) projects and filters items"
            }
            InternalFunction::CountDistinct => {
                "Counts the distinct non-null values; countDistinct(items, #.merchant, #.ok) projects and filters items"
            }
            InternalFunction::CountDistinctApprox => {
                "Estimates the number of distinct non-null values with HyperLogLog; countDistinctApprox(items, #.merchant, #.ok) projects and filters items"
            }
            InternalFunction::Distinct => {
                "The distinct non-null values in first-seen order; distinct(items, #.merchant, #.ok) projects and filters items"
            }
            InternalFunction::TopK => {
                "The k most frequent non-null values, most frequent first; topK(items, #.merchant, 3, #.ok) projects and filters items"
            }
            InternalFunction::First => {
                "The first non-null item; first(items, #.ok) the first matching item; first(items, #.amount, #.ok) its projection"
            }
            InternalFunction::Last => {
                "The last non-null item; last(items, #.ok) the last matching item; last(items, #.amount, #.ok) its projection"
            }
            InternalFunction::Take => "The first n items of the array",
            InternalFunction::Md5 => "MD5 hash of a string, as lower-case hex",
            InternalFunction::Sha256 => "SHA-256 hash of a string, as lower-case hex",
            InternalFunction::Xxhash64 => "xxHash64 (seed 0) of a string, as 16 lower-case hex digits",
            InternalFunction::JsonParse => "Parses a JSON string into a value",
            InternalFunction::JsonString => "Writes a value as a JSON string",
            InternalFunction::Entries => "The [key, value] pairs of an object",
            InternalFunction::Zip => "Pairs the items of two arrays: [[a0, b0], [a1, b1]], as long as the shorter one",
            InternalFunction::GroupBy => {
                "Groups items by a key into an object of arrays: groupBy(items, #.status); null keys are skipped"
            }
            InternalFunction::Clamp => "Limits a number to a range: clamp(x, low, high)",
            InternalFunction::Sqrt => "Square root of a number",
            InternalFunction::Ln => "Natural logarithm of a positive number",
            InternalFunction::Log => "Logarithm of a positive number, base 10 or the given base",
            InternalFunction::Exp => "e raised to the power of a number",
            InternalFunction::Sign => "-1, 0 or 1 for a negative, zero or positive number",
            InternalFunction::PadStart => {
                "Pads the start of a string to a length, with spaces or the given text"
            }
            InternalFunction::PadEnd => {
                "Pads the end of a string to a length, with spaces or the given text"
            }
            InternalFunction::SplitPart => {
                "The nth part (from 1; negative counts from the end) of a string split by a delimiter, null when missing"
            }
            InternalFunction::ExtractAll => {
                "Every match of a regular expression, or of its first group when it has one"
            }
            InternalFunction::TakeLast => "The last n items of the array",
            InternalFunction::Find => {
                "The first item matching a condition: find(items, #.status == 'declined')"
            }
            InternalFunction::FindLast => {
                "The last item matching a condition: findLast(items, #.status == 'declined')"
            }
            InternalFunction::FindIndex => {
                "The position of the first item matching a condition, -1 when none"
            }
            InternalFunction::FindLastIndex => {
                "The position of the last item matching a condition, -1 when none"
            }
            InternalFunction::Sort => {
                "Sorts numbers, strings or dates, 'asc' (default) or 'desc'; nulls go last"
            }
            InternalFunction::SortBy => {
                "Sorts items by a key: sortBy(items, #.amount, 'desc'); stable, nulls go last"
            }
            InternalFunction::IsEmpty => "True for null, an empty string, array or object",
            InternalFunction::ArgMax => {
                "The item with the largest key: argMax(items, #.amount), or a projection of it: argMax(items, #.id, #.amount, #.ok)"
            }
            InternalFunction::ArgMin => {
                "The item with the smallest key: argMin(items, #.amount), or a projection of it: argMin(items, #.id, #.amount, #.ok)"
            }
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
            InternalFunction::Join => vec!["arr", "separator"],
            InternalFunction::Replace | InternalFunction::ReplaceAll => {
                vec!["str", "pattern", "replacement"]
            }
            InternalFunction::Unaccent
            | InternalFunction::Squish
            | InternalFunction::TokenSort
            | InternalFunction::NameKey
            | InternalFunction::Soundex => vec!["str"],
            InternalFunction::JaroWinkler | InternalFunction::TokenSetSimilarity => {
                vec!["str", "other"]
            }
            InternalFunction::Abs | InternalFunction::Floor | InternalFunction::Ceil => {
                vec!["num"]
            }
            InternalFunction::Sum
            | InternalFunction::Avg
            | InternalFunction::Min
            | InternalFunction::Max
            | InternalFunction::Median
            | InternalFunction::Mode
            | InternalFunction::AggregateSum
            | InternalFunction::AggregateAvg
            | InternalFunction::AggregateMin
            | InternalFunction::AggregateMax
            | InternalFunction::AggregateMedian
            | InternalFunction::AggregateMode
            | InternalFunction::Stddev
            | InternalFunction::Variance
            | InternalFunction::StddevPop
            | InternalFunction::VariancePop
            | InternalFunction::Skew
            | InternalFunction::Kurtosis
            | InternalFunction::CountDistinct
            | InternalFunction::CountDistinctApprox
            | InternalFunction::Distinct
            | InternalFunction::First
            | InternalFunction::Last => vec!["arr"],
            InternalFunction::Percentile | InternalFunction::PercentileApprox => {
                vec!["arr", "fraction"]
            }
            InternalFunction::TopK => vec!["arr", "k"],
            InternalFunction::Md5
            | InternalFunction::Sha256
            | InternalFunction::Xxhash64
            | InternalFunction::JsonParse => vec!["str"],
            InternalFunction::JsonString => vec!["value"],
            InternalFunction::Entries => vec!["obj"],
            InternalFunction::Zip => vec!["arr", "other"],
            InternalFunction::GroupBy => vec!["pairs"],
            InternalFunction::Clamp => vec!["num", "low", "high"],
            InternalFunction::Sqrt
            | InternalFunction::Ln
            | InternalFunction::Exp
            | InternalFunction::Sign => vec!["num"],
            InternalFunction::Log => vec!["num", "base"],
            InternalFunction::PadStart | InternalFunction::PadEnd => vec!["str", "length", "pad"],
            InternalFunction::SplitPart => vec!["str", "delimiter", "part"],
            InternalFunction::ExtractAll => vec!["str", "pattern"],
            InternalFunction::Take | InternalFunction::TakeLast => vec!["arr", "n"],
            InternalFunction::Find
            | InternalFunction::FindLast
            | InternalFunction::FindIndex
            | InternalFunction::FindLastIndex => vec!["arr"],
            InternalFunction::Sort => vec!["arr", "direction"],
            InternalFunction::SortBy => vec!["pairs", "direction"],
            InternalFunction::IsEmpty => vec!["value"],
            InternalFunction::ArgMax | InternalFunction::ArgMin => vec!["pairs"],
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
        FunctionKind::Closure(_) | FunctionKind::Host(_) => vec![],
    }
}

fn function_signature(fk: &FunctionKind) -> String {
    match fk {
        FunctionKind::Internal(_) | FunctionKind::Deprecated(_) | FunctionKind::Host(_) => {
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

            let plain = format!(
                "({}) -> {}",
                params.join(", "),
                definition.return_type_str()
            );
            match fk {
                FunctionKind::Internal(i) => match callback_forms(i) {
                    Some(forms) => format!("{plain} | {forms}"),
                    None => plain,
                },
                _ => plain,
            }
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

fn callback_forms(function: &InternalFunction) -> Option<&'static str> {
    use InternalFunction as F;
    Some(match function {
        F::Sum
        | F::Avg
        | F::Min
        | F::Max
        | F::Median
        | F::Mode
        | F::Stddev
        | F::Variance
        | F::StddevPop
        | F::VariancePop
        | F::Skew
        | F::Kurtosis
        | F::CountDistinct
        | F::CountDistinctApprox
        | F::Distinct => {
            "<T>(array: T[], value: Callback<T, any>, filter?: Callback<T, boolean>)"
        }
        F::Percentile | F::PercentileApprox => {
            "<T>(array: T[], value: Callback<T, number>, fraction: number, filter?: Callback<T, boolean>) -> number"
        }
        F::TopK => {
            "<T, U>(array: T[], value: Callback<T, U>, k: number, filter?: Callback<T, boolean>) -> U[]"
        }
        F::First | F::Last => {
            "<T>(array: T[], filter: Callback<T, boolean>) -> T | <T, U>(array: T[], value: Callback<T, U>, filter: Callback<T, boolean>) -> U"
        }
        F::Find | F::FindLast => "<T>(array: T[], condition: Callback<T, boolean>) -> T",
        F::FindIndex | F::FindLastIndex => {
            "<T>(array: T[], condition: Callback<T, boolean>) -> number"
        }
        F::GroupBy => "<T>(array: T[], key: Callback<T, string>) -> { [key: string]: T[] }",
        F::SortBy => "<T>(array: T[], key: Callback<T, any>, direction?: string) -> T[]",
        F::ArgMax | F::ArgMin => {
            "<T>(array: T[], key: Callback<T, any>) -> T | <T, U>(array: T[], value: Callback<T, U>, key: Callback<T, any>, filter?: Callback<T, boolean>) -> U"
        }
        _ => return None,
    })
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
            DateMethod::Set => vec!["unit", "value"],
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
