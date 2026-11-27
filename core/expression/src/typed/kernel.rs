use crate::lane::Bits;
use crate::typed::hir::Cmp;
use crate::typed::num::Dec;
use std::cmp::Ordering;

pub(crate) struct K;

pub(crate) trait Lane:
    Copy
    + std::ops::BitAnd<Output = Self>
    + std::ops::BitOr<Output = Self>
    + std::ops::Not<Output = Self>
{
    fn splat(bit: u64) -> Self;
}

impl Lane for i64 {
    #[inline(always)]
    fn splat(bit: u64) -> Self {
        (bit as i64).wrapping_neg()
    }
}

impl Lane for i32 {
    #[inline(always)]
    fn splat(bit: u64) -> Self {
        (bit as i32).wrapping_neg()
    }
}

impl Lane for u8 {
    #[inline(always)]
    fn splat(bit: u64) -> Self {
        (bit as u8).wrapping_neg()
    }
}

impl K {
    const MAGIC: u64 = 0x0102_0408_1020_4080;
    pub(crate) const P10: [i64; 32] = {
        let mut t = [0i64; 32];
        let mut k = 0;
        while k < Dec::POW10.len() {
            t[k] = Dec::POW10[k];
            k += 1;
        }
        t
    };

    #[inline(always)]
    pub(crate) fn bits(bytes: &[u8; 64]) -> u64 {
        let mut m = 0u64;
        for (k, c) in bytes.as_chunks::<8>().0.iter().enumerate() {
            m |= (u64::from_le_bytes(*c).wrapping_mul(Self::MAGIC) >> 56) << (k * 8);
        }
        m
    }

    #[inline(always)]
    pub(crate) fn pack1<A: Copy>(a: &[A], out: &mut [u64], f: impl Fn(A) -> bool) {
        let (full, rem) = a.as_chunks::<64>();
        let mut o = out.iter_mut();
        for (c, w) in full.iter().zip(&mut o) {
            let mut bytes = [0u8; 64];
            for (b, &x) in bytes.iter_mut().zip(c) {
                *b = f(x) as u8;
            }
            *w = Self::bits(&bytes);
        }
        if let Some(w) = o.next() {
            let mut m = 0u64;
            for (j, &x) in rem.iter().enumerate() {
                m |= (f(x) as u64) << j;
            }
            *w = m;
        }
    }

    #[inline(always)]
    pub(crate) fn pack2<A: Copy, B: Copy>(
        a: &[A],
        b: &[B],
        out: &mut [u64],
        f: impl Fn(A, B) -> bool,
    ) {
        let (fa, ra) = a.as_chunks::<64>();
        let (fb, rb) = b.as_chunks::<64>();
        let mut o = out.iter_mut();
        for ((x, y), w) in fa.iter().zip(fb).zip(&mut o) {
            let mut bytes = [0u8; 64];
            for ((b, &x), &y) in bytes.iter_mut().zip(x).zip(y) {
                *b = f(x, y) as u8;
            }
            *w = Self::bits(&bytes);
        }
        if let Some(w) = o.next() {
            let mut m = 0u64;
            for (j, (&x, &y)) in ra.iter().zip(rb).enumerate() {
                m |= (f(x, y) as u64) << j;
            }
            *w = m;
        }
    }

    #[inline(always)]
    pub(crate) fn ovf1(
        a: &[i64],
        out: &mut [i64],
        fault: &mut [u64],
        f: impl Fn(i64) -> (i64, bool),
    ) {
        let (fa, ra) = a.as_chunks::<64>();
        let (fo, ro) = out.as_chunks_mut::<64>();
        let mut fw = fault.iter_mut();
        for ((x, o), w) in fa.iter().zip(fo.iter_mut()).zip(&mut fw) {
            let mut bytes = [0u8; 64];
            for ((b, o), &x) in bytes.iter_mut().zip(o.iter_mut()).zip(x) {
                let (r, v) = f(x);
                *o = r;
                *b = v as u8;
            }
            *w |= Self::bits(&bytes);
        }
        if let Some(w) = fw.next() {
            let mut m = 0u64;
            for (j, (o, &x)) in ro.iter_mut().zip(ra).enumerate() {
                let (r, v) = f(x);
                *o = r;
                m |= (v as u64) << j;
            }
            *w |= m;
        }
    }

