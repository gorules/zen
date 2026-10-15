use crate::compiled::policy::schedule::{Demand, Event};
use crate::compiled::graph::table::{Matching, Table};
use crate::compiled::leaf::{ColumnBuilder, Leaf, Literal};
use crate::policy::blocks::TableSelection;
use crate::policy::host::Pick;
use zen_expression::Variable;
use super::{Pass, Units};

impl<'s, 'a> Pass<'s, 'a> {
    pub(super) fn table_select(&mut self, block: usize, table: &Table, units: &Units) {
        let count = units.units.len();
        let words = table.words();
        let mut matching = table.matching(count);
        for (input, field) in table.fields().enumerate() {
            let leaf = match self.direct(units, field) {
                Some(leaf) => leaf,
                None => self.evaluate(units, field),
            };
            table.matched(self.runner, input, &leaf, &mut matching);
        }
        let Matching { all, host, .. } = matching;
        let outputs = table.picked().len();
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
            for (c, first) in table.firsts(&all[local * words..(local + 1) * words]).enumerate() {
                self.chosen[at].1[unit * outputs + c] = first;
            }
        }
        let hosted: Vec<usize> = (0..count).filter(|&local| host[local]).map(|local| units.owners[local]).collect();
        if hosted.is_empty() {
            return;
        }
        if units.member.is_some() {
            hosted.into_iter().for_each(|row| self.fall_hosted(row));
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

    pub(super) fn table_commit(&mut self, block: usize, table: &Table, units: &Units) {
        let outputs = table.picked().len();
        let slot = self.chosen.iter().position(|(b, _)| *b == block);
        let chosen = slot.map(|at| std::mem::take(&mut self.chosen[at].1)).unwrap_or_default();
        for (c, output) in table.picked().iter().enumerate() {
            if let Some((values, remap)) = output.constants() {
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
                    values.push(output.constants[*rule as usize].as_ref().map_or(Variable::Null, Literal::variable));
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

    pub(super) fn table_demand(&self, block: usize, row: usize) -> Option<Demand> {
        if self.hosted(block, row) {
            return None;
        }
        let table = self.plan.tables[block].as_ref()?;
        let (_, chosen) = self.chosen.iter().find(|(b, _)| *b == block)?;
        let plan = self.artifact.read_plans.get(&self.plan.blocks.refs[block])?;
        let mut demanded = Vec::new();
        match self.entity_of(block) {
            None => Pick::Table(TableSelection::used(table.used(chosen, row))).collect_reads(plan, &mut demanded),
            Some(at) => {
                let (a, b) = self.sheet.entities[at].frame.range(row).unwrap_or((0, 0));
                for child in a..b {
                    Pick::Table(TableSelection::used(table.used(chosen, child))).collect_reads(plan, &mut demanded);
                }
                demanded.sort();
                demanded.dedup();
            }
        }
        Some(demanded.into())
    }
}
