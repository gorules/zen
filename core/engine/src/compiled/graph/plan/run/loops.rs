use crate::compiled::graph::plan::view::{Ref, SlotId};
use crate::compiled::graph::data::{Data, Layer, Mask};
use zen_expression::Variable;
use zen_types::symbol::Symbol;
use crate::compiled::graph::plan::SegPlan;
use crate::compiled::shred::Shred;
use crate::compiled::leaf::{Bits, ColumnBuilder, Field, Items, Leaf, Records};
use crate::compiled::graph::data::Shape as DataShape;
use crate::compiled::graph::{Body, CompiledGraph, Kind};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::Values;
use super::Run;
use super::store::Shredded;

impl<'a> Run<'_, 'a> {
    pub(super) fn looped(&mut self, seg: &SegPlan, node: petgraph::prelude::NodeIndex, list: Ref, out: SlotId, positions: Option<&Rc<[usize]>>) {
        let graph = self.graph;
        let Some((step, Kind::Node { body, transform })) = graph.step_at(node).map(|step| (step, &step.kind)) else {
            return;
        };
        let leaf = self.store.value(seg, list);
        let rows = self.store.rows;
        let mut at: Vec<usize> = match positions {
            Some(positions) => positions.to_vec(),
            None => (0..rows).collect(),
        };
        let mut native: Option<(Leaf<'a>, Vec<usize>)> = None;
        match leaf.items() {
            Some(items) => {
                if self.native_loop(step, body, transform.pass_through, &items, out, &at) {
                    return;
                }
            }
            None => {
                if let Some((records, odd)) = self.shredded(&leaf, rows) {
                    let (rest, lifted): (Vec<usize>, Vec<usize>) = at.iter().partition(|&&row| Bits::get(&odd, row));
                    if self.native_loop(step, body, transform.pass_through, &Items::Records(records), out, &lifted) {
                        if rest.is_empty() {
                            return;
                        }
                        if let Some(leaf) = self.store.slots[out as usize].take() {
                            native = Some((leaf, lifted));
                            at = rest;
                        }
                    }
                }
            }
        }
        let mut items: Vec<Variable> = Vec::new();
        let mut spans: Vec<(usize, usize, usize)> = Vec::with_capacity(at.len());
        for &row in &at {
            match leaf.get(row).as_array() {
                Some(array) => {
                    let start = items.len();
                    items.extend(array.borrow().iter().cloned());
                    spans.push((row, start, items.len()));
                }
                None => self.store.fail(row, &step.node, Rc::from("Expected an array")),
            }
        }
        let produced = CompiledGraph::items(&mut self.runner, step, body, &items, &[]);
        let mut values = vec![Variable::Null; rows];
        for (row, start, end) in spans {
            let mut collected = Vec::with_capacity(end - start);
            let mut failure = None;
            for (item, output) in items[start..end].iter().zip(&produced[start..end]) {
                match output {
                    Ok(out) => collected.push(match transform.pass_through {
                        true => item.clone().merge_clone(out),
                        false => out.clone(),
                    }),
                    Err(e) => {
                        failure = Some(e.clone());
                        break;
                    }
                }
            }
            match failure {
                Some(e) => self.store.fail(row, &step.node, Rc::from(e)),
                None => values[row] = Variable::from_array(collected),
            }
        }
        if let Some((leaf, lifted)) = native {
            lifted.into_iter().for_each(|row| values[row] = leaf.get(row));
        }
        self.store.slots[out as usize] = Some(Leaf::Any(values.into()));
    }

    fn shredded(&mut self, leaf: &Leaf<'a>, rows: usize) -> Shredded<'a> {
        if let Some((_, cached)) = self.shreds.iter().find(|(seen, _)| seen.same(leaf)) {
            return cached.clone();
        }
        let shredded = Self::shred(leaf, rows);
        self.shreds.push((leaf.clone(), shredded.clone()));
        shredded
    }

    fn shred(leaf: &Leaf<'a>, rows: usize) -> Shredded<'a> {
        let column = leaf.column();
        let shred = Shred::of(&column, rows)?;
        let mut odd = vec![0u64; rows.div_ceil(64)];
        shred.hosted.iter().filter(|&&row| leaf.get(row).as_array().is_some()).for_each(|&row| Bits::set(&mut odd, row, true));
        let valid = match shred.validity {
            Some(bits) => bits.to_vec(),
            None => Bits::ones(rows),
        };
        let count = shred.children;
        let fields = shred
            .fields
            .into_iter()
            .map(|(name, array)| {
                let present: Rc<[u64]> = match array.validity_bits() {
                    Some(bits) => Bits::window(bits, 0, count).into(),
                    None => Bits::ones(count).into(),
                };
                (Symbol::from(name.as_ref()), Leaf::typed(array), present)
            })
            .collect();
        Some((Rc::new(Records::new(rows, shred.offsets.to_vec(), valid, fields, None)), odd.into()))
    }

    fn native_loop(&mut self, step: &crate::compiled::graph::Step, body: &Body, pass_through: bool, source: &Items<'a>, out: SlotId, at: &[usize]) -> bool {
        let Some(keys) = CompiledGraph::item_keys(step, body) else {
            return false;
        };
        let Some(fields) = source.fields() else {
            return false;
        };
        let scalar = |leaf: &Leaf| !matches!(leaf.column().values, Values::Any(_) | Values::List { .. } | Values::Struct { .. });
        let flat = fields.iter().all(|(name, leaf, _)| !name.is_empty() && !name.contains('.') && (scalar(leaf) || !keys.iter().any(|k| k.strip_prefix(name.as_str()).is_some_and(|r| r.starts_with('.')))));
        if !flat {
            return false;
        }
        let rows = self.store.rows;
        let mut picked: Vec<usize> = Vec::new();
        let mut spans: Vec<(usize, usize, usize)> = Vec::with_capacity(at.len());
        let mut missing: Vec<usize> = Vec::new();
        for &row in at {
            match source.range(row) {
                Some((a, b)) => {
                    let start = picked.len();
                    picked.extend(a..b);
                    spans.push((row, start, picked.len()));
                }
                None => missing.push(row),
            }
        }
        let count = picked.len();
        let identity = count == source.count() && picked.iter().enumerate().all(|(i, p)| i == *p);
        let picks: Rc<[usize]> = picked.into();
        let local = |leaf: &Leaf<'a>, present: &Rc<[u64]>| -> (Leaf<'a>, Rc<[u64]>) {
            match identity {
                true => (leaf.clone(), present.clone()),
                false => (leaf.pick(&picks), Bits::gather(present, &picks).into()),
            }
        };
        let fields: Vec<Field<'a>> = fields
            .iter()
            .map(|(name, leaf, present)| {
                let (leaf, present) = local(leaf, present);
                (name.clone(), leaf, present)
            })
            .collect();
        let mut leaves = Vec::with_capacity(keys.len());
        let mut present = Vec::with_capacity(keys.len());
        for key in &keys {
            match fields.iter().find(|(name, _, _)| name.as_str() == key.as_ref()) {
                Some((_, leaf, bits)) => {
                    leaves.push(leaf.clone());
                    present.push(Mask::Bits(bits.clone()));
                }
                None => {
                    leaves.push(Leaf::nulls(count));
                    present.push(Mask::Bits(vec![0u64; count.div_ceil(64)].into()));
                }
            }
        }
        let items: Rc<[usize]> = (0..count).collect();
        let data = Data::record(&items, Layer::new(keys.into(), leaves, Some(present)), None);
        let (output, failures) = CompiledGraph::item_run(&mut self.runner, step, body, &data);
        let paths: Vec<(Arc<str>, Leaf<'a>)> = output.leaves().to_vec();
        let shaped = matches!(output.shape, DataShape::Patched { base: None, .. })
            && paths.iter().all(|(path, _)| !path.is_empty() && !path.split('.').any(str::is_empty))
            && !paths.iter().any(|(a, _)| paths.iter().any(|(b, _)| b.strip_prefix(a.as_ref()).is_some_and(|r| r.starts_with('.'))));
        let nulled: Option<Rc<[u64]>> = match (&output.shape, pass_through) {
            (DataShape::Patched { nulls: Some(nulls), .. }, false) => Some(nulls.clone()),
            _ => None,
        };
        let mut owner = vec![0usize; count];
        for &(row, start, end) in &spans {
            owner[start..end].iter_mut().for_each(|o| *o = row);
        }
        let mut failed = vec![false; rows];
        let mut failures = failures;
        failures.sort_by_key(|(item, _)| *item);
        for (item, message) in failures {
            let Some(&row) = owner.get(item) else {
                continue;
            };
            if !failed[row] {
                failed[row] = true;
                self.store.fail(row, &step.node, Rc::from(message));
            }
        }
        for row in missing {
            self.store.fail(row, &step.node, Rc::from("Expected an array"));
        }
        let merged: Option<Vec<Field<'a>>> = match (shaped, pass_through) {
            (false, _) => None,
            (true, false) => Some(
                paths
                    .iter()
                    .map(|(path, _)| {
                        let (leaf, bits) = output.present(path);
                        (Symbol::from(path.as_ref()), leaf, bits.into())
                    })
                    .collect(),
            ),
            (true, true) => Self::passed(&fields, &output, &paths, count, &scalar),
        };
        let mut offsets = Vec::with_capacity(rows + 1);
        let mut valid = vec![0u64; rows.div_ceil(64)];
        let mut next = spans.iter().peekable();
        let mut total = 0i32;
        offsets.push(0);
        for row in 0..rows {
            if let Some(&&(span_row, start, end)) = next.peek() {
                if span_row == row {
                    total += (end - start) as i32;
                    if !failed[row] {
                        Bits::set(&mut valid, row, true);
                    }
                    next.next();
                }
            }
            offsets.push(total);
        }
        let leaf = match merged {
            Some(fields) => Leaf::Records(Rc::new(Records::new(rows, offsets, valid, fields, nulled))),
            None => {
                let values = output.materialized();
                let mut arrays = vec![Variable::Null; rows];
                for &(row, start, end) in &spans {
                    if failed[row] {
                        continue;
                    }
                    let collected: Vec<Variable> = (start..end)
                        .map(|item| {
                            let value = values.get(item).cloned().unwrap_or(Variable::Null);
                            match pass_through {
                                true => Records::object(&fields, item).merge_clone(&value),
                                false => value,
                            }
                        })
                        .collect();
                    arrays[row] = Variable::from_array(collected);
                }
                Leaf::Any(arrays.into())
            }
        };
        self.store.slots[out as usize] = Some(leaf);
        true
    }

    fn passed(fields: &[Field<'a>], output: &Data<'a>, paths: &[(Arc<str>, Leaf<'a>)], count: usize, scalar: &dyn Fn(&Leaf) -> bool) -> Option<Vec<Field<'a>>> {
        let mut merged: Vec<Field<'a>> = fields.to_vec();
        for (path, _) in paths {
            let (leaf, bits) = output.present(path);
            if let Some((top, _)) = path.split_once('.') {
                if fields.iter().any(|(name, _, _)| name.as_str() == top) {
                    return None;
                }
                merged.push((Symbol::from(path.as_ref()), leaf, bits.into()));
                continue;
            }
            let valued: Vec<u64> = leaf.valued(count).iter().zip(&bits).map(|(v, b)| v & b).collect();
            match merged.iter().position(|(name, _, _)| name.as_str() == path.as_ref()) {
                None => merged.push((Symbol::from(path.as_ref()), leaf, valued.into())),
                Some(at) => {
                    let (name, item, present) = merged[at].clone();
                    if !scalar(&item) {
                        return None;
                    }
                    let (patch, base) = (leaf.column(), item.column());
                    let mut builder = ColumnBuilder::with_capacity(count);
                    let mut kept = vec![0u64; count.div_ceil(64)];
                    for i in 0..count {
                        match (Bits::get(&valued, i), Bits::get(&bits, i), Bits::get(&present, i)) {
                            (true, _, _) => {
                                builder.push_cell(&patch, i);
                                Bits::set(&mut kept, i, true);
                            }
                            (false, false, true) => {
                                builder.push_cell(&base, i);
                                Bits::set(&mut kept, i, true);
                            }
                            _ => builder.push_null(),
                        }
                    }
                    merged[at] = (name, Leaf::typed(builder.finish()), kept.into());
                }
            }
        }
        Some(merged)
    }
}
