use std::ops::Range;
use std::rc::Rc;

use ahash::{HashMap, HashSet};
use rust_decimal::Decimal;

use super::intern::Interner;
use super::partition::Layout;
use super::value_set::{Bound, StringSet, ValueSet};
use super::verify::MAX_FRAGMENTS;

const DIRECT_ROWS: usize = 8;
const WORD: usize = u64::BITS as usize;

pub(super) struct Dense;

impl Dense {
    const REGION_WORDS: usize = 128;
    const TABLE_WORDS: usize = 16 << 20;

    pub(super) fn atoms<'s>(sets: impl IntoIterator<Item = &'s ValueSet>) -> usize {
        let mut strings: HashSet<&Rc<str>> = HashSet::default();
        let mut points: HashSet<Decimal> = HashSet::default();
        for set in sets {
            match &set.strings {
                StringSet::Finite(keys) | StringSet::CoFinite(keys) => strings.extend(keys),
            }
            for interval in set.numbers.intervals() {
                for bound in [interval.lo, interval.hi] {
                    if let Bound::Inclusive(point) | Bound::Exclusive(point) = bound {
                        points.insert(point);
                    }
                }
            }
        }
        strings.len() + 2 * points.len() + 6
    }

    pub(super) fn fits(rows: usize, atoms: impl Iterator<Item = usize>) -> bool {
        let stride: usize = atoms.map(Bits::words).sum();
        stride <= Self::REGION_WORDS && rows.saturating_mul(stride) <= Self::TABLE_WORDS
    }
}

pub(super) struct Bits;

impl Bits {
    pub(super) fn words(atoms: usize) -> usize {
        atoms.div_ceil(WORD).max(1)
    }

    pub(super) fn has(words: &[u64], atom: usize) -> bool {
        words[atom / WORD] >> (atom % WORD) & 1 == 1
    }

    pub(super) fn set(words: &mut [u64], atom: usize) {
        words[atom / WORD] |= 1 << (atom % WORD);
    }

    pub(super) fn ones(words: &[u64]) -> impl Iterator<Item = usize> + '_ {
        words.iter().enumerate().flat_map(|(index, &word)| {
            let mut rest = word;
            std::iter::from_fn(move || {
                (rest != 0).then(|| {
                    let atom = index * WORD + rest.trailing_zeros() as usize;
                    rest &= rest - 1;
                    atom
                })
            })
        })
    }
}

pub(super) struct Axis {
    offset: usize,
    words: usize,
    atoms: usize,
    strings: usize,
    rest: Option<usize>,
    numbers: Range<usize>,
    piece_of: Vec<u32>,
    last_piece: u32,
    string_mask: Vec<u64>,
    number_mask: Vec<u64>,
    tail_mask: Vec<u64>,
    consecutive: Vec<u64>,
}

impl Axis {
    fn new(layout: Layout, offset: usize) -> Self {
        let words = Bits::words(layout.atoms);
        let mut string_mask = vec![0u64; words];
        let mut number_mask = vec![0u64; words];
        let mut tail_mask = vec![0u64; words];
        let mut consecutive = vec![0u64; words];
        (0..layout.strings).for_each(|atom| Bits::set(&mut string_mask, atom));
        layout
            .numbers
            .clone()
            .for_each(|atom| Bits::set(&mut number_mask, atom));
        (layout.numbers.end..layout.atoms).for_each(|atom| Bits::set(&mut tail_mask, atom));
        for (index, pair) in layout.piece_of.windows(2).enumerate() {
            if pair[1] == pair[0] + 1 {
                Bits::set(&mut consecutive, layout.numbers.start + index);
            }
        }
        Self {
            offset,
            words,
            atoms: layout.atoms,
            strings: layout.strings,
            rest: layout.rest,
            numbers: layout.numbers,
            piece_of: layout.piece_of,
            last_piece: layout.last_piece,
            string_mask,
            number_mask,
            tail_mask,
            consecutive,
        }
    }

    pub(super) fn range(&self) -> Range<usize> {
        self.offset..self.offset + self.words
    }

    fn piece(&self, atom: usize) -> u32 {
        self.piece_of[atom - self.numbers.start]
    }

