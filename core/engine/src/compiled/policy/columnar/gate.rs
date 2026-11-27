use super::{Pass, Sheet};
use std::cmp::Ordering;
use std::rc::Rc;
use zen_expression::Variable;

impl<'s, 'a> Pass<'s, 'a> {
    fn held(&self, path: &str) -> Option<Rc<[bool]>> {
        let rows = self.sheet.rows;
        let object = |value: Option<Variable>| matches!(value, Some(Variable::Object(_)));
        if self.shared {
            let held: Vec<bool> = (0..rows)
                .map(|row| object(self.stores[row].get().and_then(|store| store.dot(path))))
                .collect();
            return (!held.iter().all(|&h| h)).then(|| held.into());
        }
        let inputs = || {
            self.sheet.entries[..self.base]
                .iter()
                .filter(|entry| Sheet::live(entry))
                .filter_map(|entry| Some((entry, Sheet::related(&entry.path, path)?)))
        };
        if inputs().any(|(_, order)| order != Ordering::Greater) {
            let held: Vec<bool> = (0..rows)
                .map(|row| object(self.sheet.object(row).dot(path)))
                .collect();
            return (!held.iter().all(|&h| h)).then(|| held.into());
        }
        let mut held = vec![false; rows];
        for (entry, _) in inputs() {
            if Sheet::solid(entry) {
                return None;
            }
            for (row, held) in held.iter_mut().enumerate() {
                *held = *held
                    || Sheet::present(entry, row)
                    || !matches!(entry.leaf.get(row), Variable::Null);
            }
        }
        (!held.iter().all(|&h| h)).then(|| held.into())
    }

    fn presence(&self, gate: usize) -> Option<&Rc<[bool]>> {
        self.present[gate]
            .get_or_init(|| self.held(&self.plan.gates[gate]))
            .as_ref()
    }

    pub(super) fn gated(&self, gate: usize, rows: &[usize]) -> Option<Vec<usize>> {
        let held = self.presence(gate)?;
        Some(rows.iter().copied().filter(|&row| held[row]).collect())
    }

    pub(super) fn absent(&self, block: usize, row: usize) -> bool {
        self.plan
            .gate(block)
            .and_then(|gate| self.presence(gate))
            .is_some_and(|held| !held[row])
    }
}
