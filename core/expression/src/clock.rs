use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;
use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

pub use chrono_tz::Tz as Timezone;

pub type Instant = DateTime<Utc>;

const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
const DRAWN_BITS: u64 = (1 << 53) - 1;

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

pub fn row_seed(base: u64, row: usize) -> u64 {
    mix(base ^ mix((row as u64).wrapping_add(1).wrapping_mul(GAMMA))) & DRAWN_BITS
}

pub fn system_now() -> Instant {
    static MOCK: OnceLock<Option<Instant>> = OnceLock::new();
    MOCK.get_or_init(|| {
        std::env::var("__ZEN_MOCK_UTC_TIME")
            .ok()
            .and_then(|v| v.parse::<Instant>().ok())
    })
    .unwrap_or_else(Utc::now)
}

pub fn system_timezone() -> Tz {
    static CACHED: OnceLock<Tz> = OnceLock::new();
    *CACHED.get_or_init(|| {
        iana_time_zone::get_timezone()
            .ok()
            .and_then(|tz| Tz::from_str(&tz).ok())
            .unwrap_or(Tz::UTC)
    })
}

pub fn parse_timezone(name: &str) -> Option<Tz> {
    Tz::from_str(name).ok()
}

pub fn parse_instant(text: &str) -> Option<Instant> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

pub fn instant_from_millis(ms: i64) -> Option<Instant> {
    DateTime::from_timestamp_millis(ms)
}

