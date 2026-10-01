use std::rc::Rc;

use ahash::HashMap;

use super::cell::CellConstraint;
use super::partition::Points;
use rust_decimal::Decimal;

use super::value_set::{Interval, NumberSet, StringSet, ValueSet};

struct Segments {
    pieces: usize,
    nodes: Vec<Vec<usize>>,
    totals: Vec<usize>,
}

impl Segments {
    fn new(pieces: usize) -> Self {
        Self {
            pieces,
            nodes: vec![Vec::new(); 4 * pieces.max(1)],
            totals: vec![0; 4 * pieces.max(1)],
        }
    }

    fn insert(&mut self, (lo, hi): (usize, usize), row: usize) {
        self.insert_at(1, 0, self.pieces - 1, lo, hi, row);
    }

    fn insert_at(
        &mut self,
        node: usize,
        l: usize,
        r: usize,
        lo: usize,
        hi: usize,
        row: usize,
    ) -> usize {
        if hi < l || r < lo {
            return 0;
        }
        let added = if lo <= l && r <= hi {
            self.nodes[node].push(row);
            1
        } else {
            let mid = (l + r) / 2;
            self.insert_at(2 * node, l, mid, lo, hi, row)
                + self.insert_at(2 * node + 1, mid + 1, r, lo, hi, row)
        };
        self.totals[node] += added;
        added
    }

    fn count(&self, (lo, hi): (usize, usize)) -> usize {
        self.count_at(1, 0, self.pieces - 1, lo, hi)
    }

    fn count_at(&self, node: usize, l: usize, r: usize, lo: usize, hi: usize) -> usize {
        if hi < l || r < lo || self.totals[node] == 0 {
            return 0;
        }
        if (lo <= l && r <= hi) || l == r {
            return self.totals[node];
        }
        let mid = (l + r) / 2;
        self.nodes[node].len()
            + self.count_at(2 * node, l, mid, lo, hi)
            + self.count_at(2 * node + 1, mid + 1, r, lo, hi)
    }

    fn query(&self, (lo, hi): (usize, usize), visit: &mut impl FnMut(usize)) {
        self.query_at(1, 0, self.pieces - 1, lo, hi, visit);
    }

    fn query_at(
        &self,
        node: usize,
        l: usize,
        r: usize,
        lo: usize,
        hi: usize,
        visit: &mut impl FnMut(usize),
    ) {
        if hi < l || r < lo || self.totals[node] == 0 {
            return;
        }
        self.nodes[node].iter().for_each(|&row| visit(row));
        if l == r {
            return;
        }
        let mid = (l + r) / 2;
        self.query_at(2 * node, l, mid, lo, hi, visit);
        self.query_at(2 * node + 1, mid + 1, r, lo, hi, visit);
    }
}

struct ColumnIndex {
    strings: HashMap<Rc<str>, Vec<usize>>,
    bools: [Vec<usize>; 2],
    points: Points,
    numbers: Segments,
    wild: Vec<usize>,
}

pub(super) struct RowIndex {
    columns: Vec<ColumnIndex>,
    stamp: Vec<usize>,
    query: usize,
    blocking: bool,
}

