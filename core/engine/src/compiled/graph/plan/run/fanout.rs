use crate::compiled::graph::plan::{Cond, SegPlan};
use crate::compiled::leaf::Bits;
use std::rc::Rc;
use super::Run;
use super::store::Branch;

impl<'a> Run<'_, 'a> {
    pub(super) fn fan_out(&mut self, seg: &SegPlan, positions: Option<&Rc<[usize]>>, first: bool, conditions: &[Cond], children: &mut Vec<Branch>) -> Result<(), String> {
        let rows = self.store.rows;
        let truths: Vec<Option<Rc<[u64]>>> = conditions
            .iter()
            .map(|condition| match condition {
                Cond::Always => Some(self.store.ones.clone()),
                Cond::Never => None,
                Cond::Rows(op) => Some(self.rows_condition(seg, op, positions).into()),
                Cond::Lane(op) => {
                    let uniform = op.uniform;
                    let first: Rc<[usize]>;
                    let local = match uniform {
                        true => {
                            first = Rc::from([positions.and_then(|p| p.first().copied()).unwrap_or(0)]);
                            Some(&first)
                        }
                        false => positions,
                    };
                    self.lane(seg, op, local, false);
                    let slot = op.outs.first().copied().unwrap_or_default();
                    let leaf = self.store.leaf(slot);
                    match (uniform, leaf.scatter_parts()) {
                        (true, Some((inner, _))) => inner.truthy(0).then(|| self.store.ones.clone()),
                        _ => Some(leaf.truths(rows).into()),
                    }
                }
            })
            .collect();
        let words = conditions.len().div_ceil(64).max(1);
        let ones = self.store.ones.clone();
        let owned: Vec<u64>;
        let reach_bits: &[u64] = match positions {
            None => &ones,
            Some(p) => {
                let mut bits = vec![0u64; rows.div_ceil(64)];
                p.iter().for_each(|&row| Bits::set(&mut bits, row, true));
                owned = bits;
                &owned
            }
        };
        let uniform = truths.iter().all(|t| t.as_ref().is_none_or(|t| Rc::ptr_eq(t, &ones)));
        let mut keys: Vec<u64> = Vec::with_capacity(words * (conditions.len() + 1));
        let mut sets: Vec<Vec<usize>> = Vec::with_capacity(conditions.len() + 1);
        let members = |bits: &[u64]| -> Vec<usize> {
            let mut found = Vec::with_capacity(bits.iter().map(|word| word.count_ones() as usize).sum());
            found.extend(
                bits.iter()
                    .enumerate()
                    .flat_map(|(w, &word)| {
                        let mut word = word;
                        std::iter::from_fn(move || {
                            (word != 0).then(|| {
                                let bit = word.trailing_zeros() as usize;
                                word &= word - 1;
                                w * 64 + bit
                            })
                        })
                    }),
            );
            found
        };
        if uniform || first {
            let mut remaining = reach_bits.to_vec();
            let mut hits = vec![0u64; remaining.len()];
            for (index, truth) in truths.iter().enumerate() {
                let Some(truth) = truth else {
                    continue;
                };
                let hit = &mut hits[..remaining.len().min(truth.len())];
                hit.iter_mut().zip(remaining.iter().zip(truth.iter())).for_each(|(h, (r, t))| *h = r & t);
                if Bits::none(hit) {
                    continue;
                }
                let at = keys.len();
                keys.resize(at + words, 0);
                if uniform && !first {
                    for (i, t) in truths.iter().enumerate() {
                        if t.is_some() {
                            keys[at + i / 64] |= 1 << (i % 64);
                        }
                    }
                    sets.push(members(hit));
                    remaining.fill(0);
                    break;
                }
                keys[at + index / 64] |= 1 << (index % 64);
                remaining.iter_mut().zip(hit.iter()).for_each(|(r, h)| *r &= !h);
                sets.push(match *hit == *reach_bits {
                    true => Vec::new(),
                    false => members(hit),
                });
            }
            if !Bits::none(&remaining) {
                keys.resize(keys.len() + words, 0);
                sets.push(match remaining.as_slice() == reach_bits {
                    true => Vec::new(),
                    false => members(&remaining),
                });
            }
        } else if words == 1 {
            let mut last = 0usize;
            let mut visit = |row: usize| {
                let key = truths
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.as_ref().is_some_and(|t| Bits::get(t, row)))
                    .fold(0u64, |k, (index, _)| k | 1 << index);
                let at = match keys.get(last).is_some_and(|k| *k == key) {
                    true => last,
                    false => match keys.iter().position(|k| *k == key) {
                        Some(g) => g,
                        None => {
                            keys.push(key);
                            sets.push(Vec::new());
                            keys.len() - 1
                        }
                    },
                };
                sets[at].push(row);
                last = at;
            };
            match positions {
                Some(p) => p.iter().for_each(|&row| visit(row)),
                None => (0..rows).for_each(&mut visit),
            }
        } else {
            let mut key = vec![0u64; words];
            let mut last: Option<usize> = None;
            let mut visit = |row: usize| {
                key.fill(0);
                for (index, truth) in truths.iter().enumerate() {
                    if truth.as_ref().is_some_and(|t| Bits::get(t, row)) {
                        key[index / 64] |= 1 << (index % 64);
                    }
                }
                let at = match last.filter(|&g| keys.get(g * words..(g + 1) * words) == Some(key.as_slice())) {
                    Some(g) => g,
                    None => match keys.chunks_exact(words).position(|k| k == key.as_slice()) {
                        Some(g) => g,
                        None => {
                            keys.extend_from_slice(&key);
                            sets.push(Vec::new());
                            sets.len() - 1
                        }
                    },
                };
                sets[at].push(row);
                last = Some(at);
            };
            match positions {
                Some(p) => p.iter().for_each(|&row| visit(row)),
                None => (0..rows).for_each(&mut visit),
            }
        }
        for (key, members) in keys.chunks_exact(words).zip(sets) {
            let child = seg.child(self.graph, self.layout, key);
            let child = child.as_ref().as_ref().map_err(String::clone)?;
            let members: Option<Rc<[usize]>> = match (members.is_empty(), members.len() == rows) {
                (true, _) => positions.cloned(),
                (false, true) => None,
                (false, false) => Some(members.into()),
            };
            children.push((child.clone(), members));
        }
        Ok(())
    }
}
