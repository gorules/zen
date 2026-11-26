use crate::lane::columns::{Column, Values};
use crate::lane::Bits;
use chrono::{Offset, TimeZone};
use chrono_tz::Tz;
use rust_decimal::Decimal;
use std::cell::OnceCell;
use std::str::FromStr;

type Validity<'a> = Option<(&'a [u64], usize)>;

#[derive(Debug)]
enum Owned {
    Scaled { mant: Vec<i64>, scale: Vec<u8> },
    Dec(Vec<Decimal>),
    Text { offsets: Vec<i32>, data: String },
}

#[derive(Debug)]
struct Resolved {
    values: Owned,
    valid: Option<Vec<u64>>,
}

impl Resolved {
    fn column<'a>(&'a self, validity: Validity<'a>) -> Column<'a> {
        let values = match &self.values {
            Owned::Scaled { mant, scale } => Values::Scaled { mant, scale },
            Owned::Dec(values) => Values::Dec(values),
            Owned::Text { offsets, data } => Values::Text { offsets, data },
        };
        Column {
            values,
            validity: match &self.valid {
                Some(bits) => Some((bits, 0)),
                None => validity,
            },
        }
    }

    fn mark(valid: &mut Option<Vec<u64>>, validity: Validity, len: usize, row: usize) {
        let bits = valid.get_or_insert_with(|| {
            let mut bits: Vec<u64> = (0..len.div_ceil(64))
                .map(|w| match validity {
                    None => u64::MAX,
                    Some((bits, offset)) => {
                        Bits::word(bits, offset + w * 64, (len - w * 64).min(64))
                    }
                })
                .collect();
            Bits::trim(&mut bits, len);
            bits
        });
        Bits::set(bits, row, false);
    }

    fn live(validity: Validity, row: usize) -> bool {
        validity.is_none_or(|(bits, offset)| Bits::get(bits, offset + row))
    }
}

/// Decimal column borrowed as Arrow `Decimal128` stores it: one `i128` per value and one
/// scale for the column. Values read as the decimal `value * 10^-scale`, exactly like the
/// same number given as text: digits beyond the engine's 96-bit decimal are rounded away
/// and values whose integer part does not fit read as null.
#[derive(Debug)]
pub struct Decimal128<'a> {
    values: &'a [i128],
    scale: i8,
    resolved: OnceCell<Resolved>,
}

impl<'a> Decimal128<'a> {
    pub fn new(values: &'a [i128], scale: i8) -> Self {
        Self {
            values,
            scale,
            resolved: OnceCell::new(),
        }
    }

    pub fn values(&self) -> &'a [i128] {
        self.values
    }

    pub fn scale(&self) -> i8 {
        self.scale
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn column(&'a self, validity: Validity<'a>) -> Column<'a> {
        self.resolved
            .get_or_init(|| self.resolve(validity))
            .column(validity)
    }

    fn text(value: i128, scale: u32) -> String {
        let digits = value.unsigned_abs().to_string();
        let scale = scale as usize;
        let padded = match digits.len() <= scale {
            true => format!("{}{digits}", "0".repeat(scale + 1 - digits.len())),
            false => digits,
        };
        let (whole, fraction) = padded.split_at(padded.len() - scale);
        let sign = if value < 0 { "-" } else { "" };
        match fraction.is_empty() {
            true => format!("{sign}{whole}"),
            false => format!("{sign}{whole}.{fraction}"),
        }
    }

    fn resolve(&self, validity: Validity) -> Resolved {
        let len = self.values.len();
        let mut valid = None;
        let (factor, scale) = match self.scale {
            s if s < 0 => (10i128.checked_pow(u32::from(s.unsigned_abs())), 0u32),
            s => (Some(1), s as u32),
        };
        let mut values: Vec<i128> = Vec::new();
        let values: &[i128] = match factor {
            Some(1) => self.values,
            _ => {
                values.reserve(len);
                for (row, v) in self.values.iter().enumerate() {
                    match factor.and_then(|f| v.checked_mul(f)) {
                        Some(v) => values.push(v),
                        None => {
                            values.push(0);
                            if Resolved::live(validity, row) {
                                Resolved::mark(&mut valid, validity, len, row);
                            }
                        }
                    }
                }
                &values
            }
        };
        let narrow = scale <= 28
            && values
                .iter()
                .enumerate()
                .all(|(row, v)| i64::try_from(*v).is_ok() || !Resolved::live(validity, row));
        if narrow {
            return Resolved {
                values: Owned::Scaled {
                    mant: values.iter().map(|v| *v as i64).collect(),
                    scale: vec![scale as u8; len],
                },
                valid,
            };
        }
        let decimals = values
            .iter()
            .enumerate()
            .map(|(row, v)| {
                if !Resolved::live(validity, row) {
                    return Decimal::ZERO;
                }
                let parsed = match scale <= 28 {
                    true => Decimal::try_from_i128_with_scale(*v, scale).ok(),
                    false => None,
                };
                let parsed = parsed.or_else(|| Decimal::from_str(&Self::text(*v, scale)).ok());
                parsed.unwrap_or_else(|| {
                    Resolved::mark(&mut valid, validity, len, row);
                    Decimal::ZERO
                })
            })
            .collect();
        Resolved {
            values: Owned::Dec(decimals),
            valid,
        }
    }
}

