use crate::lane::mask::LaneSet;
use rust_decimal::Decimal;

pub(crate) struct Scaled;

impl Scaled {
    pub(crate) const POW10: [i64; 19] = [
        1,
        10,
        100,
        1_000,
        10_000,
        100_000,
        1_000_000,
        10_000_000,
        100_000_000,
        1_000_000_000,
        10_000_000_000,
        100_000_000_000,
        1_000_000_000_000,
        10_000_000_000_000,
        100_000_000_000_000,
        1_000_000_000_000_000,
        10_000_000_000_000_000,
        100_000_000_000_000_000,
        1_000_000_000_000_000_000,
    ];

    #[inline]
    pub(crate) fn decimal(m: i64, s: u8) -> Decimal {
        Decimal::new(m, s as u32)
    }

    #[inline]
    fn signed(input: i64, result: (i64, u8)) -> Option<(i64, u8)> {
        (result.0 != 0 || input >= 0).then_some(result)
    }

    pub(crate) fn divide(a: (i64, u8), b: (i64, u8)) -> Option<(i64, u8)> {
        let ((m1, s1), (m2, s2)) = (a, b);
        if m2 == 0 {
            return None;
        }
        if m1 == 0 {
            return Some((0, 0));
        }
        if m2.unsigned_abs() > u32::MAX as u64 {
            let (n, d) = (m1 as i128, m2 as i128);
            if n % d != 0 {
                return None;
            }
            let q = n / d;
            return match s1 >= s2 {
                true => i64::try_from(q).ok().map(|q| (q, s1 - s2)),
                false => {
                    let q = q.checked_mul(10i128.checked_pow((s2 - s1) as u32)?)?;
                    i64::try_from(q).ok().map(|q| (q, 0))
                }
            };
        }
        Self::terminating(m1, s1, m2, s2)
    }

    fn terminating(m1: i64, s1: u8, m2: i64, s2: u8) -> Option<(i64, u8)> {
        const LOW64: u128 = 1 << 64;
        const LIMIT: u128 = 1 << 96;
        let d = u32::try_from(m2.unsigned_abs()).ok()? as u64;
        let negative = (m1 < 0) != (m2 < 0);
        let n = m1.unsigned_abs();
        let first = n % d;
        if first != 0 {
            let mut odd = d >> d.trailing_zeros();
            while odd % 5 == 0 {
                odd /= 5;
            }
            if odd > 1 && n % odd != 0 {
                return None;
            }
        }
        let mut q = (n / d) as u128;
        let mut r = first;
        let mut scale = s1 as i32 - s2 as i32;
        loop {
            let power = match r {
                0 if scale >= 0 => break,
                0 => 9.min(-scale),
                _ if scale == 28 => {
                    let twice = r << 1;
                    if twice > d || (twice == d && q & 1 == 1) {
                        q += 1;
                        if q >= LIMIT {
                            return None;
                        }
                    }
                    break;
                }
                _ if q >= LOW64 => return None,
                _ if scale > 19 => 28 - scale,
                _ => 9,
            };
            scale += power;
            let factor = Self::POW10[power as usize] as u64;
            q = q.checked_mul(factor as u128).filter(|q| *q < LIMIT)?;
            let scaled = r * factor;
            q += (scaled / d) as u128;
            r = scaled % d;
            if q >= LIMIT {
                return None;
            }
        }
        if first != 0 {
            Self::unscale(&mut q, &mut scale);
        }
        let q = i64::try_from(q).ok()?;
        Some((if negative { -q } else { q }, scale as u8))
    }

    #[inline]
    fn unscale(q: &mut u128, scale: &mut i32) {
        while *q & 0xFFFF_FFFF == 0 && *scale >= 8 && Self::strip(q, 100_000_000) {
            *scale -= 8;
        }
        for (bits, step, power) in [(0xF, 4, 10_000u64), (0x3, 2, 100), (0x1, 1, 10)] {
            if *q & bits == 0 && *scale >= step && Self::strip(q, power) {
                *scale -= step;
            }
        }
    }

    pub(crate) fn power(divisor: (i64, u8)) -> Option<u32> {
        let m = divisor.0;
        (m > 0).then_some(())?;
        (0..=9u32).find(|&k| Self::POW10[k as usize] == m)
    }