    fn localize(&self, bounds: &[u64], scratch: &mut Scratch) -> usize {
        let words = self.words;
        let distinct = scratch.sets.len() as u32;
        for buffer in [
            &mut scratch.mentioned,
            &mut scratch.starts,
            &mut scratch.ends,
            &mut scratch.count_mask,
        ] {
            buffer.clear();
            buffer.resize(words, 0);
        }
        for id in 0..distinct {
            let set = scratch.sets.get(id);
            let flip = match self.rest {
                Some(rest) if Bits::has(set, rest) => u64::MAX,
                _ => 0,
            };
            for index in 0..words {
                scratch.mentioned[index] |= (set[index] ^ flip) & self.string_mask[index];
                let numbers = set[index] & self.number_mask[index];
                let after = match set.get(index + 1) {
                    Some(next) => (next & self.number_mask[index + 1]) << (WORD - 1),
                    None => 0,
                };
                let before = match index {
                    0 => 0,
                    _ => {
                        (set[index - 1] & self.number_mask[index - 1] & self.consecutive[index - 1])
                            >> (WORD - 1)
                    }
                };
                let continues = numbers & self.consecutive[index] & ((numbers >> 1) | after);
                let continued = numbers & (((numbers & self.consecutive[index]) << 1) | before);
                scratch.ends[index] |= numbers & !continues;
                scratch.starts[index] |= numbers & !continued;
            }
        }

        scratch.marks.clear();
        for atom in Bits::ones(&scratch.ends) {
            let piece = self.piece(atom);
            if piece < self.last_piece {
                scratch.marks.push(piece / 2);
            }
        }
        for atom in Bits::ones(&scratch.starts) {
            let piece = self.piece(atom);
            if piece >= 1 {
                scratch.marks.push((piece - 1) / 2);
            }
        }
        scratch.marks.sort_unstable();
        scratch.marks.dedup();

        scratch.order.clear();
        let mut lump: Option<usize> = None;
        for index in 0..words {
            let strings = bounds[index] & self.string_mask[index];
            scratch.mentioned[index] &= strings;
            let unlisted = strings & !scratch.mentioned[index];
            if lump.is_none() && unlisted != 0 {
                lump = Some(index * WORD + unlisted.trailing_zeros() as usize);
            }
        }
        scratch
            .order
            .extend(Bits::ones(&scratch.mentioned).map(|a| a as u32));
        let lump = lump.or(self.rest.filter(|&rest| Bits::has(bounds, rest)));
        scratch.lump = lump.map(|atom| atom as u32);
        scratch.order.extend(scratch.lump);

        let mut below = 0usize;
        let mut previous: Option<u32> = None;
        for index in 0..words {
            let mut numbers = bounds[index] & self.number_mask[index];
            while numbers != 0 {
                let atom = index * WORD + numbers.trailing_zeros() as usize;
                numbers &= numbers - 1;
                let piece = self.piece(atom);
                let point = piece / 2;
                while scratch.marks.get(below).is_some_and(|&mark| mark < point) {
                    below += 1;
                }
                let marked = piece % 2 == 1 && scratch.marks.get(below) == Some(&point);
                let local = 2 * below as u32 + u32::from(marked);
                if previous != Some(local) {
                    previous = Some(local);
                    scratch.order.push(atom as u32);
                }
            }
        }
        for index in 0..words {
            let mut tail = bounds[index] & self.tail_mask[index];
            while tail != 0 {
                scratch
                    .order
                    .push((index * WORD + tail.trailing_zeros() as usize) as u32);
                tail &= tail - 1;
            }
        }

        if scratch.slot_of.len() < self.atoms {
            scratch.slot_of.resize(self.atoms, 0);
        }
        for (slot, &atom) in scratch.order.iter().enumerate() {
            scratch.slot_of[atom as usize] = slot as u32;
            Bits::set(&mut scratch.count_mask, atom as usize);
        }

        (0..distinct)
            .map(|id| {
                scratch
                    .sets
                    .get(id)
                    .iter()
                    .zip(&scratch.count_mask)
                    .map(|(set, mask)| (set & mask).count_ones() as usize)
                    .sum::<usize>()
            })
            .sum()
    }

