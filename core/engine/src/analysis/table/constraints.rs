use std::sync::Arc;

use zen_expression::intellisense::IntelliSense;

use super::cell::Condition;
use super::value_set::ValueSet;

#[derive(Debug, Clone, Default)]
pub(crate) struct PathConstraints {
    entries: Vec<(Arc<str>, ValueSet)>,
}

impl PathConstraints {
    pub(crate) fn get(&self, path: &str) -> Option<&ValueSet> {
        self.entries
            .iter()
            .find(|(key, _)| key.as_ref() == path)
            .map(|(_, set)| set)
    }

    fn with(mut self, path: Arc<str>, set: ValueSet) -> Self {
        match self.entries.iter_mut().find(|(key, _)| *key == path) {
            Some((_, existing)) => *existing = existing.intersect(&set),
            None => self.entries.push((path, set)),
        }
        self
    }

    pub(crate) fn when_holds(self, is: &mut IntelliSense, condition: &str) -> Self {
        Condition::holds(is, condition)
            .into_iter()
            .fold(self, |acc, (path, set)| acc.with(path, set))
    }

    pub(crate) fn when_fails(self, is: &mut IntelliSense, condition: &str) -> Self {
        match Condition::fails(is, condition) {
            Some((path, set)) => self.with(path, set),
            None => self,
        }
    }

    pub(crate) fn join(parts: &[PathConstraints]) -> Self {
        let Some((first, rest)) = parts.split_first() else {
            return Self::default();
        };
        let entries = first
            .entries
            .iter()
            .filter_map(|(path, set)| {
                rest.iter()
                    .try_fold(set.clone(), |acc, part| {
                        part.get(path).map(|other| acc.union(other))
                    })
                    .map(|union| (path.clone(), union))
            })
            .collect();
        Self { entries }
    }

    pub(crate) fn without<'s>(mut self, written: impl IntoIterator<Item = &'s str>) -> Self {
        let written: Vec<&str> = written.into_iter().collect();
        let overlaps = |a: &str, b: &str| {
            a == b
                || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('.'))
                || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('.'))
        };
        self.entries
            .retain(|(path, _)| !written.iter().any(|w| overlaps(path, w)));
        self
    }
}
