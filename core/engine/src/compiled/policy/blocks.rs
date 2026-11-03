use crate::compiled::host::decision_table::index::TableIndex;
use crate::policy::blocks::{AssertionIr, DecisionTableIr};
use crate::workspace::types::ConditionTrace;
use ahash::HashMap;
use std::fmt::{Debug, Write};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub(crate) struct TableSelection {
    pub(crate) matched_rows: Vec<u32>,
    pub(crate) used_cells: Vec<(u32, Arc<str>)>,
    pub(crate) input_bits: Option<Vec<u8>>,
}

impl TableSelection {
    pub(crate) fn used(used_cells: Vec<(u32, Arc<str>)>) -> Self {
        Self {
            matched_rows: Vec::new(),
            used_cells,
            input_bits: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MatchSelection {
    pub(crate) matched_arm: Option<Arc<str>>,
    pub(crate) arms: Vec<ConditionTrace>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operator {
    And,
    Or,
}

impl Operator {
    pub(crate) fn of(operator: &impl Debug) -> Self {
        struct Named {
            text: &'static str,
            at: usize,
            same: bool,
        }
        impl Write for Named {
            fn write_str(&mut self, s: &str) -> std::fmt::Result {
                let end = self.at + s.len();
                self.same = self.same && self.text.get(self.at..end) == Some(s);
                self.at = end;
                Ok(())
            }
        }
        let mut named = Named {
            text: "And",
            at: 0,
            same: true,
        };
        let _ = write!(named, "{operator:?}");
        match named.same && named.at == named.text.len() {
            true => Operator::And,
            false => Operator::Or,
        }
    }

    fn apply(self, left: bool, right: bool) -> bool {
        match self {
            Operator::And => left && right,
            Operator::Or => left || right,
        }
    }
}

struct Frame {
    acc: bool,
    started: bool,
    combine: Operator,
}

impl Frame {
    fn close(&self, group: bool) -> bool {
        match self.started {
            true => self.combine.apply(self.acc, group),
            false => group,
        }
    }
}

impl AssertionIr {
    pub(crate) fn fold(&self, results: &[bool]) -> bool {
        if self.conditions.is_empty() {
            return false;
        }

        let mut stack: Vec<Frame> = Vec::new();
        let mut acc = false;
        let mut started = false;
        let mut depth = 0u32;
        let mut combine_next = Operator::And;

        for (condition, &cond_result) in self.conditions.iter().zip(results) {
            while condition.depth > depth {
                stack.push(Frame {
                    acc,
                    started,
                    combine: combine_next,
                });
                acc = false;
                started = false;
                combine_next = Operator::And;
                depth += 1;
            }
            while condition.depth < depth {
                if let Some(frame) = stack.pop() {
                    acc = frame.close(acc);
                    started = true;
                }
                depth -= 1;
            }

            acc = match started {
                true => combine_next.apply(acc, cond_result),
                false => cond_result,
            };
            started = true;
            combine_next = Operator::of(&condition.operator);
        }

        while let Some(frame) = stack.pop() {
            acc = frame.close(acc);
        }

        acc
    }
}

#[derive(Default)]
pub(crate) struct TableIndexes {
    kept: Mutex<HashMap<usize, Arc<Option<TableIndex>>>>,
}

impl TableIndexes {
    pub(crate) fn of(&self, ir: &DecisionTableIr) -> Arc<Option<TableIndex>> {
        let key = ir as *const DecisionTableIr as usize;
        if let Some(kept) = self
            .kept
            .lock()
            .ok()
            .and_then(|kept| kept.get(&key).cloned())
        {
            return kept;
        }
        let index = Arc::new(TableIndex::build(&ir.inputs, &ir.rules));
        if let Ok(mut kept) = self.kept.lock() {
            kept.insert(key, index.clone());
        }
        index
    }
}
