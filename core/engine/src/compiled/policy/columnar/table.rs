use crate::compiled::policy::schedule::{Demand, Event};
use crate::compiled::policy::table::TableNative;
use crate::compiled::policy::Fixed;
use crate::compiled::leaf::{Bits, ColumnBuilder, Leaf};
use crate::policy::blocks::TableSelection;
use crate::policy::host::Pick;
use std::sync::Arc;
use zen_expression::lane::{Binding, Columns};
use zen_expression::{Scope, Variable};
use super::{Pass, Units};

impl<'s, 'a> Pass<'s, 'a> {
    pub(super) fn table_select(&mut self, block: usize, table: &TableNative, units: &Units) {
        let count = units.units.len();
        let words = table.words;
        let tail = match (table.rules, table.rules % 64) {
            (0, _) => 0,
            (_, 0) => u64::MAX,
            (_, t) => (1u64 << t) - 1,
        };
        let mut all = vec![u64::MAX; count * words];
        all.chunks_exact_mut(words).for_each(|row| {
            if let Some(last) = row.last_mut() {
                *last &= tail;
            }
        });
        let mut bits = Vec::new();
        let mut host = vec![false; count];
        for input in &table.inputs {
            let leaf = match self.direct(units, &input.field) {
                Some(leaf) => leaf,
                None => self.evaluate(units, &input.field),
            };
            let column = leaf.column();
            if input.ordered {
                for (local, hosted) in host.iter_mut().enumerate() {
                    if !column.valid(local) || column.number(local).is_none() {
                        *hosted = true;
                    }
                }
            }
            input.cells.indexed_column(&column, count, &mut bits);
            for other in 0..input.cells.others() {
                let Some((program, rules)) = input.cells.other(other) else {
                    continue;
                };
                let locals: Vec<usize> = (0..count)
                    .filter(|&local| all[local * words..(local + 1) * words].iter().zip(rules).any(|(a, r)| a & r != 0))
                    .collect();
                if locals.is_empty() {
                    continue;
                }
                let scopes: Vec<Scope> = locals
                    .iter()
                    .map(|&local| {
                        let mut scope = Scope::new(Variable::Null);
                        scope.set_local(Variable::dollar_key(), leaf.get(local));
                        scope
                    })
                    .collect();
                let mut passed: Vec<Option<bool>> = vec![None; locals.len()];
                self.runner.evaluate_bound(program, &scopes, &Columns::new(locals.len()), &|_| Binding::Row, None, |at, result| {
                    if let Some(slot) = passed.get_mut(at) {
                        *slot = result.ok().map(|values| matches!(values.as_slice(), [Variable::Bool(true)]));
                    }
                });
                for (&local, pass) in locals.iter().zip(passed) {
                    match pass {
                        None => host[local] = true,
                        Some(true) => bits[local * words..(local + 1) * words].iter_mut().zip(rules).for_each(|(b, r)| *b |= r),
                        Some(false) => {}
                    }
                }
            }
            all.iter_mut().zip(&bits).for_each(|(a, b)| *a &= b);
        }
        let outputs = table.outputs.len();
        let total = match units.member {
            Some(at) => self.sheet.entities[at].frame.children,
            None => self.sheet.rows,
        };
        let at = match self.chosen.iter().position(|(b, _)| *b == block) {
            Some(at) => at,
            None => {
                self.chosen.push((block, vec![u32::MAX; total * outputs]));
                self.chosen.len() - 1
            }
        };
        for (local, &unit) in units.units.iter().enumerate() {
            let matched = &all[local * words..(local + 1) * words];
            for (c, output) in table.outputs.iter().enumerate() {
                let first = matched
                    .iter()
                    .zip(&output.filled)
                    .enumerate()
                    .find_map(|(w, (m, f))| (m & f != 0).then(|| (w * 64) as u32 + (m & f).trailing_zeros()));
                self.chosen[at].1[unit * outputs + c] = first.unwrap_or(u32::MAX);
            }
        }
        let hosted: Vec<usize> = (0..count).filter(|&local| host[local]).map(|local| units.owners[local]).collect();
        if hosted.is_empty() {
            return;
        }
        if units.member.is_some() {
            hosted.into_iter().for_each(|row| self.fall(row));
            return;
        }
        let at = match self.hosting.iter().position(|(b, _)| *b == block) {
            Some(at) => at,
            None => {
                self.hosting.push((block, vec![false; self.sheet.rows]));
                self.hosting.len() - 1
            }
        };
        hosted.iter().for_each(|&row| self.hosting[at].1[row] = true);
        self.delegate(Event::Select(block), &hosted);
    }

