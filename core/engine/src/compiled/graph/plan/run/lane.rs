use crate::compiled::graph::plan::{LaneOp, Program, SegPlan};
use crate::compiled::graph::bound::{Bound, Outs};
use crate::compiled::leaf::{Array, Bits, Leaf, Records};
use crate::compiled::graph::{Body, CompiledGraph, Condition, Kind};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns, LaneProgram, Values};
use super::Run;
use super::store::Entries;

impl<'a> Run<'_, 'a> {
    fn program<'g>(graph: &'g CompiledGraph, op: &'g LaneOp) -> Option<(&'g LaneProgram, Entries<'g>)> {
        if let Some(own) = &op.own {
            return Some((&own.program, Some(&own.entries)));
        }
        let step = graph.step_at(op.node)?;
        match (&step.kind, op.program) {
            (Kind::Node { body: Body::Expression { program, entries, .. }, .. }, Program::Expression) => Some((program, Some(entries))),
            (Kind::Node { transform, .. }, Program::Field) => transform.input_field.as_ref().map(|(program, _)| (program, None)),
            (Kind::Switch { conditions, .. }, Program::Condition(index)) => match conditions.get(index) {
                Some((_, Condition::Program(program))) => Some((program, None)),
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn distinct(leaves: &[Leaf<'a>], hints: &[Option<(Rc<[i32]>, usize)>], positions: Option<&Rc<[usize]>>, rows: usize) -> Option<(Vec<usize>, Vec<u32>)> {
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

    pub(super) fn lane(&mut self, seg: &SegPlan, op: &LaneOp, positions: Option<&Rc<[usize]>>, report: bool) {
        let Some((program, entries)) = Self::program(self.graph, op) else {
            return;
        };
        let rows = self.store.rows;
        let leaves: Vec<Leaf<'a>> = op.reads.iter().map(|(_, r)| self.store.value(seg, *r)).collect();
        let held: Vec<Option<Rc<Records<'a>>>> = match Records::LANE_VIEW {
            true => leaves.iter().map(Leaf::records).collect(),
            false => Vec::new(),
        };
        let parts: Vec<Option<Vec<(&str, Column)>>> = match held.iter().any(Option::is_some) {
            true => held.iter().map(|r| r.as_ref().map(|r| r.parts())).collect(),
            false => Vec::new(),
        };
        let items: Vec<Option<Column>> = held.iter().zip(&parts).map(|(r, p)| Some(r.as_ref()?.item(p.as_ref()?))).collect();
        let mut columns = Columns::new(rows);
        columns.columns.reserve_exact(op.reads.len());
        for (at, ((key, _), leaf)) in op.reads.iter().zip(&leaves).enumerate() {
            let view = match (held.get(at).and_then(Option::as_ref), items.get(at).and_then(Option::as_ref)) {
                (Some(record), Some(item)) => record.list(item),
                _ => leaf.column(),
            };
            columns = columns.column(key.as_ref(), view);
        }
        let program = match (op.specialized.get(), self.settled) {
            (Some(special), _) => special.as_ref().unwrap_or(program),
            (None, true) => op.specialized.get_or_init(|| program.specialize_columns(&columns).ok()).as_ref().unwrap_or(program),
            (None, false) => program,
        };
        let scopes = Bound::blank(rows);
        let mut outs = Outs::take();
        let mut failures = std::mem::take(&mut self.failures);
        failures.clear();
        let local = positions.map_or(rows, |p| p.len());
        let single = [positions.and_then(|p| p.first().copied()).unwrap_or(0)];
        let uniform = op.uniform && local > 1;
        let keyed = match op.stable && !uniform {
            true => {
                let hints: Vec<Option<(Rc<[i32]>, usize)>> = op
                    .reads
                    .iter()
                    .map(|(_, r)| self.store.hints.iter().find(|(slot, _, _)| *slot == r.slot).map(|(_, codes, size)| (codes.clone(), *size)))
                    .collect();
                Self::distinct(&leaves, &hints, positions, rows)
            }
            false => None,
        };
        let subset = match (uniform, &keyed) {
            (true, _) => Some(&single[..]),
            (false, Some((reps, _))) => Some(&reps[..]),
            (false, None) => positions.map(|p| p.as_ref()),
        };
        self.runner.evaluate_sites(program, scopes.slice(), &columns, &op.bound, subset, &mut outs, |row, stage, _| {
            failures.push((row, stage))
        });
        if uniform {
            let stages: Vec<usize> = failures.iter().map(|(_, stage)| *stage).collect();
            failures = stages.into_iter().flat_map(|stage| (0..local).map(move |at| (at, stage))).collect();
        }
        if let (Some((reps, ids)), false) = (&keyed, failures.is_empty()) {
            let mut stages: Vec<Vec<usize>> = vec![Vec::new(); reps.len()];
            failures.iter().for_each(|&(rep, stage)| stages[rep].push(stage));
            failures = ids
                .iter()
                .enumerate()
                .flat_map(|(at, id)| stages[*id as usize].iter().map(move |&stage| (at, stage)))
                .collect();
        }
        for (slot, out) in op.outs.iter().zip(outs.iter_mut()) {
            let array = Array::from_output(out);
            let leaf = match (uniform, &keyed) {
                (true, _) => match Self::templated(&array, local) {
                    Some(leaf) => leaf,
                    None => array.broadcast(local).map_or_else(|| Leaf::nulls(local), Leaf::typed),
                },
                (false, Some((_, ids))) => Leaf::typed(array.expand(ids)),
                (false, None) => Leaf::typed(array),
            };
            let leaf = self.store.place(leaf, positions);
            self.store.slots[*slot as usize] = Some(leaf);
        }
        let graph = self.graph;
        match (report, op.program, graph.step_at(op.node), entries) {
            (true, Program::Field, Some(step), _) => {
                if let Some((source, _)) = Self::program(graph, op) {
                    self.fielded(step, source, &columns, positions, &failures);
                }
            }
            (true, _, Some(step), Some(entries)) => self.report(entries, step, positions, &failures),
            _ => {}
        }
        self.failures = failures;
    }

    fn fielded(&mut self, step: &crate::compiled::graph::Step, source: &LaneProgram, columns: &Columns, positions: Option<&Rc<[usize]>>, failures: &[(usize, usize)]) {
        for &(at, _) in failures {
            let row = positions.map_or(at, |p| p.get(at).copied().unwrap_or(at));
            if self.store.errors.get(row).is_none_or(Option::is_some) {
                continue;
            }
            let scope = zen_expression::Scope::new(columns.row(row));
            let message = match self.runner.evaluate(source, std::slice::from_ref(&scope)).into_iter().next() {
                Some(Err(e)) => format!("Failed to evaluate expression: {e}"),
                _ => "Failed to evaluate expression".to_string(),
            };
            self.store.fail(row, &step.node, Rc::from(message));
        }
    }

    fn report(&mut self, entries: &[(Arc<str>, Arc<str>)], step: &crate::compiled::graph::Step, positions: Option<&Rc<[usize]>>, failures: &[(usize, usize)]) {
        let mut messages: Vec<Option<Rc<str>>> = vec![None; entries.len().max(1)];
        for &(at, stage) in failures {
            let row = positions.map_or(at, |p| p.get(at).copied().unwrap_or(at));
            if self.store.errors.get(row).is_none_or(Option::is_some) {
                continue;
            }
            let message = messages.get_mut(stage).map(|m| {
                m.get_or_insert_with(|| {
                    let source = entries.get(stage).map(|(_, v)| v.as_ref()).unwrap_or_default();
                    Rc::from(format!(r#"Failed to evaluate expression: "{source}""#))
                })
                .clone()
            });
            self.store.fail(row, &step.node, message.unwrap_or_else(|| Rc::from("")));
        }
    }
}
