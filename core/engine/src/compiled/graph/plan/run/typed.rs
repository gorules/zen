use crate::compiled::graph::plan::view::{M, SlotId};
use crate::compiled::graph::table::{First, Table};
use crate::compiled::core::exec::{Choice, Dedup, Hint, ListPart, Select, TypedCall};
use crate::compiled::core::retry::Take;
use crate::compiled::core::switch::{Density, Path};
use crate::compiled::graph::plan::typed::{Deferred, Grouping, Piece, Target, TypedOp};
use crate::compiled::graph::plan::{Op, SegPlan, TableOp};
use zen_expression::lane::Values;
use zen_expression::typed::{Data as Slotted, Outputs, Slot};
use crate::compiled::shred::{Normalizer, Seen};
use crate::compiled::leaf::{Bits, Leaf};
use crate::compiled::graph::{Body, Kind};
use std::rc::Rc;
use std::time::Instant;
use super::Run;
use crate::compiled::graph::plan::census::Census;
use super::store::{Lowered, Prepared, Store};

type Held<'a> = (Choice, Vec<Leaf<'a>>, Vec<Hint>);

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
                        match Census::on() {
                            true => self.timed(seg, unit, live.as_ref()),
                            false => self.unit(seg, unit, live.as_ref()),
                        }
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
        if Density::sparse(local, rows) {
            match Census::on() {
                true => self.fell("sparse", seg, units, positions, local),
                false => self.laned(seg, units, positions),
            }
            return;
        }
        let path = op.cost.path();
        let start = Instant::now();
        let mut held: Held<'a> = (Choice::default(), Vec::new(), Vec::new());
        let typed = match path.0 {
            Path::Typed => self.lowered(seg, op, positions, &mut held).map(|lowered| self.merged(seg, op, units, positions, lowered)).is_some(),
            Path::Lane => false,
        };
        if !typed {
            match Census::on() {
                true => self.fell(&format!("{:?}", path.0), seg, units, positions, local),
                false => self.laned(seg, units, positions),
            }
        }
        let (choice, leaves, hints) = held;
        op.dedup.settle(&op.cost, path, choice, start.elapsed(), local);
        op.dedup.probe(&op.cost, self, local, |run| run.coded(op, &leaves, &hints, positions));
    }

    #[cold]
    #[inline(never)]
    fn fell(&mut self, what: &str, seg: &SegPlan, units: &[Op], positions: Option<&Rc<[usize]>>, local: usize) {
        let start = Instant::now();
        self.laned(seg, units, positions);
        Census::record(format!("typed->lane | {what}"), local, start.elapsed().as_nanos());
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
        let spots = positions.map(|p| &p[..]);
        let mut take = Take::new(rows);
        take.seed(prefault.as_deref(), spots);
        let mut faulted = take.any();
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
                take.absorb(&failed, ids, spots, local);
                faulted = true;
            }
            if faulted {
                let errors = &self.store.errors;
                let rerun: Vec<usize> = take.drain(|row| errors[row].is_none());
                if !rerun.is_empty() {
                    let rerun: Rc<[usize]> = rerun.into();
                    self.settled = false;
                    self.unit(seg, unit, Some(&rerun));
                    self.settled = true;
                    rerun.iter().filter(|&&row| self.store.errors[row].is_none()).for_each(|&row| take.set(row));
                }
                faulted = take.any();
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
                        Leaf::stitched(vec![leaf, self.store.leaf(id)], take.located())
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
                    bits.iter_mut().zip(laned.iter().zip(take.bits())).for_each(|(bit, (lane, fault))| *bit = (*bit & !fault) | (lane & fault));
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

    fn lowered(&mut self, seg: &SegPlan, op: &TypedOp, positions: Option<&Rc<[usize]>>, held: &mut Held<'a>) -> Option<Lowered> {
        let rows = self.store.rows;
        let mut hints: Vec<Hint> = op
            .columns
            .iter()
            .map(|read| self.store.hints.iter().find(|(slot, _, _)| *slot == read.slot).map(|(_, codes, size)| (codes.clone(), *size)))
            .collect();
        let raw: Vec<Leaf<'a>> = op.columns.iter().map(|read| self.store.leaf(read.slot)).collect();
        let eligible = op.lists.is_empty() && op.dedup.eligible(&op.program, &raw, &hints);
        let keyed = op.dedup.ready() && eligible;
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
        match op.dedup.learn(keyed, &leaves, &mut hints, positions, rows) {
            Some((reps, ids)) => {
                let picks: Rc<[usize]> = reps.into();
                let picked: Vec<Leaf<'a>> = leaves.iter().map(|leaf| leaf.pick(&picks)).collect();
                let call = TypedCall {
                    rows: picks.len(),
                    columns: &picked,
                    lists: Vec::new(),
                    select: None,
                };
                Some((call.run(&op.program)?, Some(ids), None, prefault))
            }
            None => {
                let select: Option<Vec<u64>> = positions.map(|positions| Select::bits(rows, positions));
                let local = positions.map_or(rows, |p| p.len());
                let (out, ids) = match op.lists.is_empty() && !eligible {
                    true => {
                        let (out, choice) = op.dedup.choose(self, local, |run| run.plain(seg, op, &leaves, select.as_deref()).map(|out| (out, None)), |run| run.coded(op, &leaves, &hints, positions));
                        *held = (choice, leaves, hints);
                        out?
                    }
                    false => (self.plain(seg, op, &leaves, select.as_deref())?, None),
                };
                let select = match ids {
                    Some(_) => None,
                    None => select,
                };
                Some((out, ids, select, prefault))
            }
        }
    }

    fn plain(&mut self, seg: &SegPlan, op: &TypedOp, leaves: &[Leaf<'a>], select: Option<&[u64]>) -> Option<Outputs> {
        let rows = self.store.rows;
        let mut listed: Vec<Prepared<'a>> = Vec::with_capacity(op.lists.len());
        for (read, chain) in &op.lists {
            let chain: Vec<&str> = chain.iter().map(|field| field.as_ref()).collect();
            let listing = self.store.listing(read.slot, &chain)?;
            let (validity, faults) = match (chain.is_empty(), Store::implied(seg, *read)) {
                (true, false) => {
                    let mask = self.store.mask(seg, read.present);
                    let mut validity = listing.frame.validity.as_deref().map_or_else(|| Bits::ones(rows), <[u64]>::to_vec);
                    let mut faults = listing.hosted.as_deref().map_or_else(|| vec![0u64; rows.div_ceil(64)], <[u64]>::to_vec);
                    for row in (0..rows).filter(|&row| !Bits::get(&mask, row)) {
                        match listing.frame.offsets[row] == listing.frame.offsets[row + 1] {
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
        let call = TypedCall {
            rows,
            columns: leaves,
            lists: listed.iter().map(|(listing, validity, faults)| ListPart::Listing(listing, validity.as_deref(), faults.as_deref())).collect(),
            select,
        };
        call.run(&op.program)
    }

    fn coded(&mut self, op: &TypedOp, leaves: &[Leaf<'a>], hints: &[Hint], positions: Option<&Rc<[usize]>>) -> Option<(Outputs, Option<Vec<u32>>)> {
        let rows = self.store.rows;
        let mut codes: Vec<Hint> = Vec::with_capacity(leaves.len());
        for (at, ((read, leaf), hint)) in op.columns.iter().zip(leaves).zip(hints).enumerate() {
            let code = match (hint, leaf.column().values) {
                (Some(hint), _) => Some(hint.clone()),
                (None, Values::Dict { .. } | Values::Bool { .. }) => None,
                (None, _) => Some(self.store.codes(read.slot, op.shared.get(at).copied().unwrap_or(false), leaf)?),
            };
            codes.push(code);
        }
        Dedup::reps(&op.program, leaves, &codes, positions, rows).map(|(out, ids)| (out, Some(ids)))
    }
}
