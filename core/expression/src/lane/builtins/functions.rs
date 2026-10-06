use crate::functions::{
    DateMethod as M, DeprecatedFunction as D, FunctionKind, InternalFunction as F, MethodKind,
};
use crate::lane::builtins::arrays::Arrays;
use crate::lane::builtins::convert::Convert;
use crate::lane::builtins::dates::Dates;
use crate::lane::builtins::legacy::Legacy;
use crate::lane::builtins::math::Math;
use crate::lane::builtins::text::Text;
use crate::lane::builtins::{Arg, Builtin, Fail, Out};
use crate::lane::exec::{CallFault, Fault};
use crate::lane::ops::Ops;
use crate::vm::VMError;
use smallvec::SmallVec;
use std::borrow::Cow;

pub(crate) struct Builtins;

impl Builtins {
    pub(crate) fn of(kind: &FunctionKind) -> Option<&'static Builtin> {
        let function = match kind {
            FunctionKind::Internal(function) => function,
            FunctionKind::Deprecated(function) => {
                return Some(match function {
                    D::Date => &Legacy::DATE,
                    D::Time => &Legacy::TIME,
                    D::Duration => &Legacy::DURATION,
                    D::Year => &Legacy::YEAR,
                    D::DayOfWeek => &Legacy::DAY_OF_WEEK,
                    D::DayOfMonth => &Legacy::DAY_OF_MONTH,
                    D::DayOfYear => &Legacy::DAY_OF_YEAR,
                    D::WeekOfYear => &Legacy::WEEK_OF_YEAR,
                    D::MonthOfYear => &Legacy::MONTH_OF_YEAR,
                    D::MonthString => &Legacy::MONTH_STRING,
                    D::DateString => &Legacy::DATE_STRING,
                    D::WeekdayString => &Legacy::WEEKDAY_STRING,
                    D::StartOf => &Legacy::START_OF,
                    D::EndOf => &Legacy::END_OF,
                })
            }
            FunctionKind::Closure(_) => return None,
        };
        Some(match function {
            F::Len => &Text::LEN,
            F::Contains => &Text::CONTAINS,
            F::Upper => &Text::UPPER,
            F::Lower => &Text::LOWER,
            F::Trim => &Text::TRIM,
            F::StartsWith => &Text::STARTS_WITH,
            F::EndsWith => &Text::ENDS_WITH,
            F::Matches => &Text::MATCHES,
            F::Extract => &Text::EXTRACT,
            F::FuzzyMatch => &Text::FUZZY_MATCH,
            F::Split => &Text::SPLIT,
            F::Abs => &Math::ABS,
            F::Sum => &Math::SUM,
            F::Avg => &Math::AVG,
            F::Min => &Math::MIN,
            F::Max => &Math::MAX,
            F::Rand => &Math::RAND,
            F::Median => &Math::MEDIAN,
            F::Mode => &Math::MODE,
            F::Floor => &Math::FLOOR,
            F::Ceil => &Math::CEIL,
            F::Round => &Math::ROUND,
            F::Trunc => &Math::TRUNC,
            F::Flatten => &Arrays::FLATTEN,
            F::Merge => &Arrays::MERGE,
            F::MergeDeep => &Arrays::MERGE_DEEP,
            F::Keys => &Arrays::KEYS,
            F::Values => &Arrays::VALUES,
            F::IsNumeric => &Convert::IS_NUMERIC,
            F::String => &Convert::STRING,
            F::Number => &Convert::NUMBER,
            F::Bool => &Convert::BOOL,
            F::Type => &Convert::TYPE,
            F::Date => &Convert::DATE,
        })
    }

    pub(crate) fn method(kind: &MethodKind) -> &'static Builtin {
        let MethodKind::DateMethod(method) = kind;
        match method {
            M::Add => &Dates::ADD,
            M::Sub => &Dates::SUB,
            M::Set => &Dates::SET,
            M::Format => &Dates::FORMAT,
            M::StartOf => &Dates::START_OF,
            M::EndOf => &Dates::END_OF,
            M::Diff => &Dates::DIFF,
            M::Tz => &Dates::TZ,
            M::IsSame => &Dates::IS_SAME,
            M::IsBefore => &Dates::IS_BEFORE,
            M::IsAfter => &Dates::IS_AFTER,
            M::IsSameOrBefore => &Dates::IS_SAME_OR_BEFORE,
            M::IsSameOrAfter => &Dates::IS_SAME_OR_AFTER,
            M::Second => &Dates::SECOND,
            M::Minute => &Dates::MINUTE,
            M::Hour => &Dates::HOUR,
            M::Day => &Dates::DAY,
            M::DayOfYear => &Dates::DAY_OF_YEAR,
            M::Week => &Dates::WEEK,
            M::Weekday => &Dates::WEEKDAY,
            M::Month => &Dates::MONTH,
            M::Quarter => &Dates::QUARTER,
            M::Year => &Dates::YEAR,
            M::Timestamp => &Dates::TIMESTAMP,
            M::OffsetName => &Dates::OFFSET_NAME,
            M::IsValid => &Dates::IS_VALID,
            M::IsYesterday => &Dates::IS_YESTERDAY,
            M::IsToday => &Dates::IS_TODAY,
            M::IsTomorrow => &Dates::IS_TOMORROW,
            M::IsLeapYear => &Dates::IS_LEAP_YEAR,
        }
    }

    fn attempt(builtin: &Builtin, args: &[Arg]) -> Option<Result<Out, Fail>> {
        builtin.overloads.iter().find_map(|overload| overload(args))
    }

    #[cold]
    #[inline(never)]
    fn textual(builtin: &Builtin, args: &[Arg]) -> Option<Result<Out, Fail>> {
        let texts: SmallVec<[Option<Cow<str>>; 4]> = args
            .iter()
            .map(|a| a.date().and_then(|_| a.text()))
            .collect();
        if texts.iter().all(Option::is_none) {
            return None;
        }
        [1, 0].into_iter().find_map(|from| {
            let converted: SmallVec<[Arg; 4]> = args
                .iter()
                .zip(&texts)
                .enumerate()
                .map(|(i, (a, text))| match text {
                    Some(text) if i >= from => Arg::Str(text),
                    _ => *a,
                })
                .collect();
            Self::attempt(builtin, &converted)
        })
    }

    #[inline]
    fn resolve(builtin: &Builtin, args: &[Arg]) -> Result<Out, Fail> {
        Self::attempt(builtin, args)
            .or_else(|| Self::textual(builtin, args))
            .unwrap_or_else(|| Err((builtin.fail)(args)))
    }

    pub(crate) fn call_method(
        builtin: &Builtin,
        kind: &MethodKind,
        args: &[Arg],
    ) -> Result<Out, VMError> {
        Self::resolve(builtin, args)
            .map_err(|message| Ops::error("CallMethod", format!("Method `{kind}` failed: {message}")))
    }

    #[inline]
    pub(crate) fn call_hinted(
        builtin: &'static Builtin,
        kind: &FunctionKind,
        args: &[Arg],
        hint: &mut usize,
    ) -> Result<Out, Fault> {
        let failed = |message: Fail| CallFault::message(kind, message);
        if let Some(overload) = builtin.overloads.get(*hint) {
            if let Some(result) = overload(args) {
                return result.map_err(failed);
            }
        }
        for (index, overload) in builtin.overloads.iter().enumerate() {
            if let Some(result) = overload(args) {
                *hint = index;
                return result.map_err(failed);
            }
        }
        match Self::textual(builtin, args) {
            Some(result) => result.map_err(failed),
            None => Err(CallFault::fail(kind, builtin, args)),
        }
    }

    pub(crate) fn raw(builtin: &Builtin, args: &[Arg]) -> Result<Out, Fail> {
        Self::resolve(builtin, args)
    }

    pub(crate) fn call(
        builtin: &Builtin,
        kind: &FunctionKind,
        args: &[Arg],
    ) -> Result<Out, VMError> {
        Self::resolve(builtin, args).map_err(|message| {
            Ops::error(
                "CallFunction",
                format!("Function `{kind}` failed: {message}"),
            )
        })
    }
}
