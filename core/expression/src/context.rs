use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;
use std::fmt::{Debug, Display, Formatter};
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

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

static CLOCK_READS: AtomicU64 = AtomicU64::new(0);
static UNROWED: AtomicU64 = AtomicU64::new(0);

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

pub fn clock_reads() -> u64 {
    CLOCK_READS.load(Ordering::Relaxed)
}

pub fn unrowed_draws() -> u64 {
    UNROWED.load(Ordering::Relaxed)
}

const LOCALE_CAPACITY: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Locale {
    len: u8,
    bytes: [u8; LOCALE_CAPACITY],
}

impl Locale {
    pub const DEFAULT: Locale = Locale::constant("en-US");

    const fn constant(tag: &str) -> Self {
        let mut bytes = [0u8; LOCALE_CAPACITY];
        let source = tag.as_bytes();
        let mut i = 0;
        while i < source.len() {
            bytes[i] = source[i];
            i += 1;
        }
        Self {
            len: source.len() as u8,
            bytes,
        }
    }

    pub fn parse(tag: &str) -> Option<Self> {
        let canonical = icu_locale_core::Locale::try_from_str(tag).ok()?.to_string();
        let source = canonical.as_bytes();
        if source.is_empty() || source.len() > LOCALE_CAPACITY || !source.is_ascii() {
            return None;
        }
        let mut bytes = [0u8; LOCALE_CAPACITY];
        bytes[..source.len()].copy_from_slice(source);
        Some(Self {
            len: source.len() as u8,
            bytes,
        })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("en-US")
    }
}

impl Default for Locale {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Debug for Locale {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(self.as_str(), f)
    }
}

impl Display for Locale {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Locale {
    type Err = String;

