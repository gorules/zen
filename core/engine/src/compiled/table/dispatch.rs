use crate::compiled::bound::Bound;
use crate::compiled::leaf::Array;
use zen_expression::lane::{LaneRunner, Pieces};
use super::{Column, Table};

pub(crate) struct Dispatch {
    pieces: Vec<Pieces>,
    strides: Vec<usize>,
    lut: Option<Vec<u32>>,
    candidates: Vec<Vec<u64>>,
    pub(super) chosen: Option<Vec<i32>>,
}

impl Dispatch {
    const LUT: usize = 1 << 16;

    pub(super) fn build(columns: &[Column], rules: usize, words: usize) -> Option<Dispatch> {
        let pieces = columns
            .iter()
            .map(|column| match column {
                Column::Field { cells, empty, .. } => cells.pieces(empty),
                Column::Plain { .. } => None,
            })
            .collect::<Option<Vec<Pieces>>>()?;
        let mut strides = Vec::with_capacity(pieces.len());
        let mut size = 1usize;
        for p in &pieces {
            strides.push(size);
            size = size.saturating_mul(p.len().max(1));
        }
        let mut candidates: Vec<Vec<u64>> = Vec::new();
        let lut = (size <= Self::LUT).then(|| {
            (0..size)
                .map(|index| {
                    let mut bits = vec![u64::MAX; words];
                    if let Some(last) = bits.last_mut() {
                        *last = match (rules, rules % 64) {
                            (0, _) => 0,
                            (_, 0) => u64::MAX,
                            (_, t) => (1u64 << t) - 1,
                        };
                    }
                    for (p, stride) in pieces.iter().zip(&strides) {
                        let piece = (index / stride) % p.len().max(1);
                        if let Some(b) = p.bits.get(piece) {
                            bits.iter_mut().zip(b).for_each(|(a, b)| *a &= b);
                        }
                    }
                    match candidates.iter().position(|c| *c == bits) {
                        Some(at) => at as u32,
                        None => {
                            candidates.push(bits);
                            (candidates.len() - 1) as u32
                        }
                    }
                })
                .collect()
        });
        let firsts: Vec<i32> = candidates
            .iter()
            .map(|bits| {
                bits.iter()
                    .enumerate()
                    .find(|(_, w)| **w != 0)
                    .map_or(-1, |(i, w)| (i * 64 + w.trailing_zeros() as usize) as i32)
            })
            .collect();
        let chosen = lut.as_ref().map(|lut: &Vec<u32>| {
            lut.iter()
                .map(|&at| firsts.get(at as usize).copied().unwrap_or(-1))
                .collect()
        });
        Some(Dispatch {
            pieces,
            strides,
            lut,
            candidates,
            chosen,
        })
    }
}

impl Table {
    pub(super) fn keyed(&self, dispatch: &Dispatch, runner: &mut LaneRunner, bound: &Bound) -> Option<Vec<u32>> {
        dispatch.lut.as_ref()?;
        let rows = bound.rows();
        let mut keys = vec![0u32; rows];
        let mut pieces = vec![0u16; rows];
        let mut failed = vec![false; rows];
        for ((column, p), stride) in self.columns.iter().zip(&dispatch.pieces).zip(&dispatch.strides) {
            let Column::Field { field, cells, .. } = column else {
                continue;
            };
            failed.iter_mut().for_each(|f| *f = false);
            let (direct, typed) = Self::field_column(runner, bound, field, &mut failed);
            let view = direct.or_else(|| typed.as_ref().map(Array::column));
            match view {
                Some(view) => cells.classify(p, &view, rows, &mut pieces),
                None => pieces.iter_mut().for_each(|x| *x = p.failed),
            }
            let stride = *stride as u32;
            match failed.iter().any(|f| *f) {
                false => keys.iter_mut().zip(&pieces).for_each(|(key, piece)| *key += *piece as u32 * stride),
                true => {
                    for ((key, piece), failed) in keys.iter_mut().zip(&pieces).zip(&failed) {
                        *key += match failed {
                            true => p.failed as u32,
                            false => *piece as u32,
                        } * stride;
                    }
                }
            }
        }
        Some(keys)
    }

    pub(super) fn dispatched(&self, dispatch: &Dispatch, runner: &mut LaneRunner, bound: &Bound) -> Vec<u64> {
        if let (Some(keys), Some(lut), 1) = (self.keyed(dispatch, runner, bound), &dispatch.lut, self.words) {
            return keys
                .iter()
                .map(|key| {
                    lut.get(*key as usize)
                        .and_then(|i| dispatch.candidates.get(*i as usize))
                        .and_then(|bits| bits.first().copied())
                        .unwrap_or(0)
                })
                .collect();
        }
        let (rows, words) = (bound.rows(), self.words);
        let mut keys = vec![0u32; rows];
        let mut pieces = vec![0u16; rows];
        let mut failed = vec![false; rows];
        let tail = match (self.rules, self.rules % 64) {
            (0, _) => 0,
            (_, 0) => u64::MAX,
            (_, t) => (1u64 << t) - 1,
        };
        let mut all: Vec<u64> = match dispatch.lut {
            Some(_) => Vec::new(),
            None => {
                let mut all = vec![u64::MAX; rows * words];
                for row in 0..rows {
                    all[row * words + words - 1] &= tail;
                }
                all
            }
        };
        for ((column, p), stride) in self.columns.iter().zip(&dispatch.pieces).zip(&dispatch.strides) {
            let Column::Field { field, cells, .. } = column else {
                continue;
            };
            failed.iter_mut().for_each(|f| *f = false);
            let (direct, typed) = Self::field_column(runner, bound, field, &mut failed);
            let view = direct.or_else(|| typed.as_ref().map(Array::column));
            match view {
                Some(view) => cells.classify(p, &view, rows, &mut pieces),
                None => pieces.iter_mut().for_each(|x| *x = p.failed),
            }
            for (piece, failed) in pieces.iter_mut().zip(&failed) {
                if *failed {
                    *piece = p.failed;
                }
            }
            match dispatch.lut {
                Some(_) => keys.iter_mut().zip(&pieces).for_each(|(k, piece)| *k += *piece as u32 * *stride as u32),
                None => {
                    for (row, piece) in pieces.iter().enumerate() {
                        if let Some(bits) = p.bits.get(*piece as usize) {
                            all[row * words..(row + 1) * words].iter_mut().zip(bits).for_each(|(a, b)| *a &= b);
                        }
                    }
                }
            }
        }
        if let Some(lut) = &dispatch.lut {
            all = Vec::with_capacity(rows * words);
            for key in &keys {
                let index = lut.get(*key as usize).copied().unwrap_or(0) as usize;
                match dispatch.candidates.get(index) {
                    Some(bits) => all.extend(bits.iter().enumerate().map(|(w, b)| if w + 1 == words { b & tail } else { *b })),
                    None => all.extend(std::iter::repeat_n(0, words)),
                }
            }
        }
        all
    }
}
