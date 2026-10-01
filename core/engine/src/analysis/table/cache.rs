use std::cell::RefCell;
use std::collections::VecDeque;
use std::hash::{BuildHasher, Hash, Hasher};
use std::rc::Rc;

use ahash::RandomState;
use zen_expression::intellisense::IntelliSense;

use super::verify::{Finding, VerifyTable};

const CAPACITY: usize = 256;

thread_local! {
    static FINDINGS: RefCell<VecDeque<(u128, Rc<Vec<Finding>>)>> =
        RefCell::new(VecDeque::with_capacity(CAPACITY));
}

struct Fingerprint {
    low: ahash::AHasher,
    high: ahash::AHasher,
}

impl Hasher for Fingerprint {
    fn write(&mut self, bytes: &[u8]) {
        self.low.write(bytes);
        self.high.write(bytes);
    }

    fn finish(&self) -> u64 {
        self.low.finish()
    }
}

impl Fingerprint {
    fn new() -> Self {
        Self {
            low: RandomState::with_seeds(1, 2, 3, 4).build_hasher(),
            high: RandomState::with_seeds(5, 6, 7, 8).build_hasher(),
        }
    }

    fn key(&self) -> u128 {
        ((self.high.finish() as u128) << 64) | self.low.finish() as u128
    }
}

impl VerifyTable<'_> {
    pub(super) fn cached_findings(&self, is: &mut IntelliSense) -> Rc<Vec<Finding>> {
        let key = self.fingerprint();
        let hit = FINDINGS.with(|cache| {
            cache
                .borrow()
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, findings)| findings.clone())
        });
        if let Some(findings) = hit {
            return findings;
        }
        let findings = Rc::new(self.verify(is));
        FINDINGS.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() == CAPACITY {
                cache.pop_front();
            }
            cache.push_back((key, findings.clone()));
        });
        findings
    }

    fn fingerprint(&self) -> u128 {
        let mut state = Fingerprint::new();
        self.mode.hash(&mut state);
        self.inputs.len().hash(&mut state);
        for col in &self.inputs {
            col.id.hash(&mut state);
            col.unary.hash(&mut state);
            col.analyzable.hash(&mut state);
            col.dated.hash(&mut state);
            col.input.hash(&mut state);
            col.field.hash(&mut state);
            col.path.hash(&mut state);
            col.prefer.hash(&mut state);
            col.label.hash(&mut state);
            col.domain.hash(&mut state);
        }
        self.outputs.len().hash(&mut state);
        for col in &self.outputs {
            col.id.hash(&mut state);
            col.collect.hash(&mut state);
            col.label.hash(&mut state);
            col.values.hash(&mut state);
        }
        self.rules.len().hash(&mut state);
        for rule in self.rules {
            let (low, high) = rule.iter().fold((0u64, 0u64), |(low, high), entry| {
                let mut entry_state = Fingerprint::new();
                entry.hash(&mut entry_state);
                (
                    low.wrapping_add(entry_state.low.finish()),
                    high.wrapping_add(entry_state.high.finish()),
                )
            });
            (rule.len(), low, high).hash(&mut state);
        }
        state.key()
    }
}
