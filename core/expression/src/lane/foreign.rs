use crate::lane::columns::{Column, Values};
use crate::lane::Bits;
use chrono::{Offset, TimeZone};
use chrono_tz::Tz;
use rust_decimal::prelude::FromPrimitive;
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

/// Floating point column borrowed as Arrow `Float64` stores it. Each value reads as the
/// decimal with the fewest digits that still identifies it, exactly like [`Values::F64`]
/// and like the same number given as JSON; the conversion runs once per column.
#[derive(Debug)]
pub struct Float64<'a> {
    values: &'a [f64],
    resolved: OnceCell<Option<Resolved>>,
}

impl<'a> Float64<'a> {
    pub fn new(values: &'a [f64]) -> Self {
        Self {
            values,
            resolved: OnceCell::new(),
        }
    }

    pub fn values(&self) -> &'a [f64] {
        self.values
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn column(&'a self, validity: Validity<'a>) -> Column<'a> {
        match self.resolved.get_or_init(|| self.resolve(validity)) {
            Some(resolved) => resolved.column(validity),
            None => Column {
                values: Values::F64(self.values),
                validity,
            },
        }
    }

    fn resolve(&self, validity: Validity) -> Option<Resolved> {
        let len = self.values.len();
        let mut mant = Vec::with_capacity(len);
        let mut scale = Vec::with_capacity(len);
        let mut hint = 2u8;
        for (row, f) in self.values.iter().enumerate() {
            match Float::guided(*f, &mut hint) {
                Some((m, s)) => {
                    mant.push(m);
                    scale.push(s);
                }
                None if !Resolved::live(validity, row) => {
                    mant.push(0);
                    scale.push(0);
                }
                None => return self.wide(validity),
            }
        }
        Some(Resolved {
            values: Owned::Scaled { mant, scale },
            valid: None,
        })
    }

    fn wide(&self, validity: Validity) -> Option<Resolved> {
        let decimals = self
            .values
            .iter()
            .enumerate()
            .map(|(row, f)| match Resolved::live(validity, row) {
                true => Float::decimal(*f),
                false => Some(Float::decimal(*f).unwrap_or(Decimal::ZERO)),
            })
            .collect::<Option<Vec<Decimal>>>()?;
        Some(Resolved {
            values: Owned::Dec(decimals),
            valid: None,
        })
    }
}

pub(crate) struct Float;