/// Unit of the values of a [`Temporal`] column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeUnit {
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
}

impl TimeUnit {
    fn per_second(self) -> i64 {
        match self {
            TimeUnit::Second => 1,
            TimeUnit::Millisecond => 1_000,
            TimeUnit::Microsecond => 1_000_000,
            TimeUnit::Nanosecond => 1_000_000_000,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Zone {
    Naive,
    Utc,
    Fixed(i32),
    Named(Tz),
}

impl Zone {
    fn parse(zone: Option<&str>) -> Option<Zone> {
        let Some(zone) = zone else {
            return Some(Zone::Naive);
        };
        if matches!(
            zone,
            "UTC" | "utc" | "Z" | "z" | "Etc/UTC" | "GMT" | "Etc/GMT"
        ) {
            return Some(Zone::Utc);
        }
        if let Some(seconds) = Self::offset(zone) {
            return Some(match seconds {
                0 => Zone::Utc,
                _ => Zone::Fixed(seconds),
            });
        }
        zone.parse::<Tz>().ok().map(Zone::Named)
    }

    fn offset(text: &str) -> Option<i32> {
        let bytes = text.as_bytes();
        let sign = match bytes.first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let digits: Vec<u8> = bytes[1..].iter().copied().filter(|b| *b != b':').collect();
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let number = |s: &[u8]| s.iter().fold(0i32, |n, d| n * 10 + i32::from(d - b'0'));
        let (hours, minutes) = match digits.len() {
            2 => (number(&digits), 0),
            4 => (number(&digits[..2]), number(&digits[2..])),
            _ => return None,
        };
        (hours < 24 && minutes < 60).then_some(sign * (hours * 3600 + minutes * 60))
    }
}

#[derive(Debug, Clone, Copy)]
enum Stamps<'a> {
    Days(&'a [i32]),
    DayMillis(&'a [i64]),
    Instants(&'a [i64], TimeUnit),
    Times32(&'a [i32], TimeUnit),
    Times64(&'a [i64], TimeUnit),
}

/// Date, timestamp or time-of-day column borrowed as Arrow stores it. Each value reads as
/// its ISO 8601 text, exactly like the same instant given as text: `2024-03-01` for dates,
/// `2024-03-01T12:30:00Z` (UTC), `2024-03-01T13:30:00+01:00` (other zones) or
/// `2024-03-01T12:30:00` (no zone) for timestamps, `12:30:00` for times, with a fraction of
/// 3, 6 or 9 digits only when it is not zero, as RFC 3339 writers do. Values outside the years 0 to 9999 read as null.
#[derive(Debug)]
pub struct Temporal<'a> {
    stamps: Stamps<'a>,
    zone: Zone,
    resolved: OnceCell<Resolved>,
}

impl<'a> Temporal<'a> {
    fn of(stamps: Stamps<'a>, zone: Zone) -> Self {
        Self {
            stamps,
            zone,
            resolved: OnceCell::new(),
        }
    }

    /// Days since 1970-01-01, as Arrow `Date32`.
    pub fn date32(days: &'a [i32]) -> Self {
        Self::of(Stamps::Days(days), Zone::Naive)
    }

    /// Milliseconds since 1970-01-01 of whole days, as Arrow `Date64`.
    pub fn date64(millis: &'a [i64]) -> Self {
        Self::of(Stamps::DayMillis(millis), Zone::Naive)
    }

    /// Instants since the epoch, as Arrow `Timestamp(unit, zone)`. `None` when the zone is
    /// neither an IANA name nor a `+hh:mm` offset.
    pub fn timestamp(values: &'a [i64], unit: TimeUnit, zone: Option<&str>) -> Option<Self> {
        Some(Self::of(Stamps::Instants(values, unit), Zone::parse(zone)?))
    }

    /// Time of day, as Arrow `Time32(unit)` (seconds or milliseconds).
    pub fn time32(values: &'a [i32], unit: TimeUnit) -> Self {
        Self::of(Stamps::Times32(values, unit), Zone::Naive)
    }

    /// Time of day, as Arrow `Time64(unit)` (microseconds or nanoseconds).
    pub fn time64(values: &'a [i64], unit: TimeUnit) -> Self {
        Self::of(Stamps::Times64(values, unit), Zone::Naive)
    }

    pub(crate) fn len(&self) -> usize {
        match self.stamps {
            Stamps::Days(v) | Stamps::Times32(v, _) => v.len(),
            Stamps::DayMillis(v) | Stamps::Instants(v, _) | Stamps::Times64(v, _) => v.len(),
        }
    }

    pub(crate) fn column(&'a self, validity: Validity<'a>) -> Column<'a> {
        self.resolved
            .get_or_init(|| self.resolve(validity))
            .column(validity)
    }

    fn resolve(&self, validity: Validity) -> Resolved {
        let len = self.len();
        let mut valid = None;
        let mut data = Vec::with_capacity(len * 20);
        let mut offsets = Vec::with_capacity(len + 1);
        offsets.push(0i32);
        for row in 0..len {
            if Resolved::live(validity, row) {
                let mark = data.len();
                if !self.write(&mut data, row) {
                    data.truncate(mark);
                    Resolved::mark(&mut valid, validity, len, row);
                }
            }
            offsets.push(i32::try_from(data.len()).unwrap_or(i32::MAX));
        }
        Resolved {
            values: Owned::Text {
                offsets,
                data: String::from_utf8(data).unwrap_or_default(),
            },
            valid,
        }
    }

    fn write(&self, out: &mut Vec<u8>, row: usize) -> bool {
        match self.stamps {
            Stamps::Days(days) => Clock::date(out, i64::from(days[row])),
            Stamps::DayMillis(millis) => Clock::date(out, millis[row].div_euclid(86_400_000)),
            Stamps::Instants(values, unit) => Clock::timestamp(out, values[row], unit, self.zone),
            Stamps::Times32(values, unit) => Clock::time_of_day(out, i64::from(values[row]), unit),
            Stamps::Times64(values, unit) => Clock::time_of_day(out, values[row], unit),
        }
    }
}

struct Clock;

impl Clock {
    fn civil(days: i64) -> (i64, u32, u32) {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if m <= 2 { y + 1 } else { y }, m, d)
    }

    fn two(out: &mut Vec<u8>, n: u32) {
        out.push(b'0' + (n / 10) as u8);
        out.push(b'0' + (n % 10) as u8);
    }

    fn date(out: &mut Vec<u8>, days: i64) -> bool {
        if !(-719_528..=2_932_896).contains(&days) {
            return false;
        }
        let (y, m, d) = Self::civil(days);
        let y = y as u32;
        Self::two(out, y / 100);
        Self::two(out, y % 100);
        out.push(b'-');
        Self::two(out, m);
        out.push(b'-');
        Self::two(out, d);
        true
    }

    fn time(out: &mut Vec<u8>, seconds: i64, nanos: i64) {
        let seconds = seconds.rem_euclid(86_400) as u32;
        Self::two(out, seconds / 3600);
        out.push(b':');
        Self::two(out, seconds / 60 % 60);
        out.push(b':');
        Self::two(out, seconds % 60);
        if nanos != 0 {
            let digits = match (nanos % 1_000_000, nanos % 1_000) {
                (0, _) => 3,
                (_, 0) => 6,
                _ => 9,
            };
            out.push(b'.');
            let mut buffer = [b'0'; 9];
            let mut rest = nanos;
            for at in (0..9).rev() {
                buffer[at] = b'0' + (rest % 10) as u8;
                rest /= 10;
            }
            out.extend_from_slice(&buffer[..digits]);
        }
    }

    fn timestamp(out: &mut Vec<u8>, value: i64, unit: TimeUnit, zone: Zone) -> bool {
        let per = unit.per_second();
        let (seconds, fraction) = (value.div_euclid(per), value.rem_euclid(per));
        let shift = match zone {
            Zone::Naive | Zone::Utc => 0,
            Zone::Fixed(offset) => offset,
            Zone::Named(tz) => {
                let Some(utc) = chrono::DateTime::from_timestamp(seconds, 0) else {
                    return false;
                };
                tz.offset_from_utc_datetime(&utc.naive_utc())
                    .fix()
                    .local_minus_utc()
            }
        };
        let Some(local) = seconds.checked_add(i64::from(shift)) else {
            return false;
        };
        if !Self::date(out, local.div_euclid(86_400)) {
            return false;
        }
        out.push(b'T');
        Self::time(
            out,
            local.rem_euclid(86_400),
            fraction * (1_000_000_000 / per),
        );
        match zone {
            Zone::Naive => {}
            Zone::Utc => out.push(b'Z'),
            Zone::Fixed(_) | Zone::Named(_) => {
                out.push(if shift < 0 { b'-' } else { b'+' });
                let minutes = shift.unsigned_abs() / 60;
                Self::two(out, minutes / 60);
                out.push(b':');
                Self::two(out, minutes % 60);
            }
        }
        true
    }

    fn time_of_day(out: &mut Vec<u8>, value: i64, unit: TimeUnit) -> bool {
        let per = unit.per_second();
        if !(0..86_400 * per).contains(&value) {
            return false;
        }
        Self::time(out, value / per, value % per * (1_000_000_000 / per));
        true
    }
}