    fn group(&self, scratch: &mut Scratch) {
        let slots = scratch.order.len();
        if scratch.touched.len() < slots {
            scratch.touched.resize_with(slots, Vec::new);
        }
        scratch.touched[..slots].iter_mut().for_each(Vec::clear);
        for id in 0..scratch.sets.len() as u32 {
            let set = scratch.sets.get(id);
            for index in 0..self.words {
                let mut local = set[index] & scratch.count_mask[index];
                while local != 0 {
                    let atom = index * WORD + local.trailing_zeros() as usize;
                    local &= local - 1;
                    scratch.touched[scratch.slot_of[atom] as usize].push(id);
                }
            }
        }

        scratch.lookup.clear();
        scratch.group_of.clear();
        scratch.leader.clear();
        for slot in 0..slots {
            let ids = &scratch.touched[slot];
            let hash = ids.iter().fold(0x9E37_79B9_7F4A_7C15u64, |hash, &id| {
                (hash ^ u64::from(id)).wrapping_mul(0xA24B_AED4_963E_E407)
            });
            let mut probe = 0u32;
            let group = loop {
                match scratch.lookup.get(&(hash, ids.len(), probe)) {
                    Some(&group)
                        if scratch.touched[scratch.leader[group as usize] as usize] == *ids =>
                    {
                        break group;
                    }
                    Some(_) => probe += 1,
                    None => {
                        let group = scratch.leader.len() as u32;
                        scratch.lookup.insert((hash, ids.len(), probe), group);
                        scratch.leader.push(slot as u32);
                        break group;
                    }
                }
            };
            scratch.group_of.push(group);
        }
    }

    fn masks(&self, bounds: &[u64], scratch: &Scratch) -> Vec<u64> {
        let mut masks = vec![0u64; scratch.leader.len() * self.words];
        let lump = scratch.lump.map(|atom| scratch.slot_of[atom as usize]);
        let mut piece_slot = 0u32;
        for atom in Bits::ones(bounds) {
            let own = || scratch.slot_of[atom];
            let slot = if atom < self.strings {
                match Bits::has(&scratch.mentioned, atom) {
                    true => Some(own()),
                    false => lump,
                }
            } else if Some(atom) == self.rest {
                lump
            } else if self.numbers.contains(&atom) {
                if Bits::has(&scratch.count_mask, atom) {
                    piece_slot = own();
                }
                Some(piece_slot)
            } else {
                Some(own())
            };
            let Some(slot) = slot else {
                continue;
            };
            let group = scratch.group_of[slot as usize] as usize;
            Bits::set(
                &mut masks[group * self.words..(group + 1) * self.words],
                atom,
            );
        }
        masks
    }
}

pub(super) struct Space {
    axes: Vec<Axis>,
    stride: usize,
}

impl Space {
    pub(super) fn new(layouts: Vec<Layout>) -> Self {
        let mut stride = 0;
        let axes = layouts
            .into_iter()
            .map(|layout| {
                let axis = Axis::new(layout, stride);
                stride += axis.words;
                axis
            })
            .collect();
        Self { axes, stride }
    }

    pub(super) fn stride(&self) -> usize {
        self.stride
    }

    pub(super) fn axes(&self) -> &[Axis] {
        &self.axes
    }

    fn full(&self) -> Vec<u64> {
        let mut region = vec![0u64; self.stride];
        for axis in &self.axes {
            (0..axis.atoms).for_each(|atom| Bits::set(&mut region[axis.range()], atom));
        }
        region
    }

    fn meets(&self, cut: &[u64], region: &[u64]) -> bool {
        self.axes
            .iter()
            .all(|axis| axis.range().any(|index| cut[index] & region[index] != 0))
    }

    fn within(region: &[u64], cut: &[u64]) -> bool {
        region.iter().zip(cut).all(|(set, cut)| set & !cut == 0)
    }

