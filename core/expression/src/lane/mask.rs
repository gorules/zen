use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

pub const LANES: usize = 1024;
const WORDS: usize = LANES / 64;

#[derive(Clone, Copy, Debug)]
pub struct Mask {
    words: [u64; WORDS],
    used: u8,
}

impl Default for Mask {
    fn default() -> Self {
        Self::none(LANES)
    }
}

impl Mask {
    #[inline]
    pub fn span(width: usize) -> usize {
        width.div_ceil(64).clamp(1, WORDS)
    }

    #[inline]
    pub fn none(width: usize) -> Self {
        Self {
            words: [0; WORDS],
            used: Self::span(width) as u8,
        }
    }

    #[inline]
    pub fn all(width: usize) -> Self {
        let mut mask = Self::none(width);
        if width <= 64 {
            mask.words[0] = if width == 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            return mask;
        }
        let full = width / 64;
        mask.words[..full.min(WORDS)].fill(u64::MAX);
        if full < WORDS && !width.is_multiple_of(64) {
            mask.words[full] = (1u64 << (width % 64)) - 1;
        }
        mask
    }

    #[inline]
    pub fn bit(words: &[u64], at: usize) -> bool {
        words.get(at / 64).is_some_and(|w| w >> (at % 64) & 1 == 1)
    }

    #[inline]
    pub fn used(&self) -> &[u64] {
        &self.words[..self.used as usize]
    }

    #[inline]
    pub fn used_mut(&mut self) -> &mut [u64] {
        &mut self.words[..self.used as usize]
    }

    #[inline(always)]
    pub fn word(&self, index: usize) -> u64 {
        self.words[index & (WORDS - 1)]
    }

    #[inline(always)]
    pub fn set_word(&mut self, index: usize, word: u64) {
        self.words[index & (WORDS - 1)] = word;
    }

    #[inline(always)]
    pub fn get(&self, lane: usize) -> bool {
        self.words[(lane >> 6) & (WORDS - 1)] >> (lane & 63) & 1 == 1
    }

    #[inline(always)]
    pub fn set(&mut self, lane: usize) {
        self.words[(lane >> 6) & (WORDS - 1)] |= 1 << (lane & 63);
    }

    #[inline(always)]
    pub fn unset(&mut self, lane: usize) {
        self.words[(lane >> 6) & (WORDS - 1)] &= !(1 << (lane & 63));
    }

    #[inline(always)]
    pub fn put(&mut self, lane: usize, on: bool) {
        let w = &mut self.words[(lane >> 6) & (WORDS - 1)];
        *w = (*w & !(1 << (lane & 63))) | ((on as u64) << (lane & 63));
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        match self.used {
            1 => self.words[0] == 0,
            _ => self.used().iter().all(|w| *w == 0),
        }
    }

    #[inline]
    pub fn count(&self) -> usize {
        self.used().iter().map(|w| w.count_ones() as usize).sum()
    }

    #[inline]
    pub fn first(&self) -> Option<usize> {
        self.used()
            .iter()
            .enumerate()
            .find(|(_, w)| **w != 0)
            .map(|(i, w)| i * 64 + w.trailing_zeros() as usize)
    }

    #[inline(always)]
    fn zip(self, other: Mask, f: impl Fn(u64, u64) -> u64) -> Mask {
        if self.used == 1 && other.used == 1 {
            let mut out = self;
            out.words[0] = f(self.words[0], other.words[0]);
            return out;
        }
        let mut out = if self.used >= other.used { self } else { other };
        for i in 0..out.used as usize {
            out.words[i] = f(self.words[i], other.words[i]);
        }
        out
    }
}

impl PartialEq for Mask {
    #[inline]
    fn eq(&self, other: &Mask) -> bool {
        let used = self.used.max(other.used) as usize;
        self.words[..used] == other.words[..used]
    }
}

impl Eq for Mask {}

impl BitAnd for Mask {
    type Output = Mask;

    #[inline]
    fn bitand(self, other: Mask) -> Mask {
        self.zip(other, |a, b| a & b)
    }
}

impl BitOr for Mask {
    type Output = Mask;

    #[inline]
    fn bitor(self, other: Mask) -> Mask {
        self.zip(other, |a, b| a | b)
    }
}

impl Not for Mask {
    type Output = Mask;

    #[inline(always)]
    fn not(self) -> Mask {
        let mut out = self;
        if out.used == 1 {
            out.words[0] = !out.words[0];
            return out;
        }
        for w in out.used_mut() {
            *w = !*w;
        }
        out
    }
}

impl BitAndAssign for Mask {
    #[inline]
    fn bitand_assign(&mut self, other: Mask) {
        *self = *self & other;
    }
}

impl BitOrAssign for Mask {
    #[inline]
    fn bitor_assign(&mut self, other: Mask) {
        *self = *self | other;
    }
}

pub trait LaneSet:
    Copy
    + Eq
    + std::fmt::Debug
    + Default
    + BitAnd<Output = Self>
    + BitOr<Output = Self>
    + Not<Output = Self>
    + BitAndAssign
    + BitOrAssign
{
    const LANES: usize;

    fn none(width: usize) -> Self;
    fn all(width: usize) -> Self;
    fn words(&self) -> usize;
    fn word(&self, index: usize) -> u64;
    fn set_word(&mut self, index: usize, word: u64);
    fn get(&self, lane: usize) -> bool;
    fn set(&mut self, lane: usize);
    fn unset(&mut self, lane: usize);
    fn put(&mut self, lane: usize, on: bool);
    fn is_empty(&self) -> bool;
    fn count(&self) -> usize;
    fn first(&self) -> Option<usize>;

    #[inline]
    fn any(&self) -> bool {
        !self.is_empty()
    }

    #[inline]
    fn store(&self, target: &mut [u64], at: usize) {
        for i in 0..self.words() {
            if let Some(slot) = target.get_mut(at + i) {
                *slot = self.word(i);
            }
        }
    }
}

