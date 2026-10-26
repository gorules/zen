pub struct Bits;

impl Bits {
    #[inline]
    pub fn word(bits: &[u64], at: usize, width: usize) -> u64 {
        let all = match width >= 64 {
            true => u64::MAX,
            false => (1u64 << width) - 1,
        };
        if at == usize::MAX {
            return all;
        }
        let (word, shift) = (at / 64, at % 64);
        let low = bits.get(word).copied().unwrap_or(0) >> shift;
        let high = match shift {
            0 => 0,
            _ => bits.get(word + 1).copied().unwrap_or(0) << (64 - shift),
        };
        (low | high) & all
    }

    #[inline]
    pub fn on(bits: &mut [u64], row: usize) {
        if let Some(word) = bits.get_mut(row >> 6) {
            *word |= 1 << (row & 63);
        }
    }

    #[inline(always)]
    pub fn each(word: u64, base: usize, mut f: impl FnMut(usize)) {
        let mut word = word;
        while word != 0 {
            f(base + word.trailing_zeros() as usize);
            word &= word - 1;
        }
    }

    pub fn range(bits: &[u64], lo: usize, hi: usize, ones: bool) -> bool {
        let mut i = lo;
        while i < hi {
            let (w, s) = (i / 64, i % 64);
            let e = (s + (hi - i)).min(64);
            let mask = if e - s == 64 {
                u64::MAX
            } else {
                ((1u64 << (e - s)) - 1) << s
            };
            let v = bits.get(w).copied().unwrap_or(0) & mask;
            if (ones && v != mask) || (!ones && v != 0) {
                return false;
            }
            i += e - s;
        }
        true
    }

    #[inline]
    pub fn get(bits: &[u64], row: usize) -> bool {
        bits.get(row >> 6).is_some_and(|w| w >> (row & 63) & 1 == 1)
    }

    #[inline]
    pub fn set(bits: &mut [u64], row: usize, on: bool) {
        if let Some(word) = bits.get_mut(row >> 6) {
            *word = (*word & !(1 << (row & 63))) | (u64::from(on) << (row & 63));
        }
    }

    pub fn ones(rows: usize) -> Vec<u64> {
        let mut bits = vec![u64::MAX; rows.div_ceil(64)];
        Self::trim(&mut bits, rows);
        bits
    }

    pub fn trim(bits: &mut [u64], rows: usize) {
        if let (Some(last), tail @ 1..) = (bits.last_mut(), rows & 63) {
            *last &= (1u64 << tail) - 1;
        }
    }

    pub fn of(rows: usize, mut f: impl FnMut(usize) -> bool) -> Vec<u64> {
        let mut bits = vec![0u64; rows.div_ceil(64)];
        for (w, word) in bits.iter_mut().enumerate() {
            let base = w << 6;
            let mut value = 0u64;
            for bit in 0..(rows - base).min(64) {
                value |= u64::from(f(base + bit)) << bit;
            }
            *word = value;
        }
        bits
    }

    pub fn window(bits: &[u64], offset: usize, rows: usize) -> Vec<u64> {
        match offset {
            0 if bits.len() >= rows.div_ceil(64) => {
                let mut out = bits[..rows.div_ceil(64)].to_vec();
                Self::trim(&mut out, rows);
                out
            }
            _ => (0..rows.div_ceil(64))
                .map(|w| Self::word(bits, offset + (w << 6), (rows - (w << 6)).min(64)))
                .collect(),
        }
    }

    pub fn partial(validity: Option<(&[u64], usize)>, rows: usize) -> Option<(&[u64], usize)> {
        validity.filter(|(bits, offset)| !Self::full(bits, *offset, rows))
    }

    pub fn validity(validity: Option<(&[u64], usize)>, rows: usize) -> Option<Vec<u64>> {
        Self::partial(validity, rows).map(|(bits, offset)| Self::window(bits, offset, rows))
    }

    pub fn present(validity: Option<(&[u64], usize)>, rows: usize) -> Vec<u64> {
        match validity {
            Some((bits, offset)) => Self::window(bits, offset, rows),
            None => Self::ones(rows),
        }
    }

    pub fn gather(bits: &[u64], positions: &[usize]) -> Vec<u64> {
        Self::of(positions.len(), |i| Self::get(bits, positions[i]))
    }

    pub fn fill(bits: &mut [u64], start: usize, len: usize) {
        let end = start + len;
        let mut at = start;
        while at < end {
            let (word, bit) = (at >> 6, at & 63);
            let take = (64 - bit).min(end - at);
            let mask = match take {
                64 => u64::MAX,
                t => ((1u64 << t) - 1) << bit,
            };
            if let Some(w) = bits.get_mut(word) {
                *w |= mask;
            }
            at += take;
        }
    }

    pub fn splice(bits: &mut [u64], start: usize, source: &[u64], len: usize) {
        let mut at = 0;
        while at < len {
            let take = (len - at).min(64);
            let word = Self::word(source, at, take);
            let (dst, bit) = ((start + at) >> 6, (start + at) & 63);
            if let Some(w) = bits.get_mut(dst) {
                *w |= word << bit;
            }
            if bit > 0 {
                if let Some(w) = bits.get_mut(dst + 1) {
                    *w |= word >> (64 - bit);
                }
            }
            at += take;
        }
    }

    pub fn full(bits: &[u64], offset: usize, len: usize) -> bool {
        Self::range(bits, offset, offset + len, true)
    }

    pub fn rows(bits: &[u64]) -> impl Iterator<Item = usize> + '_ {
        bits.iter().enumerate().flat_map(|(w, &word)| {
            let mut word = word;
            std::iter::from_fn(move || {
                (word != 0).then(|| {
                    let at = word.trailing_zeros() as usize;
                    word &= word - 1;
                    w * 64 + at
                })
            })
        })
    }

    #[inline]
    pub fn or(into: &mut [u64], from: &[u64]) {
        into.iter_mut().zip(from).for_each(|(a, b)| *a |= b);
    }

    #[inline]
    pub fn and(into: &mut [u64], from: &[u64]) {
        into.iter_mut().zip(from).for_each(|(a, b)| *a &= b);
    }

    pub fn none(bits: &[u64]) -> bool {
        bits.iter().all(|w| *w == 0)
    }
}
