use crate::compiled::graph::Step;
use crate::model::DecisionNodeKind;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub(crate) struct Census;

#[derive(Default, Clone, Copy)]
pub(crate) struct Tally {
    pub calls: u64,
    pub rows: u64,
    pub ns: u128,
}

impl Census {
    thread_local! {
        static PATH: Cell<&'static str> = const { Cell::new("") };
        static STARTS: RefCell<Vec<Instant>> = const { RefCell::new(Vec::new()) };
    }

    pub fn begin() {
        if Self::on() {
            Self::STARTS.with_borrow_mut(|starts| starts.push(Instant::now()));
        }
    }

    pub fn elapsed() -> u128 {
        Self::STARTS.with_borrow_mut(|starts| starts.pop().map_or(0, |start| start.elapsed().as_nanos()))
    }

    pub fn on() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var("ZEN_CENSUS").is_ok())
    }

    fn table() -> &'static Mutex<BTreeMap<String, Tally>> {
        static TABLE: OnceLock<Mutex<BTreeMap<String, Tally>>> = OnceLock::new();
        TABLE.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    pub fn mark(path: &'static str) {
        if Self::on() {
            Self::PATH.with(|p| p.set(path));
        }
    }

    pub fn path() -> &'static str {
        Self::PATH.with(|p| p.replace(""))
    }

    pub fn kind(step: &Step) -> &'static str {
        match &step.node.kind {
            DecisionNodeKind::InputNode { .. } => "input",
            DecisionNodeKind::OutputNode { .. } => "output",
            DecisionNodeKind::SwitchNode { .. } => "switch",
            DecisionNodeKind::ExpressionNode { .. } => "expression",
            DecisionNodeKind::DecisionTableNode { .. } => "table",
            DecisionNodeKind::FunctionNode { .. } => "function",
            DecisionNodeKind::DecisionNode { .. } => "decision",
            DecisionNodeKind::CustomNode { .. } => "custom",
        }
    }

    pub fn record(key: String, rows: usize, ns: u128) {
        if let Ok(mut table) = Self::table().lock() {
            let tally = table.entry(key).or_default();
            tally.calls += 1;
            tally.rows += rows as u64;
            tally.ns += ns;
        }
    }

    pub fn take() -> Vec<(String, u64, u64, u128)> {
        match Self::table().lock() {
            Ok(mut table) => std::mem::take(&mut *table).into_iter().map(|(k, t)| (k, t.calls, t.rows, t.ns)).collect(),
            Err(_) => Vec::new(),
        }
    }
}
