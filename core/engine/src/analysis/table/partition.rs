use std::collections::BTreeSet;
use std::rc::Rc;

use ahash::HashMap;
use rust_decimal::Decimal;

use super::value_set::{Bound, Interval, NumberSet, StringSet, ValueSet};

pub(super) struct Points(Vec<Decimal>);

impl Points {
    pub(super) fn new<'a>(intervals: impl Iterator<Item = &'a Interval>) -> Self {
        let mut points: Vec<Decimal> = intervals
            .flat_map(|i| [Self::value(i.lo), Self::value(i.hi)])
            .flatten()
            .collect();
        points.sort_unstable();
        points.dedup();
        Self(points)
    }

    fn value(bound: Bound) -> Option<Decimal> {
        match bound {
            Bound::Unbounded => None,
            Bound::Inclusive(x) | Bound::Exclusive(x) => Some(x),
        }
    }

    pub(super) fn pieces(&self) -> usize {
        2 * self.0.len() + 1
    }

    fn piece(&self, piece: usize) -> Interval {
        match piece % 2 {
            1 => Interval::point(self.0[piece / 2]),
            _ => Interval::new(
                match piece {
                    0 => Bound::Unbounded,
                    _ => Bound::Exclusive(self.0[piece / 2 - 1]),
                },
                match self.0.get(piece / 2) {
                    Some(&p) => Bound::Exclusive(p),
                    None => Bound::Unbounded,
                },
            ),
        }
    }

    pub(super) fn representative(&self, piece: usize) -> Option<Decimal> {
        let points = &self.0;
        match (piece % 2, points.len()) {
            (1, _) => Some(points[piece / 2]),
            (_, 0) => Some(Decimal::ZERO),
            _ if piece == 0 => points[0].checked_sub(Decimal::ONE),
            _ if piece / 2 == points.len() => points[points.len() - 1].checked_add(Decimal::ONE),
            _ => Interval::midpoint(points[piece / 2 - 1], points[piece / 2]),
        }
    }

    pub(super) fn range(&self, interval: &Interval) -> Option<(usize, usize)> {
        let position = |x: Decimal| {
            let p = self.0.partition_point(|p| *p < x);
            (p, self.0.get(p) == Some(&x))
        };
        let first = match interval.lo {
            Bound::Unbounded => 0,
            Bound::Inclusive(x) | Bound::Exclusive(x) => match (position(x), interval.lo) {
                ((p, false), _) => 2 * p,
                ((p, true), Bound::Inclusive(_)) => 2 * p + 1,
                ((p, true), _) => 2 * p + 2,
            },
        };
        let last = match interval.hi {
            Bound::Unbounded => 2 * self.0.len(),
            Bound::Inclusive(x) | Bound::Exclusive(x) => match (position(x), interval.hi) {
                ((p, false), _) => 2 * p,
                ((p, true), Bound::Inclusive(_)) => 2 * p + 1,
                ((p, true), _) => 2 * p,
            },
        };
        (first <= last).then_some((first, last))
    }
}

pub(super) struct Partition {
    atoms: Vec<ValueSet>,
    strings: HashMap<Rc<str>, usize>,
    string_rest: Option<usize>,
    points: Points,
    pieces: Vec<Option<usize>>,
    bools: [Option<usize>; 2],
    null: Option<usize>,
    other: Option<usize>,
}

impl Partition {
    pub(super) fn groups(region: &ValueSet, sets: &[ValueSet]) -> Vec<(ValueSet, Vec<usize>)> {
        let partition = Self::new(region, sets);
        let mut touched: Vec<Vec<usize>> = vec![Vec::new(); partition.atoms.len()];
        for (id, set) in sets.iter().enumerate() {
            for atom in partition.touching(set) {
                if touched[atom].last() != Some(&id) {
                    touched[atom].push(id);
                }
            }
        }
        let mut index: HashMap<Vec<usize>, usize> = HashMap::default();
        let mut members: Vec<(Vec<usize>, Vec<usize>)> = Vec::new();
        for (atom, ids) in touched.into_iter().enumerate() {
            let next = members.len();
            let slot = *index.entry(ids.clone()).or_insert(next);
            if slot == next {
                members.push((ids, Vec::new()));
            }
            members[slot].1.push(atom);
        }
        members
            .into_iter()
            .map(|(ids, atoms)| (Self::union(atoms.iter().map(|&a| &partition.atoms[a])), ids))
            .collect()
    }

