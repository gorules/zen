use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::rc::Rc;

use rust_decimal::Decimal;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Bound {
    Unbounded,
    Inclusive(Decimal),
    Exclusive(Decimal),
}

impl Bound {
    fn lo_key(self) -> (i8, Decimal, i8) {
        match self {
            Bound::Unbounded => (-1, Decimal::ZERO, 0),
            Bound::Inclusive(x) => (0, x, 0),
            Bound::Exclusive(x) => (0, x, 1),
        }
    }

    fn hi_key(self) -> (i8, Decimal, i8) {
        match self {
            Bound::Unbounded => (1, Decimal::ZERO, 0),
            Bound::Inclusive(x) => (0, x, 1),
            Bound::Exclusive(x) => (0, x, 0),
        }
    }

    fn flip(self) -> Bound {
        match self {
            Bound::Unbounded => Bound::Unbounded,
            Bound::Inclusive(x) => Bound::Exclusive(x),
            Bound::Exclusive(x) => Bound::Inclusive(x),
        }
    }

    fn value(self) -> Option<Decimal> {
        match self {
            Bound::Unbounded => None,
            Bound::Inclusive(x) | Bound::Exclusive(x) => Some(x),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Interval {
    pub(crate) lo: Bound,
    pub(crate) hi: Bound,
}

impl Interval {
    pub(crate) fn new(lo: Bound, hi: Bound) -> Self {
        Self { lo, hi }
    }

    pub(crate) fn point(x: Decimal) -> Self {
        Self::new(Bound::Inclusive(x), Bound::Inclusive(x))
    }

    fn is_empty(&self) -> bool {
        match (self.lo, self.hi) {
            (Bound::Unbounded, _) | (_, Bound::Unbounded) => false,
            (lo, hi) => {
                let (Some(a), Some(b)) = (lo.value(), hi.value()) else {
                    return false;
                };
                match a.cmp(&b) {
                    Ordering::Less => false,
                    Ordering::Equal => {
                        !(matches!(lo, Bound::Inclusive(_)) && matches!(hi, Bound::Inclusive(_)))
                    }
                    Ordering::Greater => true,
                }
            }
        }
    }

    fn overlaps(&self, other: &Interval) -> bool {
        let lo = if self.lo.lo_key() >= other.lo.lo_key() {
            self.lo
        } else {
            other.lo
        };
        let hi = if self.hi.hi_key() <= other.hi.hi_key() {
            self.hi
        } else {
            other.hi
        };
        !Interval::new(lo, hi).is_empty()
    }

    fn touches(&self, next: &Interval) -> bool {
        match (self.hi, next.lo) {
            (Bound::Unbounded, _) | (_, Bound::Unbounded) => true,
            (hi, lo) => {
                let (Some(x), Some(y)) = (hi.value(), lo.value()) else {
                    return true;
                };
                match x.cmp(&y) {
                    Ordering::Greater => true,
                    Ordering::Less => false,
                    Ordering::Equal => {
                        !(matches!(hi, Bound::Exclusive(_)) && matches!(lo, Bound::Exclusive(_)))
                    }
                }
            }
        }
    }

    fn contains(&self, x: Decimal) -> bool {
        let above = match self.lo {
            Bound::Unbounded => true,
            Bound::Inclusive(l) => x >= l,
            Bound::Exclusive(l) => x > l,
        };
        let below = match self.hi {
            Bound::Unbounded => true,
            Bound::Inclusive(h) => x <= h,
            Bound::Exclusive(h) => x < h,
        };
        above && below
    }

    fn example(&self) -> Decimal {
        let candidates = match (self.lo, self.hi) {
            (Bound::Unbounded, Bound::Unbounded) => vec![Decimal::ZERO],
            (Bound::Inclusive(l), _) => vec![l],
            (Bound::Exclusive(l), Bound::Unbounded) => vec![l.floor() + Decimal::ONE],
            (Bound::Unbounded, Bound::Inclusive(h)) => vec![h],
            (Bound::Unbounded, Bound::Exclusive(h)) => vec![h.ceil() - Decimal::ONE],
            (Bound::Exclusive(l), hi) => {
                let h = hi.value().unwrap_or(l);
                vec![l.floor() + Decimal::ONE, (l + h) / Decimal::TWO]
            }
        };
        candidates
            .into_iter()
            .find(|c| self.contains(*c))
            .unwrap_or(Decimal::ZERO)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub(crate) struct NumberSet {
    intervals: Vec<Interval>,
}

impl NumberSet {
    pub(crate) fn all() -> Self {
        Self {
            intervals: vec![Interval::new(Bound::Unbounded, Bound::Unbounded)],
        }
    }

    pub(crate) fn from_intervals(intervals: Vec<Interval>) -> Self {
        let mut set = Self { intervals };
        set.normalize();
        set
    }

    pub(crate) fn intervals(&self) -> &[Interval] {
        &self.intervals
    }

    pub(crate) fn contains(&self, x: Decimal) -> bool {
        self.intervals.iter().any(|i| i.contains(x))
    }

    fn normalize(&mut self) {
        self.intervals.retain(|i| !i.is_empty());
        self.intervals.sort_by_key(|i| i.lo.lo_key());
        let mut merged: Vec<Interval> = Vec::with_capacity(self.intervals.len());
        for interval in self.intervals.drain(..) {
            match merged.last_mut() {
                Some(last) if last.touches(&interval) => {
                    if interval.hi.hi_key() > last.hi.hi_key() {
                        last.hi = interval.hi;
                    }
                }
                _ => merged.push(interval),
            }
        }
        self.intervals = merged;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    pub(crate) fn is_all(&self) -> bool {
        matches!(
            self.intervals.as_slice(),
            [Interval {
                lo: Bound::Unbounded,
                hi: Bound::Unbounded
            }]
        )
    }

    pub(crate) fn union(&self, other: &Self) -> Self {
        let mut intervals = self.intervals.clone();
        intervals.extend(other.intervals.iter().copied());
        Self::from_intervals(intervals)
    }

    pub(crate) fn intersect(&self, other: &Self) -> Self {
        let mut out = Vec::new();
        for a in &self.intervals {
            for b in &other.intervals {
                let lo = if a.lo.lo_key() >= b.lo.lo_key() {
                    a.lo
                } else {
                    b.lo
                };
                let hi = if a.hi.hi_key() <= b.hi.hi_key() {
                    a.hi
                } else {
                    b.hi
                };
                out.push(Interval::new(lo, hi));
            }
        }
        Self::from_intervals(out)
    }

    fn is_subset(&self, other: &Self) -> bool {
        self.intervals.iter().all(|a| {
            other
                .intervals
                .iter()
                .any(|b| b.lo.lo_key() <= a.lo.lo_key() && a.hi.hi_key() <= b.hi.hi_key())
        })
    }

    fn intersects(&self, other: &Self) -> bool {
        self.intervals
            .iter()
            .any(|a| other.intervals.iter().any(|b| a.overlaps(b)))
    }

    pub(crate) fn complement(&self) -> Self {
        let mut out = Vec::new();
        let mut cursor = Some(Bound::Unbounded);
        for interval in &self.intervals {
            let Some(lo) = cursor else {
                break;
            };
            if interval.lo != Bound::Unbounded {
                out.push(Interval::new(lo, interval.lo.flip()));
            }
            cursor = match interval.hi {
                Bound::Unbounded => None,
                hi => Some(hi.flip()),
            };
        }
        if let Some(lo) = cursor {
            out.push(Interval::new(lo, Bound::Unbounded));
        }
        Self::from_intervals(out)
    }

    fn example(&self) -> Option<Decimal> {
        self.intervals.first().map(Interval::example)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum StringSet {
    Finite(BTreeSet<Rc<str>>),
    CoFinite(BTreeSet<Rc<str>>),
}

impl Default for StringSet {
    fn default() -> Self {
        StringSet::Finite(BTreeSet::new())
    }
}

impl StringSet {
    pub(crate) fn all() -> Self {
        StringSet::CoFinite(BTreeSet::new())
    }

    pub(crate) fn is_empty(&self) -> bool {
        matches!(self, StringSet::Finite(s) if s.is_empty())
    }

    pub(crate) fn is_all(&self) -> bool {
        matches!(self, StringSet::CoFinite(s) if s.is_empty())
    }

    pub(crate) fn complement(&self) -> Self {
        match self {
            StringSet::Finite(s) => StringSet::CoFinite(s.clone()),
            StringSet::CoFinite(s) => StringSet::Finite(s.clone()),
        }
    }

    pub(crate) fn union(&self, other: &Self) -> Self {
        match (self, other) {
            (StringSet::Finite(a), StringSet::Finite(b)) => {
                StringSet::Finite(a.union(b).cloned().collect())
            }
            (StringSet::CoFinite(a), StringSet::CoFinite(b)) => {
                StringSet::CoFinite(a.intersection(b).cloned().collect())
            }
            (StringSet::Finite(f), StringSet::CoFinite(c))
            | (StringSet::CoFinite(c), StringSet::Finite(f)) => {
                StringSet::CoFinite(c.difference(f).cloned().collect())
            }
        }
    }

    pub(crate) fn intersect(&self, other: &Self) -> Self {
        match (self, other) {
            (StringSet::Finite(a), StringSet::Finite(b)) => {
                StringSet::Finite(a.intersection(b).cloned().collect())
            }
            (StringSet::CoFinite(a), StringSet::CoFinite(b)) => {
                StringSet::CoFinite(a.union(b).cloned().collect())
            }
            (StringSet::Finite(f), StringSet::CoFinite(c))
            | (StringSet::CoFinite(c), StringSet::Finite(f)) => {
                StringSet::Finite(f.difference(c).cloned().collect())
            }
        }
    }

    fn is_subset(&self, other: &Self) -> bool {
        match (self, other) {
            (StringSet::Finite(a), StringSet::Finite(b)) => a.iter().all(|s| b.contains(s)),
            (StringSet::Finite(a), StringSet::CoFinite(b)) => a.iter().all(|s| !b.contains(s)),
            (StringSet::CoFinite(_), StringSet::Finite(_)) => false,
            (StringSet::CoFinite(a), StringSet::CoFinite(b)) => b.iter().all(|s| a.contains(s)),
        }
    }

    fn intersects(&self, other: &Self) -> bool {
        match (self, other) {
            (StringSet::Finite(a), StringSet::Finite(b)) => {
                let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
                small.iter().any(|s| large.contains(s))
            }
            (StringSet::CoFinite(_), StringSet::CoFinite(_)) => true,
            (StringSet::Finite(f), StringSet::CoFinite(c))
            | (StringSet::CoFinite(c), StringSet::Finite(f)) => f.iter().any(|s| !c.contains(s)),
        }
    }

    fn example(&self) -> Option<Rc<str>> {
        match self {
            StringSet::Finite(s) => s.iter().next().cloned(),
            StringSet::CoFinite(excluded) => (0..=excluded.len())
                .map(|i| match i {
                    0 => Rc::from("other"),
                    n => Rc::from(format!("other{n}")),
                })
                .find(|candidate: &Rc<str>| !excluded.contains(candidate)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ValueKind {
    Number,
    String,
    Bool,
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub(crate) struct ValueSet {
    pub(crate) numbers: NumberSet,
    pub(crate) strings: StringSet,
    pub(crate) bools: u8,
    pub(crate) null: bool,
    pub(crate) other: bool,
}

impl ValueSet {
    pub(crate) const FALSE: u8 = 1;
    pub(crate) const TRUE: u8 = 2;

    pub(crate) fn empty() -> Self {
        Self::default()
    }

    pub(crate) fn all() -> Self {
        Self {
            numbers: NumberSet::all(),
            strings: StringSet::all(),
            bools: Self::FALSE | Self::TRUE,
            null: true,
            other: true,
        }
    }

    pub(crate) fn scalars() -> Self {
        Self {
            other: false,
            ..Self::all()
        }
    }

    pub(crate) fn numbers(numbers: NumberSet) -> Self {
        Self {
            numbers,
            ..Self::empty()
        }
    }

    pub(crate) fn number(x: Decimal) -> Self {
        Self::numbers(NumberSet::from_intervals(vec![Interval::point(x)]))
    }

    pub(crate) fn string(s: &str) -> Self {
        Self {
            strings: StringSet::Finite(BTreeSet::from([Rc::from(s)])),
            ..Self::empty()
        }
    }

    #[cfg(test)]
    pub(crate) fn strings(values: impl IntoIterator<Item = Rc<str>>) -> Self {
        Self {
            strings: StringSet::Finite(values.into_iter().collect()),
            ..Self::empty()
        }
    }

    pub(crate) fn bool(b: bool) -> Self {
        Self {
            bools: if b { Self::TRUE } else { Self::FALSE },
            ..Self::empty()
        }
    }

    pub(crate) fn null() -> Self {
        Self {
            null: true,
            ..Self::empty()
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.numbers.is_empty()
            && self.strings.is_empty()
            && self.bools == 0
            && !self.null
            && !self.other
    }

    pub(crate) fn is_all(&self) -> bool {
        self.numbers.is_all()
            && self.strings.is_all()
            && self.bools == Self::FALSE | Self::TRUE
            && self.null
            && self.other
    }

    pub(crate) fn union(&self, other: &Self) -> Self {
        Self {
            numbers: self.numbers.union(&other.numbers),
            strings: self.strings.union(&other.strings),
            bools: self.bools | other.bools,
            null: self.null || other.null,
            other: self.other || other.other,
        }
    }

    pub(crate) fn intersect(&self, other: &Self) -> Self {
        Self {
            numbers: self.numbers.intersect(&other.numbers),
            strings: self.strings.intersect(&other.strings),
            bools: self.bools & other.bools,
            null: self.null && other.null,
            other: self.other && other.other,
        }
    }

    pub(crate) fn complement(&self) -> Self {
        Self {
            numbers: self.numbers.complement(),
            strings: self.strings.complement(),
            bools: !self.bools & (Self::FALSE | Self::TRUE),
            null: !self.null,
            other: !self.other,
        }
    }

    pub(crate) fn difference(&self, other: &Self) -> Self {
        self.intersect(&other.complement())
    }

    pub(crate) fn is_subset(&self, other: &Self) -> bool {
        self.bools & !other.bools == 0
            && (!self.null || other.null)
            && (!self.other || other.other)
            && self.strings.is_subset(&other.strings)
            && self.numbers.is_subset(&other.numbers)
    }

    pub(crate) fn intersects(&self, other: &Self) -> bool {
        self.bools & other.bools != 0
            || (self.null && other.null)
            || (self.other && other.other)
            || self.strings.intersects(&other.strings)
            || self.numbers.intersects(&other.numbers)
    }

    pub(crate) fn example(&self, prefer: Option<ValueKind>) -> Option<Value> {
        let order: [ValueKind; 4] = match prefer {
            Some(ValueKind::String) => [
                ValueKind::String,
                ValueKind::Number,
                ValueKind::Bool,
                ValueKind::Null,
            ],
            Some(ValueKind::Bool) => [
                ValueKind::Bool,
                ValueKind::Number,
                ValueKind::String,
                ValueKind::Null,
            ],
            Some(ValueKind::Null) => [
                ValueKind::Null,
                ValueKind::Number,
                ValueKind::String,
                ValueKind::Bool,
            ],
            _ => [
                ValueKind::Number,
                ValueKind::String,
                ValueKind::Bool,
                ValueKind::Null,
            ],
        };
        order
            .into_iter()
            .find_map(|kind| self.example_of(kind))
            .or_else(|| self.other.then(|| Value::Array(Vec::new())))
    }

    fn example_of(&self, kind: ValueKind) -> Option<Value> {
        match kind {
            ValueKind::Number => self.numbers.example().map(decimal_json),
            ValueKind::String => self.strings.example().map(|s| Value::String(s.to_string())),
            ValueKind::Bool => match self.bools {
                0 => None,
                b => Some(Value::Bool(b & Self::TRUE != 0)),
            },
            ValueKind::Null => self.null.then_some(Value::Null),
        }
    }
}

pub(crate) fn decimal_json(d: Decimal) -> Value {
    serde_json::from_str(&d.normalize().to_string()).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).expect("decimal")
    }

    fn range(lo: Bound, hi: Bound) -> ValueSet {
        ValueSet::numbers(NumberSet::from_intervals(vec![Interval::new(lo, hi)]))
    }

    #[test]
    fn number_union_merges_touching_intervals() {
        let a = range(Bound::Unbounded, Bound::Exclusive(dec("30")));
        let b = range(Bound::Inclusive(dec("30")), Bound::Unbounded);
        assert!(a.union(&b).numbers.is_all());

        let c = range(Bound::Unbounded, Bound::Exclusive(dec("30")));
        let d = range(Bound::Exclusive(dec("30")), Bound::Unbounded);
        let joined = c.union(&d);
        assert!(!joined.numbers.is_all());
        assert!(!ValueSet::number(dec("30")).is_subset(&joined));
        assert!(ValueSet::number(dec("31")).is_subset(&joined));
    }

    #[test]
    fn number_complement_round_trips() {
        let a = NumberSet::from_intervals(vec![
            Interval::new(Bound::Inclusive(dec("18")), Bound::Inclusive(dec("30"))),
            Interval::new(Bound::Exclusive(dec("40")), Bound::Unbounded),
        ]);
        let c = a.complement();
        assert_eq!(
            c.intervals(),
            &[
                Interval::new(Bound::Unbounded, Bound::Exclusive(dec("18"))),
                Interval::new(Bound::Exclusive(dec("30")), Bound::Inclusive(dec("40"))),
            ]
        );
        assert_eq!(c.complement(), a);
        assert!(a.intersect(&c).is_empty());
        assert!(a.union(&c).is_all());
    }

    #[test]
    fn reversed_interval_is_empty() {
        let a = range(Bound::Inclusive(dec("5")), Bound::Inclusive(dec("3")));
        assert!(a.is_empty());
        let b = range(Bound::Exclusive(dec("5")), Bound::Inclusive(dec("5")));
        assert!(b.is_empty());
        assert!(!ValueSet::number(dec("5")).is_empty());
    }

    #[test]
    fn strings_finite_and_cofinite() {
        let gold = ValueSet::string("gold");
        let not_gold = gold.complement();
        assert!(!gold.intersects(&not_gold));
        assert!(gold.union(&not_gold).is_all());
        assert!(ValueSet::string("silver").is_subset(&not_gold));
        assert!(ValueSet::null().is_subset(&not_gold));
        let pair = ValueSet::strings([Rc::from("gold"), Rc::from("silver")]);
        assert_eq!(
            pair.difference(&gold).strings,
            StringSet::Finite(BTreeSet::from([Rc::from("silver")]))
        );
    }

    #[test]
    fn examples_prefer_kind_and_stay_inside() {
        let a = range(Bound::Exclusive(dec("18")), Bound::Exclusive(dec("19")));
        let example = a.example(None);
        assert_eq!(example, Some(decimal_json(dec("18.5"))));
        let b = range(Bound::Unbounded, Bound::Exclusive(dec("18")));
        assert_eq!(b.example(None), Some(serde_json::json!(17)));
        let not_gold = ValueSet::string("gold").complement();
        assert_eq!(
            not_gold.example(Some(ValueKind::String)),
            Some(serde_json::json!("other"))
        );
        assert_eq!(ValueSet::null().example(None), Some(Value::Null));
    }
}
