use super::list::Frame;
use crate::compiled::leaf::{Bits, Leaf};
use crate::compiled::shred::{Coder, Listing};
use std::rc::Rc;
use zen_expression::lane::{Column, Values};
use zen_expression::typed::{Batch, Kind, Outputs, Program};

pub(crate) type Hint = Option<(Rc<[i32]>, usize)>;

pub(crate) enum ListPart<'l, 'a> {
    Frame(&'l Frame, Vec<Column<'l>>),
    Listing(&'l Listing<'a>, Option<&'l [u64]>, Option<&'l [u64]>),
}

pub(crate) struct TypedCall<'l, 'a> {
    pub rows: usize,
    pub columns: &'l [Leaf<'a>],
    pub lists: Vec<ListPart<'l, 'a>>,
    pub select: Option<&'l [u64]>,
}

pub(crate) struct Select;

pub(crate) enum Part<'p> {
    Codes(&'p [i32], usize, Option<(&'p [u64], usize)>),
    Bits(&'p [u64], usize, Option<(&'p [u64], usize)>),
}

pub(crate) struct Dedup;

impl TypedCall<'_, '_> {
    pub fn run(
        self,
        program: &Program,
        context: &zen_expression::context::EvaluationContext,
    ) -> Option<Outputs> {
        let mut batch = Batch::new(self.rows);
        self.columns
            .iter()
            .for_each(|leaf| batch.push_column(leaf.column()));
        for (part, def) in self.lists.into_iter().zip(&program.hir().schema.lists) {
            batch.push_list(match part {
                ListPart::Frame(frame, fields) => frame.list(None, fields),
                ListPart::Listing(listing, validity, faults) => {
                    let list = listing.frame.list(validity, listing.columns(&def.fields)?);
                    match faults.or(listing.hosted.as_deref()) {
                        Some(bits) => list.faults(bits, 0),
                        None => list,
                    }
                }
            });
        }
        let mut out = program.outputs();
        program.run(&batch, &mut out, self.select, context).ok()?;
        Some(out)
    }
}

impl Select {
    pub fn bits(rows: usize, positions: &[usize]) -> Vec<u64> {
        let mut bits = vec![0u64; rows.div_ceil(64)];
        positions
            .iter()
            .for_each(|&row| Bits::set(&mut bits, row, true));
        bits
    }
}

impl Dedup {
    pub(crate) const MIN: usize = 16;
    const SAMPLE: usize = 1024;
    pub(crate) const TEXT: usize = 2;
    const EFFORT: usize = 4;
    const COSTLY: usize = 2;

    pub(crate) fn effort(program: &Program) -> usize {
        program.instructions() + Self::COSTLY * program.costly()
    }

    pub(crate) fn heavy(program: &Program) -> bool {
        program.hir().outputs.iter().any(|out| {
            out.collect.is_some()
                || out.shape.is_some()
                || matches!(out.ty.kind, Kind::Text | Kind::Date)
        })
    }

    pub(crate) fn weight(column: &Column) -> usize {
        match column.values {
            Values::Dict { .. } | Values::Bool { .. } => 0,
            Values::Text { .. } | Values::Utf8 { .. } => Self::TEXT,
            _ => 1,
        }
    }

    pub(crate) fn worth(effort: usize, heavy: bool, weight: usize) -> bool {
        heavy || effort >= Self::EFFORT * weight
    }

    pub fn reps(
        program: &Program,
        leaves: &[Leaf],
        codes: &[Hint],
        positions: Option<&Rc<[usize]>>,
        rows: usize,
        context: &zen_expression::context::EvaluationContext,
    ) -> Option<(Outputs, Vec<u32>)> {
        let (reps, ids) = Self::distinct(leaves, codes, positions, rows)?;
        let picks: Rc<[usize]> = reps.into();
        let picked: Vec<Leaf> = leaves.iter().map(|leaf| leaf.pick(&picks)).collect();
        let call = TypedCall {
            rows: picks.len(),
            columns: &picked,
            lists: Vec::new(),
            select: None,
        };
        Some((call.run(program, context)?, ids))
    }

