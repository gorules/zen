use super::list::Frame;
use super::switch::{Cost, Path};
use crate::compiled::leaf::{Bits, Leaf};
use crate::compiled::shred::{Coder, Listing};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use zen_expression::lane::{Column, Values};
use zen_expression::typed::{Batch, Outputs, Program};

pub(crate) type Hint = Option<(Rc<[i32]>, usize)>;

pub(crate) enum ListPart<'l, 'a> {
    Frame(&'l Frame, Vec<Column<'l>>),
    Listing(&'l Listing<'a>, Option<&'l [u64]>, Option<&'l [u64]>),
}

pub(crate) struct TypedCall<'l, 'a> {
    pub rows: usize,
    pub columns: &'l [Leaf<'a>],
    pub lists: Vec<ListPart<'l, 'a>>,
    pub select: Option<&'l [u64]>,
}

pub(crate) struct Select;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Rows {
    #[default]
    Plain,
    Coded,
    Warm,
    Probe,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Choice {
    rows: Rows,
    spent: Duration,
}

pub(crate) struct Dedup {
    sparse: AtomicU32,
    calls: AtomicU32,
    plain: AtomicU64,
    part: AtomicU64,
    coded: AtomicU64,
    trial: AtomicBool,
    pending: AtomicBool,
}

impl TypedCall<'_, '_> {
    pub fn run(self, program: &Program) -> Option<Outputs> {
        let mut batch = Batch::new(self.rows);
        self.columns.iter().for_each(|leaf| batch.push_column(leaf.column()));
        for (part, def) in self.lists.into_iter().zip(&program.hir().schema.lists) {
            batch.push_list(match part {
                ListPart::Frame(frame, fields) => frame.list(None, fields),
                ListPart::Listing(listing, validity, faults) => {
                    let list = listing.frame.list(validity, listing.columns(&def.fields)?);
                    match faults.or(listing.hosted.as_deref()) {
                        Some(bits) => list.faults(bits, 0),
                        None => list,
                    }
                }
            });
        }
        let mut out = program.outputs();
        program.run(&batch, &mut out, self.select).ok()?;
        Some(out)
    }
}

impl Select {
    pub fn bits(rows: usize, positions: &[usize]) -> Vec<u64> {
        let mut bits = vec![0u64; rows.div_ceil(64)];
        positions.iter().for_each(|&row| Bits::set(&mut bits, row, true));
        bits
    }
}

impl Dedup {
    const SPARSE: u32 = 16;
    const LONG: usize = 24;
    const PERIOD: u32 = 64;
    const MIN: usize = 16;
    const MARGIN: u64 = 4;
    const FAILED: u64 = u64::MAX - 1;

    pub fn new() -> Self {
        Self {
            sparse: AtomicU32::new(0),
            calls: AtomicU32::new(0),
            plain: AtomicU64::new(0),
            part: AtomicU64::new(u64::MAX),
            coded: AtomicU64::new(u64::MAX),
            trial: AtomicBool::new(false),
            pending: AtomicBool::new(false),
        }
    }

    fn rows(&self) -> Rows {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let coded = self.coded.load(Ordering::Relaxed);
        match call % Self::PERIOD {
            0 if call == 0 => Rows::Warm,
            1 if call > 1 || coded != Self::FAILED => Rows::Probe,
            _ if coded < self.plain.load(Ordering::Relaxed) => Rows::Coded,
            _ => Rows::Plain,
        }
    }

    pub fn choose<C, T>(&self, cx: &mut C, local: usize, plain: impl FnOnce(&mut C) -> Option<T>, coded: impl FnOnce(&mut C) -> Option<T>) -> (Option<T>, Choice) {
        let rows = match local >= Self::MIN {
            true => self.rows(),
            false => Rows::Plain,
        };
        let choice = |rows: Rows, spent: Duration| Choice { rows, spent };
        match rows {
            Rows::Plain => (plain(cx), Choice::default()),
            Rows::Coded => match coded(cx) {
                Some(out) => (Some(out), choice(Rows::Coded, Duration::ZERO)),
                None => {
                    self.coded.store(u64::MAX, Ordering::Relaxed);
                    self.trial.store(false, Ordering::Relaxed);
                    (plain(cx), Choice::default())
                }
            },
            Rows::Warm => {
                let out = plain(cx);
                let begin = Instant::now();
                if out.is_some() && coded(cx).is_none() {
                    self.coded.store(Self::FAILED, Ordering::Relaxed);
                }
                (out, choice(Rows::Warm, begin.elapsed()))
            }
            Rows::Probe => {
                let begin = Instant::now();
                let out = plain(cx);
                if out.is_some() {
                    self.part.store(Self::per(begin.elapsed(), local), Ordering::Relaxed);
                    self.pending.store(true, Ordering::Relaxed);
                }
                (out, choice(Rows::Probe, Duration::ZERO))
            }
        }
    }

