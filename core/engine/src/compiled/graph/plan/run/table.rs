use crate::compiled::graph::data::Mask;
use zen_expression::Variable;
use crate::compiled::graph::plan::{SegPlan, TableMode, TableOp};
use crate::compiled::graph::bound::Bound;
use crate::compiled::leaf::{Array, Bits, Leaf, Literal, Records, Store as Buffer, Templated};
use crate::compiled::graph::{Body, Kind};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Binding as Bind, Column, Columns};
use crate::compiled::core::exec::Dedup;
use super::Run;
use super::store::{Spread, Store};

impl<'a> Run<'_, 'a> {
    pub(super) fn table(&mut self, seg: &SegPlan, op: &TableOp, positions: Option<&Rc<[usize]>>) {
        let Some(step) = self.graph.step_at(op.node) else {
            return;
        };
        let Kind::Node {
            body: Body::Table(table),
            ..
        } = &step.kind
        else {
            return;
        };
        let local = positions.map_or(self.store.rows, |p| p.len());
        let mut leaves: Vec<Option<Leaf<'a>>> = op
            .reads
            .iter()
            .map(|(_, r)| {
                r.map(|r| {
                    let leaf = self.store.value(seg, r);
                    match positions {
                        Some(positions) => leaf.regroup(positions),
                        None => leaf,
                    }
                })
            })
            .collect();
        let keyed = match (op.mode, table.stable) {
            (TableMode::First, true) => {
                let present: Vec<Leaf<'a>> = leaves.iter().flatten().cloned().collect();
                match present.len() > 1 {
                    true => Dedup::distinct(&present, &[], None, local),
                    false => None,
                }
            }
            _ => None,
        };
        let evaluated = match &keyed {
            Some((reps, _)) => {
                let reps: Rc<[usize]> = reps.as_slice().into();
                leaves = leaves.into_iter().map(|leaf| leaf.map(|leaf| leaf.regroup(&reps))).collect();
                reps.len()
            }
            None => local,
        };
        let mut columns = Columns::new(evaluated);
        columns.columns.reserve_exact(op.reads.len());
        let mut names: Vec<(Arc<str>, Bind)> = Vec::with_capacity(op.reads.len());
        let held: Vec<Option<Rc<Records<'a>>>> = match Records::LANE_VIEW {
            true => leaves.iter().map(|leaf| leaf.as_ref().and_then(Leaf::records)).collect(),
            false => Vec::new(),
        };
        let parts: Vec<Option<Vec<(&str, Column)>>> = match held.iter().any(Option::is_some) {
            true => held.iter().map(|r| r.as_ref().map(|r| r.parts())).collect(),
            false => Vec::new(),
        };
        let items: Vec<Option<Column>> = held.iter().zip(&parts).map(|(r, p)| Some(r.as_ref()?.item(p.as_ref()?))).collect();
        let views: Vec<Option<Column>> = leaves
            .iter()
            .enumerate()
            .map(|(at, leaf)| match (held.get(at).and_then(Option::as_ref), items.get(at).and_then(Option::as_ref)) {
                (Some(record), Some(item)) => Some(record.list(item)),
                _ => leaf.as_ref().map(Leaf::column),
            })
            .collect();
        for ((key, _), view) in op.reads.iter().zip(&views) {
            match view {
                Some(view) => {
                    names.push((key.clone(), Bind::Column(columns.columns.len())));
                    columns = columns.column(key.as_ref(), *view);
                }
                None => names.push((key.clone(), Bind::Absent)),
            }
        }
        let bound = Bound {
            scopes: Bound::blank(evaluated),
            columns,
            bind: Box::new(move |key: &str| match key.starts_with('$') {
                true => Bind::Row,
                false => names
                    .iter()
                    .find(|(k, _)| k.as_ref() == key)
                    .map_or(Bind::Absent, |(_, b)| *b),
            }),
        };
        let (columns, present, unmatched) = match op.mode {
            TableMode::First => table.columns(table.first(&mut self.runner, &bound)),
            TableMode::Collected => {
                let collected = table.first_collect(&mut self.runner, &bound);
                (collected.columns, collected.present, collected.unmatched)
            }
            TableMode::Rows => {
                if let Some((records, codes, size)) = table.collect_records(&mut self.runner, &bound) {
                    let codes: Rc<[i32]> = match positions {
                        None => codes.into(),
                        Some(positions) => {
                            let mut full = vec![-1i32; self.store.rows];
                            positions.iter().zip(&codes).for_each(|(&row, &code)| full[row] = code);
                            full.into()
                        }
                    };
                    let leaf = match positions {
                        None => Leaf::Records(Rc::new(records)),
                        Some(positions) => match records.spread(positions, self.store.rows) {
                            Some(spread) => Leaf::Records(Rc::new(spread)),
                            None => self.store.place(Leaf::Records(Rc::new(records)), Some(positions)),
                        },
                    };
                    if let Some(slot) = op.outs.first() {
                        self.store.slots[*slot as usize] = Some(leaf);
                        self.store.hints.push((*slot, codes, size));
                    }
                    return;
                }
                let (values, hint) = match table.collect_keyed(&mut self.runner, &bound) {
                    Some((values, codes, size)) => {
                        let codes: Rc<[i32]> = match positions {
                            None => codes.into(),
                            Some(positions) => {
                                let mut full = vec![-1i32; self.store.rows];
                                positions.iter().zip(&codes).for_each(|(&row, &code)| full[row] = code);
                                full.into()
                            }
                        };
                        (values, Some((codes, size)))
                    }
                    None => (
                        table.evaluate(&mut self.runner, &bound).into_iter().map(|out| table.value(out)).collect::<Vec<Variable>>(),
                        None,
                    ),
                };
                let leaf = self.store.place(Leaf::Any(values.into()), positions);
                if let Some(slot) = op.outs.first() {
                    self.store.slots[*slot as usize] = Some(leaf);
                    if let Some((codes, size)) = hint {
                        self.store.hints.push((*slot, codes, size));
                    }
                }
                return;
            }
        };
        let (columns, present, unmatched) = match &keyed {
            Some((_, ids)) => (
                columns.iter().map(|leaf| leaf.expand(ids)).collect(),
                present
                    .iter()
                    .map(|mask| Mask::Bits(Bits::of(local, |row| mask.get(ids[row] as usize)).into()))
                    .collect(),
                Bits::of(local, |row| Bits::get(&unmatched, ids[row] as usize)),
            ),
            None => (columns, present, unmatched),
        };
        let mut spread: Vec<Spread> = Vec::new();
        let mut cached = |store: &Store<'a>, dense: Rc<[u64]>| match spread.iter().find(|(d, _)| Rc::ptr_eq(d, &dense) || *d == dense) {
            Some((_, bits)) => bits.clone(),
            None => {
                let bits = store.spread(&dense, positions);
                spread.push((dense, bits.clone()));
                bits
            }
        };
        for ((slot, leaf), (m, mask)) in op.outs.iter().zip(columns).zip(op.present.iter().zip(present)) {
            let leaf = self.store.place(leaf, positions);
            self.store.slots[*slot as usize] = Some(leaf);
            let bits = cached(&self.store, mask.dense(local));
            self.store.produce(*m, bits);
        }
        let mut matched: Vec<u64> = unmatched.iter().map(|w| !w).collect();
        Bits::trim(&mut matched, local);
        let matched = cached(&self.store, matched.into());
        self.store.produce(op.matched, matched);
    }

    pub(super) fn templated(array: &Array, rows: usize) -> Option<Leaf<'a>> {
        if !array.valid(0) {
            return None;
        }
        let Buffer::Any(values) = array.store() else {
            return None;
        };
        let literal = match values.first()? {
            Variable::Array(items) if items.borrow().iter().all(|item| matches!(item, Variable::Null | Variable::Bool(_) | Variable::Number(_) | Variable::String(_))) => return None,
            value @ (Variable::Array(_) | Variable::Object(_)) => Literal::of(value)?,
            _ => return None,
        };
        Some(Templated::leaf(vec![0i32; rows].into(), Rc::from(vec![literal])))
    }
}