    fn from_str(tag: &str) -> Result<Self, Self::Err> {
        Self::parse(tag).ok_or_else(|| format!("invalid locale {tag}, expected a BCP 47 tag"))
    }
}

pub fn parse_locale(tag: &str) -> Option<Locale> {
    Locale::parse(tag)
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pinned {
    pub now: Option<Instant>,
    pub timezone: Option<Tz>,
    pub seed: Option<u64>,
    pub locale: Option<Locale>,
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
        if let Some(node) = self.rows.get_mut(row as usize) {
            *node = Node::default();
        }
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
}

#[derive(Debug)]
struct Shared {
    now: OnceCell<Instant>,
    timezone: Tz,
    locale: Locale,
    batch: bool,
    streams: RefCell<Streams>,
}

impl Shared {
    fn new(pinned: Pinned, batch: bool) -> Self {
        let streams = Streams {
            root: Node {
                seed: pinned.seed,
                ..Node::default()
            },
            ..Streams::default()
        };
        Self {
            now: pinned.now.map_or_else(OnceCell::new, OnceCell::from),
            timezone: pinned.timezone.unwrap_or_else(system_timezone),
            locale: pinned.locale.unwrap_or_default(),
            batch,
            streams: RefCell::new(streams),
        }
    }

    fn now(&self) -> Instant {
        *self.now.get_or_init(|| {
            CLOCK_READS.fetch_add(1, Ordering::Relaxed);
            system_now()
        })
    }

    fn row_stream(&self, row: usize) -> Stream {
        match self.batch {
            true => Stream::Row(row as u32),
            false => Stream::Root,
        }
    }

    fn child(&self, parent: Stream, index: usize) -> Stream {
        self.streams.borrow_mut().child(parent, index)
    }

    fn existing(&self, pick: Pick) -> Option<Stream> {
        match pick {
            Pick::Stream(stream) => Some(stream),
            Pick::Child(parent, index) => self
                .streams
                .borrow()
                .children
                .get(&(parent, index))
                .map(|&item| Stream::Item(item)),
        }
    }

    fn seed(&self) -> Option<u64> {
        let streams = self.streams.borrow();
        let root = &streams.root;
        let drew = match self.batch {
            true => root.draws > 0,
            false => root.drew(),
        };
        drew.then_some(root.seed).flatten()
    }

    fn stream_seed(&self, stream: Stream) -> Option<u64> {
        if stream == Stream::Root {
            return self.seed();
        }
        let streams = self.streams.borrow();
        streams
            .peek(stream)
            .filter(|node| node.drew())
            .and_then(|node| node.seed)
    }

    fn next_u64(&self, stream: Stream) -> u64 {
        if self.batch && stream == Stream::Root {
            UNROWED.fetch_add(1, Ordering::Relaxed);
        }
        self.streams.borrow_mut().next_u64(stream)
    }
}

#[derive(Debug, Clone, Copy)]
enum Pick {
    Stream(Stream),
    Child(Stream, usize),
}

impl Pick {
    fn resolve(self, shared: &Shared) -> Stream {
        match self {
            Pick::Stream(stream) => stream,
            Pick::Child(parent, index) => shared.child(parent, index),
        }
    }
}

#[derive(Debug, Clone, Default)]
enum Rows {
    #[default]
    Direct,
    One(Pick),
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
    fn pick(&self, shared: &Shared, row: usize) -> Pick {
        match self {
            Rows::Direct => Pick::Stream(shared.row_stream(row)),
            Rows::One(pick) => *pick,
            Rows::Picked(picked) => {
                let row = picked.rows.get(row).copied().unwrap_or(row);
                picked.rest.pick(shared, row)
            }
            Rows::Items(items) => match items.items.get(row) {
                Some(&(at, index)) => {
                    Pick::Child(items.rest.pick(shared, at).resolve(shared), index)
                }
                None => items.rest.pick(shared, row),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvaluationContext {
    shared: Rc<Shared>,
    rows: Rows,
}

impl Default for EvaluationContext {
    fn default() -> Self {
        Self::new(Pinned::default())
    }
}

impl EvaluationContext {
    pub fn new(pinned: Pinned) -> Self {
        Self {
            shared: Rc::new(Shared::new(pinned, false)),
            rows: Rows::Direct,
        }
    }

    pub fn batch(pinned: Pinned) -> Self {
        Self {
            shared: Rc::new(Shared::new(pinned, true)),
            rows: Rows::Direct,
        }
    }

    pub fn is_batch(&self) -> bool {
        self.shared.batch
    }

    pub fn now(&self) -> Instant {
        self.shared.now()
    }

    pub fn timezone(&self) -> Tz {
        self.shared.timezone
    }

    pub fn locale(&self) -> Locale {
        self.shared.locale
    }

    pub fn row(&self, row: usize) -> RowContext<'_> {
        RowContext { context: self, row }
    }

    pub fn stream(&self, row: usize) -> Stream {
        self.rows.pick(&self.shared, row).resolve(&self.shared)
    }

    pub fn is_direct(&self) -> bool {
        matches!(self.rows, Rows::Direct)
    }

    fn uniform(&self) -> bool {
        match self.rows {
            Rows::Direct => !self.shared.batch,
            Rows::One(_) => true,
            Rows::Picked(_) | Rows::Items(_) => false,
        }
    }

    fn narrowed(&self, rows: Rows) -> Self {
        Self {
            shared: self.shared.clone(),
            rows,
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
        self.narrowed(Rows::Picked(Rc::new(Picked {
            rows,
            rest: self.rows.clone(),
        })))
    }

    pub fn items(&self, items: &[(usize, usize)]) -> Self {
        self.narrowed(Rows::Items(Rc::new(Items {
            items: Rc::from(items),
            rest: self.rows.clone(),
        })))
    }

    pub fn item(&self, index: usize) -> Self {
        let parent = self.rows.pick(&self.shared, 0).resolve(&self.shared);
        self.narrowed(Rows::One(Pick::Child(parent, index)))
    }

    pub fn at(&self, row: usize) -> Self {
        if self.uniform() {
            return self.clone();
        }
        self.narrowed(Rows::One(self.rows.pick(&self.shared, row)))
    }

    pub fn restart(&self, rows: &[usize]) {
        if !self.is_direct() || !self.shared.batch {
            return;
        }
        let mut streams = self.shared.streams.borrow_mut();
        for &row in rows {
            streams.restart(row as u32);
        }
    }

    pub fn seed(&self) -> Option<u64> {
        match self.rows {
            Rows::One(pick) => self
                .shared
                .existing(pick)
                .and_then(|stream| self.shared.stream_seed(stream)),
            _ => self.shared.seed(),
        }
    }

    pub fn row_seed(&self, row: usize) -> Option<u64> {
        match (&self.rows, self.shared.batch) {
            (Rows::Direct, true) => self.shared.stream_seed(Stream::Row(row as u32)),
            _ => self.seed(),
        }
    }

    pub fn next_u64(&self, row: usize) -> u64 {
        self.shared.next_u64(self.stream(row))
    }

    pub fn random_f64(&self, row: usize) -> f64 {
        (self.next_u64(row) >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn random_int(&self, row: usize, upper: i64) -> i64 {
        fastrand::Rng::with_seed(self.next_u64(row)).i64(0..=upper)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RowContext<'a> {
    context: &'a EvaluationContext,
    row: usize,
}

impl<'a> RowContext<'a> {
    pub fn new(context: &'a EvaluationContext, row: usize) -> Self {
        Self { context, row }
    }

    pub fn context(&self) -> &'a EvaluationContext {
        self.context
    }

    pub fn index(&self) -> usize {
        self.row
    }

    pub fn now(&self) -> Instant {
        self.context.now()
    }

    pub fn timezone(&self) -> Tz {
        self.context.timezone()
    }

    pub fn locale(&self) -> Locale {
        self.context.locale()
    }

    pub fn next_u64(&self) -> u64 {
        self.context.next_u64(self.row)
    }

    pub fn random_f64(&self) -> f64 {
        self.context.random_f64(self.row)
    }

    pub fn random_int(&self, upper: i64) -> i64 {
        self.context.random_int(self.row, upper)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(seed: u64) -> Pinned {
        Pinned {
            seed: Some(seed),
            ..Pinned::default()
        }
    }

    #[test]
    fn a_seeded_context_repeats_its_stream() {
        let pinned = Pinned {
            timezone: Some(Tz::UTC),
            ..seeded(42)
        };
        let a = EvaluationContext::new(pinned);
        let b = EvaluationContext::new(pinned);
        let xs: Vec<i64> = (0..8).map(|_| a.random_int(0, 1000)).collect();
        let ys: Vec<i64> = (0..8).map(|_| b.random_int(0, 1000)).collect();
        assert_eq!(xs, ys);
        assert_eq!(a.seed(), Some(42));
    }

    #[test]
    fn a_context_reads_now_once() {
        let before = clock_reads();
        let context = EvaluationContext::default();
        assert_eq!(clock_reads(), before);
        let first = context.now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(context.now(), first);
        assert_eq!(context.at(3).item(2).now(), first);
        assert_eq!(clock_reads(), before + 1);
        assert_eq!(context.seed(), None);
    }

    #[test]
    fn a_batch_reads_now_once_for_all_rows() {
        let before = clock_reads();
        let batch = EvaluationContext::batch(Pinned::default());
        let rows: Vec<Instant> = (0..16).map(|row| batch.at(row).now()).collect();
        assert!(rows.iter().all(|at| *at == rows[0]));
        assert_eq!(clock_reads(), before + 1);
        let pinned = EvaluationContext::batch(Pinned {
            now: parse_instant("2026-11-04T09:30:00Z"),
            ..Pinned::default()
        });
        for row in 0..16 {
            assert_eq!(pinned.at(row).now(), pinned.now());
        }
        assert_eq!(clock_reads(), before + 1);
    }

    #[test]
    fn narrowing_creates_no_streams() {
        let batch = EvaluationContext::batch(seeded(3));
        for row in 0..64 {
            let at = batch.at(row);
            let _ = at.item(row).at(0).now();
        }
        let streams = batch.shared.streams.borrow();
        assert!(streams.rows.is_empty());
        assert!(streams.items.is_empty());
        assert!(streams.children.is_empty());
    }

    #[test]
    fn rows_replay_alone() {
        let batch = EvaluationContext::batch(seeded(7));
        let drawn: Vec<u64> = (0..4).map(|_| batch.next_u64(3)).collect();
        let alone = EvaluationContext::new(Pinned {
            now: Some(batch.now()),
            seed: batch.row_seed(3),
            ..Pinned::default()
        });
        let again: Vec<u64> = (0..4).map(|_| alone.next_u64(0)).collect();
        assert_eq!(drawn, again);
        assert_eq!(batch.row_seed(3), Some(row_seed(7, 3)));
        assert_eq!(batch.seed(), None);
    }

    #[test]
    fn items_derive_from_their_row() {
        let batch = EvaluationContext::batch(seeded(5));
        let items = batch.rows(&[2, 4]).items(&[(1, 0), (1, 1), (0, 3)]);
        let drawn: Vec<u64> = (0..3).map(|i| items.next_u64(i)).collect();
        let alone = EvaluationContext::new(seeded(row_seed(5, 4)));
        let looped = alone.items(&[(0, 0), (0, 1)]);
        assert_eq!(drawn[..2], [looped.next_u64(0), looped.next_u64(1)]);
        assert_eq!(batch.row_seed(4), Some(row_seed(5, 4)));
        assert_eq!(alone.seed(), Some(row_seed(5, 4)));
        assert_eq!(batch.row_seed(1), None);
    }

    #[test]
    fn an_item_matches_its_loop_position() {
        let alone = EvaluationContext::new(seeded(11));
        let looped = alone.items(&[(0, 0), (0, 1), (0, 2)]);
        let expected: Vec<u64> = (0..3).map(|i| looped.next_u64(i)).collect();
        let again = EvaluationContext::new(seeded(11));
        let single: Vec<u64> = (0..3).map(|i| again.item(i).next_u64(0)).collect();
        assert_eq!(expected, single);
    }

    #[test]
    fn two_contexts_do_not_share_streams() {
        let a = EvaluationContext::new(seeded(1));
        let b = EvaluationContext::new(seeded(1));
        let first = a.next_u64(0);
        assert_eq!(b.next_u64(0), first);
        assert_ne!(a.next_u64(0), first);
    }

    #[test]
    fn row_contexts_read_their_row() {
        let batch = EvaluationContext::batch(seeded(9));
        let row = batch.row(2);
        assert_eq!(row.index(), 2);
        let drawn = row.next_u64();
        let alone = EvaluationContext::new(seeded(row_seed(9, 2)));
        assert_eq!(alone.row(0).next_u64(), drawn);
    }

    #[test]
    fn locales_are_canonical() {
        assert_eq!(EvaluationContext::default().locale().as_str(), "en-US");
        assert_eq!(
            Locale::parse("de-ch").map(|l| l.to_string()),
            Some("de-CH".into())
        );
        assert_eq!(
            Locale::parse("zh-hant-tw").map(|l| l.to_string()),
            Some("zh-Hant-TW".into())
        );
        assert!(Locale::parse("not a locale").is_none());
        assert!(Locale::parse("").is_none());
    }
}