    pub fn coded(
        program: &Program,
        leaves: &[Leaf],
        codes: &[Hint],
        positions: Option<&Rc<[usize]>>,
        rows: usize,
        context: &zen_expression::context::EvaluationContext,
    ) -> Option<Outputs> {
        let (mut out, ids) = Self::reps(program, leaves, codes, positions, rows, context)?;
        out.spread(rows, &ids, positions.map(|p| &p[..]))
            .then_some(out)
    }

    pub fn codes(column: &Column, rows: usize) -> Hint {
        match column.values {
            Values::Scaled { .. } | Values::I64(_) => Self::numbered(column, rows),
            Values::Text { offsets, data } => Coder::keys(data.as_bytes(), offsets.get(..=rows)?),
            Values::Utf8 { offsets, data } => Coder::keys(data, offsets.get(..=rows)?),
            _ => None,
        }
    }

    pub fn distinct(
        leaves: &[Leaf],
        hints: &[Hint],
        positions: Option<&Rc<[usize]>>,
        rows: usize,
    ) -> Option<(Vec<usize>, Vec<u32>)> {
        let local = positions.map_or(rows, |p| p.len());
        if leaves.is_empty() || local < 16 {
            return None;
        }
        let listed: Vec<bool> = leaves.iter().map(|leaf| leaf.records().is_some()).collect();
        if listed
            .iter()
            .enumerate()
            .any(|(at, &listed)| listed && hints.get(at).is_none_or(Option::is_none))
        {
            return None;
        }
        let held: Vec<Option<Vec<u64>>> = leaves
            .iter()
            .zip(&listed)
            .map(|(leaf, &listed)| listed.then(|| leaf.validity(rows)))
            .collect();
        let columns: Vec<Column> = leaves
            .iter()
            .zip(&held)
            .map(|(leaf, held)| match held {
                Some(bits) => Column {
                    values: Values::Any(&[]),
                    validity: Some((bits.as_slice(), 0)),
                },
                None => leaf.column(),
            })
            .collect();
        let mut parts: Vec<Part> = Vec::with_capacity(columns.len());
        for (at, column) in columns.iter().enumerate() {
            let hint = hints.get(at).and_then(Option::as_ref);
            parts.push(match (hint, column.values) {
                (Some((codes, size)), _) => Part::Codes(codes, *size, column.validity),
                (None, Values::Dict { keys, values }) => {
                    Part::Codes(keys, values.len(), column.validity)
                }
                (None, Values::Bool { bits, offset }) => Part::Bits(bits, offset, column.validity),
                _ => return None,
            });
        }
        Self::groups(&parts, positions, rows)
    }

