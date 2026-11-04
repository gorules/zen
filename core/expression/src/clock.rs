use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

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

#[derive(Debug, Clone, Copy)]
enum Source {
    Fresh,
    Row(usize),
    Batch,
}

#[derive(Debug)]
pub struct Clock {
    now: OnceLock<Instant>,
    stamp: OnceLock<Arc<str>>,
    timezone: Tz,
    seed: OnceLock<u64>,
    draws: AtomicU64,
    touched: AtomicBool,
    source: Source,
    parent: Option<Arc<Clock>>,
    streams: Mutex<BTreeMap<usize, Arc<Clock>>>,
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

impl Clock {
    pub fn new(now: Option<Instant>, timezone: Option<Tz>, seed: Option<u64>) -> Arc<Self> {
        let timezone = timezone.unwrap_or_else(system_timezone);
        if let Some(mut clock) = POOL.with(|p| p.borrow_mut().pop()) {
            if let Some(c) = Arc::get_mut(&mut clock) {
                c.now = OnceLock::new();
                c.stamp = OnceLock::new();
                c.seed = OnceLock::new();
                *c.draws.get_mut() = 0;
                *c.touched.get_mut() = false;
                c.timezone = timezone;
                c.source = Source::Fresh;
                c.parent = None;
                if let Ok(streams) = c.streams.get_mut() {
                    streams.clear();
                }
                if let Some(now) = now {
                    let _ = c.now.set(now);
                }
                if let Some(seed) = seed {
                    let _ = c.seed.set(seed);
                }
                return clock;
            }
        }
        let clock = Self {
            now: OnceLock::new(),
            stamp: OnceLock::new(),
            timezone,
            seed: OnceLock::new(),
            draws: AtomicU64::new(0),
            touched: AtomicBool::new(false),
            source: Source::Fresh,
            parent: None,
            streams: Mutex::new(BTreeMap::new()),
        };
        if let Some(now) = now {
            let _ = clock.now.set(now);
        }
        if let Some(seed) = seed {
            let _ = clock.seed.set(seed);
        }
        Arc::new(clock)
    }

    fn root(self: &Arc<Self>) -> Arc<Self> {
        match (self.source, &self.parent) {
            (Source::Batch, Some(parent)) => parent.clone(),
            _ => self.clone(),
        }
    }

