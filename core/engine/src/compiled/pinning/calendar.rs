use chrono::{DateTime, Duration, LocalResult, Offset, TimeZone};
use chrono_tz::Tz;
use std::fmt::Write;

const DAY: f64 = 86_400_000.0;
pub(super) const LOCALE_BOTH: u8 = 0;
pub(super) const LOCALE_DATE: u8 = 1;
pub(super) const LOCALE_TIME: u8 = 2;

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTH_DAYS: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

#[derive(Clone, Copy)]
pub(super) struct Zone {
    pub(super) tz: Tz,
    fixed: Option<i32>,
}

impl Zone {
    pub(super) fn of(tz: Tz) -> Self {
        let name = tz.name();
        let fixed = matches!(
            name,
            "UTC" | "UCT" | "GMT" | "GMT0" | "GMT+0" | "GMT-0" | "Greenwich" | "Universal" | "Zulu"
        ) || name.starts_with("Etc/");
        Self {
            tz,
            fixed: fixed.then(|| {
                tz.offset_from_utc_datetime(&chrono::NaiveDateTime::default())
                    .fix()
                    .local_minus_utc()
            }),
        }
    }

    pub(super) fn offset(&self, ms: f64) -> Option<i32> {
        if let Some(fixed) = self.fixed {
            return Some(fixed);
        }
        let at = modelled(ms)?;
        Some(
            self.tz
                .offset_from_utc_datetime(&at)
                .fix()
                .local_minus_utc(),
        )
    }

    pub(super) fn utc(&self, local: f64) -> f64 {
        if let Some(fixed) = self.fixed {
            return local - fixed as f64 * 1000.0;
        }
        let Some(naive) = modelled(local) else {
            return f64::NAN;
        };
        let offset = match self.tz.offset_from_local_datetime(&naive) {
            LocalResult::Single(offset) => offset.fix(),
            LocalResult::Ambiguous(earlier, _) => earlier.fix(),
            LocalResult::None => {
                match self
                    .tz
                    .offset_from_local_datetime(&(naive - Duration::hours(3)))
                    .earliest()
                {
                    Some(offset) => offset.fix(),
                    None => return f64::NAN,
                }
            }
        };
        local - offset.local_minus_utc() as f64 * 1000.0
    }
}

fn modelled(ms: f64) -> Option<chrono::NaiveDateTime> {
    use chrono::Datelike;
    const CYCLE: f64 = 146_097.0 * 86_400_000.0;
    const SPAN: f64 = 250_000.0 * 365.2425 * 86_400_000.0;
    if !ms.is_finite() {
        return None;
    }
    let cycles = match ms.abs() > SPAN {
        true => ((ms.abs() - SPAN) / CYCLE).ceil() * ms.signum(),
        false => 0.0,
    };
    let at = DateTime::from_timestamp_millis((ms - cycles * CYCLE) as i64)?.naive_utc();
    if at.year() <= 2099 {
        return Some(at);
    }
    let year = chrono::NaiveDate::from_yo_opt(at.year(), 1)?;
    let twin = (2072..=2099).find(|candidate| {
        chrono::NaiveDate::from_yo_opt(*candidate, 1).is_some_and(|first| {
            first.leap_year() == year.leap_year() && first.weekday() == year.weekday()
        })
    })?;
    Some(chrono::NaiveDate::from_yo_opt(twin, at.ordinal())?.and_time(at.time()))
}

fn floor_div(a: i64, b: i64) -> i64 {
    let m = a % b;
    (a - (m + (m < 0) as i64 * b)) / b
}

fn math_mod(a: i64, b: i64) -> i64 {
    let m = a % b;
    m + (m < 0) as i64 * b
}

fn days_from_year(y: i64) -> i64 {
    365 * (y - 1970) + floor_div(y - 1969, 4) - floor_div(y - 1901, 100) + floor_div(y - 1601, 400)
}