impl LaneSet for Mask {
    const LANES: usize = LANES;

    #[inline(always)]
    fn none(width: usize) -> Self {
        Mask::none(width)
    }

    #[inline(always)]
    fn all(width: usize) -> Self {
        Mask::all(width)
    }

    #[inline(always)]
    fn words(&self) -> usize {
        self.used as usize
    }

    #[inline(always)]
    fn word(&self, index: usize) -> u64 {
        Mask::word(self, index)
    }

    #[inline(always)]
    fn set_word(&mut self, index: usize, word: u64) {
        Mask::set_word(self, index, word)
    }

    #[inline(always)]
    fn get(&self, lane: usize) -> bool {
        Mask::get(self, lane)
    }

    #[inline(always)]
    fn set(&mut self, lane: usize) {
        Mask::set(self, lane)
    }

    #[inline(always)]
    fn unset(&mut self, lane: usize) {
        Mask::unset(self, lane)
    }

    #[inline(always)]
    fn put(&mut self, lane: usize, on: bool) {
        Mask::put(self, lane, on)
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        Mask::is_empty(self)
    }

    #[inline(always)]
    fn count(&self) -> usize {
        Mask::count(self)
    }

    #[inline(always)]
    fn first(&self) -> Option<usize> {
        Mask::first(self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Word(u64);

impl LaneSet for Word {
    const LANES: usize = 64;

    #[inline]
    fn none(_: usize) -> Self {
        Word(0)
    }

    #[inline]
    fn all(width: usize) -> Self {
        Word(if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        })
    }

    #[inline]
    fn words(&self) -> usize {
        1
    }

    #[inline(always)]
    fn word(&self, index: usize) -> u64 {
        if index == 0 {
            self.0
        } else {
            0
        }
    }

    #[inline(always)]
    fn set_word(&mut self, index: usize, word: u64) {
        if index == 0 {
            self.0 = word;
        }
    }

    #[inline(always)]
    fn get(&self, lane: usize) -> bool {
        self.0 >> (lane & 63) & 1 == 1
    }

    #[inline(always)]
    fn set(&mut self, lane: usize) {
        self.0 |= 1 << (lane & 63);
    }

    #[inline(always)]
    fn unset(&mut self, lane: usize) {
        self.0 &= !(1 << (lane & 63));
    }

    #[inline(always)]
    fn put(&mut self, lane: usize, on: bool) {
        self.0 = (self.0 & !(1 << (lane & 63))) | ((on as u64) << (lane & 63));
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.0 == 0
    }

    #[inline]
    fn count(&self) -> usize {
        self.0.count_ones() as usize
    }

    #[inline]
    fn first(&self) -> Option<usize> {
        (self.0 != 0).then(|| self.0.trailing_zeros() as usize)
    }
}

impl BitAnd for Word {
    type Output = Word;

    #[inline]
    fn bitand(self, other: Word) -> Word {
        Word(self.0 & other.0)
    }
}

impl BitOr for Word {
    type Output = Word;

    #[inline]
    fn bitor(self, other: Word) -> Word {
        Word(self.0 | other.0)
    }
}

impl Not for Word {
    type Output = Word;

    #[inline]
    fn not(self) -> Word {
        Word(!self.0)
    }
}

impl BitAndAssign for Word {
    #[inline]
    fn bitand_assign(&mut self, other: Word) {
        self.0 &= other.0;
    }
}

impl BitOrAssign for Word {
    #[inline]
    fn bitor_assign(&mut self, other: Word) {
        self.0 |= other.0;
    }
}

pub struct Lanes<M> {
    mask: M,
    index: usize,
    current: u64,
}

impl<M: LaneSet> Lanes<M> {
    #[inline]
    pub fn of(mask: M) -> Self {
        Self {
            current: mask.word(0),
            mask,
            index: 0,
        }
    }
}

impl<M: LaneSet> Iterator for Lanes<M> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        loop {
            if self.current != 0 {
                let bit = self.current.trailing_zeros() as usize;
                self.current &= self.current - 1;
                return Some(self.index * 64 + bit);
            }
            self.index += 1;
            if self.index >= self.mask.words() {
                return None;
            }
            self.current = self.mask.word(self.index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LaneSet, Mask, Word};

    #[test]
    fn lanes_and_ops() {
        let mut m = Mask::none(300);
        for lane in [0, 63, 64, 129, 299] {
            m.set(lane);
        }
        assert_eq!(
            super::Lanes::of(m).collect::<Vec<_>>(),
            vec![0, 63, 64, 129, 299]
        );
        assert_eq!(m.count(), 5);
        assert_eq!(m.first(), Some(0));
        let all = Mask::all(300);
        assert_eq!(all.count(), 300);
        assert_eq!((!m & all).count(), 295);
        assert!(Mask::none(1).is_empty());
        assert_eq!(Mask::all(64).count(), 64);
        assert_eq!(Mask::all(1024).count(), 1024);
        let w = Word::all(10) & !Word::none(10);
        assert_eq!(super::Lanes::of(w).count(), 10);
    }
}
