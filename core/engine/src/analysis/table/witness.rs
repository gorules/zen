use std::rc::Rc;

use rust_decimal::Decimal;

use super::cell::CellConstraint;
use super::index::RowIndex;
use super::value_set::{Bound, StringSet, ValueSet};
use super::verify::VerifyTable;

#[derive(Clone)]
enum Point {
    Number(Decimal),
    String(Rc<str>),
    Bool(u8),
    Null,
    Other,
}

const UNLISTED: &str = "\u{0}unlisted";

impl Point {
    fn within(&self, set: &ValueSet) -> bool {
        match self {
            Point::Number(x) => set.numbers.contains(*x),
            Point::String(s) => match &set.strings {
                StringSet::Finite(keys) => keys.contains(s),
                StringSet::CoFinite(excluded) => !excluded.contains(s),
            },
            Point::Bool(bit) => set.bools & bit != 0,
            Point::Null => set.null,
            Point::Other => set.other,
        }
    }

    fn candidates(set: &ValueSet, index: &RowIndex, col: usize) -> Vec<Point> {
        let mut out = Vec::new();
        if let StringSet::CoFinite(_) = &set.strings {
            out.push(Point::String(Rc::from(UNLISTED)));
        }
        let intervals = set.numbers.intervals();
        let ends = [intervals.last(), intervals.first()];
        for interval in ends.into_iter().flatten() {
            for x in index.inner_points(col, interval) {
                if set.numbers.contains(x) {
                    out.push(Point::Number(x));
                }
            }
            let fallback = match (interval.lo, interval.hi) {
                (_, Bound::Inclusive(h)) => Some(h),
                (Bound::Inclusive(l), _) => Some(l),
                (Bound::Unbounded, Bound::Exclusive(h)) => Some(h - Decimal::ONE),
                (Bound::Exclusive(l), Bound::Unbounded) => Some(l + Decimal::ONE),
                (Bound::Unbounded, Bound::Unbounded) => Some(Decimal::ZERO),
                (Bound::Exclusive(l), Bound::Exclusive(h)) => Some((l + h) / Decimal::TWO),
            };
            if let Some(x) = fallback.filter(|x| set.numbers.contains(*x)) {
                out.push(Point::Number(x));
            }
        }
        if let StringSet::Finite(keys) = &set.strings {
            out.extend(keys.iter().next().map(|k| Point::String(k.clone())));
            out.extend(keys.iter().next_back().map(|k| Point::String(k.clone())));
        }
        for bit in [ValueSet::TRUE, ValueSet::FALSE] {
            if set.bools & bit != 0 {
                out.push(Point::Bool(bit));
            }
        }
        if set.null {
            out.push(Point::Null);
        }
        if set.other {
            out.push(Point::Other);
        }
        out
    }
}

impl VerifyTable<'_> {
    pub(super) fn escapes(
        cells: &[Vec<CellConstraint>],
        row: usize,
        region: &[ValueSet],
        earlier: &[usize],
        index: &RowIndex,
    ) -> bool {
        let candidates: Vec<Vec<Point>> = region
            .iter()
            .enumerate()
            .map(|(col, set)| Point::candidates(set, index, col))
            .collect();
        if candidates.iter().any(Vec::is_empty) {
            return false;
        }
        let depth = candidates.iter().map(Vec::len).max().unwrap_or(0).min(4);
        (0..depth).any(|pick| {
            let witness: Vec<&Point> = candidates
                .iter()
                .map(|options| &options[pick.min(options.len() - 1)])
                .collect();
            !earlier
                .iter()
                .any(|&e| Self::holds(&cells[e], &cells[row], &witness))
        })
    }

    fn holds(earlier: &[CellConstraint], row: &[CellConstraint], witness: &[&Point]) -> bool {
        earlier
            .iter()
            .zip(row)
            .zip(witness)
            .all(|((e, r), point)| match e {
                CellConstraint::Any => true,
                CellConstraint::Known(set) => point.within(set),
                CellConstraint::Opaque(atom) => {
                    matches!(r, CellConstraint::Opaque(own) if own == atom)
                }
            })
    }
}