    pub(super) fn table_commit(&mut self, block: usize, table: &TableNative, units: &Units) {
        let outputs = table.outputs.len();
        let slot = self.chosen.iter().position(|(b, _)| *b == block);
        let chosen = slot.map(|at| std::mem::take(&mut self.chosen[at].1)).unwrap_or_default();
        for (c, output) in table.outputs.iter().enumerate() {
            if let Some((values, remap)) = Self::table_constants(output) {
                let codes: Vec<i32> = units
                    .units
                    .iter()
                    .map(|&unit| {
                        let rule = chosen.get(unit * outputs + c).copied().unwrap_or(u32::MAX);
                        remap.get(rule as usize).copied().unwrap_or(-1)
                    })
                    .collect();
                let leaf = Self::coded(&values, codes);
                match units.member {
                    Some(at) => self.member_write(at, &output.field, leaf, &units.units),
                    None => self.write(&output.field, leaf, &units.units),
                }
                continue;
            }
            let mut values: Vec<Variable> = vec![Variable::Null; units.units.len()];
            let mut groups: Vec<(u32, Vec<usize>)> = Vec::new();
            for (local, &unit) in units.units.iter().enumerate() {
                let rule = chosen.get(unit * outputs + c).copied().unwrap_or(u32::MAX);
                if rule == u32::MAX {
                    continue;
                }
                match groups.iter_mut().find(|(r, _)| *r == rule) {
                    Some((_, locals)) => locals.push(local),
                    None => groups.push((rule, vec![local])),
                }
            }
            if groups.iter().all(|(rule, _)| output.constants.get(*rule as usize).is_some_and(Option::is_some)) {
                let mut values: Vec<Variable> = Vec::with_capacity(groups.len());
                let mut codes = vec![-1i32; units.units.len()];
                for (rule, locals) in &groups {
                    let code = values.len() as i32;
                    values.push(output.constants[*rule as usize].as_ref().map_or(Variable::Null, Fixed::variable));
                    locals.iter().for_each(|&local| codes[local] = code);
                }
                let leaf = Self::coded(&values, codes);
                match units.member {
                    Some(at) => self.member_write(at, &output.field, leaf, &units.units),
                    None => self.write(&output.field, leaf, &units.units),
                }
                continue;
            }
            for (rule, locals) in groups {
                if let Some(Some(value)) = output.constants.get(rule as usize) {
                    let value = value.variable();
                    locals.iter().for_each(|&local| values[local] = value.clone());
                    continue;
                }
                let Some(Some(program)) = output.cells.get(rule as usize) else {
                    continue;
                };
                let leaf = self.evaluate(&units.subset(&locals), program);
                for (i, &local) in locals.iter().enumerate() {
                    values[local] = leaf.get(i);
                }
            }
            let mut builder = ColumnBuilder::with_capacity(values.len());
            values.into_iter().for_each(|value| builder.push_variable(value));
            let leaf = Leaf::typed(builder.finish());
            match units.member {
                Some(at) => self.member_write(at, &output.field, leaf, &units.units),
                None => self.write(&output.field, leaf, &units.units),
            }
        }
        if let Some(at) = slot {
            self.chosen[at].1 = chosen;
        }
    }

    fn table_constants(output: &crate::compiled::policy::table::TableOutput) -> Option<(Vec<Variable>, Vec<i32>)> {
        let mut values = Vec::new();
        let mut remap = vec![-1i32; output.constants.len()];
        for (rule, constant) in output.constants.iter().enumerate() {
            if !Bits::get(&output.filled, rule) {
                continue;
            }
            remap[rule] = values.len() as i32;
            values.push(constant.as_ref()?.variable());
        }
        Some((values, remap))
    }

    fn table_cells(table: &TableNative, chosen: &[u32], unit: usize) -> Vec<(u32, Arc<str>)> {
        let outputs = table.outputs.len();
        let mut used: Vec<(u32, usize)> = (0..outputs)
            .filter_map(|c| {
                let rule = chosen.get(unit * outputs + c).copied()?;
                (rule != u32::MAX).then_some((rule, c))
            })
            .collect();
        used.sort();
        used.into_iter().map(|(rule, c)| (rule, table.outputs[c].id.clone())).collect()
    }

    pub(super) fn table_demand(&self, block: usize, row: usize) -> Option<Demand> {
        if self.hosted(block, row) {
            return None;
        }
        let table = self.plan.tables[block].as_ref()?;
        let (_, chosen) = self.chosen.iter().find(|(b, _)| *b == block)?;
        let plan = self.artifact.read_plans.get(&self.plan.blocks.refs[block])?;
        let mut demanded = Vec::new();
        match self.entity_of(block) {
            None => Pick::Table(TableSelection::used(Self::table_cells(table, chosen, row))).collect_reads(plan, &mut demanded),
            Some(at) => {
                let (a, b) = self.sheet.entities[at].frame.range(row).unwrap_or((0, 0));
                for child in a..b {
                    Pick::Table(TableSelection::used(Self::table_cells(table, chosen, child))).collect_reads(plan, &mut demanded);
                }
                demanded.sort();
                demanded.dedup();
            }
        }
        Some(demanded.into())
    }
}