    #[inline(always)]
    pub(crate) fn ovf2(
        a: &[i64],
        b: &[i64],
        out: &mut [i64],
        fault: &mut [u64],
        f: impl Fn(i64, i64) -> (i64, bool),
    ) {
        let (fa, ra) = a.as_chunks::<64>();
        let (fb, rb) = b.as_chunks::<64>();
        let (fo, ro) = out.as_chunks_mut::<64>();
        let mut fw = fault.iter_mut();
        for (((x, y), o), w) in fa.iter().zip(fb).zip(fo.iter_mut()).zip(&mut fw) {
            let mut bytes = [0u8; 64];
            for (((b, o), &x), &y) in bytes.iter_mut().zip(o.iter_mut()).zip(x).zip(y) {
                let (r, v) = f(x, y);
                *o = r;
                *b = v as u8;
            }
            *w |= Self::bits(&bytes);
        }
        if let Some(w) = fw.next() {
            let mut m = 0u64;
            for (j, ((o, &x), &y)) in ro.iter_mut().zip(ra).zip(rb).enumerate() {
                let (r, v) = f(x, y);
                *o = r;
                m |= (v as u64) << j;
            }
            *w |= m;
        }
    }

    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dyn2(
        a: &[i64],
        b: &[i64],
        sa: &[u8],
        sb: &[u8],
        out: &mut [i64],
        scale: &mut [u8],
        fault: &mut [u64],
        f: impl Fn(i64, i64, u8, u8) -> (i64, u8, bool),
    ) {
        let (fa, ra) = a.as_chunks::<64>();
        let (fb, rb) = b.as_chunks::<64>();
        let (ga, qa) = sa.as_chunks::<64>();
        let (gb, qb) = sb.as_chunks::<64>();
        let (fo, ro) = out.as_chunks_mut::<64>();
        let (go, qo) = scale.as_chunks_mut::<64>();
        let mut fw = fault.iter_mut();
        for ((((((x, y), p), q), o), s), w) in fa
            .iter()
            .zip(fb)
            .zip(ga)
            .zip(gb)
            .zip(fo.iter_mut())
            .zip(go.iter_mut())
            .zip(&mut fw)
        {
            let mut bytes = [0u8; 64];
            for ((((((b, o), s), &x), &y), &p), &q) in bytes
                .iter_mut()
                .zip(o.iter_mut())
                .zip(s.iter_mut())
                .zip(x)
                .zip(y)
                .zip(p)
                .zip(q)
            {
                let (r, t, v) = f(x, y, p, q);
                *o = r;
                *s = t;
                *b = v as u8;
            }
            *w |= Self::bits(&bytes);
        }
        if let Some(w) = fw.next() {
            let mut m = 0u64;
            for (j, (((((o, s), &x), &y), &p), &q)) in ro
                .iter_mut()
                .zip(qo.iter_mut())
                .zip(ra)
                .zip(rb)
                .zip(qa)
                .zip(qb)
                .enumerate()
            {
                let (r, t, v) = f(x, y, p, q);
                *o = r;
                *s = t;
                m |= (v as u64) << j;
            }
            *w |= m;
        }
    }

    #[inline(always)]
    pub(crate) fn add(sub: bool, x: i64, y: i64, p: u8, q: u8) -> (i64, u8, bool) {
        let s = p.max(q);
        let fa = Self::P10[((s - p) & 31) as usize];
        let fb = Self::P10[((s - q) & 31) as usize];
        let (xa, o1) = x.overflowing_mul(fa);
        let (xb, o2) = y.overflowing_mul(fb);
        let (r, o3) = if sub {
            xa.overflowing_sub(xb)
        } else {
            xa.overflowing_add(xb)
        };
        let (zx, zy) = (x == 0, y == 0);
        let ny = if sub { y.wrapping_neg() } else { y };
        let m = if zx {
            ny
        } else if zy {
            x
        } else {
            r
        };
        let t = if zx {
            q
        } else if zy {
            p
        } else {
            s
        };
        let o = (!zx & !zy & (o1 | o2 | o3 | (fa == 0) | (fb == 0))) | (zx & sub & (y == i64::MIN));
        (m, t, o)
    }

    #[inline(always)]
    pub(crate) fn mul(x: i64, y: i64, p: u8, q: u8) -> (i64, u8, bool) {
        let (r, of) = x.overflowing_mul(y);
        let s = p.wrapping_add(q);
        let zero = (x == 0) | (y == 0);
        let m = if zero { 0 } else { r };
        let t = if zero { 0 } else { s };
        (m, t, !zero & (of | (s > Dec::MAX_SCALE)))
    }

    #[inline(always)]
    pub(crate) fn div_pow(m: i64, sc: i32, k: u32) -> (i64, u8, bool) {
        if m == 0 {
            return (0, 0, false);
        }
        if !(0..=19).contains(&sc) || m == i64::MIN {
            return (m, 0, true);
        }
        if k >= 2 && m % 10 != 0 {
            return (m, (sc + k as i32) as u8, false);
        }
        let (mut a, mut t) = (m.unsigned_abs(), 0u32);
        while t < k && a % 10 == 0 {
            a /= 10;
            t += 1;
        }
        let (q, s) = match k - t {
            0 => (a, sc),
            1 if a.trailing_zeros() >= 24 => (a, sc + 1),
            1 => match a.checked_mul(10).filter(|x| *x <= i64::MAX as u64) {
                Some(x) => (x, sc + 2),
                None => return (m, 0, true),
            },
            left => (a, sc + left as i32),
        };
        let q = q as i64;
        (if m < 0 { -q } else { q }, s as u8, false)
    }

