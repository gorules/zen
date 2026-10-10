use crate::compiled::plan::view::{M, SlotId};
use crate::compiled::table::{First, Table};
use crate::compiled::plan::typed::{Deferred, Grouping, Path, Piece, Target, TypedOp};
use crate::compiled::plan::{Op, SegPlan, TableOp};
use zen_expression::typed::{Batch, Data as Slotted, Slot};
use crate::compiled::shred::{Normalizer, Seen};
use crate::compiled::leaf::{Bits, Leaf};
use crate::compiled::{Body, Kind};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use zen_expression::lane::{Column, Values};
use super::Run;
use super::store::{Lowered, Prepared, Store};

impl<'a> Run<'_, 'a> {
    pub(super) fn deferred(&mut self, seg: &SegPlan, op: &Deferred, units: &[Op], positions: Option<&Rc<[usize]>>) {
        let rows = self.store.rows;
        let seen: Vec<(SlotId, Seen)> = op.keys.iter().map(|slot| (*slot, Normalizer::seen(&self.store.leaf(*slot), rows))).collect();
        let key: Vec<Seen> = seen.iter().map(|(_, seen)| *seen).collect();
        let graph = self.graph;
        let store = &self.store;
        let lister = |slot: SlotId, chain: &[&str]| store.listing(slot, chain).map(|listing| listing.classes());
        let pieces = op.variant(&key, units.len(), || {
            Grouping {
                graph,
                masks: &seg.masks,
                slots: &seg.slots,
                seen: &seen,
                listing: Some(&lister),
            }
            .pieces(units)
        });
        let mut at = 0;
        let mut seen = self.store.failed;
        let mut live = positions.cloned();
        for piece in pieces.iter() {
            if self.store.failed != seen {
                live = self.alive(live, &mut seen);
            }
            match piece {
                Piece::Unit => {
                    if let Some(unit) = units.get(at) {
                        self.store.counts.1 += 1;
                        self.unit(seg, unit, live.as_ref());
                    }
                    at += 1;
                }
                Piece::Typed(op, n) => {
                    self.typed(seg, op, units.get(at..at + n).unwrap_or_default(), live.as_ref());
                    at += n;
                }
                Piece::Deferred(op, n) => {
                    self.deferred(seg, op, units.get(at..at + n).unwrap_or_default(), live.as_ref());
                    at += n;
                }
            }
        }
    }

    pub(super) fn typed(&mut self, seg: &SegPlan, op: &TypedOp, units: &[Op], positions: Option<&Rc<[usize]>>) {
        let rows = self.store.rows;
        let local = positions.map_or(rows, |p| p.len());
        if local * 4 < rows {
            self.laned(seg, units, positions);
            return;
        }
        let path = op.cost.path();
        let start = Instant::now();
        let typed = match path.0 {
            Path::Typed => self.lowered(seg, op, positions).map(|lowered| self.merged(seg, op, units, positions, lowered)).is_some(),
            Path::Lane => false,
        };
        if !typed {
            self.laned(seg, units, positions);
        }
        op.cost.record(path, start.elapsed(), local);
    }

    fn laned(&mut self, seg: &SegPlan, units: &[Op], positions: Option<&Rc<[usize]>>) {
        self.store.counts.1 += units.len();
        let mut seen = self.store.failed;
        let mut live = positions.cloned();
        for unit in units {
            if self.store.failed != seen {
                live = self.alive(live, &mut seen);
            }
            self.unit(seg, unit, live.as_ref());
        }
    }

    fn merged(&mut self, seg: &SegPlan, op: &TypedOp, units: &[Op], positions: Option<&Rc<[usize]>>, (mut out, keyed, select, prefault): Lowered) {
        let rows = self.store.rows;
        let local = positions.map_or(rows, |p| p.len());
        self.store.counts.0 += units.len();
        let ids = keyed.as_deref();
        let words = rows.div_ceil(64);
        let mut take = vec![0u64; words];
        if let Some(prefault) = prefault {
            match positions {
                Some(positions) => positions.iter().filter(|&&row| Bits::get(&prefault, row)).for_each(|&row| Bits::set(&mut take, row, true)),
                None => take.iter_mut().zip(&prefault).for_each(|(t, f)| *t |= f),
            }
        }
        let mut faulted = !Bits::none(&take);
        let mut dicts = op.exports.dictionaries();
        let mut at = 0;
        for (unit, targets) in units.iter().zip(&op.targets) {
            let slots = out.slots().get(at..at + targets.len()).unwrap_or_default();
            if slots.iter().any(|slot| slot.fault.any()) {
                let mut failed: Vec<u64> = Vec::new();
                for slot in slots.iter().filter(|slot| slot.fault.any()) {
                    let words = slot.fault.words();
                    failed.resize(failed.len().max(words.len()), 0);
                    failed.iter_mut().zip(words).for_each(|(f, w)| *f |= w);
                }
                match ids {
                    Some(ids) => (0..local).filter(|&at| Bits::get(&failed, ids[at] as usize)).for_each(|at| Bits::set(&mut take, positions.map_or(at, |p| p[at]), true)),
                    None => take.iter_mut().zip(&failed).for_each(|(t, f)| *t |= f),
                }
                Bits::trim(&mut take, rows);
                faulted = true;
            }
            if faulted {
                let rerun: Vec<usize> = Bits::rows(&take).filter(|&row| self.store.errors[row].is_none()).collect();
                take.iter_mut().for_each(|word| *word = 0);
                if !rerun.is_empty() {
                    let rerun: Rc<[usize]> = rerun.into();
                    self.settled = false;
                    self.unit(seg, unit, Some(&rerun));
                    self.settled = true;
                    rerun.iter().filter(|&&row| self.store.errors[row].is_none()).for_each(|&row| Bits::set(&mut take, row, true));
                }
                faulted = !Bits::none(&take);
            }
            let mut leaves: Vec<(SlotId, Leaf<'a>)> = Vec::with_capacity(targets.len());
            let mut masks: Vec<(M, Rc<[u64]>)> = Vec::new();
            for target in targets {
                let Some(slot) = out.slot_mut(at) else {
                    return;
                };
                at += 1;
                let listed = slot.offsets.is_some();
                if let (Some(select), false) = (&select, listed) {
                    slot.valid.resize(select.len(), 0);
                    slot.valid.iter_mut().zip(select).for_each(|(valid, chosen)| *valid &= chosen);
                }
                match (target, unit) {
                    (Target::Slot(id), _) if listed => {
                        let leaf = op.exports.leaf(slot, &mut dicts);
                        let leaf = match &select {
                            Some(select) => Leaf::masked(leaf, &Rc::from(select.as_slice())),
                            None => leaf,
                        };
                        leaves.push((*id, leaf));
                    }
                    (Target::Slot(id), _) => leaves.push((*id, op.exports.leaf(slot, &mut dicts))),
                    (Target::Choice, Op::Table(table)) => self.choice(table, slot, &mut leaves, &mut masks),
                    (Target::Choice, _) => {}
                }
            }
            for (id, leaf) in leaves {
                let leaf = self.widen(leaf, ids, positions);
                let leaf = match faulted {
                    false => leaf,
                    true => {
                        let located: Rc<[(usize, usize)]> = (0..rows).map(|row| (usize::from(Bits::get(&take, row)), row)).collect();
                        Leaf::stitched(vec![leaf, self.store.leaf(id)], located)
                    }
                };
                if let Some(entry) = self.store.slots.get_mut(id as usize) {
                    *entry = Some(leaf);
                }
            }
            for (m, bits) in masks {
                let mut bits: Vec<u64> = match ids {
                    Some(ids) => {
                        let local: Rc<[u64]> = Bits::of(local, |at| Bits::get(&bits, ids[at] as usize)).into();
                        self.store.spread(&local, positions).to_vec()
                    }
                    None => bits.to_vec(),
                };
                bits.resize(rows.div_ceil(64), 0);
                Bits::trim(&mut bits, rows);
                if faulted {
                    let laned = self.store.mask(seg, m);
                    bits.iter_mut().zip(laned.iter().zip(&take)).for_each(|(bit, (lane, fault))| *bit = (*bit & !fault) | (lane & fault));
                }
                self.store.produce(m, bits.into());
            }
        }
    }

    fn decision(&self, op: &TableOp) -> Option<&Table> {
        match self.graph.step_at(op.node).map(|step| &step.kind) {
            Some(Kind::Node { body: Body::Table(table), .. }) => Some(table),
            _ => None,
        }
    }

    fn choice(&self, op: &TableOp, slot: &Slot, leaves: &mut Vec<(SlotId, Leaf<'a>)>, masks: &mut Vec<(M, Rc<[u64]>)>) {
        let Some(table) = self.decision(op) else {
            return;
        };
        let count = slot.len;
        let rules = table.rules();
        let codes: Vec<i32> = match &slot.data {
            Slotted::Num { mant, .. } => (0..count)
                .map(|at| match (slot.is_valid(at), mant.get(at).and_then(|m| usize::try_from(*m).ok())) {
                    (true, Some(rule)) if rule < rules => rule as i32,
                    _ => -1,
                })
                .collect(),
            _ => vec![-1; count],
        };
        let matched: Rc<[u64]> = Bits::of(count, |at| codes[at] >= 0).into();
        match table.literal() {
            true => {
                let (columns, present, _) = table.columns(First::coded(rules, codes));
                for ((id, leaf), (m, mask)) in op.outs.iter().zip(columns).zip(op.present.iter().zip(present)) {
                    leaves.push((*id, leaf));
                    masks.push((*m, mask.dense(count)));
                }
            }
            false => {
                for (index, m) in op.present.iter().enumerate() {
                    let bits: Rc<[u64]> = Bits::of(count, |at| usize::try_from(codes[at]).is_ok_and(|rule| table.writes(rule, index))).into();
                    masks.push((*m, bits));
                }
            }
        }
        masks.push((op.matched, matched));
    }

    fn widen(&self, leaf: Leaf<'a>, ids: Option<&[u32]>, positions: Option<&Rc<[usize]>>) -> Leaf<'a> {
        match ids {
            Some(ids) => self.store.place(leaf.expand(ids), positions),
            None => leaf,
        }
    }

    fn lowered(&mut self, seg: &SegPlan, op: &TypedOp, positions: Option<&Rc<[usize]>>) -> Option<Lowered> {
        let rows = self.store.rows;
        let mut hints: Vec<Option<(Rc<[i32]>, usize)>> = op
            .columns
            .iter()
            .map(|read| self.store.hints.iter().find(|(slot, _, _)| *slot == read.slot).map(|(_, codes, size)| (codes.clone(), *size)))
            .collect();
        let long = op.program.instructions() >= TypedOp::LONG * op.columns.len();
        let attempt = op.sparse.load(Ordering::Relaxed) == 0 && op.lists.is_empty();
        let raw: Vec<Leaf<'a>> = op.columns.iter().map(|read| self.store.leaf(read.slot)).collect();
        let keyed = attempt
            && raw.iter().zip(&hints).all(|(leaf, hint)| {
                hint.is_some()
                    || match leaf.column().values {
                        Values::Dict { .. } | Values::Bool { .. } => true,
                        Values::Scaled { .. } | Values::I64(_) | Values::Dec(_) | Values::Text { .. } => long,
                        _ => false,
                    }
            });
        let mut prefault: Option<Vec<u64>> = None;
        let mut leaves: Vec<Leaf<'a>> = Vec::with_capacity(op.columns.len());
        for (at, read) in op.columns.iter().enumerate() {
            let (leaf, faults) = self.store.viewed(seg, *read, keyed || op.shared.get(at).copied().unwrap_or(false));
            if let Some(faults) = faults {
                let bits = prefault.get_or_insert_with(|| vec![0u64; rows.div_ceil(64)]);
                bits.iter_mut().zip(faults.iter()).for_each(|(bit, fault)| *bit |= fault);
            }
            leaves.push(leaf);
        }
        let distinct = match keyed {
            true => {
                for (leaf, hint) in leaves.iter().zip(hints.iter_mut()).filter(|(_, hint)| hint.is_none()) {
                    *hint = Self::numbered(&leaf.column(), rows);
                }
                let distinct = Self::distinct(&leaves, &hints, positions, rows);
                if distinct.is_none() {
                    op.sparse.store(TypedOp::SPARSE, Ordering::Relaxed);
                }
                distinct
            }
            false => {
                let _ = op.sparse.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(1));
                None
            }
        };
        let mut out = op.program.outputs();
        match distinct {
            Some((reps, ids)) => {
                let picks: Rc<[usize]> = reps.into();
                let picked: Vec<Leaf<'a>> = leaves.iter().map(|leaf| leaf.pick(&picks)).collect();
                let mut batch = Batch::new(picks.len());
                picked.iter().for_each(|leaf| batch.push_column(leaf.column()));
                op.program.run(&batch, &mut out, None).ok()?;
                Some((out, Some(ids), None, prefault))
            }
            None => {
                let select: Option<Vec<u64>> = positions.map(|positions| {
                    let mut bits = vec![0u64; rows.div_ceil(64)];
                    positions.iter().for_each(|&row| Bits::set(&mut bits, row, true));
                    bits
                });
                let mut listed: Vec<Prepared<'a>> = Vec::with_capacity(op.lists.len());
                for (read, chain) in &op.lists {
                    let chain: Vec<&str> = chain.iter().map(|field| field.as_ref()).collect();
                    let listing = self.store.listing(read.slot, &chain)?;
                    let (validity, faults) = match (chain.is_empty(), Store::implied(seg, *read)) {
                        (true, false) => {
                            let mask = self.store.mask(seg, read.present);
                            let mut validity = listing.validity.as_deref().map_or_else(|| Bits::ones(rows), <[u64]>::to_vec);
                            let mut faults = listing.hosted.as_deref().map_or_else(|| vec![0u64; rows.div_ceil(64)], <[u64]>::to_vec);
                            for row in (0..rows).filter(|&row| !Bits::get(&mask, row)) {
                                match listing.offsets[row] == listing.offsets[row + 1] {
                                    true => Bits::set(&mut validity, row, false),
                                    false => Bits::set(&mut faults, row, true),
                                }
                            }
                            (Some(validity), Some(faults))
                        }
                        _ => (None, None),
                    };
                    listed.push((listing, validity, faults));
                }
                let mut batch = Batch::new(rows);
                leaves.iter().for_each(|leaf| batch.push_column(leaf.column()));
                for ((listing, validity, faults), def) in listed.iter().zip(&op.program.hir().schema.lists) {
                    let validity = validity.as_deref().or(listing.validity.as_deref()).map(|bits| (bits, 0));
                    let list = zen_expression::typed::List::new(&listing.offsets, validity, listing.columns(&def.fields)?);
                    batch.push_list(match faults.as_deref().or(listing.hosted.as_deref()) {
                        Some(bits) => list.faults(bits, 0),
                        None => list,
                    });
                }
                op.program.run(&batch, &mut out, select.as_deref()).ok()?;
                Some((out, None, select, prefault))
            }
        }
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