fn days_in_year(y: i64) -> i64 {
    365 + (y % 4 == 0) as i64 - (y % 100 == 0) as i64 + (y % 400 == 0) as i64
}

#[cfg(test)]
fn year_from_days(days: &mut i64) -> i64 {
    let d = *days;
    let mut y = floor_div(d * 10000, 3652425) + 1970;
    loop {
        let mut d1 = d - days_from_year(y);
        if d1 < 0 {
            y -= 1;
            d1 += days_in_year(y);
            *days = d1;
            return y;
        }
        let nd = days_in_year(y);
        if d1 < nd {
            *days = d1;
            return y;
        }
        y += 1;
    }
}

pub(super) fn time_clip(t: f64) -> f64 {
    if (-8.64e15..=8.64e15).contains(&t) {
        t.trunc() + 0.0
    } else {
        f64::NAN
    }
}

fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 2 } else { mp - 10 };
    (yoe + era * 400 + (month <= 1) as i64, month, day)
}

pub(super) fn decompose(d: i64) -> [f64; 8] {
    let ms_of_day = math_mod(d, 86_400_000);
    let days = (d - ms_of_day) / 86_400_000;
    let (y, month, day) = civil(days);
    [
        y as f64,
        month as f64,
        day as f64,
        math_mod(days + 4, 7) as f64,
        (ms_of_day / 3_600_000) as f64,
        (ms_of_day / 60_000 % 60) as f64,
        (ms_of_day / 1000 % 60) as f64,
        (ms_of_day % 1000) as f64,
    ]
}

fn make_date(fields: [f64; 7]) -> f64 {
    let [y, m, dt, h, mi, s, milli] = fields;
    let ym = y + (m / 12.0).floor();
    let mut mn = m % 12.0;
    if mn < 0.0 {
        mn += 12.0;
    }
    if !(-271821.0..=275760.0).contains(&ym) {
        return f64::NAN;
    }
    let yi = ym as i64;
    let mi_count = mn as usize;
    let mut days = days_from_year(yi);
    for (i, md) in MONTH_DAYS.iter().enumerate().take(mi_count) {
        days += md;
        if i == 1 {
            days += days_in_year(yi) - 365;
        }
    }
    let day = days as f64 + dt - 1.0;
    let mut time = h * 3_600_000.0;
    time += mi * 60_000.0;
    time += s * 1000.0;
    time += milli;
    let tv = day * DAY + time;
    if !tv.is_finite() {
        return f64::NAN;
    }
    time_clip(tv)
}

pub(super) fn local_time(parts: [f64; 7]) -> f64 {
    if parts.iter().any(|p| !p.is_finite()) {
        return f64::NAN;
    }
    let first = make_date([parts[0], parts[1], parts[2], 0.0, 0.0, 0.0, 0.0]);
    if first.is_nan() {
        return f64::NAN;
    }
    let f = decompose(first as i64);
    make_date([f[0], f[1], f[2], parts[3], parts[4], parts[5], parts[6]])
}

#[derive(Clone, Copy)]
pub(super) struct Fields {
    pub(super) values: [f64; 8],
    pub(super) offset: f64,
}

pub(super) fn fields(t: f64, zone: Zone) -> Option<Fields> {
    let seconds = zone.offset(t)?;
    let offset = seconds as f64 / 60.0;
    let shifted = time_clip(t + offset * 60000.0);
    let values = match shifted.is_nan() {
        true => [f64::NAN; 8],
        false => decompose(shifted as i64),
    };
    Some(Fields { values, offset })
}

pub(super) fn from_local(parts: [f64; 7], zone: Zone) -> f64 {
    let local = local_time(parts);
    if local.is_nan() {
        return f64::NAN;
    }
    zone.utc(local)
}

fn number(n: f64) -> String {
    let mut out = String::new();
    push_number(&mut out, n);
    out
}

fn push_number(out: &mut String, n: f64) {
    if n.is_nan() {
        out.push_str("NaN");
    } else {
        let _ = write!(out, "{}", n as i64);
    }
}