    #[inline]
    pub(crate) fn divide_power(a: (i64, u8), k: u32, s2: u8) -> Option<(i64, u8)> {
        let (m1, s1) = a;
        if m1 == 0 {
            return Some((0, 0));
        }
        let scale = s1 as i32 - s2 as i32;
        if !(0..=19).contains(&scale) {
            return Self::divide(a, (Self::POW10[k as usize], s2));
        }
        if k >= 2 && m1 % 10 != 0 && m1 != i64::MIN {
            return Some((m1, (scale + k as i32) as u8));
        }
        let (mut m, mut t) = (m1.unsigned_abs(), 0u32);
        while t < k && m % 10 == 0 {
            m /= 10;
            t += 1;
        }
        let (q, scale) = match (t == k, 9 - k + t == 8, m.trailing_zeros() >= 24) {
            (true, _, _) => (m, scale),
            (false, true, true) => (m, scale + 1),
            (false, true, false) => (m.checked_mul(10)?, scale + 2),
            (false, false, _) => (m, scale + (k - t) as i32),
        };
        let q = i64::try_from(q).ok()?;
        Some((if m1 < 0 { -q } else { q }, scale as u8))
    }

    #[inline]
    fn strip(q: &mut u128, power: u64) -> bool {
        match u64::try_from(*q) {
            Ok(small) if small % power == 0 => {
                *q = (small / power) as u128;
                true
            }
            Ok(_) => false,
            Err(_) if *q % power as u128 == 0 => {
                *q /= power as u128;
                true
            }
            Err(_) => false,
        }
    }


    pub(crate) fn remainder(a: (i64, u8), b: (i64, u8)) -> Option<(i64, u8)> {
        let ((m1, s1), (m2, s2)) = (a, b);
        if m1 == 0 || m2 == 0 {
            return None;
        }
        let top = s1.max(s2);
        let x = Self::rescale(m1, s1, top)?;
        let y = Self::rescale(m2, s2, top)?;
        if x.unsigned_abs() < y.unsigned_abs() {
            return Some((m1, s1));
        }
        let r = x.checked_rem(y)?;
        (r != 0).then_some((r, top))
    }

    pub(crate) fn abs(m: i64, s: u8) -> Option<(i64, u8)> {
        Some((m.checked_abs()?, s))
    }

    pub(crate) fn trunc(m: i64, s: u8, places: u8) -> Option<(i64, u8)> {
        let r = match places >= s {
            true => (
                m.checked_mul(*Self::POW10.get((places - s) as usize)?)?,
                places,
            ),
            false => (m / Self::POW10.get((s - places) as usize)?, places),
        };
        Self::signed(m, r)
    }

    pub(crate) fn round(m: i64, s: u8, places: u8) -> Option<(i64, u8)> {
        if places >= s {
            return Some((m, s));
        }
        let f = *Self::POW10.get((s - places) as usize)?;
        let (q, r) = (m / f, m % f);
        let up = r.unsigned_abs() >= (f as u64).div_ceil(2);
        let q = match (up, m < 0) {
            (true, true) => q.checked_sub(1)?,
            (true, false) => q.checked_add(1)?,
            _ => q,
        };
        Self::signed(m, (q, places))
    }

    pub(crate) fn floor(m: i64, s: u8) -> Option<(i64, u8)> {
        let f = *Self::POW10.get(s as usize)?;
        let (q, r) = (m / f, m % f);
        Self::signed(m, (if r < 0 { q.checked_sub(1)? } else { q }, 0))
    }

    pub(crate) fn ceil(m: i64, s: u8) -> Option<(i64, u8)> {
        let f = *Self::POW10.get(s as usize)?;
        let (q, r) = (m / f, m % f);
        Self::signed(m, (if r > 0 { q.checked_add(1)? } else { q }, 0))
    }

    pub(crate) fn write(m: i64, s: u8, out: &mut String) {
        let mut digits = [0u8; 48];
        let mut n = m.unsigned_abs();
        let mut len = 0usize;
        while n > 0 || len <= s as usize {
            digits[len] = b'0' + (n % 10) as u8;
            n /= 10;
            len += 1;
        }
        if m < 0 {
            out.push('-');
        }
        for i in (0..len).rev() {
            out.push(digits[i] as char);
            if i == s as usize && s > 0 {
                out.push('.');
            }
        }
    }