enum Keys<'a> {
    Exact(&'a ValueSet),
    Numbers(&'a NumberSet),
    Wild,
    Never,
}

impl RowIndex {
    pub(super) fn new(cells: &[Vec<CellConstraint>], columns: usize) -> Self {
        let index = (0..columns)
            .map(|col| {
                let points =
                    Points::new(cells.iter().flat_map(|row| match Self::keys(&row[col]) {
                        Keys::Numbers(numbers) => numbers.intervals().iter(),
                        _ => [].iter(),
                    }));
                ColumnIndex {
                    strings: HashMap::default(),
                    bools: [Vec::new(), Vec::new()],
                    numbers: Segments::new(points.pieces()),
                    points,
                    wild: Vec::new(),
                }
            })
            .collect();
        Self {
            columns: index,
            stamp: vec![0; cells.len()],
            query: 0,
            blocking: false,
        }
    }

    pub(super) fn blocking(cells: &[Vec<CellConstraint>], columns: usize) -> Self {
        let mut index = Self::new(cells, columns);
        index.blocking = true;
        for (row, row_cells) in cells.iter().enumerate() {
            index.insert(row_cells, row);
        }
        index
    }

    pub(super) fn inner_points(&self, col: usize, interval: &Interval) -> Vec<Decimal> {
        let points = &self.columns[col].points;
        match points.range(interval) {
            Some((first, last)) => {
                vec![points.representative(last), points.representative(first)]
            }
            None => Vec::new(),
        }
    }

    pub(super) fn insert_strings<'a>(
        &mut self,
        col: usize,
        keys: impl IntoIterator<Item = &'a Rc<str>>,
        row: usize,
    ) {
        let column = &mut self.columns[col];
        for key in keys {
            column.strings.entry(key.clone()).or_default().push(row);
        }
    }

    pub(super) fn insert(&mut self, row_cells: &[CellConstraint], row: usize) {
        for (column, cell) in self.columns.iter_mut().zip(row_cells) {
            match Self::keys(cell) {
                Keys::Exact(set) => {
                    if let StringSet::Finite(strings) = &set.strings {
                        for s in strings {
                            column.strings.entry(s.clone()).or_default().push(row);
                        }
                    }
                    for (bit, list) in column.bools.iter_mut().enumerate() {
                        if set.bools & (1 << bit) != 0 {
                            list.push(row);
                        }
                    }
                }
                Keys::Numbers(numbers) => {
                    for interval in numbers.intervals() {
                        if let Some(range) = column.points.range(interval) {
                            column.numbers.insert(range, row);
                        }
                    }
                }
                Keys::Wild => column.wild.push(row),
                Keys::Never if self.blocking => column.wild.push(row),
                Keys::Never => {}
            }
        }
    }

    fn keys(cell: &CellConstraint) -> Keys<'_> {
        match cell {
            CellConstraint::Any => Keys::Wild,
            CellConstraint::Opaque(_) => Keys::Never,
            CellConstraint::Known(set) => Self::set_keys(set),
        }
    }

    fn set_keys(set: &ValueSet) -> Keys<'_> {
        if set.null || set.other {
            return Keys::Wild;
        }
        match &set.strings {
            StringSet::Finite(_) if set.numbers.is_empty() => Keys::Exact(set),
            StringSet::Finite(strings) if strings.is_empty() && set.bools == 0 => {
                Keys::Numbers(&set.numbers)
            }
            _ => Keys::Wild,
        }
    }

    pub(super) fn candidates(&mut self, cells: &[Vec<CellConstraint>], row: usize) -> Vec<usize> {
        let row_cells = &cells[row];
        let driver = row_cells
            .iter()
            .enumerate()
            .filter_map(|(col, cell)| Some((col, self.estimate(col, Self::keys(cell))?)))
            .min_by_key(|(_, size)| *size)
            .map(|(col, _)| col);
        let Some(driver) = driver else {
            return (0..row).collect();
        };
        let mut out: Vec<usize> = Vec::new();
        self.drive(driver, Self::keys(&row_cells[driver]), row, |e| out.push(e));
        out.retain(|&e| {
            row_cells.iter().enumerate().all(|(col, cell)| {
                col == driver
                    || match (Self::keys(cell), &cells[e][col]) {
                        (Keys::Exact(_) | Keys::Numbers(_), CellConstraint::Opaque(_)) => false,
                        (Keys::Exact(set), CellConstraint::Known(earlier)) => {
                            earlier.intersects(set)
                        }
                        (Keys::Numbers(_), CellConstraint::Known(earlier)) => {
                            cell.known_set().is_some_and(|own| earlier.intersects(&own))
                        }
                        _ => true,
                    }
            })
        });
        out.sort_unstable();
        out
    }

    pub(super) fn overlapping(
        &mut self,
        region: &[ValueSet],
        limit: usize,
        visit: impl FnMut(usize),
    ) -> bool {
        let driver = region
            .iter()
            .enumerate()
            .filter_map(|(col, set)| Some((col, self.estimate(col, Self::set_keys(set))?)))
            .min_by_key(|(_, size)| *size)
            .filter(|(_, size)| *size < limit)
            .map(|(col, _)| col);
        let Some(driver) = driver else {
            return false;
        };
        self.drive(driver, Self::set_keys(&region[driver]), usize::MAX, visit);
        true
    }

    fn drive(&mut self, driver: usize, keys: Keys, below: usize, mut visit: impl FnMut(usize)) {
        self.query += 1;
        let query = self.query;
        let stamp = &mut self.stamp;
        let column = &self.columns[driver];
        let mut seen = |e: usize| {
            if e < below && stamp[e] != query {
                stamp[e] = query;
                visit(e);
            }
        };
        let prefix = |list: &[usize]| match below {
            usize::MAX => list.len(),
            _ => list.partition_point(|&e| e < below),
        };
        column.wild[..prefix(&column.wild)]
            .iter()
            .for_each(|&e| seen(e));
        match keys {
            Keys::Exact(set) => {
                if let StringSet::Finite(strings) = &set.strings {
                    for list in strings.iter().filter_map(|s| column.strings.get(s)) {
                        list[..prefix(list)].iter().for_each(|&e| seen(e));
                    }
                }
                for (bit, list) in column.bools.iter().enumerate() {
                    if set.bools & (1 << bit) != 0 {
                        list[..prefix(list)].iter().for_each(|&e| seen(e));
                    }
                }
            }
            Keys::Numbers(numbers) => {
                for interval in numbers.intervals() {
                    if let Some(range) = column.points.range(interval) {
                        column.numbers.query(range, &mut seen);
                    }
                }
            }
            Keys::Wild | Keys::Never => {}
        }
    }

    fn estimate(&self, col: usize, keys: Keys) -> Option<usize> {
        let column = &self.columns[col];
        let lists = column.wild.len();
        match keys {
            Keys::Exact(set) => {
                let strings = match &set.strings {
                    StringSet::Finite(strings) => strings
                        .iter()
                        .filter_map(|s| column.strings.get(s))
                        .map(Vec::len)
                        .sum(),
                    StringSet::CoFinite(_) => 0,
                };
                let bools: usize = column
                    .bools
                    .iter()
                    .enumerate()
                    .filter(|(bit, _)| set.bools & (1 << bit) != 0)
                    .map(|(_, list)| list.len())
                    .sum();
                Some(lists + strings + bools)
            }
            Keys::Numbers(numbers) => Some(
                lists
                    + numbers
                        .intervals()
                        .iter()
                        .filter_map(|interval| column.points.range(interval))
                        .map(|range| column.numbers.count(range))
                        .sum::<usize>(),
            ),
            Keys::Wild | Keys::Never => None,
        }
    }
}