    #[inline(always)]
    pub(crate) fn pack_idx(n: usize, out: &mut [u64], mut f: impl FnMut(usize) -> bool) {
        for (w, o) in out.iter_mut().enumerate() {
            let base = w * 64;
            let end = (base + 64).min(n);
            if end - base == 64 {
                let mut bytes = [0u8; 64];
                for (j, b) in bytes.iter_mut().enumerate() {
                    *b = f(base + j) as u8;
                }
                *o = Self::bits(&bytes);
            } else {
                let mut m = 0u64;
                for j in 0..end.saturating_sub(base) {
                    m |= (f(base + j) as u64) << j;
                }
                *o = m;
            }
        }
    }

    #[inline(always)]
    pub(crate) fn select<T: Lane>(m: &[u64], t: T, e: T, out: &mut [T]) {
        for (&w, co) in m.iter().zip(out.chunks_mut(64)) {
            for (j, o) in co.iter_mut().enumerate() {
                let k = T::splat((w >> j) & 1);
                *o = (t & k) | (e & !k);
            }
        }
    }

    #[inline(always)]
    pub(crate) fn mask(m: &[u64], src: &[i64], out: &mut [i64]) {
        for ((&w, cs), co) in m.iter().zip(src.chunks(64)).zip(out.chunks_mut(64)) {
            for (j, (o, &x)) in co.iter_mut().zip(cs).enumerate() {
                *o = x & i64::splat((w >> j) & 1);
            }
        }
    }

    pub(crate) fn table(k: Dec) -> (Vec<i64>, u32) {
        let mut tab = vec![0i64; Dec::MAX_SCALE as usize + 1];
        let mut fits = 0u32;
        for (s, t) in tab.iter_mut().enumerate() {
            if let Some(u) = k.upscale(s as u8) {
                *t = u.mant;
                fits |= 1 << s;
            }
        }
        (tab, fits)
    }

    #[inline(always)]
    pub(crate) fn ord(op: Cmp, o: Ordering) -> bool {
        match op {
            Cmp::Eq | Cmp::Ne => o == Ordering::Equal,
            Cmp::Lt => o == Ordering::Less,
            Cmp::Le => o != Ordering::Greater,
            Cmp::Gt => o == Ordering::Greater,
            Cmp::Ge => o != Ordering::Less,
        }
    }

    #[inline(always)]
    pub(crate) fn cmp_k(op: Cmp, x: &[i64], c: i64, out: &mut [u64]) {
        match op {
            Cmp::Eq | Cmp::Ne => Self::pack1(x, out, |m| m == c),
            Cmp::Lt => Self::pack1(x, out, |m| m < c),
            Cmp::Le => Self::pack1(x, out, |m| m <= c),
            Cmp::Gt => Self::pack1(x, out, |m| m > c),
            Cmp::Ge => Self::pack1(x, out, |m| m >= c),
        }
    }

    #[inline(always)]
    pub(crate) fn blend<T: Lane>(m: &[u64], t: &[T], e: &[T], out: &mut [T]) {
        for (((&w, ct), ce), co) in m
            .iter()
            .zip(t.chunks(64))
            .zip(e.chunks(64))
            .zip(out.chunks_mut(64))
        {
            if ct.len() == 64 {
                for (j, ((o, &x), &y)) in co.iter_mut().zip(ct).zip(ce).enumerate().take(64) {
                    let k = T::splat((w >> j) & 1);
                    *o = (x & k) | (y & !k);
                }
            } else {
                for (j, ((o, &x), &y)) in co.iter_mut().zip(ct).zip(ce).enumerate() {
                    let k = T::splat((w >> j) & 1);
                    *o = (x & k) | (y & !k);
                }
            }
        }
    }

    #[inline(always)]
    pub(crate) fn copy_masked<T: Copy>(m: &[u64], src: &[T], out: &mut [T]) {
        let n = out.len();
        for (w, &mw) in m.iter().enumerate() {
            if mw == 0 {
                continue;
            }
            let base = w * 64;
            let end = (base + 64).min(n);
            if mw == u64::MAX && end - base == 64 {
                out[base..end].copy_from_slice(&src[base..end]);
                continue;
            }
            Bits::each(mw, base, |i| {
                if i < end {
                    out[i] = src[i];
                }
            });
        }
    }

    #[inline(always)]
    pub(crate) fn fill_masked<T: Copy>(m: &[u64], v: T, out: &mut [T]) {
        let n = out.len();
        for (w, &mw) in m.iter().enumerate() {
            if mw == 0 {
                continue;
            }
            let base = w * 64;
            let end = (base + 64).min(n);
            if mw == u64::MAX && end - base == 64 {
                out[base..end].fill(v);
                continue;
            }
            Bits::each(mw, base, |i| {
                if i < end {
                    out[i] = v;
                }
            });
        }
    }

    pub(crate) fn words(n: usize) -> usize {
        n.div_ceil(64)
    }
}