pub fn format_instant(at: &Instant) -> String {
    use chrono::Datelike;
    let year = at.year();
    if !(0..=9999).contains(&year) {
        let nanos = at.nanosecond() % 1_000_000_000;
        let format = match (nanos % 1_000_000, nanos % 1_000) {
            (0, _) => "%Y-%m-%dT%H:%M:%S%.3fZ",
            (_, 0) => "%Y-%m-%dT%H:%M:%S%.6fZ",
            _ => "%Y-%m-%dT%H:%M:%S%.9fZ",
        };
        return at.format(format).to_string();
    }
    let mut out = String::with_capacity(30);
    fn put(out: &mut String, n: u32, width: usize) {
        let mut buf = [b'0'; 9];
        let mut n = n;
        for d in buf[..width].iter_mut().rev() {
            *d = b'0' + (n % 10) as u8;
            n /= 10;
        }
        out.extend(buf[..width].iter().map(|&b| b as char));
    }
    put(&mut out, year as u32, 4);
    out.push('-');
    put(&mut out, at.month(), 2);
    out.push('-');
    put(&mut out, at.day(), 2);
    out.push('T');
    put(&mut out, at.hour(), 2);
    out.push(':');
    put(&mut out, at.minute(), 2);
    out.push(':');
    let nanos = at.nanosecond();
    put(&mut out, at.second() + nanos / 1_000_000_000, 2);
    out.push('.');
    let nanos = nanos % 1_000_000_000;
    match (nanos % 1_000_000, nanos % 1_000) {
        (0, _) => put(&mut out, nanos / 1_000_000, 3),
        (_, 0) => put(&mut out, nanos / 1_000, 6),
        _ => put(&mut out, nanos, 9),
    }
    out.push('Z');
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stream {
    Root,
    Row(u32),
    Item(u32),
}

#[derive(Debug, Clone, Copy, Default)]
struct Node {
    seed: Option<u64>,
    draws: u64,
    touched: bool,
}

impl Node {
    fn drew(&self) -> bool {
        self.draws > 0 || self.touched
    }
}

#[derive(Debug)]
struct Item {
    parent: Stream,
    index: usize,
    node: Node,
}

#[derive(Debug, Default)]
struct Streams {
    root: Node,
    rows: Vec<Node>,
    items: Vec<Item>,
    children: BTreeMap<(Stream, usize), u32>,
}

impl Streams {
    fn row(&mut self, row: u32) -> &mut Node {
        let at = row as usize;
        if self.rows.len() <= at {
            self.rows.resize(at + 1, Node::default());
        }
        &mut self.rows[at]
    }

    fn node(&mut self, stream: Stream) -> Option<&mut Node> {
        match stream {
            Stream::Root => Some(&mut self.root),
            Stream::Row(row) => Some(self.row(row)),
            Stream::Item(item) => self.items.get_mut(item as usize).map(|item| &mut item.node),
        }
    }

    fn peek(&self, stream: Stream) -> Option<&Node> {
        match stream {
            Stream::Root => Some(&self.root),
            Stream::Row(row) => self.rows.get(row as usize),
            Stream::Item(item) => self.items.get(item as usize).map(|item| &item.node),
        }
    }

    fn parent(&self, stream: Stream) -> Option<Stream> {
        match stream {
            Stream::Root => None,
            Stream::Row(_) => Some(Stream::Root),
            Stream::Item(item) => self.items.get(item as usize).map(|item| item.parent),
        }
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn base(&mut self, stream: Stream) -> u64 {
        if let Some(seed) = self.node(stream).and_then(|node| node.seed) {
            return seed;
        }
        let seed = match stream {
            Stream::Root => fastrand::u64(..) & DRAWN_BITS,
            Stream::Row(row) => row_seed(self.base(Stream::Root), row as usize),
            Stream::Item(item) => match self.items.get(item as usize) {
                Some(&Item { parent, index, .. }) => row_seed(self.base(parent), index),
                None => fastrand::u64(..) & DRAWN_BITS,
            },
        };
        if let Some(node) = self.node(stream) {
            node.seed = Some(seed);
        }
        seed
    }

    fn touch(&mut self, stream: Stream) {
        let mut up = self.parent(stream);
        while let Some(parent) = up {
            match self.node(parent) {
                Some(node) if !node.touched => node.touched = true,
                _ => break,
            }
            up = self.parent(parent);
        }
    }

    fn next_u64(&mut self, stream: Stream) -> u64 {
        let base = self.base(stream);
        let k = match self.node(stream) {
            Some(node) => {
                node.draws += 1;
                node.draws - 1
            }
            None => 0,
        };
        self.touch(stream);
        mix(base.wrapping_add(k.wrapping_add(1).wrapping_mul(GAMMA)))
    }

    fn child(&mut self, parent: Stream, index: usize) -> Stream {
        if let Some(&item) = self.children.get(&(parent, index)) {
            return Stream::Item(item);
        }
        let item = self.items.len() as u32;
        self.items.push(Item {
            parent,
            index,
            node: Node::default(),
        });
        self.children.insert((parent, index), item);
        Stream::Item(item)
    }

    fn restart(&mut self, row: u32) {
        *self.row(row) = Node::default();
        let parent = Stream::Row(row);
        let stale: Vec<(Stream, usize)> = self
            .children
            .range((parent, 0)..=(parent, usize::MAX))
            .map(|(key, _)| *key)
            .collect();
        for key in stale {
            self.children.remove(&key);
        }
    }

    fn reset(&mut self) {
        self.rows.clear();
        self.items.clear();
        self.children.clear();
    }
}

#[derive(Debug)]
pub struct Clock {
    now: OnceCell<Instant>,
    stamp: OnceCell<Arc<str>>,
    zone: OnceCell<Arc<str>>,
    timezone: Tz,
    batch: bool,
    streams: RefCell<Streams>,
}

impl Default for Clock {
    fn default() -> Self {
        Self::new(None, None, None)
    }
}

impl Clock {
    pub fn new(now: Option<Instant>, timezone: Option<Tz>, seed: Option<u64>) -> Self {
        let streams = Streams {
            root: Node {
                seed,
                ..Node::default()
            },
            ..Streams::default()
        };
        Self {
            now: now.map_or_else(OnceCell::new, OnceCell::from),
            stamp: OnceCell::new(),
            zone: OnceCell::new(),
            timezone: timezone.unwrap_or_else(system_timezone),
            batch: false,
            streams: RefCell::new(streams),
        }
    }

    pub fn batch(now: Option<Instant>, timezone: Option<Tz>, seed: Option<u64>) -> Self {
        let clock = Self {
            batch: true,
            ..Self::new(now, timezone, seed)
        };
        clock.now();
        clock
    }

    pub fn is_batch(&self) -> bool {
        self.batch
    }

    pub fn now(&self) -> Instant {
        *self.now.get_or_init(system_now)
    }

    pub fn evaluated_at(&self) -> Arc<str> {
        self.stamp
            .get_or_init(|| Arc::from(format_instant(&self.now())))
            .clone()
    }

    pub fn timezone(&self) -> Tz {
        self.timezone
    }

    pub fn zone(&self) -> Arc<str> {
        self.zone
            .get_or_init(|| Arc::from(self.timezone.name()))
            .clone()
    }

    pub fn seed(&self) -> Option<u64> {
        let streams = self.streams.borrow();
        let root = &streams.root;
        let drew = match self.batch {
            true => root.draws > 0,
            false => root.drew(),
        };
        drew.then_some(root.seed).flatten()
    }

    pub fn stream_seed(&self, stream: Stream) -> Option<u64> {
        if stream == Stream::Root {
            return self.seed();
        }
        let streams = self.streams.borrow();
        streams
            .peek(stream)
            .filter(|node| node.drew())
            .and_then(|node| node.seed)
    }

    pub fn row_stream(&self, row: usize) -> Stream {
        match self.batch {
            true => Stream::Row(row as u32),
            false => Stream::Root,
        }
    }

    pub fn child(&self, parent: Stream, index: usize) -> Stream {
        self.streams.borrow_mut().child(parent, index)
    }

    pub fn restart(&self, rows: &[usize]) {
        if !self.batch {
            return;
        }
        let mut streams = self.streams.borrow_mut();
        for &row in rows {
            streams.restart(row as u32);
        }
    }

    pub fn reset(&self) {
        self.streams.borrow_mut().reset();
    }

    pub fn next_u64(&self, stream: Stream) -> u64 {
        if self.batch && stream == Stream::Root {
            UNROWED.fetch_add(1, Ordering::Relaxed);
        }
        self.streams.borrow_mut().next_u64(stream)
    }

    pub fn next_f64(&self, stream: Stream) -> f64 {
        (self.next_u64(stream) >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn next_int(&self, stream: Stream, upper: i64) -> i64 {
        fastrand::Rng::with_seed(self.next_u64(stream)).i64(0..=upper)
    }
}

static UNROWED: AtomicU64 = AtomicU64::new(0);

pub fn unrowed_draws() -> u64 {
    UNROWED.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Default)]
enum Rows {
    #[default]
    Direct,
    One(Stream),
    Picked(Rc<Picked>),
    Items(Rc<Items>),
}

#[derive(Debug)]
struct Picked {
    rows: Rc<[usize]>,
    rest: Rows,
}

#[derive(Debug)]
struct Items {
    items: Rc<[(usize, usize)]>,
    rest: Rows,
}

impl Rows {
    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn stream(&self, clock: &Clock, row: usize) -> Stream {
        match self {
            Rows::Direct => clock.row_stream(row),
            Rows::One(stream) => *stream,
            Rows::Picked(picked) => {
                let row = picked.rows.get(row).copied().unwrap_or(row);
                picked.rest.stream(clock, row)
            }
            Rows::Items(items) => match items.items.get(row) {
                Some(&(at, index)) => clock.child(items.rest.stream(clock, at), index),
                None => items.rest.stream(clock, row),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvalContext {
    clock: Rc<Clock>,
    rows: Rows,
}

impl Default for EvalContext {
    fn default() -> Self {
        Self::new(Clock::default())
    }
}

impl EvalContext {
    pub fn new(clock: Clock) -> Self {
        Self {
            clock: Rc::new(clock),
            rows: Rows::Direct,
        }
    }

    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    pub fn now(&self) -> Instant {
        self.clock.now()
    }

    pub fn timezone(&self) -> Tz {
        self.clock.timezone
    }

    pub fn stream(&self, row: usize) -> Stream {
        self.rows.stream(&self.clock, row)
    }

    pub fn is_direct(&self) -> bool {
        matches!(self.rows, Rows::Direct)
    }

    fn uniform(&self) -> bool {
        match self.rows {
            Rows::Direct => !self.clock.batch,
            Rows::One(_) => true,
            Rows::Picked(_) | Rows::Items(_) => false,
        }
    }

    pub fn rows(&self, rows: &[usize]) -> Self {
        if self.uniform() {
            return self.clone();
        }
        self.picked(Rc::from(rows))
    }

    pub fn picked(&self, rows: Rc<[usize]>) -> Self {
        if self.uniform() {
            return self.clone();
        }
        Self {
            clock: self.clock.clone(),
            rows: Rows::Picked(Rc::new(Picked {
                rows,
                rest: self.rows.clone(),
            })),
        }
    }

    pub fn items(&self, items: &[(usize, usize)]) -> Self {
        Self {
            clock: self.clock.clone(),
            rows: Rows::Items(Rc::new(Items {
                items: Rc::from(items),
                rest: self.rows.clone(),
            })),
        }
    }

    pub fn item(&self, index: usize) -> Self {
        self.items(&[(0, index)])
    }

    pub fn at(&self, row: usize) -> Self {
        if self.uniform() {
            return self.clone();
        }
        Self {
            clock: self.clock.clone(),
            rows: Rows::One(self.stream(row)),
        }
    }

    pub fn restart(&self, rows: &[usize]) {
        if self.is_direct() {
            self.clock.restart(rows);
        }
    }

    pub fn seed(&self) -> Option<u64> {
        match self.rows {
            Rows::One(stream) => self.clock.stream_seed(stream),
            _ => self.clock.seed(),
        }
    }

    pub fn row_seed(&self, row: usize) -> Option<u64> {
        match (&self.rows, self.clock.batch) {
            (Rows::Direct, true) => self.clock.stream_seed(Stream::Row(row as u32)),
            _ => self.seed(),
        }
    }

    pub fn next_u64(&self, row: usize) -> u64 {
        self.clock.next_u64(self.stream(row))
    }

    pub fn random_f64(&self, row: usize) -> f64 {
        self.clock.next_f64(self.stream(row))
    }

    pub fn random_int(&self, row: usize, upper: i64) -> i64 {
        self.clock.next_int(self.stream(row), upper)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seeded_clock_repeats_its_stream() {
        let a = EvalContext::new(Clock::new(None, Some(Tz::UTC), Some(42)));
        let b = EvalContext::new(Clock::new(None, Some(Tz::UTC), Some(42)));
        let xs: Vec<i64> = (0..8).map(|_| a.random_int(0, 1000)).collect();
        let ys: Vec<i64> = (0..8).map(|_| b.random_int(0, 1000)).collect();
        assert_eq!(xs, ys);
        assert_eq!(a.seed(), Some(42));
    }

    #[test]
    fn a_clock_reads_now_once() {
        let clock = Clock::new(None, None, None);
        let first = clock.now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(clock.now(), first);
        assert_eq!(clock.seed(), None);
    }

    #[test]
    fn rows_replay_alone() {
        let batch = EvalContext::new(Clock::batch(None, None, Some(7)));
        let drawn: Vec<u64> = (0..4).map(|_| batch.next_u64(3)).collect();
        let alone = EvalContext::new(Clock::new(Some(batch.now()), None, batch.row_seed(3)));
        let again: Vec<u64> = (0..4).map(|_| alone.next_u64(0)).collect();
        assert_eq!(drawn, again);
        assert_eq!(batch.row_seed(3), Some(row_seed(7, 3)));
        assert_eq!(batch.seed(), None);
    }

    #[test]
    fn items_derive_from_their_row() {
        let batch = EvalContext::new(Clock::batch(None, None, Some(5)));
        let items = batch.rows(&[2, 4]).items(&[(1, 0), (1, 1), (0, 3)]);
        let drawn: Vec<u64> = (0..3).map(|i| items.next_u64(i)).collect();
        let alone = EvalContext::new(Clock::new(None, None, Some(row_seed(5, 4))));
        let looped = alone.items(&[(0, 0), (0, 1)]);
        assert_eq!(drawn[..2], [looped.next_u64(0), looped.next_u64(1)]);
        assert_eq!(batch.row_seed(4), Some(row_seed(5, 4)));
        assert_eq!(alone.seed(), Some(row_seed(5, 4)));
        assert_eq!(batch.row_seed(1), None);
    }

    #[test]
    fn two_contexts_do_not_share_streams() {
        let a = EvalContext::new(Clock::new(None, None, Some(1)));
        let b = EvalContext::new(Clock::new(None, None, Some(1)));
        let first = a.next_u64(0);
        assert_eq!(b.next_u64(0), first);
        assert_ne!(a.next_u64(0), first);
    }

    #[test]
    fn instants_keep_their_precision() {
        let at = parse_instant("2026-11-04T09:30:00.125Z").unwrap();
        assert_eq!(format_instant(&at), "2026-11-04T09:30:00.125Z");
        let at = parse_instant("2026-11-04T09:30:00.12525Z").unwrap();
        assert_eq!(format_instant(&at), "2026-11-04T09:30:00.125250Z");
        let at = parse_instant("2026-11-04T09:30:00.000000001Z").unwrap();
        assert_eq!(format_instant(&at), "2026-11-04T09:30:00.000000001Z");
        let at = parse_instant("2026-11-04T10:30:00+01:00").unwrap();
        assert_eq!(format_instant(&at), "2026-11-04T09:30:00.000Z");
        for text in ["0001-01-01T00:00:00.000Z", "1999-12-31T23:59:59.999999999Z"] {
            let at = parse_instant(text).unwrap();
            assert_eq!(format_instant(&at), text);
            assert_eq!(
                at.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string()[..19],
                text[..19]
            );
        }
    }
}