    pub fn settle(&self, cost: &Cost, path: (Path, bool), choice: Choice, elapsed: Duration, local: usize) {
        let elapsed = elapsed.saturating_sub(choice.spent);
        let per = Self::per(elapsed, local);
        match choice.rows {
            Rows::Probe => self.plain.store(per, Ordering::Relaxed),
            Rows::Coded if self.trial.swap(false, Ordering::Relaxed) => {
                self.coded.store(per, Ordering::Relaxed);
                cost.typed(per.min(self.plain.load(Ordering::Relaxed)));
                return;
            }
            Rows::Coded => {
                self.coded.fetch_min(per, Ordering::Relaxed);
            }
            Rows::Plain | Rows::Warm => {}
        }
        cost.record(path, elapsed, local);
    }

    pub fn probe<C, T>(&self, cost: &Cost, cx: &mut C, local: usize, coded: impl FnOnce(&mut C) -> Option<T>) {
        if !self.pending.swap(false, Ordering::Relaxed) {
            return;
        }
        let begin = Instant::now();
        let tried = coded(cx).is_some();
        let part = Self::per(begin.elapsed(), local);
        let saving = self.part.load(Ordering::Relaxed).saturating_sub(part);
        match (tried, saving > 0 && saving.saturating_mul(Self::MARGIN) >= self.plain.load(Ordering::Relaxed)) {
            (true, true) => {
                self.coded.store(self.plain.load(Ordering::Relaxed).saturating_sub(saving), Ordering::Relaxed);
                self.trial.store(true, Ordering::Relaxed);
                cost.credit(saving);
            }
            (true, false) => self.coded.store(u64::MAX, Ordering::Relaxed),
            (false, _) => self.coded.store(Self::FAILED, Ordering::Relaxed),
        }
    }

    fn per(elapsed: Duration, rows: usize) -> u64 {
        u64::try_from(elapsed.as_nanos().saturating_mul(1024) / rows.max(1) as u128).unwrap_or(Self::FAILED)
    }

    pub fn reps(program: &Program, leaves: &[Leaf], codes: &[Hint], positions: Option<&Rc<[usize]>>, rows: usize) -> Option<(Outputs, Vec<u32>)> {
        let (reps, ids) = Self::distinct(leaves, codes, positions, rows)?;
        let picks: Rc<[usize]> = reps.into();
        let picked: Vec<Leaf> = leaves.iter().map(|leaf| leaf.pick(&picks)).collect();
        let call = TypedCall {
            rows: picks.len(),
            columns: &picked,
            lists: Vec::new(),
            select: None,
        };
        Some((call.run(program)?, ids))
    }

    pub fn coded(program: &Program, leaves: &[Leaf], codes: &[Hint], positions: Option<&Rc<[usize]>>, rows: usize) -> Option<Outputs> {
        let (mut out, ids) = Self::reps(program, leaves, codes, positions, rows)?;
        out.spread(rows, &ids, positions.map(|p| &p[..])).then_some(out)
    }

    pub fn codes(column: &Column, rows: usize) -> Hint {
        match column.values {
            Values::Scaled { .. } | Values::I64(_) => Self::numbered(column, rows),
            Values::Text { offsets, data } => Coder::keys(data.as_bytes(), offsets.get(..=rows)?),
            Values::Utf8 { offsets, data } => Coder::keys(data, offsets.get(..=rows)?),
            _ => None,
        }
    }

    pub fn ready(&self) -> bool {
        self.sparse.load(Ordering::Relaxed) == 0
    }

    pub fn eligible(&self, program: &Program, leaves: &[Leaf], hints: &[Hint]) -> bool {
        let long = program.instructions() >= Self::LONG * leaves.len();
        leaves.iter().zip(hints).all(|(leaf, hint)| {
            hint.is_some()
                || match leaf.column().values {
                    Values::Dict { .. } | Values::Bool { .. } => true,
                    Values::Scaled { .. } | Values::I64(_) | Values::Dec(_) | Values::Text { .. } => long,
                    _ => false,
                }
        })
    }

    pub fn learn(&self, keyed: bool, leaves: &[Leaf], hints: &mut [Hint], positions: Option<&Rc<[usize]>>, rows: usize) -> Option<(Vec<usize>, Vec<u32>)> {
        if !keyed {
            let _ = self.sparse.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(1));
            return None;
        }
        for (leaf, hint) in leaves.iter().zip(hints.iter_mut()).filter(|(_, hint)| hint.is_none()) {
            *hint = Self::numbered(&leaf.column(), rows);
        }
        let distinct = Self::distinct(leaves, hints, positions, rows);
        if distinct.is_none() {
            self.sparse.store(Self::SPARSE, Ordering::Relaxed);
        }
        distinct
    }

