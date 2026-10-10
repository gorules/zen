use crate::compiled::policy::schedule::Event;
use crate::compiled::leaf::Bits;
use crate::compiled::policy::Host;
use std::sync::Arc;
use zen_expression::Variable;
use super::{Pass, Sheet};

impl<'s, 'a> Pass<'s, 'a> {
    pub(super) fn host(&mut self, row: usize) -> Option<&mut Host<'s>> {
        self.state.ensure();
        if self.state.hosts[row].is_none() {
            let prepared = self.stores[row].get().is_some();
            let store = self.stores[row].get_or_init(|| self.sheet.object(row));
            self.state.hosts[row] = Some(Host::new(self.artifact, store, self.entry, false, false, &self.plan.programs));
            self.synced[row] = match prepared {
                true => self.base,
                false => self.sheet.entries.len(),
            };
        }
        let from = self.synced[row];
        self.synced[row] = self.sheet.entries.len();
        let host = self.state.hosts[row].as_mut()?;
        for entry in &self.sheet.entries[from..] {
            if Sheet::present(entry, row) {
                host.state.write(&entry.path, entry.leaf.get(row));
            }
        }
        Some(host)
    }

    fn readback(&self, block: usize) -> Vec<Arc<str>> {
        let rule = &self.plan.blocks.rules[block];
        let top = |path: &str| -> Arc<str> { Arc::from(path.split(['.', '[']).next().unwrap_or(path)) };
        let mut paths: Vec<Arc<str>> = match &self.plan.iterations[block] {
            Some((_, path, _)) => vec![top(path)],
            None => rule
                .kind
                .write_sites()
                .into_iter()
                .map(|site| match site.path.contains('[') {
                    true => top(&site.path),
                    false => site.path,
                })
                .collect(),
        };
        paths.sort();
        paths.dedup();
        let nested: Vec<Arc<str>> = paths
            .iter()
            .filter(|p| paths.iter().any(|q| Sheet::related(p, q) == Some(std::cmp::Ordering::Greater)))
            .cloned()
            .collect();
        paths.retain(|p| !nested.contains(p));
        paths
    }

    pub(super) fn delegate(&mut self, event: Event, positions: &[usize]) {
        let (block, commit) = match event {
            Event::Select(block) => (block, false),
            Event::Commit(block) => (block, true),
        };
        self.state.ensure();
        self.delegated = true;
        for &row in positions {
            self.host(row);
        }
        match commit {
            false => self.plan.select(block, positions, &mut self.state),
            true => self.plan.commit(block, positions, &mut self.state),
        }
        let mut done: Vec<usize> = Vec::with_capacity(positions.len());
        for &row in positions {
            match self.state.failures[row].is_some() {
                true => self.fall(row),
                false => done.push(row),
            }
        }
        if !commit || done.is_empty() {
            return;
        }
        for path in self.readback(block) {
            let mut values = vec![Variable::Null; self.sheet.rows];
            let mut present = vec![0u64; self.sheet.rows.div_ceil(64)];
            for &row in &done {
                if let Some(value) = self.stores[row].get().and_then(|store| store.dot(&path)) {
                    values[row] = value;
                    Bits::set(&mut present, row, true);
                }
            }
            self.sheet.hosted(path, values, present);
        }
        for &row in &done {
            self.synced[row] = self.sheet.entries.len();
        }
    }

    pub(super) fn quiet(&self, block: usize) -> bool {
        use crate::policy::blocks::ConditionalReads;
        match self.artifact.read_plans.get(&self.plan.blocks.refs[block]).map(|plan| &plan.conditional) {
            None | Some(ConditionalReads::None) => true,
            Some(ConditionalReads::Match(arms)) => arms.iter().all(|arm| arm.value_reads.is_empty()),
            Some(ConditionalReads::DecisionTable(cells)) => cells.iter().all(|cell| cell.cell_reads.is_empty()),
        }
    }

    pub(super) fn hosted(&self, block: usize, row: usize) -> bool {
        self.hosting.iter().any(|(b, rows)| *b == block && rows[row])
    }
}
