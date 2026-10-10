use crate::compiled::policy::PolicyPlan;
use crate::compiled::graph::bound::Outs;
use crate::compiled::leaf::{Array, Leaf};
use std::sync::Arc;
use zen_expression::lane::{Binding, Column, Columns, Dictionary, LaneProgram, Values};
use zen_expression::{Scope, Variable};
use super::{Bind, Pass, Sheet, Slot};

impl<'s, 'a> Pass<'s, 'a> {
    pub(super) fn run(&mut self, program: &LaneProgram, positions: &[usize]) -> (Leaf<'a>, Vec<bool>) {
        let (mut leaves, failed) = self.run_outs(program, positions);
        let leaf = match leaves.is_empty() {
            true => Leaf::nulls(positions.len()),
            false => leaves.swap_remove(0),
        };
        (leaf, failed)
    }

    pub(super) fn run_outs(&mut self, program: &LaneProgram, positions: &[usize]) -> (Vec<Leaf<'a>>, Vec<bool>) {
        let p = program.program();
        let nested = p.site_keys.iter().flatten().any(|key| {
            self.sheet.entities.iter().any(|e| e.active && e.path.as_ref() == key.as_str() && !e.flat())
        });
        let cached = nested || !PolicyPlan::scalar(program);
        if cached {
            for key in p.site_keys.iter().flatten() {
                if let Some(entity) = self.sheet.entities.iter_mut().find(|e| e.active && e.path.as_ref() == key.as_str()) {
                    entity.materialize();
                }
            }
        }
        let mut failed = vec![false; positions.len()];
        if p.writes_env || p.chain || p.opaque() || p.rows {
            let scopes: Vec<Scope> = positions.iter().map(|&row| self.scope(row)).collect();
            let mut values = vec![Variable::Null; positions.len()];
            self.runner.evaluate_with(program, &scopes, |at, result| match result {
                Ok(value) => values[at] = value,
                Err(_) => failed[at] = true,
            });
            return (vec![Leaf::Any(values.into())], failed);
        }
        for key in p.site_keys.iter().flatten() {
            let stale: Vec<usize> = self
                .sheet
                .entities
                .iter()
                .enumerate()
                .filter(|(_, entity)| entity.active && Sheet::related(key, &entity.path).is_some_and(|o| o != std::cmp::Ordering::Equal))
                .map(|(at, _)| at)
                .collect();
            stale.into_iter().for_each(|at| self.sheet.retire(at));
        }
        let mut columns = Columns::new(self.sheet.rows);
        let mut used: Vec<Slot> = Vec::new();
        let mut rows = false;
        let bound: Vec<Binding> = p
            .site_keys
            .iter()
            .map(|key| {
                let slot = match key.as_deref() {
                    None => return Binding::Row,
                    Some(key) => match self.sheet.entities.iter().position(|e| e.active && e.path.as_ref() == key) {
                        Some(entity) => Slot::Composed(entity),
                        None => match self.sheet.bind(key, positions, self.shared) {
                            Bind::Absent => return Binding::Absent,
                            Bind::Row => {
                                rows = true;
                                return Binding::Row;
                            }
                            Bind::Column(entry) => Slot::Entry(entry),
                        },
                    },
                };
                match used.iter().position(|e| *e == slot) {
                    Some(at) => Binding::Column(at),
                    None => {
                        used.push(slot);
                        Binding::Column(used.len() - 1)
                    }
                }
            })
            .collect();
        let scopes = match rows {
            true => crate::compiled::graph::bound::Scopes::Owned(self.scopes(positions)),
            false => crate::compiled::graph::bound::Bound::blank(self.sheet.rows),
        };
        let views: Vec<(Arc<str>, Leaf<'a>)> = used
            .iter()
            .filter_map(|slot| match slot {
                Slot::Entry(entry) => Some((self.sheet.entries[*entry].path.clone(), self.sheet.entries[*entry].leaf.clone())),
                Slot::Composed(at) if cached => {
                    let entity = &self.sheet.entities[*at];
                    let values: Vec<Variable> = (0..self.sheet.rows).map(|row| entity.compose(row).unwrap_or(Variable::Null)).collect();
                    Some((entity.path.clone(), Leaf::Any(values.into())))
                }
                Slot::Composed(_) => None,
            })
            .collect();
        let parts: Vec<(usize, Vec<(Arc<str>, Leaf<'a>)>)> = used
            .iter()
            .filter_map(|slot| match slot {
                Slot::Composed(at) if !cached => {
                    let entity = &self.sheet.entities[*at];
                    Some((*at, (0..entity.fields.len()).map(|f| (entity.fields[f].name.clone(), entity.view(f))).collect()))
                }
                _ => None,
            })
            .collect();
        let shapes: Vec<(usize, &[i32], Option<(&[u64], usize)>)> = parts
            .iter()
            .map(|(at, _)| {
                let entity = &self.sheet.entities[*at];
                (entity.children, entity.offsets.as_ref(), entity.validity.as_deref().map(|bits| (bits, 0)))
            })
            .collect();
        let fields: Vec<Vec<(&str, Column)>> = parts.iter().map(|(_, leaves)| leaves.iter().map(|(n, l)| (n.as_ref(), l.column())).collect()).collect();
        let structs: Vec<Column> = fields
            .iter()
            .zip(&shapes)
            .map(|(f, (len, _, _))| Column::new(Values::Struct { fields: f, len: *len }))
            .collect();
        let lists: Vec<(&str, Column)> = structs
            .iter()
            .zip(&shapes)
            .zip(&parts)
            .map(|((child, (_, offsets, validity)), (at, _))| {
                (
                    self.sheet.entities[*at].path.as_ref(),
                    Column {
                        values: Values::List {
                            offsets,
                            child: Dictionary::Column(child),
                        },
                        validity: *validity,
                    },
                )
            })
            .collect();
        let (mut next_view, mut next_list) = (views.iter(), lists.iter());
        for slot in &used {
            match (slot, cached) {
                (Slot::Entry(_), _) | (Slot::Composed(_), true) => {
                    if let Some((path, view)) = next_view.next() {
                        columns = columns.column(path.as_ref(), view.column());
                    }
                }
                (Slot::Composed(_), false) => {
                    if let Some((path, list)) = next_list.next() {
                        columns = columns.column(path, *list);
                    }
                }
            }
        }
        let mut outs = Outs::take();
        let subset = (positions.len() != self.sheet.rows).then_some(positions);
        let special = self.plan.special(program, &columns);
        let program = special.as_deref().unwrap_or(program);
        Self::prefer_codes(program, &mut outs);
        self.runner.evaluate_sites(program, scopes.slice(), &columns, &bound, subset, &mut outs, |at, _, _| {
            if let Some(f) = failed.get_mut(at) {
                *f = true;
            }
        });
        let leaves = outs.iter_mut().map(|out| Leaf::typed(Array::from_output(out))).collect();
        outs.iter_mut().for_each(zen_expression::lane::Output::prefer_text);
        (leaves, failed)
    }

    pub(super) fn prefer_codes(program: &LaneProgram, outs: &mut Outs) {
        let p = program.program();
        outs.resize_with(p.outputs.len().max(1), zen_expression::lane::Output::new);
        outs.iter_mut().for_each(zen_expression::lane::Output::prefer_codes);
    }
}
