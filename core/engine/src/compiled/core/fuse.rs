use std::hash::Hash;
use std::sync::RwLock;
use zen_expression::lane::{Fusion, LaneProgram};

pub(crate) struct Fused<S> {
    pub program: LaneProgram,
    pub sinks: Vec<S>,
}

pub(crate) struct Runs<K> {
    runs: RwLock<ahash::HashMap<K, usize>>,
}

impl<S> Fused<S> {
    pub fn build(entries: &[Fusion], sinks: Vec<S>) -> Option<Self> {
        let outputs = entries.iter().filter(|entry| !matches!(entry, Fusion::Hidden { .. })).count();
        let program = LaneProgram::compile_fused(entries).ok()?;
        let p = program.program();
        (!(p.writes_env || p.chain || p.opaque() || p.rows) && p.outputs.len() == outputs).then_some(Self { program, sinks })
    }
}

impl<K> Default for Runs<K> {
    fn default() -> Self {
        Self {
            runs: RwLock::new(ahash::HashMap::default()),
        }
    }
}

impl<K: Hash + Eq> Runs<K> {
    pub fn run(&self, key: &K) -> Option<usize> {
        self.runs.read().ok().and_then(|runs| runs.get(key).copied())
    }

    pub fn ran(&self, key: K, run: usize) {
        if self.run(&key) == Some(run) {
            return;
        }
        if let Ok(mut runs) = self.runs.write() {
            runs.insert(key, run);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Runs;

    #[test]
    fn runs_remember_the_latest_length() {
        let runs: Runs<(Vec<usize>, bool)> = Runs::default();
        assert_eq!(runs.run(&(vec![1, 2], false)), None);
        runs.ran((vec![1, 2], false), 2);
        runs.ran((vec![1, 2], true), 5);
        assert_eq!(runs.run(&(vec![1, 2], false)), Some(2));
        runs.ran((vec![1, 2], false), 1);
        assert_eq!(runs.run(&(vec![1, 2], false)), Some(1));
        assert_eq!(runs.run(&(vec![1, 2], true)), Some(5));
    }
}