    pub fn groups(
        parts: &[Part],
        positions: Option<&Rc<[usize]>>,
        rows: usize,
    ) -> Option<(Vec<usize>, Vec<u32>)> {
        let local = positions.map_or(rows, |p| p.len());
        let full = |validity: Option<(&[u64], usize)>| {
            validity.is_none_or(|(bits, offset)| {
                positions.is_none() && Bits::range(bits, offset, offset + local, true)
            })
        };
        let parts: Vec<Part> = parts
            .iter()
            .filter_map(|part| match *part {
                Part::Bits(bits, offset, validity) if positions.is_none() && full(validity) => {
                    let constant = Bits::range(bits, offset, offset + local, true)
                        || Bits::range(bits, offset, offset + local, false);
                    (!constant).then_some(Part::Bits(bits, offset, None))
                }
                Part::Codes(codes, size, validity) if full(validity) => {
                    Some(Part::Codes(codes, size, None))
                }
                Part::Codes(codes, size, validity) => Some(Part::Codes(codes, size, validity)),
                Part::Bits(bits, offset, validity) => Some(Part::Bits(bits, offset, validity)),
            })
            .collect();
        let parts = parts.as_slice();
        let mut strides = Vec::with_capacity(parts.len());
        let mut size = 1u64;
        for part in parts {
            let width = match part {
                Part::Codes(_, size, _) => *size as u64 + 1,
                Part::Bits(..) => 3,
            };
            strides.push(size);
            size = size.checked_mul(width)?;
        }
        let mut keys = vec![0u64; local];
        for (part, stride) in parts.iter().zip(&strides) {
            let validity = match part {
                Part::Codes(_, _, validity) | Part::Bits(_, _, validity) => *validity,
            };
            let valid =
                |row: usize| validity.is_none_or(|(bits, offset)| Bits::get(bits, offset + row));
            let code = |row: usize, code: i32| match valid(row) {
                true => u64::try_from(code).map_or(0, |k| k + 1),
                false => 0,
            };
            match part {
                Part::Codes(codes, ..)
                    if positions.is_none() && validity.is_none() && codes.len() >= local =>
                {
                    for (key, &code) in keys.iter_mut().zip(codes.iter()) {
                        *key += (code.max(-1) + 1) as u64 * stride;
                    }
                }
                Part::Codes(codes, ..) if positions.is_none() && codes.len() >= local => {
                    let valid = validity.map_or_else(
                        || Bits::ones(local),
                        |(bits, offset)| Bits::window(bits, offset, local),
                    );
                    for ((at, key), &code) in keys.iter_mut().enumerate().zip(codes.iter()) {
                        let live = (valid[at >> 6] >> (at & 63)) & 1;
                        *key += (code.max(-1) + 1) as u64 * live * stride;
                    }
                }
                Part::Codes(codes, ..) => {
                    for (at, key) in keys.iter_mut().enumerate() {
                        let row = positions.map_or(at, |p| p[at]);
                        *key += code(row, codes.get(row).copied().unwrap_or(-1)) * stride;
                    }
                }
                Part::Bits(bits, offset, None) if positions.is_none() => {
                    for (at, key) in keys.iter_mut().enumerate() {
                        let row = offset + at;
                        *key += (((bits[row >> 6] >> (row & 63)) & 1) + 1) * stride;
                    }
                }
                Part::Bits(bits, offset, _) => {
                    for (at, key) in keys.iter_mut().enumerate() {
                        let row = positions.map_or(at, |p| p[at]);
                        *key += code(row, i32::from(Bits::get(bits, offset + row))) * stride;
                    }
                }
            }
        }
        if size <= 4096 {
            let mut table = vec![u32::MAX; size as usize];
            let mut reps = Vec::new();
            let mut ids = vec![0u32; local];
            for ((at, &key), id) in keys.iter().enumerate().zip(ids.iter_mut()) {
                let entry = table.get_mut(key as usize)?;
                if *entry == u32::MAX {
                    *entry = reps.len() as u32;
                    reps.push(positions.map_or(at, |p| p[at]));
                    if reps.len() * 4 > local {
                        return None;
                    }
                }
                *id = *entry;
            }
            return Some((reps, ids));
        }
        let slot =
            |key: u64, mask: usize| (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & mask;
        let mut table = vec![(u64::MAX, u32::MAX); 64];
        let mut mask = table.len() - 1;
        let mut reps = Vec::new();
        let mut ids = vec![0u32; local];
        let mut last = (u64::MAX, u32::MAX);
        for ((at, &key), id) in keys.iter().enumerate().zip(ids.iter_mut()) {
            if last.1 != u32::MAX && last.0 == key {
                *id = last.1;
                continue;
            }
            let mut at_slot = slot(key, mask);
            while table[at_slot].1 != u32::MAX && table[at_slot].0 != key {
                at_slot = (at_slot + 1) & mask;
            }
            if table[at_slot].1 == u32::MAX {
                table[at_slot] = (key, reps.len() as u32);
                reps.push(positions.map_or(at, |p| p[at]));
                if reps.len() * 4 > local {
                    return None;
                }
                if reps.len() * 2 > table.len() {
                    let grown = table.len() * 2;
                    let old = std::mem::replace(&mut table, vec![(u64::MAX, u32::MAX); grown]);
                    mask = grown - 1;
                    for entry in old.into_iter().filter(|entry| entry.1 != u32::MAX) {
                        let mut into = slot(entry.0, mask);
                        while table[into].1 != u32::MAX {
                            into = (into + 1) & mask;
                        }
                        table[into] = entry;
                    }
                    at_slot = slot(key, mask);
                    while table[at_slot].0 != key {
                        at_slot = (at_slot + 1) & mask;
                    }
                }
            }
            last = table[at_slot];
            *id = last.1;
        }
        Some((reps, ids))
    }

    pub fn coded_rows(
        parts: &[Part],
        positions: Option<&[usize]>,
        rows: usize,
    ) -> Option<(Vec<usize>, Vec<u32>)> {
        let local = positions.map_or(rows, <[usize]>::len);
        if local < Self::MIN {
            return None;
        }
        let positions: Option<Rc<[usize]>> = positions.map(Rc::from);
        Self::groups(parts, positions.as_ref(), rows)
    }

    pub fn sampled(
        keys: &[Part],
        columns: &[Column],
        positions: Option<&[usize]>,
        rows: usize,
    ) -> bool {
        let local = positions.map_or(rows, <[usize]>::len);
        let sample = local.min(Self::SAMPLE);
        if sample == 0 {
            return false;
        }
        let stride = local / sample;
        let hash = |at: usize| {
            let row = positions.map_or(at * stride, |p| p[at * stride]);
            let mut hash = 0u64;
            for part in keys {
                let value = match part {
                    Part::Codes(codes, _, validity) => {
                        let valid =
                            validity.is_none_or(|(bits, offset)| Bits::get(bits, offset + row));
                        match (valid, codes.get(row)) {
                            (true, Some(&code)) if code >= 0 => code as u64,
                            _ => u64::MAX,
                        }
                    }
                    Part::Bits(bits, offset, validity) => {
                        let valid =
                            validity.is_none_or(|(bits, offset)| Bits::get(bits, offset + row));
                        match valid {
                            true => u64::from(Bits::get(bits, offset + row)),
                            false => u64::MAX,
                        }
                    }
                };
                hash = Self::fold(hash, value);
            }
            for column in columns {
                let value = match column.values {
                    Values::List { offsets, child } => {
                        let child = child.column();
                        match (column.valid(row), offsets.get(row), offsets.get(row + 1)) {
                            (true, Some(&lo), Some(&hi)) => (lo.max(0) as usize
                                ..hi.max(0) as usize)
                                .fold((hi - lo) as u64, |hash, item| {
                                    Self::fold(hash, Self::item(&child, item).unwrap_or(u64::MAX))
                                }),
                            _ => u64::MAX,
                        }
                    }
                    _ => Self::item(column, row).unwrap_or(u64::MAX),
                };
                hash = Self::fold(hash, value);
            }
            hash
        };
        let head = sample / 4;
        if head >= 64 {
            let mut seen: Vec<u64> = (0..head).map(hash).collect();
            seen.sort_unstable();
            seen.dedup();
            if seen.len() * 4 > head * 3 {
                return false;
            }
        }
        let mut hashes: Vec<u64> = (0..sample).map(hash).collect();
        hashes.sort_unstable();
        hashes.dedup();
        hashes.len() * 4 <= sample
    }

    pub fn interned(column: &Column, rows: usize) -> Hint {
        match column.values {
            Values::Scaled { .. } | Values::I64(_) => Self::numbered(column, rows),
            Values::F64(values) => {
                let bits: Vec<i64> = values
                    .get(..rows)?
                    .iter()
                    .map(|v| v.to_bits() as i64)
                    .collect();
                Self::numbered(&Column::new(Values::I64(&bits)), rows)
            }
            Values::Dec(values) => Self::decimals(values.get(..rows)?),
            Values::Text { offsets, data } => Coder::keys(data.as_bytes(), offsets.get(..=rows)?),
            Values::Utf8 { offsets, data } => Coder::keys(data, offsets.get(..=rows)?),
            _ => None,
        }
    }

    pub fn listed(column: &Column, rows: usize) -> Hint {
        const SLOTS: usize = 256;
        const LIMIT: usize = 128;
        let Values::List { offsets, child } = column.values else {
            return None;
        };
        let child = child.column();
        if !Self::keyable(&child) {
            return None;
        }
        let range = |row: usize| -> Option<(usize, usize)> {
            match column.valid(row) {
                true => Some((
                    usize::try_from(*offsets.get(row)?).ok()?,
                    usize::try_from(*offsets.get(row + 1)?).ok()?,
                )),
                false => None,
            }
        };
        let mut table = [(0u64, u32::MAX, 0u8); SLOTS];
        let mut count = 0usize;
        let mut codes: Vec<i32> = Vec::with_capacity(rows);
        for row in 0..rows {
            let span = range(row);
            let mut hash = 0xcbf2_9ce4_8422_2325u64;
            match span {
                None => hash = Self::fold(hash, u64::MAX),
                Some((lo, hi)) => {
                    hash = Self::fold(hash, (hi - lo) as u64);
                    for at in lo..hi {
                        hash = Self::fold(hash, Self::item(&child, at)?);
                    }
                }
            }
            let mut slot = (hash.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as usize;
            let code = loop {
                let (seen, rep, code) = table[slot];
                if rep == u32::MAX {
                    if count >= LIMIT {
                        return None;
                    }
                    table[slot] = (hash, row as u32, count as u8);
                    count += 1;
                    break i32::from(count as u8 - 1);
                }
                if seen == hash {
                    let same = match (range(rep as usize), span) {
                        (None, None) => true,
                        (Some((a, b)), Some((c, d))) => {
                            b - a == d - c && (0..b - a).all(|i| Self::same(&child, a + i, c + i))
                        }
                        _ => false,
                    };
                    if same {
                        break i32::from(code);
                    }
                }
                slot = (slot + 1) % SLOTS;
            };
            codes.push(code);
        }
        Some((codes.into(), count))
    }

    #[inline(always)]
    fn fold(hash: u64, value: u64) -> u64 {
        (hash.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95)
    }

    fn item(child: &Column, at: usize) -> Option<u64> {
        if !child.valid(at) {
            return Some(u64::MAX - 1);
        }
        Some(match child.values {
            Values::Dec(values) => {
                let raw = u128::from_le_bytes(values.get(at)?.serialize());
                (raw as u64) ^ ((raw >> 64) as u64).rotate_left(17)
            }
            Values::Scaled { mant, scale } => {
                (*mant.get(at)? as u64) ^ u64::from(*scale.get(at)?).rotate_left(56)
            }
            Values::I64(values) => *values.get(at)? as u64,
            Values::F64(values) => values.get(at)?.to_bits(),
            Values::Bool { bits, offset } => u64::from(Bits::get(bits, offset + at)),
            Values::Text { .. } | Values::Utf8 { .. } => {
                let bytes = child.text(at)?.as_bytes();
                let mut chunks = bytes.chunks_exact(8);
                let mut hash = bytes.len() as u64;
                for chunk in chunks.by_ref() {
                    hash = Self::fold(hash, u64::from_le_bytes(chunk.try_into().ok()?));
                }
                let tail = chunks
                    .remainder()
                    .iter()
                    .enumerate()
                    .fold(0u64, |word, (at, &byte)| word | u64::from(byte) << (8 * at));
                Self::fold(hash, tail)
            }
            _ => return None,
        })
    }

    fn keyable(child: &Column) -> bool {
        matches!(
            child.values,
            Values::Dec(_)
                | Values::Scaled { .. }
                | Values::I64(_)
                | Values::F64(_)
                | Values::Bool { .. }
                | Values::Text { .. }
                | Values::Utf8 { .. }
        )
    }

    pub fn list_weight(column: &Column, rows: usize, text: usize) -> Option<usize> {
        let Values::List { offsets, child } = column.values else {
            return None;
        };
        let child = child.column();
        if !Self::keyable(&child) {
            return None;
        }
        let first = usize::try_from(*offsets.first()?).ok()?;
        let last = usize::try_from(*offsets.get(rows)?).ok()?;
        let items = last.saturating_sub(first).div_ceil(rows.max(1));
        let each = match child.values {
            Values::Text { .. } | Values::Utf8 { .. } => text,
            _ => 1,
        };
        Some(each * (items + 1))
    }

    fn same(child: &Column, a: usize, b: usize) -> bool {
        let (va, vb) = (child.valid(a), child.valid(b));
        if !va || !vb {
            return va == vb;
        }
        match child.values {
            Values::Dec(values) => values[a].serialize() == values[b].serialize(),
            Values::Scaled { mant, scale } => mant[a] == mant[b] && scale[a] == scale[b],
            Values::I64(values) => values[a] == values[b],
            Values::F64(values) => values[a].to_bits() == values[b].to_bits(),
            Values::Bool { bits, offset } => {
                Bits::get(bits, offset + a) == Bits::get(bits, offset + b)
            }
            Values::Text { .. } | Values::Utf8 { .. } => child.text(a) == child.text(b),
            _ => false,
        }
    }

    fn decimals(values: &[rust_decimal::Decimal]) -> Hint {
        const SLOTS: usize = 256;
        const LIMIT: usize = 128;
        let mut table = [(0u128, u8::MAX); SLOTS];
        let mut count = 0usize;
        let mut codes: Vec<i32> = Vec::with_capacity(values.len());
        let mut last: Option<(u128, i32)> = None;
        for value in values {
            let raw = u128::from_le_bytes(value.serialize());
            if let Some((_, code)) = last.filter(|(seen, _)| *seen == raw) {
                codes.push(code);
                continue;
            }
            let mut slot = (((raw as u64) ^ (raw >> 64) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                >> 56) as usize;
            let code = loop {
                let entry = &mut table[slot];
                if entry.1 == u8::MAX {
                    if count >= LIMIT {
                        return None;
                    }
                    *entry = (raw, count as u8);
                    count += 1;
                    break i32::from(entry.1);
                }
                if entry.0 == raw {
                    break i32::from(entry.1);
                }
                slot = (slot + 1) % SLOTS;
            };
            last = Some((raw, code));
            codes.push(code);
        }
        Some((codes.into(), count))
    }

    pub fn numbered(column: &Column, rows: usize) -> Option<(Rc<[i32]>, usize)> {
        const SLOTS: usize = 256;
        const LIMIT: usize = 128;
        let (mant, scale) = match column.values {
            Values::Scaled { mant, scale } => (mant.get(..rows)?, Some(scale.get(..rows)?)),
            Values::I64(values) => (values.get(..rows)?, None),
            _ => return None,
        };
        let mut table = [(0i64, 0u8, u8::MAX); SLOTS];
        let mut count = 0usize;
        let mut codes: Vec<i32> = Vec::with_capacity(rows);
        let mut last: Option<(i64, u8, i32)> = None;
        for (at, &m) in mant.iter().enumerate() {
            let s = scale.map_or(0, |scale| scale[at]);
            if let Some((_, _, code)) = last.filter(|(lm, ls, _)| *lm == m && *ls == s) {
                codes.push(code);
                continue;
            }
            let mut slot = ((m as u64 ^ u64::from(s) << 56).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                >> 56) as usize;
            let code = loop {
                let entry = &mut table[slot];
                if entry.2 == u8::MAX {
                    if count >= LIMIT {
                        return None;
                    }
                    *entry = (m, s, count as u8);
                    count += 1;
                    break i32::from(entry.2);
                }
                if entry.0 == m && entry.1 == s {
                    break i32::from(entry.2);
                }
                slot = (slot + 1) % SLOTS;
            };
            last = Some((m, s, code));
            codes.push(code);
        }
        Some((codes.into(), count))
    }
}