fn pad(out: &mut String, n: f64, width: usize) {
    let text = number(n);
    for _ in text.len()..width {
        out.push('0');
    }
    out.push_str(&text);
}

fn year(out: &mut String, y: f64) {
    if y < 0.0 {
        out.push('-');
        pad(out, -y, 4);
    } else {
        pad(out, y, 4);
    }
}

fn round(x: f64) -> f64 {
    let r = x.floor();
    if x - r >= 0.5 {
        r + 1.0
    } else {
        r
    }
}

fn gmt(out: &mut String, o: f64) {
    let a = round(o).abs();
    out.push_str("GMT");
    out.push(if o < 0.0 { '-' } else { '+' });
    pad(out, (a / 60.0).floor(), 2);
    pad(out, a % 60.0, 2);
}

fn label(list: &[&'static str], index: f64) -> &'static str {
    match index.is_nan() {
        true => "undefined",
        false => list.get(index as usize).copied().unwrap_or("undefined"),
    }
}

pub(super) fn date_part(out: &mut String, f: &Fields) {
    out.push_str(label(&DAYS, f.values[3]));
    out.push(' ');
    out.push_str(label(&MONTHS, f.values[1]));
    out.push(' ');
    pad(out, f.values[2], 2);
    out.push(' ');
    year(out, f.values[0]);
}

pub(super) fn time_part(out: &mut String, f: &Fields) {
    pad(out, f.values[4], 2);
    out.push(':');
    pad(out, f.values[5], 2);
    out.push(':');
    pad(out, f.values[6], 2);
    out.push(' ');
    gmt(out, f.offset);
}

pub(super) fn localized(f: &Fields, kind: u8) -> String {
    let mut date = String::with_capacity(24);
    pad(&mut date, f.values[1] + 1.0, 2);
    date.push('/');
    pad(&mut date, f.values[2], 2);
    date.push('/');
    year(&mut date, f.values[0]);
    if kind == LOCALE_DATE {
        return date;
    }
    let h = f.values[4];
    let twelve = match h % 12.0 {
        x if x == 0.0 || x.is_nan() => 12.0,
        x => x,
    };
    let mut clock = String::with_capacity(12);
    pad(&mut clock, twelve, 2);
    clock.push(':');
    pad(&mut clock, f.values[5], 2);
    clock.push(':');
    pad(&mut clock, f.values[6], 2);
    clock.push_str(if h < 12.0 { " AM" } else { " PM" });
    if kind == LOCALE_TIME {
        return clock;
    }
    date.push_str(", ");
    date.push_str(&clock);
    date
}

fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn is_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn digits(s: &str) -> usize {
    s.bytes().take_while(u8::is_ascii_digit).count()
}

pub(super) fn iso_date(text: &str) -> bool {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    let year = digits(body);
    if !(4..=6).contains(&year) {
        return false;
    }
    let mut rest = &body[year..];
    for _ in 0..2 {
        if rest.is_empty() {
            return true;
        }
        match rest.strip_prefix('-') {
            Some(tail) if digits(tail) >= 2 => rest = &tail[2..],
            _ => return false,
        }
    }
    rest.is_empty()
}

fn ends_with_offset(text: &str) -> bool {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let digit = |i: usize| bytes[i].is_ascii_digit();
    let sign = |i: usize| bytes[i] == b'+' || bytes[i] == b'-';
    (n >= 5 && sign(n - 5) && (n - 4..n).all(digit))
        || (n >= 6
            && sign(n - 6)
            && digit(n - 5)
            && digit(n - 4)
            && bytes[n - 3] == b':'
            && digit(n - 2)
            && digit(n - 1))
}

pub(super) fn zoned(text: &str) -> bool {
    let trimmed = text.trim_end_matches(is_space);
    if let Some(before) = trimmed.strip_suffix(['Z', 'z']) {
        if before.ends_with(|c: char| c.is_ascii_digit()) {
            return true;
        }
    }
    if ends_with_offset(trimmed) {
        return true;
    }
    if let Some(inner) = trimmed.strip_suffix(')') {
        for (i, c) in inner.char_indices().rev() {
            if is_terminator(c) {
                break;
            }
            if c == '(' && ends_with_offset(inner[..i].trim_end_matches(is_space)) {
                return true;
            }
        }
    }
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        if i > 0 && is_word(chars[i - 1]) {
            continue;
        }
        for word in ["GMT", "UTC", "UT"] {
            let len = word.len();
            if chars.len() >= i + len
                && chars[i..i + len].iter().copied().eq(word.chars())
                && chars.get(i + len).is_none_or(|c| !is_word(*c))
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn iterative(d: i64) -> [f64; 8] {
        let mut h = math_mod(d, 86_400_000);
        let mut days = (d - h) / 86_400_000;
        let ms = h % 1000;
        h = (h - ms) / 1000;
        let s = h % 60;
        h = (h - s) / 60;
        let m = h % 60;
        h = (h - m) / 60;
        let wd = math_mod(days + 4, 7);
        let y = year_from_days(&mut days);
        let mut month = 0;
        while month < 11 {
            let mut md = MONTH_DAYS[month];
            if month == 1 {
                md += days_in_year(y) - 365;
            }
            if days < md {
                break;
            }
            days -= md;
            month += 1;
        }
        [
            y as f64,
            month as f64,
            (days + 1) as f64,
            wd as f64,
            h as f64,
            m as f64,
            s as f64,
            ms as f64,
        ]
    }

    #[test]
    fn decomposition_matches_the_iterative_calendar() {
        let mut at: i64 = -8_640_000_000_000_000 - 50 * 86_400_000;
        let mut step: u64 = 1;
        while at <= 8_640_000_000_000_000 + 50 * 86_400_000 {
            assert_eq!(decompose(at), iterative(at), "{at}");
            step = step
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            at += (step >> 20) as i64 % 400_000_000_000 + 1;
        }
        for day in -1_000_000i64..1_000_000 {
            for ms in [0, 1, 43_200_000, 86_399_999] {
                let at = day * 86_400_000 + ms;
                assert_eq!(decompose(at), iterative(at), "{at}");
            }
        }
    }

    #[test]
    fn fixed_zones_never_change_offset() {
        for tz in chrono_tz::TZ_VARIANTS {
            let zone = Zone::of(tz);
            let Some(fixed) = zone.fixed else { continue };
            for ms in [-8.64e15, -1e13, -2e12, 0.0, 1.7e12, 4.1e12, 8.64e15] {
                let at = modelled(ms).expect("instant in range");
                let seconds = tz.offset_from_utc_datetime(&at).fix().local_minus_utc();
                assert_eq!(seconds, fixed, "{}", tz.name());
            }
        }
    }

    #[test]
    fn zone_patterns_follow_the_script_expressions() {
        assert!(iso_date("2026-01-15"));
        assert!(!iso_date(" 2026-01-15 "));
        assert!(iso_date("+002026-01"));
        assert!(!iso_date("\u{a0}2026\u{feff}"));
        assert!(!iso_date("2026-01-15T10:00"));
        assert!(!iso_date("1234567"));
        assert!(!iso_date("2026-1"));
        assert!(zoned("2026-01-15T10:00:00Z"));
        assert!(zoned("2026-01-15T10:00:00z \n"));
        assert!(!zoned("Z"));
        assert!(zoned("10:00 +01:00 (CET)"));
        assert!(zoned("10:00 -0330"));
        assert!(zoned("10:00 +0100 (a (b) c)"));
        assert!(!zoned("10:00 +0100 (a\nb)"));
        assert!(zoned("Jan 1 2026 UT"));
        assert!(zoned("UTC"));
        assert!(!zoned("GMTx"));
        assert!(!zoned("xUTC"));
        assert!(zoned("a-UTC"));
    }
}