    pub fn distinct(leaves: &[Leaf], hints: &[Hint], positions: Option<&Rc<[usize]>>, rows: usize) -> Option<(Vec<usize>, Vec<u32>)> {
        let local = positions.map_or(rows, |p| p.len());
        if leaves.is_empty() || local < 16 {
            return None;
        }
        let listed: Vec<bool> = leaves.iter().map(|leaf| leaf.records().is_some()).collect();
        if listed.iter().enumerate().any(|(at, &listed)| listed && hints.get(at).is_none_or(Option::is_none)) {
            return None;
        }
        let held: Vec<Option<Vec<u64>>> = leaves
            .iter()
            .zip(&listed)
            .map(|(leaf, &listed)| listed.then(|| leaf.validity(rows)))
            .collect();
        let columns: Vec<Column> = leaves
            .iter()
            .zip(&held)
            .map(|(leaf, held)| match held {
                Some(bits) => Column {
                    values: Values::Any(&[]),
                    validity: Some((bits.as_slice(), 0)),
                },
                None => leaf.column(),
            })
            .collect();
        let mut strides = Vec::with_capacity(columns.len());
        let mut size = 1u64;
        for (at, column) in columns.iter().enumerate() {
            let width = match (column.values, hints.get(at).and_then(Option::as_ref)) {
                (_, Some((_, size))) => *size as u64 + 1,
                (Values::Dict { values, .. }, None) => values.len() as u64 + 1,
                (Values::Bool { .. }, None) => 3,
                _ => return None,
            };
            strides.push(size);
            size = size.checked_mul(width)?;
        }
        let mut keys = vec![0u64; local];
        for (index, (column, stride)) in columns.iter().zip(&strides).enumerate() {
            let hint = hints.get(index).and_then(Option::as_ref);
            let code = |row: usize, code: i32| match column.valid(row) {
                true => u64::try_from(code).map_or(0, |k| k + 1),
                false => 0,
            };
            let codes: Option<&[i32]> = match (hint, column.values) {
                (Some((codes, _)), _) => Some(codes),
                (None, Values::Dict { keys, .. }) => Some(keys),
                _ => None,
            };
            match (codes, column.values) {
                (Some(codes), _) if positions.is_none() && column.validity.is_none() && codes.len() >= local => {
                    for (key, &code) in keys.iter_mut().zip(codes) {
                        *key += (code.max(-1) + 1) as u64 * stride;
                    }
                }
                (Some(codes), _) if positions.is_none() && codes.len() >= local => {
                    let valid = column.validity.map_or_else(|| Bits::ones(local), |(bits, offset)| Bits::window(bits, offset, local));
                    for ((at, key), &code) in keys.iter_mut().enumerate().zip(codes) {
                        let live = (valid[at >> 6] >> (at & 63)) & 1;
                        *key += (code.max(-1) + 1) as u64 * live * stride;
                    }
                }
                (Some(codes), _) => {
                    for (at, key) in keys.iter_mut().enumerate() {
                        let row = positions.map_or(at, |p| p[at]);
                        *key += code(row, codes.get(row).copied().unwrap_or(-1)) * stride;
                    }
                }
                (None, Values::Bool { bits, offset }) => {
                    for (at, key) in keys.iter_mut().enumerate() {
                        let row = positions.map_or(at, |p| p[at]);
                        *key += code(row, i32::from(Bits::get(bits, offset + row))) * stride;
                    }
                }
                _ => return None,
            }
        }
        let dense = size <= 4096;
        let slots = match dense {
            true => size as usize,
            false => (local / 2).next_power_of_two().max(16) * 2,
        };
        let mut table = vec![(u64::MAX, u32::MAX); slots];
        let mask = slots - 1;
        let mut reps = Vec::new();
        let mut ids = vec![0u32; local];
        for ((at, &key), id) in keys.iter().enumerate().zip(ids.iter_mut()) {
            let mut slot = match dense {
                true => key as usize,
                false => (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & mask,
            };
            while !dense && table[slot].1 != u32::MAX && table[slot].0 != key {
                slot = (slot + 1) & mask;
            }
            let entry = table.get_mut(slot)?;
            if entry.1 == u32::MAX {
                *entry = (key, reps.len() as u32);
                reps.push(positions.map_or(at, |p| p[at]));
                if reps.len() * 4 > local {
                    return None;
                }
            }
            *id = entry.1;
        }
        Some((reps, ids))
    }

    fn numbered(column: &Column, rows: usize) -> Option<(Rc<[i32]>, usize)> {
        const SLOTS: usize = 256;
        const LIMIT: usize = 128;
        let (mant, scale) = match column.values {
            Values::Scaled { mant, scale } => (mant.get(..rows)?, Some(scale.get(..rows)?)),
            Values::I64(values) => (values.get(..rows)?, None),
            _ => return None,
        };
        let mut table = [(0i64, 0u8, u8::MAX); SLOTS];
        let mut count = 0usize;
        let mut codes: Vec<i32> = Vec::with_capacity(rows);
        let mut last: Option<(i64, u8, i32)> = None;
        for (at, &m) in mant.iter().enumerate() {
            let s = scale.map_or(0, |scale| scale[at]);
            if let Some((_, _, code)) = last.filter(|(lm, ls, _)| *lm == m && *ls == s) {
                codes.push(code);
                continue;
            }
            let mut slot = ((m as u64 ^ u64::from(s) << 56).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as usize;
            let code = loop {
                let entry = &mut table[slot];
                if entry.2 == u8::MAX {
                    if count >= LIMIT {
                        return None;
                    }
                    *entry = (m, s, count as u8);
                    count += 1;
                    break i32::from(entry.2);
                }
                if entry.0 == m && entry.1 == s {
                    break i32::from(entry.2);
                }
                slot = (slot + 1) % SLOTS;
            };
            last = Some((m, s, code));
            codes.push(code);
        }
        Some((codes.into(), count))
    }
}

#[cfg(test)]
mod tests {
    use super::{Choice, Dedup};
    use crate::compiled::core::switch::{Cost, Path};
    use std::time::{Duration, Instant};

    struct Spin;

    impl Spin {
        fn run(micros: u64) -> Option<u64> {
            let until = Instant::now() + Duration::from_micros(micros);
            while Instant::now() < until {
                std::hint::spin_loop();
            }
            Some(micros)
        }

        fn call(dedup: &Dedup, cost: &Cost, plain: u64, coded: u64, merged: u64) -> Option<u64> {
            let start = Instant::now();
            let (out, choice): (Option<u64>, Choice) = dedup.choose(&mut (), 1024, |_| Self::run(plain), |_| Self::run(coded));
            Self::run(merged);
            dedup.settle(cost, (Path::Typed, false), choice, start.elapsed(), 1024);
            dedup.probe(cost, &mut (), 1024, |_| Self::run(coded));
            out
        }
    }

    #[test]
    fn a_clearly_cheaper_coded_run_is_tried_and_kept() {
        let (dedup, cost) = (Dedup::new(), Cost::new());
        let runs: Vec<Option<u64>> = (0..6).map(|_| Spin::call(&dedup, &cost, 400, 50, 20)).collect();
        assert_eq!(&runs[..2], &[Some(400), Some(400)]);
        assert!(runs[2..].iter().all(|run| *run == Some(50)));
    }

    #[test]
    fn a_coded_trial_that_loses_reverts_to_plain() {
        let (dedup, cost) = (Dedup::new(), Cost::new());
        let mut runs: Vec<Option<u64>> = (0..2).map(|_| Spin::call(&dedup, &cost, 400, 50, 20)).collect();
        let start = Instant::now();
        let (out, choice) = dedup.choose(&mut (), 1024, |_| Spin::run(400), |_| Spin::run(50));
        Spin::run(900);
        dedup.settle(&cost, (Path::Typed, false), choice, start.elapsed(), 1024);
        runs.push(out);
        runs.extend((0..3).map(|_| Spin::call(&dedup, &cost, 400, 50, 20)));
        assert_eq!(runs[2], Some(50));
        assert!(runs[3..].iter().all(|run| *run == Some(400)));
    }

    #[test]
    fn a_marginal_saving_is_not_tried() {
        let (dedup, cost) = (Dedup::new(), Cost::new());
        let runs: Vec<Option<u64>> = (0..6).map(|_| Spin::call(&dedup, &cost, 400, 360, 20)).collect();
        assert!(runs.iter().all(|run| *run == Some(400)));
    }

    #[test]
    fn a_failed_warm_up_skips_the_probe() {
        let (dedup, cost) = (Dedup::new(), Cost::new());
        let mut tried = 0;
        for _ in 0..6 {
            let (out, choice) = dedup.choose(&mut tried, 1024, |_| Some(1), |tried| {
                *tried += 1;
                None::<u32>
            });
            assert_eq!(out, Some(1));
            dedup.settle(&cost, (Path::Typed, false), choice, Duration::from_micros(10), 1024);
            dedup.probe(&cost, &mut tried, 1024, |tried| {
                *tried += 1;
                None::<u32>
            });
        }
        assert_eq!(tried, 1);
    }
}