    #[inline]
    pub(crate) fn parts(d: &Decimal) -> Option<(i64, u8)> {
        let bytes = d.serialize();
        let flags = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let lo = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as u64;
        let mid = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as u64;
        let hi = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        let negative = flags & 0x8000_0000 != 0;
        let magnitude = mid << 32 | lo;
        let limit = i64::MAX as u64 + u64::from(negative);
        if hi != 0 || magnitude > limit || (negative && magnitude == 0) {
            return None;
        }
        let m = match negative {
            true => (magnitude as i64).wrapping_neg(),
            false => magnitude as i64,
        };
        Some((m, ((flags >> 16) & 0xFF) as u8))
    }

    #[inline]
    pub(crate) fn rescale(m: i64, from: u8, to: u8) -> Option<i64> {
        m.checked_mul(*Self::POW10.get((to - from) as usize)?)
    }

    #[inline]
    pub(crate) fn add(a: (i64, u8), b: (i64, u8), subtract: bool) -> Option<(i64, u8)> {
        match (a.0 == 0, b.0 == 0) {
            (true, _) => match subtract {
                true => Some((b.0.checked_neg()?, b.1)),
                false => Some(b),
            },
            (false, true) => Some(a),
            (false, false) => {
                let s = a.1.max(b.1);
                let (x, y) = (Self::rescale(a.0, a.1, s)?, Self::rescale(b.0, b.1, s)?);
                let m = match subtract {
                    true => x.checked_sub(y)?,
                    false => x.checked_add(y)?,
                };
                Some((m, s))
            }
        }
    }

    #[inline]
    pub(crate) fn multiply(a: (i64, u8), b: (i64, u8)) -> Option<(i64, u8)> {
        if a.0 == 0 || b.0 == 0 {
            return Some((0, 0));
        }
        let s = a.1 + b.1;
        (s <= 28).then_some(())?;
        Some((a.0.checked_mul(b.0)?, s))
    }

    #[inline]
    pub(crate) fn compare(a: (i64, u8), b: (i64, u8)) -> Option<std::cmp::Ordering> {
        if a.1 == b.1 {
            return Some(a.0.cmp(&b.0));
        }
        let s = a.1.max(b.1);
        let up = |m: i64, from: u8| -> Option<i128> {
            (m as i128).checked_mul(10i128.checked_pow((s - from) as u32)?)
        };
        Some(up(a.0, a.1)?.cmp(&up(b.0, b.1)?))
    }
}

pub(crate) struct Kernel;

impl Kernel {
    #[inline]
    pub(crate) fn add(
        a: (&[i64], u8),
        b: (&[i64], u8),
        subtract: bool,
        m: &mut [i64],
        s: &mut [u8],
    ) -> bool {
        let ((xa, sa), (xb, sb)) = (a, b);
        if sa == sb {
            let mut flag = 0i64;
            for ((o, x), y) in m.iter_mut().zip(xa).zip(xb) {
                let r = match subtract {
                    true => x.wrapping_sub(*y),
                    false => x.wrapping_add(*y),
                };
                flag |= match subtract {
                    true => (x ^ y) & (x ^ r),
                    false => (x ^ r) & (y ^ r),
                };
                *o = r;
            }
            s.fill(sa);
            return flag < 0;
        }
        let (top, up_a) = (sa.max(sb), sa < sb);
        let Some(f) = Scaled::POW10.get((top - sa.min(sb)) as usize).copied() else {
            return true;
        };
        match (subtract, up_a) {
            (true, true) => Self::mixed::<true, true>(xa, xb, f, (sa, sb, top), m, s),
            (true, false) => Self::mixed::<true, false>(xa, xb, f, (sa, sb, top), m, s),
            (false, true) => Self::mixed::<false, true>(xa, xb, f, (sa, sb, top), m, s),
            (false, false) => Self::mixed::<false, false>(xa, xb, f, (sa, sb, top), m, s),
        }
    }