    fn child(&self, index: usize, parent: Arc<Self>) -> Arc<Self> {
        Arc::new(Self {
            now: OnceLock::from(self.now()),
            stamp: self.stamp.clone(),
            timezone: self.timezone,
            seed: OnceLock::new(),
            draws: AtomicU64::new(0),
            touched: AtomicBool::new(false),
            source: Source::Row(index),
            parent: Some(parent),
            streams: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn row(self: &Arc<Self>, index: usize) -> Arc<Self> {
        self.child(index, self.root())
    }

    pub fn batch(self: &Arc<Self>) -> Arc<Self> {
        if matches!(self.source, Source::Batch) {
            return self.clone();
        }
        let clock = Self {
            now: OnceLock::from(self.now()),
            stamp: self.stamp.clone(),
            timezone: self.timezone,
            seed: OnceLock::new(),
            draws: AtomicU64::new(0),
            touched: AtomicBool::new(false),
            source: Source::Batch,
            parent: Some(self.clone()),
            streams: Mutex::new(BTreeMap::new()),
        };
        Arc::new(clock)
    }

    pub fn is_batch(&self) -> bool {
        matches!(self.source, Source::Batch)
    }

    pub fn stream(self: &Arc<Self>, row: usize) -> Arc<Self> {
        if !self.is_batch() {
            return self.clone();
        }
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        streams.entry(row).or_insert_with(|| self.row(row)).clone()
    }

    pub fn item(self: &Arc<Self>, index: usize) -> Arc<Self> {
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        streams
            .entry(index)
            .or_insert_with(|| self.child(index, self.clone()))
            .clone()
    }

    pub fn reset_streams(&self) {
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        streams.clear();
    }

    pub fn fresh_stream(self: &Arc<Self>, row: usize) -> Arc<Self> {
        if !self.is_batch() {
            return self.clone();
        }
        let clock = self.row(row);
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        streams.insert(row, clock.clone());
        clock
    }

    pub fn row_seed(self: &Arc<Self>, row: usize) -> Option<u64> {
        let streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        streams.get(&row).and_then(|stream| stream.seed())
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

    pub fn timezone_name(&self) -> &'static str {
        self.timezone.name()
    }

    pub fn zone(&self) -> Arc<str> {
        ZONE.with(|z| {
            let mut z = z.borrow_mut();
            match &*z {
                Some((tz, name)) if *tz == self.timezone => name.clone(),
                _ => {
                    let name: Arc<str> = Arc::from(self.timezone.name());
                    *z = Some((self.timezone, name.clone()));
                    name
                }
            }
        })
    }

    pub fn recycle(clock: Arc<Self>) {
        if Arc::strong_count(&clock) == 1 {
            POOL.with(|p| {
                let mut p = p.borrow_mut();
                if p.len() < 4 {
                    p.push(clock);
                }
            });
        }
    }

    pub fn drew(&self) -> bool {
        self.draws.load(Ordering::Relaxed) > 0 || self.touched.load(Ordering::Relaxed)
    }

    pub fn seed(&self) -> Option<u64> {
        match self.drew() {
            true => self.seed.get().copied(),
            false => None,
        }
    }

    fn base(&self) -> u64 {
        *self.seed.get_or_init(|| match (self.source, &self.parent) {
            (Source::Row(index), Some(parent)) => row_seed(parent.base(), index),
            (Source::Batch, Some(parent)) => parent.base(),
            _ => fastrand::u64(..) & DRAWN_BITS,
        })
    }

    pub fn next_u64(&self) -> u64 {
        let base = self.base();
        let k = self.draws.fetch_add(1, Ordering::Relaxed);
        let mut up = self.parent.as_deref();
        while let Some(parent) = up {
            if parent.touched.swap(true, Ordering::Relaxed) {
                break;
            }
            up = parent.parent.as_deref();
        }
        mix(base.wrapping_add(k.wrapping_add(1).wrapping_mul(GAMMA)))
    }

    pub fn next_f64(&self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn enter(self: &Arc<Self>) -> Scope {
        Scope(CURRENT.with(|c| c.replace(Some(self.clone()))))
    }

    pub fn scope<R>(self: &Arc<Self>, f: impl FnOnce() -> R) -> R {
        let _scope = self.enter();
        f()
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Clock>>> = const { RefCell::new(None) };
    static POOL: RefCell<Vec<Arc<Clock>>> = const { RefCell::new(Vec::new()) };
    static ZONE: RefCell<Option<(Tz, Arc<str>)>> = const { RefCell::new(None) };
}

pub struct Scope(Option<Arc<Clock>>);

impl Drop for Scope {
    fn drop(&mut self) {
        let previous = self.0.take();
        CURRENT.with(|c| *c.borrow_mut() = previous);
    }
}

pub fn current_is_set() -> bool {
    CURRENT.with(|c| c.borrow().is_some())
}

pub fn current() -> Option<Arc<Clock>> {
    CURRENT.with(|c| c.borrow().clone())
}

pub fn now() -> Instant {
    CURRENT.with(|c| match c.borrow().as_ref() {
        Some(clock) => clock.now(),
        None => system_now(),
    })
}

pub fn timezone() -> Tz {
    CURRENT.with(|c| match c.borrow().as_ref() {
        Some(clock) => clock.timezone,
        None => system_timezone(),
    })
}

#[derive(Debug)]
pub enum Origin {
    Rows(Rc<[usize]>, Option<Rc<Origin>>),
    Items(Rc<[(usize, usize)]>, Option<Rc<Origin>>),
}

thread_local! {
    static ORIGIN: RefCell<Option<Rc<Origin>>> = const { RefCell::new(None) };
    static ROW: Cell<Option<usize>> = const { Cell::new(None) };
}

static UNROWED: AtomicU64 = AtomicU64::new(0);

pub fn unrowed_draws() -> u64 {
    UNROWED.load(Ordering::Relaxed)
}

pub struct Origins(Option<Rc<Origin>>);

impl Drop for Origins {
    fn drop(&mut self) {
        let previous = self.0.take();
        ORIGIN.with(|o| *o.borrow_mut() = previous);
    }
}

pub fn origin() -> Option<Rc<Origin>> {
    ORIGIN.with(|o| o.borrow().clone())
}

pub fn enter_origin(origin: Option<Rc<Origin>>) -> Origins {
    Origins(ORIGIN.with(|o| o.replace(origin)))
}

pub fn rows(rows: Rc<[usize]>) -> Origins {
    let parent = origin();
    enter_origin(Some(Rc::new(Origin::Rows(rows, parent))))
}

pub fn items(items: Rc<[(usize, usize)]>) -> Origins {
    let parent = origin();
    enter_origin(Some(Rc::new(Origin::Items(items, parent))))
}

#[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
fn resolve(origin: Option<&Rc<Origin>>, row: usize, clock: &Arc<Clock>) -> Arc<Clock> {
    match origin.map(|o| &**o) {
        None => clock.stream(row),
        Some(Origin::Rows(rows, parent)) => resolve(
            parent.as_ref(),
            rows.get(row).copied().unwrap_or(row),
            clock,
        ),
        Some(Origin::Items(items, parent)) => match items.get(row) {
            Some(&(at, index)) => resolve(parent.as_ref(), at, clock).item(index),
            None => resolve(parent.as_ref(), row, clock),
        },
    }
}

pub fn stream_at(row: usize) -> Option<Arc<Clock>> {
    let clock = current()?;
    Some(ORIGIN.with(|o| resolve(o.borrow().as_ref(), row, &clock)))
}

pub fn streamed() -> bool {
    ORIGIN.with(|o| o.borrow().is_some()) || drawing_batch()
}

pub fn stream_key(row: usize) -> usize {
    stream_at(row).map_or(0, |s| Arc::as_ptr(&s) as usize)
}

pub fn drawing_batch() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(|clock| clock.is_batch()))
}

pub fn set_row(row: Option<usize>) -> Option<usize> {
    ROW.with(|r| r.replace(row))
}

pub fn at_row<R>(row: usize, f: impl FnOnce() -> R) -> R {
    let previous = set_row(Some(row));
    let out = f();
    set_row(previous);
    out
}

fn draw(clock: &Arc<Clock>) -> u64 {
    if !streamed() {
        return clock.next_u64();
    }
    match ROW.with(Cell::get) {
        Some(row) => ORIGIN
            .with(|o| resolve(o.borrow().as_ref(), row, clock))
            .next_u64(),
        None => {
            UNROWED.fetch_add(1, Ordering::Relaxed);
            clock.next_u64()
        }
    }
}

pub fn next_u64() -> u64 {
    CURRENT.with(|c| match c.borrow().as_ref() {
        Some(clock) => draw(clock),
        None => fastrand::u64(..),
    })
}

pub fn random_f64() -> f64 {
    CURRENT.with(|c| match c.borrow().as_ref() {
        Some(clock) => (draw(clock) >> 11) as f64 / (1u64 << 53) as f64,
        None => fastrand::f64(),
    })
}

pub fn random_int(upper: i64) -> i64 {
    CURRENT.with(|c| match c.borrow().as_ref() {
        Some(clock) => fastrand::Rng::with_seed(draw(clock)).i64(0..=upper),
        None => fastrand::i64(0..=upper),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seeded_clock_repeats_its_stream() {
        let a = Clock::new(None, Some(Tz::UTC), Some(42));
        let b = Clock::new(None, Some(Tz::UTC), Some(42));
        let xs: Vec<i64> = (0..8).map(|_| a.scope(|| random_int(1000))).collect();
        let ys: Vec<i64> = (0..8).map(|_| b.scope(|| random_int(1000))).collect();
        assert_eq!(xs, ys);
        assert_eq!(a.seed(), Some(42));
    }

    #[test]
    fn a_clock_reads_now_once() {
        let clock = Clock::new(None, None, None);
        let first = clock.scope(now);
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(clock.scope(now), first);
        assert_eq!(clock.seed(), None);
    }

    #[test]
    fn rows_replay_alone() {
        let batch = Clock::new(None, None, Some(7));
        let row = batch.row(3);
        let drawn: Vec<u64> = (0..4).map(|_| row.next_u64()).collect();
        let alone = Clock::new(Some(row.now()), None, row.seed());
        let again: Vec<u64> = (0..4).map(|_| alone.next_u64()).collect();
        assert_eq!(drawn, again);
        assert_eq!(row.seed(), Some(row_seed(7, 3)));
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