    pub(super) fn subtract(
        &self,
        fragment: &[u64],
        cut: &[u64],
        prefix: &mut Vec<u64>,
        out: &mut Vec<u64>,
    ) -> bool {
        if !self.meets(cut, fragment) {
            return false;
        }
        prefix.clear();
        prefix.extend_from_slice(fragment);
        for axis in &self.axes {
            let range = axis.range();
            if range.clone().any(|index| prefix[index] & !cut[index] != 0) {
                let start = out.len();
                out.extend_from_slice(prefix);
                for index in range.clone() {
                    out[start + index] = prefix[index] & !cut[index];
                }
            }
            for index in range {
                prefix[index] &= cut[index];
            }
        }
        true
    }
}

#[derive(Default)]
struct Scratch {
    sets: Interner<u64>,
    of_row: Vec<u32>,
    key: Vec<u64>,
    mentioned: Vec<u64>,
    starts: Vec<u64>,
    ends: Vec<u64>,
    count_mask: Vec<u64>,
    marks: Vec<u32>,
    order: Vec<u32>,
    lump: Option<u32>,
    slot_of: Vec<u32>,
    touched: Vec<Vec<u32>>,
    lookup: HashMap<(u64, usize, u32), u32>,
    group_of: Vec<u32>,
    leader: Vec<u32>,
    counts: Vec<usize>,
}

struct Best {
    axis: usize,
    score: usize,
    masks: Vec<u64>,
    ids: Vec<Vec<u32>>,
    of_row: Vec<u32>,
    distinct: usize,
}

struct Piece {
    bits: Vec<u64>,
    rows: Vec<u32>,
}

struct Split {
    axis: usize,
    pieces: Vec<Piece>,
}

pub(super) struct Found {
    pub(super) patterns: Vec<Interner<u64>>,
    pub(super) fragments: Vec<u32>,
}

pub(super) struct Gaps<'c> {
    space: &'c Space,
    cuts: &'c [u64],
    work: usize,
    patterns: Vec<Interner<u64>>,
    fragments: Vec<u32>,
    scratch: Scratch,
}

impl<'c> Gaps<'c> {
    pub(super) fn new(space: &'c Space, cuts: &'c [u64], work: usize) -> Self {
        Self {
            space,
            cuts,
            work,
            patterns: space
                .axes
                .iter()
                .map(|axis| Interner::new(axis.words))
                .collect(),
            fragments: Vec::new(),
            scratch: Scratch::default(),
        }
    }

    pub(super) fn run(mut self, rows: usize) -> Option<Found> {
        let region = self.space.full();
        self.uncovered(region, (0..rows as u32).collect())?;
        Some(Found {
            patterns: self.patterns,
            fragments: self.fragments,
        })
    }

    fn spend(&mut self, amount: usize) -> Option<()> {
        self.work = self.work.checked_sub(amount.max(1))?;
        Some(())
    }