    #[inline(always)]
    fn mixed<const SUB: bool, const UP_A: bool>(
        xa: &[i64],
        xb: &[i64],
        f: i64,
        (sa, sb, top): (u8, u8, u8),
        m: &mut [i64],
        s: &mut [u8],
    ) -> bool {
        let mut flag = false;
        for (((o, sc), x), y) in m.iter_mut().zip(s.iter_mut()).zip(xa).zip(xb) {
            let (x, y) = (*x, *y);
            let ((p, o1), (q, o2)) = match UP_A {
                true => (x.overflowing_mul(f), (y, false)),
                false => ((x, false), y.overflowing_mul(f)),
            };
            let (r, o3) = match SUB {
                true => p.overflowing_sub(q),
                false => p.overflowing_add(q),
            };
            let negated = if SUB { y.wrapping_neg() } else { y };
            let (zx, zy) = (x == 0, y == 0);
            *o = if zx {
                negated
            } else if zy {
                x
            } else {
                r
            };
            *sc = if zx {
                sb
            } else if zy {
                sa
            } else {
                top
            };
            flag |= (!zx & !zy & (o1 | o2 | o3)) | (zx & SUB & (y == i64::MIN));
        }
        flag
    }

    #[inline]
    pub(crate) fn multiply(a: (&[i64], u8), b: (&[i64], u8), m: &mut [i64], s: &mut [u8]) -> bool {
        let ((xa, sa), (xb, sb)) = (a, b);
        let top = sa + sb;
        if top > 28 {
            return true;
        }
        let mut flag = false;
        for (((o, sc), x), y) in m.iter_mut().zip(s.iter_mut()).zip(xa).zip(xb) {
            let (r, of) = x.overflowing_mul(*y);
            flag |= of;
            *o = r;
            *sc = if r == 0 { 0 } else { top };
        }
        flag
    }

    #[inline]
    pub(crate) fn add_mixed(a: (&[i64], &[u8]), b: (&[i64], &[u8]), subtract: bool, m: &mut [i64], s: &mut [u8]) -> bool {
        let ((xa, sa), (xb, sb)) = (a, b);
        let mut flag = false;
        for (((((o, sc), x), y), p), q) in m.iter_mut().zip(s.iter_mut()).zip(xa).zip(xb).zip(sa).zip(sb) {
            let top = (*p).max(*q);
            let (ka, kb) = ((top - p) as usize, (top - q) as usize);
            let fa = Scaled::POW10[ka.min(18)] as i128;
            let fb = Scaled::POW10[kb.min(18)] as i128;
            let (wx, wy) = (*x as i128 * fa, *y as i128 * fb);
            let r = match subtract {
                true => wx - wy,
                false => wx + wy,
            };
            let both = *x != 0 && *y != 0;
            let outside = |v: i128| v < i64::MIN as i128 || v > i64::MAX as i128;
            let general = outside(r) || outside(wx) || outside(wy) || ka > 18 || kb > 18;
            let negate = *x == 0 && subtract && *y == i64::MIN;
            flag |= (both && general) | negate;
            let (rm, rs) = match (*x == 0, *y == 0) {
                (true, _) => (if subtract { y.wrapping_neg() } else { *y }, *q),
                (false, true) => (*x, *p),
                (false, false) => (r as i64, top),
            };
            *o = rm;
            *sc = rs;
        }
        flag
    }

    pub(crate) fn multiply_mixed(a: (&[i64], &[u8]), b: (&[i64], &[u8]), m: &mut [i64], s: &mut [u8]) -> bool {
        let ((xa, sa), (xb, sb)) = (a, b);
        let mut flag = false;
        for (((((o, sc), x), y), p), q) in m.iter_mut().zip(s.iter_mut()).zip(xa).zip(xb).zip(sa).zip(sb) {
            let (r, of) = x.overflowing_mul(*y);
            let top = p + q;
            flag |= of | (top > 28);
            *o = r;
            *sc = if r == 0 { 0 } else { top };
        }
        flag
    }

    #[inline]
    pub(crate) fn threshold<M: LaneSet>(
        values: &[i64],
        k: i64,
        hit: impl Fn(std::cmp::Ordering) -> bool,
    ) -> M {
        let mut mask = M::none(values.len());
        for (w, chunk) in values.chunks(64).enumerate() {
            let mut word = 0u64;
            for (j, eight) in chunk.chunks(8).enumerate() {
                let mut bytes = [0u8; 8];
                for (b, x) in bytes.iter_mut().zip(eight) {
                    *b = hit(x.cmp(&k)) as u8;
                }
                let packed = u64::from_le_bytes(bytes).wrapping_mul(0x0102_0408_1020_4080) >> 56;
                word |= packed << (j * 8);
            }
            mask.set_word(w, word);
        }
        mask
    }

