use super::store::{Lowered, Prepared, Store};
use super::Run;
use crate::compiled::census::Census;
use crate::compiled::core::exec::{Dedup, Hint, ListPart, Part, Select, TypedCall};
use crate::compiled::core::retry::Take;
use crate::compiled::core::switch::Density;
use crate::compiled::graph::plan::typed::{Deferred, Grouping, Piece, Target, TypedOp};
use crate::compiled::graph::plan::view::{SlotId, M};
use crate::compiled::graph::plan::{Op, SegPlan, TableMode, TableOp};
use crate::compiled::graph::{Body, Kind};
use crate::compiled::leaf::{Array, Bits, Leaf};
use crate::compiled::shred::{Listing, Normalizer, Seen};
use crate::compiled::table::{First, Table};
use std::rc::Rc;
use std::time::Instant;
use zen_expression::lane::{Column, Values};
use zen_expression::typed::{Data as Slotted, Outputs, Slot};

type Coded = (Rc<[i32]>, usize, Option<Rc<[u64]>>);
type Source<'a> = (Leaf<'a>, Option<Coded>, Option<Rc<[u64]>>);

impl<'a> Run<'_, 'a> {
    pub(super) fn deferred(
        &mut self,
        seg: &SegPlan,
        op: &Deferred,
        units: &[Op],
        positions: Option<&Rc<[usize]>>,
    ) {
        let rows = self.store.rows;
        let seen: Vec<(SlotId, Seen)> = op
            .keys
            .iter()
            .map(|slot| (*slot, Normalizer::seen(&self.store.leaf(*slot), rows)))
            .collect();
        let key: Vec<Seen> = seen.iter().map(|(_, seen)| *seen).collect();
        let graph = self.graph;
        let store = &self.store;
        let lister = |slot: SlotId, chain: &[&str]| {
            store.listing(slot, chain).map(|listing| listing.classes())
        };
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
        let mut seen = self.store.errors.generation();
        let mut live = positions.cloned();
        for piece in pieces.iter() {
            if self.store.errors.generation() != seen {
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
                    self.typed(
                        seg,
                        op,
                        units.get(at..at + n).unwrap_or_default(),
                        live.as_ref(),
                    );
                    at += n;
                }
                Piece::Deferred(op, n) => {
                    self.deferred(
                        seg,
                        op,
                        units.get(at..at + n).unwrap_or_default(),
                        live.as_ref(),
                    );
                    at += n;
                }
            }
        }
    }

    pub(super) fn typed(
        &mut self,
        seg: &SegPlan,
        op: &TypedOp,
        units: &[Op],
        positions: Option<&Rc<[usize]>>,
    ) {
        let rows = self.store.rows;
        let local = positions.map_or(rows, |p| p.len());
        let mut distinct = false;
        let keyed = self.keyed(seg, op, positions, &mut distinct);
        let unkeyed = match keyed.is_none() {
            true if Density::sparse(local, rows) => Some("sparse"),
            true if distinct && op.heavy => Some("distinct"),
            _ => None,
        };
        let unkeyed = unkeyed.or_else(|| {
            (keyed.is_none() && self.doomed(seg, op, positions)).then_some("doomed")
        });
        if let Some(why) = unkeyed {
            match Census::on() {
                true => self.fell(why, seg, units, positions, local),
                false => self.laned(seg, units, positions),
            }
            return;
        }
        let typed = keyed
            .or_else(|| self.lowered(seg, op, positions))
            .map(|lowered| self.merged(seg, op, units, positions, lowered))
            .is_some();
        if !typed {
            match Census::on() {
                true => self.fell("refused", seg, units, positions, local),
                false => self.laned(seg, units, positions),
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn fell(
        &mut self,
        what: &str,
        seg: &SegPlan,
        units: &[Op],
        positions: Option<&Rc<[usize]>>,
        local: usize,
    ) {
        let start = Instant::now();
        self.laned(seg, units, positions);
        Census::record(
            format!("typed->lane | {what}"),
            local,
            start.elapsed().as_nanos(),
        );
    }

    fn laned(&mut self, seg: &SegPlan, units: &[Op], positions: Option<&Rc<[usize]>>) {
        self.store.counts.1 += units.len();
        let mut seen = self.store.errors.generation();
        let mut live = positions.cloned();
        for unit in units {
            if self.store.errors.generation() != seen {
                live = self.alive(live, &mut seen);
            }
            self.unit(seg, unit, live.as_ref());
        }
    }

    fn merged(
        &mut self,
        seg: &SegPlan,
        op: &TypedOp,
        units: &[Op],
        positions: Option<&Rc<[usize]>>,
        Lowered {
            mut out,
            keyed,
            select,
            prefault,
        }: Lowered,
    ) {
        let rows = self.store.rows;
        let local = positions.map_or(rows, |p| p.len());
        self.store.counts.0 += units.len();
        let ids = keyed.as_ref().map(|(ids, _)| ids.as_slice());
        let reps = keyed.as_ref().map_or(0, |(_, reps)| *reps);
        let mut alias: Option<Vec<usize>> = None;
        let codes: Option<Rc<[i32]>> = match reps {
            1 => ids.map(|ids| vec![0i32; ids.len()].into()),
            _ => ids.map(|ids| ids.iter().map(|&id| id as i32).collect()),
        };
        let coded = ids.zip(codes.as_ref());
        let mut spread: Vec<(Vec<u64>, Rc<[u64]>)> = Vec::new();
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
                let rerun: Vec<usize> = match ids.filter(|_| self.trace.is_none()) {
                    Some(ids) => {
                        let mut first = vec![usize::MAX; reps];
                        let mut followers: Vec<(usize, usize)> = Vec::new();
                        let mut leaders: Vec<usize> = Vec::new();
                        for (at, &id) in ids.iter().enumerate() {
                            let row = spots.map_or(at, |p| p[at]);
                            if !Bits::get(take.bits(), row) || !self.store.errors.ok(row) {
                                continue;
                            }
                            match first.get_mut(id as usize) {
                                Some(lead) if *lead == usize::MAX => {
                                    *lead = row;
                                    leaders.push(row);
                                }
                                Some(lead) => followers.push((row, *lead)),
                                None => leaders.push(row),
                            }
                        }
                        take.drain(|_| false);
                        let alias = alias.get_or_insert_with(|| (0..rows).collect());
                        leaders.iter().for_each(|&row| alias[row] = row);
                        self.rerun(seg, unit, leaders, &mut take);
                        for (row, lead) in followers {
                            match self.store.errors.get(lead).cloned() {
                                Some(error) => self.store.errors.share(row, &error),
                                None => {
                                    alias[row] = lead;
                                    take.set(row);
                                }
                            }
                        }
                        Vec::new()
                    }
                    None => {
                        let errors = &self.store.errors;
                        take.drain(|row| errors.ok(row))
                    }
                };
                if !rerun.is_empty() {
                    self.rerun(seg, unit, rerun, &mut take);
                }
                faulted = take.any();
            }
            let mut leaves: Vec<(SlotId, Leaf<'a>)> = Vec::with_capacity(targets.len());
            let mut masks: Vec<(M, Rc<[u64]>)> = Vec::new();
            let mut hits: Vec<u64> = Vec::new();
            let mut cells: Vec<(usize, usize, Leaf<'a>)> = Vec::new();
            for target in targets {
                let Some(slot) = out.slot_mut(at) else {
                    return;
                };
                at += 1;
                let listed = slot.offsets.is_some() || slot.values.is_some();
                if let (Some(select), false) = (&select, listed) {
                    slot.valid.resize(select.len(), 0);
                    slot.valid
                        .iter_mut()
                        .zip(select)
                        .for_each(|(valid, chosen)| *valid &= chosen);
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
                    (Target::Choice, Op::Table(table)) => {
                        self.choice(table, slot, &mut leaves, &mut masks)
                    }
                    (Target::Choice, _) => {}
                    (Target::Matched(rule), Op::Table(table)) => {
                        self.matched(table, slot, *rule, &mut hits)
                    }
                    (Target::Matched(_), _) => {}
                    (Target::Cell(rule, at), Op::Table(_)) => {
                        cells.push((*rule, *at, op.exports.leaf(slot, &mut dicts)))
                    }
                    (Target::Cell(..), _) => {}
                }
            }
            if let (Op::Table(table), false) = (unit, hits.is_empty()) {
                let hint = self.collected(table, &hits, &cells, &mut leaves, &mut masks);
                if let (Some((slot, codes, size)), None, false) = (hint, positions, faulted) {
                    let codes: Rc<[i32]> = match ids {
                        Some(ids) => ids
                            .iter()
                            .map(|&id| codes.get(id as usize).copied().unwrap_or(-1))
                            .collect(),
                        None => codes.into(),
                    };
                    self.store.hints.push((slot, codes, size));
                }
            }
            for (id, leaf) in leaves {
                let leaf = self.widen(leaf, coded, positions);
                let leaf = match faulted {
                    false => leaf,
                    true => Leaf::stitched(
                        vec![leaf, self.store.leaf(id)],
                        take.located(alias.as_deref()),
                    ),
                };
                if let Some(entry) = self.store.slots.get_mut(id as usize) {
                    *entry = Some(leaf);
                }
            }
            for (m, bits) in masks {
                let mut bits: Vec<u64> = match ids {
                    Some(ids) => {
                        let mut key = bits.get(..reps.div_ceil(64)).unwrap_or_default().to_vec();
                        Bits::trim(&mut key, reps);
                        match spread.iter().find(|(seen, _)| *seen == key) {
                            Some((_, done)) => done.to_vec(),
                            None => {
                                let set = (0..reps).filter(|&rep| Bits::get(&key, rep)).count();
                                let local: Rc<[u64]> = match set {
                                    0 => vec![0u64; local.div_ceil(64)].into(),
                                    n if n == reps => {
                                        let mut ones = vec![u64::MAX; local.div_ceil(64)];
                                        Bits::trim(&mut ones, local);
                                        ones.into()
                                    }
                                    _ => Bits::of(local, |at| Bits::get(&key, ids[at] as usize))
                                        .into(),
                                };
                                let done = self.store.spread(&local, positions);
                                spread.push((key, done.clone()));
                                done.to_vec()
                            }
                        }
                    }
                    None => bits.to_vec(),
                };
                bits.resize(rows.div_ceil(64), 0);
                Bits::trim(&mut bits, rows);
                if faulted {
                    let laned = self.store.mask(seg, m);
                    match &alias {
                        Some(alias) => Bits::rows(take.bits()).for_each(|row| {
                            Bits::set(&mut bits, row, Bits::get(&laned, alias[row]))
                        }),
                        None => bits.iter_mut().zip(laned.iter().zip(take.bits())).for_each(
                            |(bit, (lane, fault))| *bit = (*bit & !fault) | (lane & fault),
                        ),
                    }
                }
                self.store.produce(m, bits.into());
            }
        }
    }

    fn rerun(&mut self, seg: &SegPlan, unit: &Op, rows: Vec<usize>, take: &mut Take) {
        if rows.is_empty() {
            return;
        }
        let rows: Rc<[usize]> = rows.into();
        self.settled = false;
        let hinted = self.store.hints.len();
        self.unit(seg, unit, Some(&rows));
        self.store.hints.truncate(hinted);
        self.settled = true;
        rows.iter()
            .filter(|&&row| self.store.errors.ok(row))
            .for_each(|&row| take.set(row));
    }

    fn decision(&self, op: &TableOp) -> Option<&Table> {
        match self.graph.step_at(op.node).map(|step| &step.kind) {
            Some(Kind::Node {
                body: Body::Table(table),
                ..
            }) => Some(table),
            _ => None,
        }
    }

    fn choice(
        &self,
        op: &TableOp,
        slot: &Slot,
        leaves: &mut Vec<(SlotId, Leaf<'a>)>,
        masks: &mut Vec<(M, Rc<[u64]>)>,
    ) {
        let Some(table) = self.decision(op) else {
            return;
        };
        let count = slot.len;
        let rules = table.rules();
        let mut codes: Vec<i32> = vec![-1; count];
        if let Slotted::Num { mant, .. } = &slot.data {
            for ((at, code), &m) in codes.iter_mut().enumerate().zip(mant.iter()) {
                let live = (slot.valid.get(at >> 6).copied().unwrap_or(0) >> (at & 63)) & 1 == 1;
                if live && (0..rules as i64).contains(&m) {
                    *code = m as i32;
                }
            }
        }
        let matched: Rc<[u64]> = Bits::of(count, |at| codes[at] >= 0).into();
        match table.literal() {
            true => {
                let (columns, present, _) = table.columns(First::coded(rules, codes));
                for ((id, leaf), (m, mask)) in op
                    .outs
                    .iter()
                    .zip(columns)
                    .zip(op.present.iter().zip(present))
                {
                    leaves.push((*id, leaf));
                    masks.push((*m, mask.dense(count)));
                }
            }
            false => {
                let outputs = op.present.len();
                let writes: Vec<Vec<usize>> = (0..rules)
                    .map(|rule| (0..outputs).filter(|&index| table.writes(rule, index)).collect())
                    .collect();
                let mut bits = vec![vec![0u64; count.div_ceil(64)]; outputs];
                for (at, &code) in codes.iter().enumerate() {
                    if let Some(indexes) = usize::try_from(code).ok().and_then(|c| writes.get(c)) {
                        for &index in indexes {
                            bits[index][at >> 6] |= 1u64 << (at & 63);
                        }
                    }
                }
                for (m, bits) in op.present.iter().zip(bits) {
                    masks.push((*m, bits.into()));
                }
            }
        }
        masks.push((op.matched, matched));
    }

    fn matched(&self, op: &TableOp, slot: &Slot, rule: usize, hits: &mut Vec<u64>) {
        let Some(table) = self.decision(op) else {
            return;
        };
        let words = table.words();
        let count = slot.len;
        hits.resize(count * words, 0);
        let Slotted::Bool(bits) = &slot.data else {
            return;
        };
        let (word, bit) = (rule / 64, 1u64 << (rule % 64));
        for row in 0..count {
            if slot.is_valid(row) && Bits::get(bits, row) {
                hits[row * words + word] |= bit;
            }
        }
    }

    fn collected(
        &self,
        op: &TableOp,
        hits: &[u64],
        cells: &[(usize, usize, Leaf<'a>)],
        leaves: &mut Vec<(SlotId, Leaf<'a>)>,
        masks: &mut Vec<(M, Rc<[u64]>)>,
    ) -> Option<(SlotId, Vec<i32>, usize)> {
        let table = self.decision(op)?;
        let rows = hits.len() / table.words().max(1);
        if op.mode == TableMode::Rows && table.computes() {
            let slot = *op.outs.first()?;
            leaves.push((slot, table.computed(hits, rows, cells)?));
            return None;
        }
        if op.mode == TableMode::Rows {
            let slot = *op.outs.first()?;
            let (leaf, codes, size) = table.assembled(hits, rows)?;
            leaves.push((slot, leaf));
            return Some((slot, codes, size));
        }
        let collected = table.first_collected(hits, rows)?;
        for ((id, leaf), (m, mask)) in op
            .outs
            .iter()
            .zip(collected.columns)
            .zip(op.present.iter().zip(collected.present))
        {
            leaves.push((*id, leaf));
            masks.push((*m, mask.dense(rows)));
        }
        let mut matched: Vec<u64> = collected.unmatched.iter().map(|w| !w).collect();
        Bits::trim(&mut matched, rows);
        masks.push((op.matched, matched.into()));
        None
    }

    fn widen(
        &self,
        leaf: Leaf<'a>,
        ids: Option<(&[u32], &Rc<[i32]>)>,
        positions: Option<&Rc<[usize]>>,
    ) -> Leaf<'a> {
        match ids {
            Some((ids, codes)) => self.store.place(leaf.expand_coded(ids, codes), positions),
            None => leaf,
        }
    }

    fn lowered(
        &mut self,
        seg: &SegPlan,
        op: &TypedOp,
        positions: Option<&Rc<[usize]>>,
    ) -> Option<Lowered> {
        let rows = self.store.rows;
        let mut prefault: Option<Vec<u64>> = None;
        let mut leaves: Vec<Leaf<'a>> = Vec::with_capacity(op.columns.len());
        for (at, read) in op.columns.iter().enumerate() {
            let code = op.shared.get(at).copied().unwrap_or(false);
            let (leaf, faults) = self.store.viewed(seg, *read, code);
            if let Some(faults) = faults {
                let bits = prefault.get_or_insert_with(|| vec![0u64; rows.div_ceil(64)]);
                bits.iter_mut()
                    .zip(faults.iter())
                    .for_each(|(bit, fault)| *bit |= fault);
            }
            leaves.push(leaf);
        }
        let select: Option<Vec<u64>> = positions.map(|positions| Select::bits(rows, positions));
        let out = self.plain(seg, op, &leaves, select.as_deref())?;
        Some(Lowered {
            out,
            keyed: None,
            select,
            prefault,
        })
    }

    fn hint(&self, slot: SlotId) -> Hint {
        self.store
            .hints
            .iter()
            .rev()
            .find(|(at, _, _)| *at == slot)
            .map(|(_, codes, size)| (codes.clone(), *size))
    }

    fn coded(&self, slot: SlotId) -> Option<Coded> {
        match self.hint(slot) {
            Some((codes, size)) => Some((codes, size, None)),
            None => self.store.leaf(slot).coded(),
        }
    }

    fn keyed(
        &mut self,
        seg: &SegPlan,
        op: &TypedOp,
        positions: Option<&Rc<[usize]>>,
        distinct: &mut bool,
    ) -> Option<Lowered> {
        let rows = self.store.rows;
        let local = positions.map_or(rows, |p| p.len());
        if local < Dedup::MIN {
            return None;
        }
        let mut sources: Vec<Source<'a>> = Vec::with_capacity(op.columns.len());
        for read in &op.columns {
            let leaf = self.store.leaf(read.slot);
            let codes = self.coded(read.slot);
            if codes.is_none() && leaf.records().is_some() {
                return None;
            }
            let mask = (!Store::implied(seg, *read)).then(|| self.store.mask(seg, read.present));
            sources.push((leaf, codes, mask));
        }
        let mut lists: Vec<(SlotId, M, Option<Coded>, Option<Rc<[u64]>>)> = Vec::new();
        for (read, _) in &op.lists {
            if lists
                .iter()
                .any(|(slot, m, ..)| *slot == read.slot && *m == read.present)
            {
                continue;
            }
            let codes = self.coded(read.slot);
            let mask = (!Store::implied(seg, *read)).then(|| self.store.mask(seg, read.present));
            lists.push((read.slot, read.present, codes, mask));
        }
        let columns: Vec<Option<Column>> = sources
            .iter()
            .map(|(leaf, codes, _)| codes.is_none().then(|| leaf.column()))
            .collect();
        let mut weight: usize = columns.iter().flatten().map(Dedup::weight).sum::<usize>();
        for (slot, _, codes, _) in &lists {
            if codes.is_none() {
                weight += match self.store.leaf(*slot) {
                    leaf @ Leaf::Input(_) => Dedup::list_weight(&leaf.column(), rows, Dedup::TEXT)?,
                    _ => return None,
                };
            }
        }
        if !Dedup::worth(op.effort, op.heavy, weight) {
            return None;
        }
        let raw: Vec<Leaf<'a>> = lists
            .iter()
            .filter(|(.., codes, _)| codes.is_none())
            .map(|(slot, ..)| self.store.leaf(*slot))
            .collect();
        let mut probe: Vec<Part> = Vec::new();
        let mut plain: Vec<Column> = raw.iter().map(Leaf::column).collect();
        for ((_, codes, mask), column) in sources.iter().zip(&columns) {
            match (codes, column.map(|column| column.values)) {
                (Some((codes, size, validity)), _) => probe.push(Part::Codes(
                    codes,
                    *size,
                    validity.as_deref().map(|bits| (bits, 0)),
                )),
                (None, Some(Values::Dict { keys, values })) => probe.push(Part::Codes(
                    keys,
                    values.len(),
                    column.and_then(|c| c.validity),
                )),
                (None, Some(Values::Bool { bits, offset })) => {
                    probe.push(Part::Bits(bits, offset, column.and_then(|c| c.validity)))
                }
                (
                    None,
                    Some(
                        Values::Scaled { .. }
                        | Values::I64(_)
                        | Values::F64(_)
                        | Values::Dec(_)
                        | Values::Text { .. }
                        | Values::Utf8 { .. },
                    ),
                ) => plain.extend(*column),
                (None, _) => return None,
            }
            if let Some(mask) = mask {
                probe.push(Part::Bits(mask, 0, None));
            }
        }
        for (_, _, codes, mask) in &lists {
            if let Some((codes, size, validity)) = codes {
                probe.push(Part::Codes(
                    codes,
                    *size,
                    validity.as_deref().map(|bits| (bits, 0)),
                ));
            }
            if let Some(mask) = mask {
                probe.push(Part::Bits(mask, 0, None));
            }
        }
        if !Dedup::sampled(&probe, &plain, positions.map(|p| &p[..]), rows) {
            *distinct = true;
            return None;
        }
        *distinct = true;
        drop(probe);
        drop(plain);
        let mut listed: Vec<(Coded, Option<Rc<[u64]>>)> = Vec::with_capacity(lists.len());
        for (slot, _, codes, mask) in lists {
            let codes = match codes {
                Some(codes) => codes,
                None => match self.store.leaf(slot) {
                    leaf @ Leaf::Input(_) => {
                        let (codes, size) = Dedup::listed(&leaf.column(), rows)?;
                        (codes, size, None)
                    }
                    _ => return None,
                },
            };
            listed.push((codes, mask));
        }
        let mut interned: Vec<Hint> = Vec::with_capacity(columns.len());
        for column in &columns {
            interned.push(match column.map(|column| column.values) {
                None | Some(Values::Dict { .. } | Values::Bool { .. }) => None,
                Some(_) => Some(Dedup::interned(column.as_ref()?, rows)?),
            });
        }
        let mut parts: Vec<Part> = Vec::with_capacity(2 * (sources.len() + listed.len()));
        for (((_, codes, mask), column), owned) in sources.iter().zip(&columns).zip(&interned) {
            parts.push(match (codes, column, owned) {
                (Some((codes, size, validity)), _, _) => {
                    Part::Codes(codes, *size, validity.as_deref().map(|bits| (bits, 0)))
                }
                (None, Some(column), Some((codes, size))) => {
                    Part::Codes(codes, *size, column.validity)
                }
                (None, Some(column), None) => match column.values {
                    Values::Dict { keys, values } => {
                        Part::Codes(keys, values.len(), column.validity)
                    }
                    Values::Bool { bits, offset } => Part::Bits(bits, offset, column.validity),
                    _ => return None,
                },
                (None, None, _) => return None,
            });
            if let Some(mask) = mask {
                parts.push(Part::Bits(mask, 0, None));
            }
        }
        for ((codes, size, validity), mask) in &listed {
            parts.push(Part::Codes(
                codes,
                *size,
                validity.as_deref().map(|bits| (bits, 0)),
            ));
            if let Some(mask) = mask {
                parts.push(Part::Bits(mask, 0, None));
            }
        }
        let (reps, ids) = match parts.is_empty() {
            true => (vec![positions.map_or(0, |p| p[0])], vec![0u32; local]),
            false => Dedup::coded_rows(&parts, positions.map(|p| &p[..]), rows)?,
        };
        *distinct = false;
        drop(parts);
        drop(columns);
        let picks: Rc<[usize]> = reps.into();
        let count = picks.len();
        let mut faults: Option<Vec<u64>> = None;
        let mut picked: Vec<Leaf<'a>> = Vec::with_capacity(sources.len());
        for (leaf, _, mask) in &sources {
            let gathered = match leaf {
                Leaf::Input(_) => Leaf::typed(Array::gather(&leaf.column(), &picks)),
                _ => leaf.regroup(&picks),
            };
            let (view, faulted) = match Normalizer::typed(&gathered, count, true) {
                Some(found) => found,
                None => (gathered, None),
            };
            if let Some(faulted) = faulted {
                let bits = faults.get_or_insert_with(|| vec![0u64; count.div_ceil(64)]);
                bits.iter_mut()
                    .zip(&faulted)
                    .for_each(|(bit, fault)| *bit |= fault);
            }
            picked.push(match mask {
                Some(mask) => {
                    let bits: Rc<[u64]> = Bits::gather(mask, &picks).into();
                    Leaf::masked(view, &bits)
                }
                None => view,
            });
        }
        let lists = self.picked(seg, op, &picks)?;
        let call = TypedCall {
            rows: count,
            columns: &picked,
            lists: lists
                .iter()
                .map(|(listing, validity, faults)| {
                    ListPart::Listing(listing, validity.as_deref(), faults.as_deref())
                })
                .collect(),
            select: None,
        };
        let out = call.run(&op.program, &self.host.extensions.context)?;
        let prefault = faults.map(|faults| {
            let mut bits = vec![0u64; rows.div_ceil(64)];
            for (at, &id) in ids.iter().enumerate() {
                if Bits::get(&faults, id as usize) {
                    Bits::set(&mut bits, positions.map_or(at, |p| p[at]), true);
                }
            }
            bits
        });
        Some(Lowered {
            out,
            keyed: Some((ids, count)),
            select: None,
            prefault,
        })
    }

    fn doomed(&mut self, seg: &SegPlan, op: &TypedOp, positions: Option<&Rc<[usize]>>) -> bool {
        const SAMPLE: usize = 64;
        let rows = self.store.rows;
        let local = positions.map_or(rows, |p| p.len());
        if local < 64 * SAMPLE || self.trace.is_some() {
            return false;
        }
        let picks: Rc<[usize]> = (0..SAMPLE)
            .map(|at| {
                let at = at * local / SAMPLE;
                positions.map_or(at, |p| p[at])
            })
            .collect();
        let mut faults = vec![0u64; SAMPLE.div_ceil(64)];
        let mut picked: Vec<Leaf<'a>> = Vec::with_capacity(op.columns.len());
        for read in &op.columns {
            let leaf = self.store.leaf(read.slot);
            if leaf.records().is_some() {
                return false;
            }
            let gathered = match leaf {
                Leaf::Input(_) => Leaf::typed(Array::gather(&leaf.column(), &picks)),
                _ => leaf.regroup(&picks),
            };
            let (view, faulted) = match Normalizer::typed(&gathered, SAMPLE, true) {
                Some(found) => found,
                None => (gathered, None),
            };
            if let Some(faulted) = faulted {
                faults.iter_mut().zip(&faulted).for_each(|(f, x)| *f |= x);
            }
            picked.push(match Store::implied(seg, *read) {
                true => view,
                false => {
                    let mask = self.store.mask(seg, read.present);
                    let bits: Rc<[u64]> = Bits::gather(&mask, &picks).into();
                    Leaf::masked(view, &bits)
                }
            });
        }
        let Some(lists) = self.picked(seg, op, &picks) else {
            return false;
        };
        let call = TypedCall {
            rows: SAMPLE,
            columns: &picked,
            lists: lists
                .iter()
                .map(|(listing, validity, faults)| {
                    ListPart::Listing(listing, validity.as_deref(), faults.as_deref())
                })
                .collect(),
            select: None,
        };
        let Some(out) = call.run(&op.program, &self.host.extensions.context) else {
            return false;
        };
        for slot in out.slots().iter().filter(|slot| slot.fault.any()) {
            faults
                .iter_mut()
                .zip(slot.fault.words())
                .for_each(|(f, x)| *f |= x);
        }
        let failed: usize = faults.iter().map(|w| w.count_ones() as usize).sum();
        failed * 8 >= SAMPLE * 7
    }

    fn picked(
        &mut self,
        seg: &SegPlan,
        op: &TypedOp,
        picks: &Rc<[usize]>,
    ) -> Option<Vec<Prepared<'a>>> {
        let count = picks.len();
        let mut bases: Vec<(SlotId, Rc<Listing<'a>>)> = Vec::new();
        let mut out: Vec<Prepared<'a>> = Vec::with_capacity(op.lists.len());
        for (read, chain) in &op.lists {
            let base = match bases.iter().find(|(slot, _)| *slot == read.slot) {
                Some((_, base)) => base.clone(),
                None => {
                    let leaf = self.store.leaf(read.slot).regroup(picks);
                    let base = Rc::new(Listing::leaf(&leaf, count)?);
                    bases.push((read.slot, base.clone()));
                    base
                }
            };
            let mut listing = base;
            for field in chain {
                listing = Rc::new(listing.nested(field)?);
            }
            let (validity, faults) = match (chain.is_empty(), Store::implied(seg, *read)) {
                (true, false) => {
                    let mask = self.store.mask(seg, read.present);
                    let mut validity = listing
                        .frame
                        .validity
                        .as_deref()
                        .map_or_else(|| Bits::ones(count), <[u64]>::to_vec);
                    let mut faults = listing
                        .hosted
                        .as_deref()
                        .map_or_else(|| vec![0u64; count.div_ceil(64)], <[u64]>::to_vec);
                    for (at, &row) in picks.iter().enumerate() {
                        if Bits::get(&mask, row) {
                            continue;
                        }
                        match listing.frame.offsets[at] == listing.frame.offsets[at + 1] {
                            true => Bits::set(&mut validity, at, false),
                            false => Bits::set(&mut faults, at, true),
                        }
                    }
                    (Some(validity), Some(faults))
                }
                _ => (None, None),
            };
            out.push((listing, validity, faults));
        }
        Some(out)
    }

    fn plain(
        &mut self,
        seg: &SegPlan,
        op: &TypedOp,
        leaves: &[Leaf<'a>],
        select: Option<&[u64]>,
    ) -> Option<Outputs> {
        let rows = self.store.rows;
        let mut listed: Vec<Prepared<'a>> = Vec::with_capacity(op.lists.len());
        for (read, chain) in &op.lists {
            let chain: Vec<&str> = chain.iter().map(|field| field.as_ref()).collect();
            let listing = self.store.listing(read.slot, &chain)?;
            let (validity, faults) = match (chain.is_empty(), Store::implied(seg, *read)) {
                (true, false) => {
                    let mask = self.store.mask(seg, read.present);
                    let mut validity = listing
                        .frame
                        .validity
                        .as_deref()
                        .map_or_else(|| Bits::ones(rows), <[u64]>::to_vec);
                    let mut faults = listing
                        .hosted
                        .as_deref()
                        .map_or_else(|| vec![0u64; rows.div_ceil(64)], <[u64]>::to_vec);
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
            lists: listed
                .iter()
                .map(|(listing, validity, faults)| {
                    ListPart::Listing(listing, validity.as_deref(), faults.as_deref())
                })
                .collect(),
            select,
        };
        call.run(&op.program, &self.host.extensions.context)
    }
}
