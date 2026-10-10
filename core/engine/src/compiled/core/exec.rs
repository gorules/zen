use super::list::Frame;
use crate::compiled::leaf::{Bits, Leaf};
use crate::compiled::shred::Listing;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
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

pub(crate) struct Dedup {
    sparse: AtomicU32,
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

    pub fn new() -> Self {
        Self { sparse: AtomicU32::new(0) }
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