impl Float {
    const LIMIT: f64 = 1e15;
    const POWERS: [f64; 23] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
        1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
    ];

    #[inline]
    pub(crate) fn parts(f: f64) -> Option<(i64, u8)> {
        let a = f.abs();
        if !(a < Self::LIMIT) {
            return None;
        }
        if a == a.trunc() {
            return (a != 0.0 || f.is_sign_positive()).then_some((f as i64, 0));
        }
        let exponent = ((a.to_bits() >> 52) as i32) - 1023;
        let digits = (exponent * 1233) >> 12;
        let mut s = (14 - digits).clamp(1, 22) as usize;
        if a * Self::POWERS[s] >= Self::LIMIT {
            s -= 1;
        }
        let scaled = (a * Self::POWERS[s]).round();
        if !(scaled < Self::LIMIT) || scaled / Self::POWERS[s] != a {
            return None;
        }
        let mut m = scaled as i64;
        for (step, by) in [(8usize, 100_000_000i64), (4, 10_000), (2, 100), (1, 10)] {
            while s >= step && m % by == 0 {
                m /= by;
                s -= step;
            }
        }
        Some((if f < 0.0 { -m } else { m }, s as u8))
    }

    #[inline]
    fn guided(f: f64, hint: &mut u8) -> Option<(i64, u8)> {
        let a = f.abs();
        let power = Self::POWERS[usize::from(*hint)];
        let scaled = (a * power).round();
        if a != a.trunc() && scaled < Self::LIMIT && scaled / power == a {
            let m = scaled as i64;
            if m % 10 != 0 {
                return Some((if f < 0.0 { -m } else { m }, *hint));
            }
        }
        let parts = Self::parts(f)?;
        if (1..=22).contains(&parts.1) {
            *hint = parts.1;
        }
        Some(parts)
    }

    #[inline]
    pub(crate) fn decimal(f: f64) -> Option<Decimal> {
        match Self::parts(f) {
            Some((m, s)) => Some(Decimal::new(m, u32::from(s))),
            None => Self::printed(f),
        }
    }

    fn printed(f: f64) -> Option<Decimal> {
        if !f.is_finite() {
            return None;
        }
        let mut buffer = [0u8; 512];
        let mut cursor = std::io::Cursor::new(&mut buffer[..]);
        let written = std::io::Write::write_fmt(&mut cursor, format_args!("{f}")).is_ok();
        let len = cursor.position() as usize;
        written
            .then(|| std::str::from_utf8(&buffer[..len]).ok())
            .flatten()
            .and_then(|text| Decimal::from_str(text).ok())
            .or_else(|| Decimal::from_f64(f))
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

#[cfg(test)]
mod float_tests {
    use super::{Float, Float64};
    use crate::lane::columns::{Column, Values};
    use rust_decimal::prelude::FromPrimitive;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn reference(f: f64) -> Option<Decimal> {
        f.is_finite()
            .then(|| {
                Decimal::from_str(&format!("{f}"))
                    .ok()
                    .or_else(|| Decimal::from_f64(f))
            })
            .flatten()
    }

    fn samples() -> Vec<f64> {
        let mut out = vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.1,
            0.2,
            0.1 + 0.2,
            0.3,
            1.0 / 3.0,
            2.0 / 3.0,
            0.5,
            0.05,
            1e-7,
            1.5e-7,
            123456.789,
            1e15,
            1e15 - 1.0,
            1e15 - 0.125,
            999_999_999_999_999.9,
            99_999_999_999_999.98,
            9_007_199_254_740_991.0,
            9_007_199_254_740_992.0,
            9_007_199_254_740_993.0,
            1.7976931348623157e308,
            2.2250738585072014e-308,
            5e-324,
            4.9406564584124654e-324,
            7.922816251426434e28,
            7.922816251426433e28,
            7.9228162514264337593543950335e28,
            1e28,
            1e29,
            1e-28,
            1e-29,
            1.2345678901234567e-12,
            1.2345678901234567e-11,
            12345678.901234567,
            0.30000000000000004,
            5.551115123125783e-17,
            179.99999999999997,
            8.41,
            4.35,
            1.005,
            2.675,
            1e21,
            1e22,
            1e23,
            123e-20,
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            f64::EPSILON,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        for power in -30..=30 {
            let p = 10f64.powi(power);
            out.extend([p, p.next_up(), p.next_down(), -p, 3.0 * p, 7.0 * p]);
        }
        for power in 0..=60 {
            let p = 2f64.powi(power);
            out.extend([p, p - 1.0, p + 1.0, p + 0.5, 1.0 / p, -1.0 / p]);
        }
        let mut state = 0x243F_6A88_85A3_08D3u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for i in 0..600_000u64 {
            let bits = next();
            let digits = 1 + (next() % 17) as u32;
            let mantissa = (next() % 10u64.pow(digits)) as f64;
            let scale = (next() % 26) as i32;
            let f = match i % 8 {
                0 => f64::from_bits(bits),
                1 => (bits % 10_000_000) as f64 / 100.0,
                2 => (bits as i64 as f64) * 1e-30,
                3 => f64::from_bits(bits % 0x0010_0000_0000_0000),
                4 => mantissa / 10f64.powi(scale),
                5 => -mantissa * 10f64.powi(-scale),
                6 => (bits % 1_000_000_000) as f64 / 1e4 / 3.0,
                _ => ((bits >> 11) as f64) * 2f64.powi((next() % 80) as i32 - 60),
            };
            out.push(f);
            out.push(f.next_up());
        }
        out
    }

    #[test]
    fn direct_conversion_matches_the_printed_number() {
        let mut short = 0usize;
        for f in samples() {
            let got = Float::decimal(f);
            assert_eq!(
                got.map(|d| d.serialize()),
                reference(f).map(|d| d.serialize()),
                "{f:e}"
            );
            if let Some((m, s)) = Float::parts(f) {
                short += 1;
                let exact = Decimal::new(m, u32::from(s));
                assert_eq!(
                    Some(exact.serialize()),
                    reference(f).map(|d| d.serialize()),
                    "{f:e}"
                );
            }
        }
        assert!(short > 200_000, "{short}");
    }

    fn same(resolved: &Column, plain: &Column, rows: usize) {
        for row in 0..rows {
            assert_eq!(resolved.valid(row), plain.valid(row), "row {row}");
            if plain.valid(row) {
                assert_eq!(
                    resolved.number(row).map(|d| d.serialize()),
                    plain.number(row).map(|d| d.serialize()),
                    "row {row}"
                );
            }
            assert_eq!(
                format!("{:?}", resolved.variable(row)),
                format!("{:?}", plain.variable(row)),
                "row {row}"
            );
        }
    }

    #[test]
    fn float64_columns_read_like_f64_columns() {
        let all = samples();
        let finite: Vec<f64> = all
            .iter()
            .copied()
            .filter(|f| Float::decimal(*f).is_some())
            .collect();
        let short: Vec<f64> = finite
            .iter()
            .copied()
            .filter(|f| Float::parts(*f).is_some())
            .collect();
        let valid: Vec<u64> = (0..all.len().div_ceil(64))
            .map(|w| 0xB7E1_5162_8AED_2A6Bu64.rotate_left(w as u32 % 64))
            .collect();
        for values in [&all, &finite, &short] {
            let floats = Float64::new(values);
            let resolved = Column::new(Values::Float64(&floats));
            same(&resolved, &Column::new(Values::F64(values)), values.len());
            same(
                &resolved.resolved(),
                &Column::new(Values::F64(values)),
                values.len(),
            );
            let floats = Float64::new(values);
            let masked = Column::with_validity(Values::Float64(&floats), &valid, 0);
            same(
                &masked,
                &Column::with_validity(Values::F64(values), &valid, 0),
                values.len(),
            );
        }
        let floats = Float64::new(&short);
        let column = Column::new(Values::Float64(&floats)).resolved();
        assert!(matches!(column.values, Values::Scaled { .. }));
        let floats = Float64::new(&finite);
        let column = Column::new(Values::Float64(&floats)).resolved();
        assert!(matches!(column.values, Values::Dec(_)));
        let floats = Float64::new(&all);
        let column = Column::new(Values::Float64(&floats)).resolved();
        assert!(matches!(column.values, Values::F64(_)));
        let hidden = [1.5, f64::NAN, 2.25, f64::INFINITY];
        let bits = [0b0101u64];
        let floats = Float64::new(&hidden);
        let column = Column::with_validity(Values::Float64(&floats), &bits, 0).resolved();
        assert!(matches!(column.values, Values::Scaled { .. }));
        same(
            &column,
            &Column::with_validity(Values::F64(&hidden), &bits, 0),
            4,
        );
    }
}