    fn cut(&self, row: u32) -> &'c [u64] {
        let cuts: &'c [u64] = self.cuts;
        let start = row as usize * self.space.stride;
        &cuts[start..start + self.space.stride]
    }

    fn emit(&mut self, region: &[u64]) {
        for (axis, patterns) in self.space.axes.iter().zip(self.patterns.iter_mut()) {
            self.fragments.push(patterns.intern(&region[axis.range()]));
        }
    }

    fn uncovered(&mut self, region: Vec<u64>, rows: Vec<u32>) -> Option<()> {
        let space = self.space;
        self.spend(rows.len() * space.axes.len())?;
        let rows: Vec<u32> = rows
            .into_iter()
            .filter(|&row| space.meets(self.cut(row), &region))
            .collect();
        if rows.is_empty() {
            self.emit(&region);
            return Some(());
        }
        if rows
            .iter()
            .any(|&row| Space::within(&region, self.cut(row)))
        {
            return Some(());
        }
        if rows.len() > DIRECT_ROWS {
            if let Some(split) = self.split(&region, &rows) {
                let range = space.axes[split.axis].range();
                for piece in split.pieces {
                    let mut narrowed = region.clone();
                    narrowed[range.clone()].copy_from_slice(&piece.bits);
                    self.uncovered(narrowed, piece.rows)?;
                }
                return Some(());
            }
        }
        self.subtract_all(region, &rows)
    }

    fn subtract_all(&mut self, region: Vec<u64>, rows: &[u32]) -> Option<()> {
        let space = self.space;
        let stride = space.stride;
        let dims = space.axes.len();
        let mut remaining = region;
        let mut next: Vec<u64> = Vec::new();
        let mut prefix: Vec<u64> = Vec::with_capacity(stride);
        for &row in rows {
            let cut = self.cut(row);
            next.clear();
            for fragment in remaining.chunks_exact(stride) {
                self.spend(dims)?;
                if !space.subtract(fragment, cut, &mut prefix, &mut next) {
                    next.extend_from_slice(fragment);
                }
            }
            if next.len() / stride > MAX_FRAGMENTS {
                return None;
            }
            std::mem::swap(&mut remaining, &mut next);
            if remaining.is_empty() {
                return Some(());
            }
        }
        for fragment in remaining.chunks_exact(stride) {
            self.emit(fragment);
        }
        Some(())
    }

    fn split(&mut self, region: &[u64], rows: &[u32]) -> Option<Split> {
        let space = self.space;
        let scratch = &mut self.scratch;
        let mut best: Option<Best> = None;
        for (index, axis) in space.axes.iter().enumerate() {
            let bounds = &region[axis.range()];
            scratch.sets.reset(axis.words);
            scratch.of_row.clear();
            for &row in rows {
                let start = row as usize * space.stride + axis.offset;
                let cut = &self.cuts[start..start + axis.words];
                scratch.key.clear();
                scratch
                    .key
                    .extend(cut.iter().zip(bounds).map(|(cut, bound)| cut & bound));
                scratch.of_row.push(scratch.sets.intern(&scratch.key));
            }
            let distinct = scratch.sets.len();
            if distinct == 1 && scratch.sets.get(0) == bounds {
                continue;
            }
            if axis.localize(bounds, scratch) > self.work {
                return None;
            }
            axis.group(scratch);
            let groups = scratch.leader.len();
            if groups < 2 {
                continue;
            }
            scratch.counts.clear();
            scratch.counts.resize(distinct, 0);
            for &id in &scratch.of_row {
                scratch.counts[id as usize] += 1;
            }
            let score: usize = scratch
                .leader
                .iter()
                .map(|&slot| {
                    scratch.touched[slot as usize]
                        .iter()
                        .map(|&id| scratch.counts[id as usize])
                        .sum::<usize>()
                })
                .sum();
            self.work = self.work.checked_sub((score + groups).max(1))?;
            if best.as_ref().is_none_or(|best| score < best.score) {
                best = Some(Best {
                    axis: index,
                    score,
                    masks: axis.masks(bounds, scratch),
                    ids: scratch
                        .leader
                        .iter()
                        .map(|&slot| scratch.touched[slot as usize].clone())
                        .collect(),
                    of_row: scratch.of_row.clone(),
                    distinct,
                });
            }
        }
        let best = best?;
        let words = space.axes[best.axis].words;
        let mut starts = vec![0u32; best.distinct + 1];
        for &id in &best.of_row {
            starts[id as usize + 1] += 1;
        }
        for id in 0..best.distinct {
            starts[id + 1] += starts[id];
        }
        let mut cursor = starts.clone();
        let mut by_set = vec![0u32; rows.len()];
        for (&row, &id) in rows.iter().zip(&best.of_row) {
            by_set[cursor[id as usize] as usize] = row;
            cursor[id as usize] += 1;
        }
        let pieces = best
            .ids
            .iter()
            .zip(best.masks.chunks_exact(words))
            .map(|(ids, mask)| {
                let mut piece_rows: Vec<u32> = ids
                    .iter()
                    .flat_map(|&id| {
                        by_set[starts[id as usize] as usize..starts[id as usize + 1] as usize]
                            .iter()
                            .copied()
                    })
                    .collect();
                piece_rows.sort_unstable();
                Piece {
                    bits: mask.to_vec(),
                    rows: piece_rows,
                }
            })
            .collect();
        Some(Split {
            axis: best.axis,
            pieces,
        })
    }
}