    fn new(region: &ValueSet, sets: &[ValueSet]) -> Self {
        let mut partition = Self {
            atoms: Vec::new(),
            strings: HashMap::default(),
            string_rest: None,
            points: Points(Vec::new()),
            pieces: Vec::new(),
            bools: [None, None],
            null: None,
            other: None,
        };
        let mut keys: BTreeSet<Rc<str>> = BTreeSet::new();
        for set in sets {
            match &set.strings {
                StringSet::Finite(s) | StringSet::CoFinite(s) => keys.extend(s.iter().cloned()),
            }
        }
        for key in keys {
            let single = StringSet::Finite(BTreeSet::from([key.clone()]));
            if !region.strings.intersect(&single).is_empty() {
                partition.strings.insert(key, partition.atoms.len());
                partition.atoms.push(ValueSet {
                    strings: single,
                    ..ValueSet::empty()
                });
            }
        }
        let listed = StringSet::Finite(partition.strings.keys().cloned().collect());
        let rest = region.strings.intersect(&listed.complement());
        if !rest.is_empty() {
            partition.string_rest = Some(partition.atoms.len());
            partition.atoms.push(ValueSet {
                strings: rest,
                ..ValueSet::empty()
            });
        }
        let points = Points::new(sets.iter().flat_map(|set| set.numbers.intervals().iter()));
        for piece in 0..points.pieces() {
            let numbers = region
                .numbers
                .intersect(&NumberSet::from_intervals(vec![points.piece(piece)]));
            partition.pieces.push((!numbers.is_empty()).then(|| {
                partition.atoms.push(ValueSet::numbers(numbers));
                partition.atoms.len() - 1
            }));
        }
        partition.points = points;
        for (bit, slot) in [ValueSet::FALSE, ValueSet::TRUE].into_iter().enumerate() {
            if region.bools & slot != 0 {
                partition.bools[bit] = Some(partition.atoms.len());
                partition.atoms.push(ValueSet {
                    bools: slot,
                    ..ValueSet::empty()
                });
            }
        }
        if region.null {
            partition.null = Some(partition.atoms.len());
            partition.atoms.push(ValueSet::null());
        }
        if region.other {
            partition.other = Some(partition.atoms.len());
            partition.atoms.push(ValueSet {
                other: true,
                ..ValueSet::empty()
            });
        }
        partition
    }

    fn touching(&self, set: &ValueSet) -> Vec<usize> {
        let mut out = Vec::new();
        match &set.strings {
            StringSet::Finite(keys) => out.extend(keys.iter().filter_map(|k| self.strings.get(k))),
            StringSet::CoFinite(excluded) => {
                out.extend(
                    self.strings
                        .iter()
                        .filter(|(k, _)| !excluded.contains(*k))
                        .map(|(_, &a)| a),
                );
                out.extend(self.string_rest);
            }
        }
        for interval in set.numbers.intervals() {
            if let Some((first, last)) = self.points.range(interval) {
                out.extend(self.pieces[first..=last].iter().flatten());
            }
        }
        for (bit, slot) in [ValueSet::FALSE, ValueSet::TRUE].into_iter().enumerate() {
            if set.bools & slot != 0 {
                out.extend(self.bools[bit]);
            }
        }
        if set.null {
            out.extend(self.null);
        }
        if set.other {
            out.extend(self.other);
        }
        out
    }

    fn union<'a>(atoms: impl Iterator<Item = &'a ValueSet>) -> ValueSet {
        let mut strings: BTreeSet<Rc<str>> = BTreeSet::new();
        let mut rest: Option<StringSet> = None;
        let mut intervals: Vec<Interval> = Vec::new();
        let mut out = ValueSet::empty();
        for atom in atoms {
            match &atom.strings {
                StringSet::Finite(s) => strings.extend(s.iter().cloned()),
                co => rest = Some(co.clone()),
            }
            intervals.extend(atom.numbers.intervals().iter().copied());
            out.bools |= atom.bools;
            out.null |= atom.null;
            out.other |= atom.other;
        }
        out.strings = match rest {
            Some(rest) => rest.union(&StringSet::Finite(strings)),
            None => StringSet::Finite(strings),
        };
        out.numbers = NumberSet::from_intervals(intervals);
        out
    }
}
