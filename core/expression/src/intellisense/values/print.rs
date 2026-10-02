use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;

use super::value_set::{Bound, Interval, StringSet, ValueSet};

pub struct DateDay;

impl DateDay {
    const DAY: i64 = 86_400;

    pub fn seconds(text: &str) -> Option<Decimal> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let digits = |range: std::ops::Range<usize>| -> Option<i64> {
            let part = &text[range];
            part.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| part.parse().ok())
                .flatten()
        };
        let (year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
        if !(1..=12).contains(&month) || day < 1 || day > Self::days_in_month(year, month) {
            return None;
        }
        Some(Decimal::from(
            Self::days_from_civil(year, month, day) * Self::DAY,
        ))
    }

    pub fn format(seconds: Decimal) -> Option<String> {
        let total = seconds.to_i64().filter(|_| seconds.fract().is_zero())?;
        if total % Self::DAY != 0 {
            return None;
        }
        let (year, month, day) = Self::civil_from_days(total / Self::DAY);
        (0..=9999)
            .contains(&year)
            .then(|| format!("{year:04}-{month:02}-{day:02}"))
    }

    fn is_leap(year: i64) -> bool {
        (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
    }

    fn days_in_month(year: i64, month: i64) -> i64 {
        match month {
            2 if Self::is_leap(year) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        }
    }

    fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (month + 9) % 12;
        let doy = (153 * mp + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn civil_from_days(days: i64) -> (i64, i64, i64) {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        (year, month, day)
    }

    pub fn example(set: &ValueSet) -> Option<Value> {
        let day = Decimal::from(Self::DAY);
        for interval in set.numbers.intervals() {
            let candidates = match (interval.lo, interval.hi) {
                (Bound::Inclusive(a), _) => vec![Some(a), a.checked_add(day)],
                (Bound::Exclusive(a), _) => vec![a.checked_add(day)],
                (Bound::Unbounded, Bound::Inclusive(b)) => vec![Some(b), b.checked_sub(day)],
                (Bound::Unbounded, Bound::Exclusive(b)) => vec![b.checked_sub(day)],
                (Bound::Unbounded, Bound::Unbounded) => vec![Some(Decimal::ZERO)],
            };
            let found = candidates.into_iter().flatten().find_map(|c| {
                ValueSet::number(c)
                    .is_subset(set)
                    .then(|| Self::format(c))
                    .flatten()
            });
            if let Some(text) = found {
                return Some(Value::String(text));
            }
        }
        set.null.then_some(Value::Null)
    }
}

pub struct CellText;

impl CellText {
    pub fn brief(text: &str) -> String {
        const KEEP: usize = 6;
        let chars: Vec<char> = text.chars().collect();
        let mut quote: Option<char> = None;
        let mut depth = 0usize;
        let mut commas: Vec<(usize, usize)> = Vec::new();
        for (i, &c) in chars.iter().enumerate() {
            match (quote, c) {
                (Some(open), _) if c == open => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'' | '`') => quote = Some(c),
                (None, '[' | '(') => depth += 1,
                (None, ']' | ')') => depth = depth.saturating_sub(1),
                (None, ',') if depth <= 1 => commas.push((i, depth)),
                _ => {}
            }
        }
        if commas.len() < KEEP + 2 {
            return text.to_string();
        }
        let (cut, cut_depth) = commas[KEEP - 1];
        let more = commas.len() + 1 - KEEP;
        let head: String = chars[..cut].iter().collect();
        match cut_depth > 0 && text.trim_end().ends_with(']') {
            true => format!("{head}, … +{more} more]"),
            false => format!("{head}, … +{more} more"),
        }
    }

    pub fn of(set: &ValueSet, domain: &ValueSet, dated: bool) -> Option<String> {
        let wanted = set.intersect(domain);
        if domain.difference(&wanted).is_empty() {
            return Some(String::new());
        }
        if wanted.is_empty() {
            return None;
        }
        let positive = Self::positive(&wanted, dated);
        let negative = Self::negative(&domain.difference(&wanted), wanted.other, dated);
        match (positive, negative) {
            (Some(p), Some(n)) if n.len() < p.len() => Some(n),
            (Some(p), _) => Some(p),
            (None, n) => n,
        }
    }

    fn positive(set: &ValueSet, dated: bool) -> Option<String> {
        if set.other || (dated && !set.strings.is_empty()) {
            return None;
        }
        let mut tokens: Vec<String> = Vec::new();
        let mut compound = false;
        if !set.numbers.is_empty() {
            if set.numbers.is_all() {
                return None;
            }
            for interval in set.numbers.intervals() {
                let (token, joined) = Self::interval(interval, dated)?;
                if joined {
                    if compound || !tokens.is_empty() {
                        return None;
                    }
                    compound = true;
                }
                tokens.push(token);
            }
        }
        match &set.strings {
            StringSet::Finite(values) => {
                for value in values {
                    tokens.push(Self::string(value)?);
                }
            }
            StringSet::CoFinite(_) => return None,
        }
        if set.bools & ValueSet::TRUE != 0 {
            tokens.push("true".to_string());
        }
        if set.bools & ValueSet::FALSE != 0 {
            tokens.push("false".to_string());
        }
        if set.null {
            tokens.push("null".to_string());
        }
        (!tokens.is_empty()).then(|| tokens.join(", "))
    }

    fn negative(excluded: &ValueSet, other: bool, dated: bool) -> Option<String> {
        if excluded.other || (dated && !excluded.strings.is_empty()) {
            return None;
        }
        let mut points: Vec<String> = Vec::new();
        for interval in excluded.numbers.intervals() {
            match (interval.lo, interval.hi) {
                (Bound::Inclusive(a), Bound::Inclusive(b)) if a == b => {
                    points.push(Self::number(a, dated)?)
                }
                _ => return None,
            }
        }
        match &excluded.strings {
            StringSet::Finite(values) => {
                for value in values {
                    points.push(Self::string(value)?);
                }
            }
            StringSet::CoFinite(_) => return None,
        }
        if excluded.bools & ValueSet::TRUE != 0 {
            points.push("true".to_string());
        }
        if excluded.bools & ValueSet::FALSE != 0 {
            points.push("false".to_string());
        }
        if excluded.null {
            points.push("null".to_string());
        }
        match points.as_slice() {
            [] => None,
            [single] => Some(format!("!= {single}")),
            _ if other => Some(
                points
                    .iter()
                    .map(|point| format!("!= {point}"))
                    .collect::<Vec<_>>()
                    .join(" and "),
            ),
            _ => Some(format!("not in [{}]", points.join(", "))),
        }
    }

    fn interval(interval: &Interval, dated: bool) -> Option<(String, bool)> {
        let n = |d: Decimal| Self::number(d, dated);
        Some(match (interval.lo, interval.hi) {
            (Bound::Inclusive(a), Bound::Inclusive(b)) if a == b => (n(a)?, false),
            (Bound::Unbounded, Bound::Inclusive(b)) => (format!("<= {}", n(b)?), false),
            (Bound::Unbounded, Bound::Exclusive(b)) => (format!("< {}", n(b)?), false),
            (Bound::Inclusive(a), Bound::Unbounded) => (format!(">= {}", n(a)?), false),
            (Bound::Exclusive(a), Bound::Unbounded) => (format!("> {}", n(a)?), false),
            (lo, hi) if dated => {
                let lower = match lo {
                    Bound::Inclusive(a) => format!(">= {}", n(a)?),
                    Bound::Exclusive(a) => format!("> {}", n(a)?),
                    Bound::Unbounded => return None,
                };
                let upper = match hi {
                    Bound::Inclusive(b) => format!("<= {}", n(b)?),
                    Bound::Exclusive(b) => format!("< {}", n(b)?),
                    Bound::Unbounded => return None,
                };
                (format!("{lower} and {upper}"), true)
            }
            (lo, hi) => {
                let (open, a) = match lo {
                    Bound::Inclusive(a) => ('[', a),
                    Bound::Exclusive(a) => ('(', a),
                    Bound::Unbounded => ('(', Decimal::ZERO),
                };
                let (close, b) = match hi {
                    Bound::Inclusive(b) => (']', b),
                    Bound::Exclusive(b) => (')', b),
                    Bound::Unbounded => (')', Decimal::ZERO),
                };
                (format!("{open}{}..{}{close}", n(a)?, n(b)?), false)
            }
        })
    }

    fn number(d: Decimal, dated: bool) -> Option<String> {
        if dated {
            return DateDay::format(d).and_then(|text| Self::string(&text));
        }
        Some(d.normalize().to_string())
    }

    pub fn string(s: &str) -> Option<String> {
        match (s.contains('"'), s.contains('\'')) {
            (false, _) => Some(format!("\"{s}\"")),
            (true, false) => Some(format!("'{s}'")),
            (true, true) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intellisense::values::cell::CellConstraint;
    use crate::intellisense::IntelliSense;
    use std::rc::Rc;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).expect("decimal")
    }

    fn round_trip_in(set: &ValueSet, domain: &ValueSet, dated: bool) {
        let text = CellText::of(set, domain, dated).expect("printable");
        let parsed = match CellConstraint::parse(&mut IntelliSense::new(), &text, true, true, dated)
        {
            CellConstraint::Any => ValueSet::all(),
            CellConstraint::Known(set) => set,
            CellConstraint::Opaque(_) => panic!("{text} parsed as opaque"),
        };
        assert_eq!(parsed.intersect(domain), set.intersect(domain), "{text}");
    }

    fn round_trip(set: &ValueSet, domain: &ValueSet) {
        round_trip_in(set, domain, false);
    }

    #[test]
    fn prints_numbers() {
        let domain = ValueSet::numbers(super::super::value_set::NumberSet::all());
        let below = ValueSet::numbers(super::super::value_set::NumberSet::from_intervals(vec![
            Interval::new(Bound::Unbounded, Bound::Exclusive(dec("18"))),
        ]));
        assert_eq!(
            CellText::of(&below, &domain, false).as_deref(),
            Some("< 18")
        );
        round_trip(&below, &domain);
        let band = ValueSet::numbers(super::super::value_set::NumberSet::from_intervals(vec![
            Interval::new(Bound::Exclusive(dec("30")), Bound::Inclusive(dec("65.5"))),
            Interval::point(dec("100")),
        ]));
        assert_eq!(
            CellText::of(&band, &domain, false).as_deref(),
            Some("(30..65.5], 100")
        );
        round_trip(&band, &domain);
        assert_eq!(CellText::of(&domain, &domain, false).as_deref(), Some(""));
    }

    #[test]
    fn prints_strings_and_complements() {
        let dict = ValueSet::strings([Rc::from("gold"), Rc::from("silver"), Rc::from("bronze")]);
        let bronze = ValueSet::string("bronze");
        assert_eq!(
            CellText::of(&bronze, &dict, false).as_deref(),
            Some("\"bronze\"")
        );
        round_trip(&bronze, &dict);

        let any_string = ValueSet {
            strings: StringSet::all(),
            ..ValueSet::empty()
        };
        let rest = any_string.difference(&ValueSet::strings([Rc::from("a"), Rc::from("b")]));
        assert_eq!(
            CellText::of(&rest, &any_string, false).as_deref(),
            Some("not in [\"a\", \"b\"]")
        );
        round_trip(&rest, &any_string);

        let nullable = any_string.union(&ValueSet::null());
        assert_eq!(
            CellText::of(&ValueSet::null(), &nullable, false).as_deref(),
            Some("null")
        );
        round_trip(&ValueSet::null(), &nullable);
        assert_eq!(
            CellText::of(&any_string, &nullable, false).as_deref(),
            Some("!= null")
        );
        round_trip(&any_string, &nullable);
    }

    #[test]
    fn prints_bools() {
        let domain = ValueSet::bool(true).union(&ValueSet::bool(false));
        assert_eq!(
            CellText::of(&ValueSet::bool(false), &domain, false).as_deref(),
            Some("false")
        );
        round_trip(&ValueSet::bool(false), &domain);
    }

    #[test]
    fn dates_round_trip() {
        let jan = DateDay::seconds("2024-01-01").expect("date");
        assert_eq!(DateDay::format(jan).as_deref(), Some("2024-01-01"));
        assert_eq!(DateDay::seconds("1970-01-01"), Some(Decimal::ZERO));
        assert_eq!(
            DateDay::format(DateDay::seconds("2024-02-29").expect("leap")).as_deref(),
            Some("2024-02-29")
        );
        assert!(DateDay::seconds("2023-02-29").is_none());
        assert!(DateDay::seconds("2024-13-01").is_none());
        assert!(DateDay::seconds("2024-01-01T00:00:00Z").is_none());

        let domain = ValueSet::numbers(super::super::value_set::NumberSet::all());
        let jun = DateDay::seconds("2024-06-01").expect("date");
        let half = ValueSet::numbers(super::super::value_set::NumberSet::from_intervals(vec![
            Interval::new(Bound::Inclusive(jan), Bound::Exclusive(jun)),
        ]));
        assert_eq!(
            CellText::of(&half, &domain, true).as_deref(),
            Some(">= \"2024-01-01\" and < \"2024-06-01\"")
        );
        round_trip_in(&half, &domain, true);
        assert_eq!(
            DateDay::example(&half),
            Some(Value::String("2024-01-01".into()))
        );
    }

    #[test]
    fn brief_shortens_long_value_lists() {
        let codes: Vec<String> = (0..20).map(|i| format!("\"C{i:02}\"")).collect();
        let listed = format!("not in [{}]", codes.join(", "));
        assert_eq!(
            CellText::brief(&listed),
            "not in [\"C00\", \"C01\", \"C02\", \"C03\", \"C04\", \"C05\", … +14 more]"
        );
        assert_eq!(
            CellText::brief(&codes.join(", ")),
            "\"C00\", \"C01\", \"C02\", \"C03\", \"C04\", \"C05\", … +14 more"
        );
        assert_eq!(CellText::brief("\"a, b\", \"c\""), "\"a, b\", \"c\"");
        assert_eq!(CellText::brief("[1..2), > 5"), "[1..2), > 5");
    }
}