    pub(crate) fn compare<M: LaneSet>(
        a: (&[i64], u8),
        b: (&[i64], u8),
        hit: impl Fn(std::cmp::Ordering) -> bool,
    ) -> Option<M> {
        let ((xa, sa), (xb, sb)) = (a, b);
        let mut mask = M::none(xa.len());
        let (fa, fb) = match sa == sb {
            true => (1, 1),
            false => {
                let top = sa.max(sb);
                (
                    *Scaled::POW10.get((top - sa) as usize)?,
                    *Scaled::POW10.get((top - sb) as usize)?,
                )
            }
        };
        for (w, (ca, cb)) in xa.chunks(64).zip(xb.chunks(64)).enumerate() {
            let mut word = 0u64;
            match sa == sb {
                true => {
                    for (i, (x, y)) in ca.iter().zip(cb).enumerate() {
                        word |= (hit(x.cmp(y)) as u64) << i;
                    }
                }
                false => {
                    for (i, (x, y)) in ca.iter().zip(cb).enumerate() {
                        word |= (hit((*x as i128 * fa as i128).cmp(&(*y as i128 * fb as i128)))
                            as u64)
                            << i;
                    }
                }
            }
            mask.set_word(w, word);
        }
        Some(mask)
    }
}

#[cfg(test)]
mod tests {
    use super::{Kernel, Scaled};
    use rust_decimal::Decimal;
    use std::cmp::Ordering;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }

        fn mantissa(&mut self) -> i64 {
            match self.next(8) {
                0 => 0,
                1 => i64::MAX - self.next(3) as i64,
                2 => i64::MIN + self.next(3) as i64,
                3 => (self.next(1 << 62) as i64) * if self.next(2) == 0 { 1 } else { -1 },
                _ => self.next(20_000) as i64 - 10_000,
            }
        }

        fn scale(&mut self) -> u8 {
            match self.next(6) {
                0 => self.next(29) as u8,
                _ => self.next(4) as u8,
            }
        }
    }

    fn exact(m: i64, s: u8, d: Decimal) -> bool {
        Scaled::parts(&d).is_some_and(|(dm, ds)| dm == m && ds == s)
            && Scaled::decimal(m, s).to_string() == d.to_string()
    }

    #[test]
    fn rounding_matches_rust_decimal() {
        use rust_decimal::RoundingStrategy;
        let mut rng = Rng(0xD1B54A32D192ED03);
        let exact = |r: Option<(i64, u8)>, d: Decimal, what: &str| {
            if let Some((m, s)) = r {
                assert_eq!(Scaled::decimal(m, s).to_string(), d.to_string(), "{what}");
                assert_eq!(Scaled::parts(&d), Some((m, s)), "{what} parts");
            }
        };
        for _ in 0..300_000 {
            let (m, s) = (rng.mantissa(), rng.scale());
            let x = Decimal::new(m, s as u32);
            let places = rng.next(6) as u8;
            if !(x.is_zero() && x.is_sign_negative()) {
                exact(Scaled::abs(m, s), x.abs(), &format!("abs {x}"));
                exact(
                    Scaled::trunc(m, s, places),
                    x.trunc_with_scale(places as u32),
                    &format!("trunc {x} {places}"),
                );
                exact(
                    Scaled::round(m, s, places),
                    x.round_dp_with_strategy(places as u32, RoundingStrategy::MidpointAwayFromZero),
                    &format!("round {x} {places}"),
                );
                exact(Scaled::floor(m, s), x.floor(), &format!("floor {x}"));
                exact(Scaled::ceil(m, s), x.ceil(), &format!("ceil {x}"));
            }
        }
    }

    #[test]
    fn divide_matches_rust_decimal() {
        let mut rng = Rng(0x94D049BB133111EB);
        let mut checked = 0usize;
        for _ in 0..500_000 {
            let (m1, s1) = (rng.mantissa() / (1 + rng.next(1000) as i64), rng.scale());
            let (m2, s2) = (
                match rng.next(3) {
                    0 => rng.next(20) as i64 - 10,
                    1 => [2, 4, 5, 8, 10, 16, 25, 40, 100, 125][rng.next(10) as usize],
                    _ => rng.mantissa() / (1 + rng.next(100_000) as i64),
                },
                rng.scale(),
            );
            let (x, y) = (Decimal::new(m1, s1 as u32), Decimal::new(m2, s2 as u32));
            if let Some((m, s)) = Scaled::divide((m1, s1), (m2, s2)) {
                let expected = x.checked_div(y).unwrap_or_default();
                assert_eq!(
                    Scaled::decimal(m, s).to_string(),
                    expected.to_string(),
                    "{x} / {y}"
                );
                checked += 1;
            }
        }
        assert!(checked > 10_000, "{checked}");
    }

    #[test]
    fn remainder_matches_rust_decimal() {
        let mut rng = Rng(0xBF58476D1CE4E5B9);
        let mut checked = 0usize;
        for _ in 0..500_000 {
            let (m1, s1) = (rng.mantissa() / (1 + rng.next(1000) as i64), rng.scale());
            let (m2, s2) = (rng.next(200) as i64 - 100, rng.scale());
            let (x, y) = (Decimal::new(m1, s1 as u32), Decimal::new(m2, s2 as u32));
            if let Some((m, s)) = Scaled::remainder((m1, s1), (m2, s2)) {
                let expected = x.checked_rem(y).unwrap_or_default();
                assert_eq!(
                    Scaled::decimal(m, s).to_string(),
                    expected.to_string(),
                    "{x} % {y}"
                );
                checked += 1;
            }
        }
        assert!(checked > 10_000, "{checked}");
    }

    #[test]
    fn write_matches_decimal_display() {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let mut out = String::new();
        for _ in 0..200_000 {
            let (m, s) = (rng.mantissa(), rng.scale());
            out.clear();
            Scaled::write(m, s, &mut out);
            assert_eq!(out, Decimal::new(m, s as u32).to_string(), "{m}e-{s}");
        }
    }

    #[test]
    fn division_matches_rust_decimal() {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let mut hits = 0usize;
        for round in 0..2_000_000 {
            let (sa, sb) = (rng.scale(), rng.scale());
            let a = rng.mantissa();
            let b = match round % 3 {
                0 => [1, 2, 4, 5, 8, 10, 16, 20, 25, 32, 40, 50, 64, 80, 100, 125, 1000, 3, 6, 7, 12, 1024, 3125][rng.next(23) as usize]
                    * if rng.next(2) == 0 { 1 } else { -1 },
                _ => rng.mantissa(),
            };
            let (x, y) = (Decimal::new(a, sa as u32), Decimal::new(b, sb as u32));
            if let Some((m, s)) = Scaled::divide((a, sa), (b, sb)) {
                let expected = x.checked_div(y);
                assert!(
                    expected.is_some_and(|e| exact(m, s, e)),
                    "divide {x} / {y} -> {m}e-{s} vs {expected:?}"
                );
                hits += 1;
            }
        }
        assert!(hits > 500_000, "{hits}");
    }

    #[test]
    fn power_division_matches_general_division() {
        let mut rng = Rng(0x51A3_77E1_9C2B_0D45);
        for round in 0..2_000_000 {
            let (sa, sb) = (rng.scale(), rng.scale());
            let a = match round % 6 {
                0 => rng.mantissa() % 1_000_000,
                1 => (rng.mantissa() % 100_000) * Scaled::POW10[rng.next(8) as usize],
                2 => (rng.mantissa() % 4096) << (20 + rng.next(12)),
                3 => ((rng.mantissa() % 4096) << 24) * Scaled::POW10[rng.next(4) as usize],
                _ => rng.mantissa(),
            };
            let k = rng.next(10) as u32;
            let b = Scaled::POW10[k as usize];
            assert_eq!(Scaled::power((b, sb)), Some(k));
            assert_eq!(Scaled::divide_power((a, sa), k, sb), Scaled::divide((a, sa), (b, sb)), "{a}e-{sa} / {b}e-{sb}");
        }
    }

    #[test]
    fn mixed_multiply_matches_scalar_multiply() {
        let mut rng = Rng(0x0DDB_A11C_AFE5_1234);
        for _ in 0..50_000 {
            let a: Vec<i64> = (0..64).map(|_| rng.mantissa()).collect();
            let b: Vec<i64> = (0..64).map(|_| rng.mantissa()).collect();
            let sa: Vec<u8> = (0..64).map(|_| rng.scale()).collect();
            let sb: Vec<u8> = (0..64).map(|_| rng.scale()).collect();
            let (mut m, mut s) = (vec![0i64; 64], vec![0u8; 64]);
            if Kernel::multiply_mixed((&a, &sa), (&b, &sb), &mut m, &mut s) {
                continue;
            }
            for i in 0..64 {
                assert_eq!(Scaled::multiply((a[i], sa[i]), (b[i], sb[i])), Some((m[i], s[i])));
            }
        }
    }

    #[test]
    fn mixed_add_matches_scalar_add() {
        let mut rng = Rng(0x7A11_5EED_0BAD_F00D);
        let mut checked = 0usize;
        for round in 0..50_000 {
            let small = round % 2 == 0;
            let pick = |rng: &mut Rng| match (small, rng.next(5)) {
                (_, 0) => 0,
                (true, _) => rng.mantissa() % 100_000,
                (false, _) => rng.mantissa(),
            };
            let width = 8;
            let a: Vec<i64> = (0..width).map(|_| pick(&mut rng)).collect();
            let b: Vec<i64> = (0..width).map(|_| pick(&mut rng)).collect();
            let sa: Vec<u8> = (0..width).map(|_| if small { rng.scale() % 8 } else { rng.scale() }).collect();
            let sb: Vec<u8> = (0..width).map(|_| if small { rng.scale() % 8 } else { rng.scale() }).collect();
            let (mut m, mut s) = (vec![0i64; width], vec![0u8; width]);
            for subtract in [false, true] {
                if Kernel::add_mixed((&a, &sa), (&b, &sb), subtract, &mut m, &mut s) {
                    continue;
                }
                checked += 1;
                for i in 0..width {
                    assert_eq!(Scaled::add((a[i], sa[i]), (b[i], sb[i]), subtract), Some((m[i], s[i])));
                }
            }
        }
        assert!(checked > 10_000, "{checked}");
    }

    #[test]
    fn kernels_match_rust_decimal() {
        let mut rng = Rng(0x2545F4914F6CDD1D);
        let mut checked = 0usize;
        for _ in 0..20_000 {
            let (sa, sb) = (rng.scale(), rng.scale());
            let a: Vec<i64> = (0..64).map(|_| rng.mantissa()).collect();
            let b: Vec<i64> = (0..64).map(|_| rng.mantissa()).collect();
            let (mut m, mut s) = (vec![0i64; 64], vec![0u8; 64]);
            for op in 0..3 {
                let overflow = match op {
                    0 => Kernel::add((&a, sa), (&b, sb), false, &mut m, &mut s),
                    1 => Kernel::add((&a, sa), (&b, sb), true, &mut m, &mut s),
                    _ => Kernel::multiply((&a, sa), (&b, sb), &mut m, &mut s),
                };
                for i in 0..64 {
                    let (x, y) = (Decimal::new(a[i], sa as u32), Decimal::new(b[i], sb as u32));
                    let expected = match op {
                        0 => x.checked_add(y),
                        1 => x.checked_sub(y),
                        _ => x.checked_mul(y),
                    };
                    let lane = match op {
                        0 => Scaled::add((a[i], sa), (b[i], sb), false),
                        1 => Scaled::add((a[i], sa), (b[i], sb), true),
                        _ => Scaled::multiply((a[i], sa), (b[i], sb)),
                    };
                    if let (Some((lm, ls)), Some(e)) = (lane, expected) {
                        assert!(
                            exact(lm, ls, e),
                            "lane op {op}: {x} {y} -> {lm}e-{ls} vs {e}"
                        );
                        checked += 1;
                    }
                    if !overflow {
                        let e = expected.unwrap_or_default();
                        assert!(
                            expected.is_some() && exact(m[i], s[i], e),
                            "kernel op {op}: {x} {y} -> {}e-{} vs {e}",
                            m[i],
                            s[i]
                        );
                    }
                }
            }
            let hit = |o: Ordering| o.is_lt();
            if let Some(word) = Kernel::compare::<crate::lane::mask::Mask>((&a, sa), (&b, sb), hit)
            {
                for i in 0..64 {
                    let (x, y) = (Decimal::new(a[i], sa as u32), Decimal::new(b[i], sb as u32));
                    assert_eq!(word.get(i), x < y, "compare {x} {y}");
                    assert_eq!(
                        Scaled::compare((a[i], sa), (b[i], sb)).map(|o| o.is_lt()),
                        Some(x < y)
                    );
                }
            }
        }
        assert!(checked > 1_000_000);
    }
}
