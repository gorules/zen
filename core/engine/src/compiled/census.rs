use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
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
        static REASON: RefCell<String> = const { RefCell::new(String::new()) };
        static NOTE: RefCell<String> = const { RefCell::new(String::new()) };
    }

    pub fn note(what: &str) {
        if Self::on() {
            Self::NOTE.with_borrow_mut(|note| {
                if note.is_empty() {
                    *note = what.to_string();
                }
            });
        }
    }

    pub fn fresh() {
        if Self::on() {
            Self::NOTE.with_borrow_mut(String::clear);
        }
    }

    pub fn reason(why: impl FnOnce() -> String) {
        if Self::on() {
            let note = Self::NOTE.with_borrow_mut(std::mem::take);
            Self::REASON.with_borrow_mut(|reason| *reason = format!("{note}{}", why()));
        }
    }

    fn rejected() -> &'static Mutex<HashMap<(usize, String), String>> {
        static REJECTED: OnceLock<Mutex<HashMap<(usize, String), String>>> = OnceLock::new();
        REJECTED.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub fn reject(key: (usize, String)) {
        if !Self::on() {
            return;
        }
        let reason = Self::REASON.with_borrow_mut(std::mem::take);
        if let Ok(mut rejected) = Self::rejected().lock() {
            rejected.insert(key, Self::normal(&reason));
        }
    }

    fn normal(reason: &str) -> String {
        let mut out = String::with_capacity(reason.len());
        let mut quoted = false;
        for c in reason.chars() {
            match c {
                '"' => {
                    quoted = !quoted;
                    if quoted {
                        out.push_str("\"…");
                    }
                }
                _ if quoted => {}
                c if c.is_ascii_digit() => {
                    if !out.ends_with('N') {
                        out.push('N');
                    }
                }
                c => out.push(c),
            }
        }
        out
    }

    pub fn recall(key: &(usize, String)) {
        if Self::on() {
            let reason = Self::rejected()
                .lock()
                .ok()
                .and_then(|rejected| rejected.get(key).cloned())
                .unwrap_or_else(|| "?".into());
            Self::REASON.with_borrow_mut(|r| *r = reason);
        }
    }

    pub fn taken() -> String {
        Self::REASON.with_borrow_mut(std::mem::take)
    }

    pub fn fallback(key: (usize, String), what: &str, rows: usize, ns: u128) {
        let reason = Self::rejected()
            .lock()
            .ok()
            .and_then(|rejected| rejected.get(&key).cloned())
            .unwrap_or_else(|| "?".into());
        Self::record(format!("lane | {what} | {reason}"), rows, ns);
    }

    pub fn begin() {
        if Self::on() {
            Self::STARTS.with_borrow_mut(|starts| starts.push(Instant::now()));
        }
    }

    pub fn elapsed() -> u128 {
        Self::STARTS
            .with_borrow_mut(|starts| starts.pop().map_or(0, |start| start.elapsed().as_nanos()))
    }

    pub fn id() -> usize {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    pub fn on() -> bool {
        crate::compiled::switches::SWITCHES.census()
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
            Ok(mut table) => std::mem::take(&mut *table)
                .into_iter()
                .map(|(k, t)| (k, t.calls, t.rows, t.ns))
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}
