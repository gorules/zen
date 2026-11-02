use super::{Chosen, Pass, Units};
use crate::compiled::leaf::{Bits, ColumnBuilder, Leaf, Literal};
use crate::compiled::policy::schedule::{Demand, Event};
use crate::compiled::table::{Matching, Picked, Table};
use crate::policy::blocks::TableSelection;
use crate::policy::host::Pick;
use std::sync::Arc;
use zen_expression::Variable;

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
        let collects = table.gathers();
        let chosen = self.chosen.entry(block, || Chosen {
            firsts: vec![u32::MAX; total * outputs],
            matched: match collects {
                true => vec![0u64; total * words],
                false => Vec::new(),
            },
        });
        for (local, &unit) in units.units.iter().enumerate() {
            let rules = &all[local * words..(local + 1) * words];
            for (c, first) in table.firsts(rules).enumerate() {
                chosen.firsts[unit * outputs + c] = first;
            }
            if collects {
                chosen.matched[unit * words..(unit + 1) * words].copy_from_slice(rules);
            }
        }
        let hosted: Vec<usize> = (0..count)
            .filter(|&local| host[local])
            .map(|local| units.owners[local])
            .collect();
        if hosted.is_empty() {
            return;
        }
        if units.member.is_some() {
            hosted.into_iter().for_each(|row| self.fall(row));
            return;
        }
        let rows = self.sheet.rows;
        let hosting = self.hosting.entry(block, || vec![false; rows]);
        hosted.iter().for_each(|&row| hosting[row] = true);
        self.delegate(Event::Select(block), &hosted);
    }

    fn put(&mut self, units: &Units, field: &Arc<str>, leaf: Leaf<'a>) {
        match units.member {
            Some(at) => self.member_write(at, field, leaf, &units.units),
            None => self.write(field, leaf, &units.units),
        }
    }

    fn cell_values(
        &mut self,
        units: &Units,
        output: &Picked,
        rule: usize,
        locals: &[usize],
    ) -> Vec<Variable> {
        if let Some(Some(value)) = output.constants.get(rule) {
            let value = value.variable();
            return locals.iter().map(|_| value.clone()).collect();
        }
        let Some(Some(program)) = output.cells.get(rule) else {
            return locals.iter().map(|_| Variable::Null).collect();
        };
        let leaf = self.evaluate(&units.subset(locals), program);
        (0..locals.len()).map(|i| leaf.get(i)).collect()
    }

    pub(super) fn table_commit(&mut self, block: usize, table: &Table, units: &Units) {
        let outputs = table.picked().len();
        let words = table.words();
        let (chosen, matched) = self
            .chosen
            .get_mut(block)
            .map(|chosen| {
                (
                    std::mem::take(&mut chosen.firsts),
                    std::mem::take(&mut chosen.matched),
                )
            })
            .unwrap_or_default();
        let rule_of =
            |unit: usize, c: usize| chosen.get(unit * outputs + c).copied().unwrap_or(u32::MAX);
        let resolved: Units;
        let units = match table.fills() {
            true => units,
            false => {
                let locals: Vec<usize> = (0..units.units.len())
                    .filter(|&local| {
                        (0..outputs).any(|c| rule_of(units.units[local], c) != u32::MAX)
                    })
                    .collect();
                resolved = units.subset(&locals);
                &resolved
            }
        };
        for (c, output) in table
            .picked()
            .iter()
            .enumerate()
            .filter(|(_, o)| !o.collect)
        {
            if let Some((values, remap)) = output.constants() {
                let codes = units
                    .units
                    .iter()
                    .map(|&unit| remap.get(rule_of(unit, c) as usize).copied().unwrap_or(-1))
                    .collect();
                self.put(units, &output.field, Self::coded(&values, codes));
                continue;
            }
            let mut groups: Vec<(u32, Vec<usize>)> = Vec::new();
            for (local, &unit) in units.units.iter().enumerate() {
                let rule = rule_of(unit, c);
                if rule == u32::MAX {
                    continue;
                }
                match groups.iter_mut().find(|(r, _)| *r == rule) {
                    Some((_, locals)) => locals.push(local),
                    None => groups.push((rule, vec![local])),
                }
            }
            let coded = groups
                .iter()
                .all(|(rule, _)| {
                    output
                        .constants
                        .get(*rule as usize)
                        .is_some_and(Option::is_some)
                })
                .then(|| {
                    let mut codes = vec![-1i32; units.units.len()];
                    let values = groups
                        .iter()
                        .enumerate()
                        .map(|(code, (rule, locals))| {
                            locals.iter().for_each(|&local| codes[local] = code as i32);
                            output.constants[*rule as usize]
                                .as_ref()
                                .map_or(Variable::Null, Literal::variable)
                        })
                        .collect::<Vec<_>>();
                    (values, codes)
                });
            if let Some((values, codes)) = coded {
                self.put(units, &output.field, Self::coded(&values, codes));
                continue;
            }
            let mut values: Vec<Variable> = vec![Variable::Null; units.units.len()];
            for (rule, locals) in groups {
                for (local, value) in
                    locals
                        .iter()
                        .zip(self.cell_values(units, output, rule as usize, &locals))
                {
                    values[*local] = value;
                }
            }
            let mut builder = ColumnBuilder::with_capacity(values.len());
            values
                .into_iter()
                .for_each(|value| builder.push_variable(value));
            self.put(units, &output.field, Leaf::typed(builder.finish()));
        }
        let mut fields: Vec<&Arc<str>> = Vec::new();
        for output in table.picked().iter().filter(|o| o.collect) {
            if !fields.contains(&&output.field) {
                fields.push(&output.field);
            }
        }
        for field in fields {
            let columns: Vec<&Picked> = table
                .picked()
                .iter()
                .filter(|o| o.collect && o.field == *field)
                .collect();
            let rows = |unit: usize| {
                matched
                    .get(unit * words..(unit + 1) * words)
                    .unwrap_or_default()
            };
            let mut any = vec![0u64; words];
            units
                .units
                .iter()
                .for_each(|&unit| Bits::or(&mut any, rows(unit)));
            let mut lists: Vec<Vec<Variable>> = vec![Vec::new(); units.units.len()];
            for rule in Bits::rows(&any) {
                for output in &columns {
                    if !output.fills(rule) {
                        continue;
                    }
                    let locals: Vec<usize> = (0..units.units.len())
                        .filter(|&local| Bits::get(rows(units.units[local]), rule))
                        .collect();
                    if locals.is_empty() {
                        continue;
                    }
                    for (local, value) in locals
                        .iter()
                        .zip(self.cell_values(units, output, rule, &locals))
                    {
                        lists[*local].push(value);
                    }
                }
            }
            let mut builder = ColumnBuilder::with_capacity(lists.len());
            lists
                .into_iter()
                .for_each(|items| builder.push_variable(Variable::from_array(items)));
            self.put(units, field, Leaf::typed(builder.finish()));
        }
        if let Some(slot) = self.chosen.get_mut(block) {
            slot.firsts = chosen;
            slot.matched = matched;
        }
    }

    pub(super) fn table_demand(&self, block: usize, row: usize) -> Option<Demand> {
        if self.hosted(block, row) {
            return None;
        }
        let table = self.plan.plans[block].table.as_ref()?;
        let Chosen {
            firsts: chosen,
            matched,
        } = self.chosen.get(block)?;
        let plan = self
            .artifact
            .read_plans
            .get(&self.plan.blocks.refs[block])?;
        let matched = (!matched.is_empty()).then_some(matched.as_slice());
        let mut demanded = Vec::new();
        match self.entity_of(block) {
            None => Pick::Table(TableSelection::used(table.used(chosen, matched, row)))
                .collect_reads(plan, &mut demanded),
            Some(at) => {
                let (a, b) = self.sheet.entities[at].frame.range(row).unwrap_or((0, 0));
                for child in a..b {
                    Pick::Table(TableSelection::used(table.used(chosen, matched, child)))
                        .collect_reads(plan, &mut demanded);
                }
                demanded.sort();
                demanded.dedup();
            }
        }
        Some(demanded.into())
    }
}
