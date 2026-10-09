use crate::lane::date::Date;
use crate::lane::{Column, Values};
use crate::typed::hir::{ColumnDef, Dict, Hir, Kind, Unsupported};
use crate::typed::kernel::K;
use crate::typed::num::Dec;
use crate::typed::output::Outputs;
use rust_decimal::Decimal;
use std::borrow::Cow;

#[derive(Clone, Debug, Default)]
pub struct Batch<'b> {
    rows: usize,
    columns: Vec<Column<'b>>,
    lists: Vec<List<'b>>,
}

#[derive(Clone, Debug)]
pub struct List<'b> {
    offsets: &'b [i32],
    validity: Option<(&'b [u64], usize)>,
    faults: Option<(&'b [u64], usize)>,
    fields: Vec<Column<'b>>,
}

#[derive(Clone, Debug)]
pub(crate) enum Scales<'x> {
    Fixed(u8),
    Each(Cow<'x, [u8]>),
}

#[derive(Clone, Debug)]
pub(crate) enum Text<'x> {
    Str { offsets: &'x [i32], data: &'x [u8] },
    Dict { keys: &'x [i32], offsets: &'x [i32], data: &'x [u8] },
}

#[derive(Clone, Debug)]
pub(crate) enum Data<'x> {
    Num { mant: Cow<'x, [i64]>, scale: Scales<'x> },
    Bool { bits: Cow<'x, [u64]>, offset: usize },
    Codes(Vec<i32>),
    Text(Text<'x>),
}

#[derive(Clone, Debug)]
pub(crate) struct Feed<'x> {
    pub(crate) data: Data<'x>,
    pub(crate) validity: Option<(&'x [u64], usize)>,
    pub(crate) holes: Option<Vec<u64>>,
    pub(crate) faults: Option<Vec<u64>>,
    pub(crate) wide: Option<(Vec<u64>, Vec<(usize, Decimal)>)>,
    pub(crate) fixed: Option<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct Lists {
    pub(crate) offsets: Vec<i32>,
    pub(crate) parent: Vec<u32>,
    pub(crate) root: Vec<u32>,
    pub(crate) null: Option<Vec<u64>>,
    pub(crate) faulted: Option<Vec<u64>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Feeds<'x> {
    pub(crate) rows: usize,
    pub(crate) columns: Vec<Feed<'x>>,
    pub(crate) fields: Vec<Vec<Feed<'x>>>,
    pub(crate) classes: Vec<Feed<'x>>,
    pub(crate) lists: Vec<Lists>,
    pub(crate) parents: Vec<Option<u16>>,
    pub(crate) now: Date,
}

#[derive(Clone, Debug)]
pub(crate) struct Matcher<'d> {
    keys: Vec<(usize, u64)>,
    lengths: u64,
    values: Cow<'d, [Box<str>]>,
    other: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Col(u16),
    Field(u16, u16),
}

impl<'b> Batch<'b> {
    pub fn new(rows: usize) -> Self {
        Self { rows, columns: Vec::new(), lists: Vec::new() }
    }

    pub fn column(mut self, column: Column<'b>) -> Self {
        self.columns.push(column);
        self
    }

    pub fn list(mut self, list: List<'b>) -> Self {
        self.lists.push(list);
        self
    }

    pub fn push_column(&mut self, column: Column<'b>) {
        self.columns.push(column);
    }

    pub fn push_list(&mut self, list: List<'b>) {
        self.lists.push(list);
    }

    pub fn rows(&self) -> usize {
        self.rows
    }
}

impl<'b> List<'b> {
    pub fn new(offsets: &'b [i32], validity: Option<(&'b [u64], usize)>, fields: Vec<Column<'b>>) -> Self {
        Self { offsets, validity, faults: None, fields }
    }

    pub fn faults(self, bits: &'b [u64], offset: usize) -> Self {
        Self { faults: Some((bits, offset)), ..self }
    }
}

impl Lists {
    pub(crate) fn len(&self) -> usize {
        self.parent.len()
    }

    pub(crate) fn owners(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }
}

impl<'x> Text<'x> {
    #[inline]
    pub(crate) fn get(&self, at: usize) -> Option<&'x [u8]> {
        let entry = match self {
            Text::Str { offsets, data } => (offsets, data, at),
            Text::Dict { keys, offsets, data } => (offsets, data, usize::try_from(*keys.get(at)?).ok()?),
        };
        let (offsets, data, i) = entry;
        let (lo, hi) = (usize::try_from(*offsets.get(i)?).ok()?, usize::try_from(*offsets.get(i + 1)?).ok()?);
        data.get(lo..hi)
    }
}

impl<'x> Feed<'x> {
    #[inline]
    pub(crate) fn valid_word(&self, at: usize, width: usize) -> u64 {
        let valid = match self.validity {
            None => match width >= 64 {
                true => u64::MAX,
                false => (1u64 << width) - 1,
            },
            Some((bits, offset)) => Column::word(bits, offset + at, width),
        };
        match &self.holes {
            None => valid,
            Some(holes) => valid & !Column::word(holes, at, width),
        }
    }

    #[inline]
    pub(crate) fn valid(&self, at: usize) -> bool {
        self.valid_word(at, 1) == 1
    }

    #[inline]
    pub(crate) fn fault_word(&self, w: usize) -> u64 {
        self.faults.as_ref().and_then(|f| f.get(w)).copied().unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn num(&self, at: usize) -> Option<Dec> {
        match &self.data {
            Data::Num { mant, scale } => {
                let m = *mant.get(at)?;
                let s = match scale {
                    Scales::Fixed(s) => *s,
                    Scales::Each(each) => *each.get(at)?,
                };
                Some(Dec::new(m, s))
            }
            _ => None,
        }
    }

    pub(crate) fn text(&self) -> Option<&Text<'x>> {
        match &self.data {
            Data::Text(t) => Some(t),
            _ => None,
        }
    }

    fn new(data: Data<'x>, validity: Option<(&'x [u64], usize)>) -> Self {
        Self { data, validity, holes: None, faults: None, wide: None, fixed: None }
    }

    fn fault(&mut self, at: usize, len: usize) {
        let faults = self.faults.get_or_insert_with(|| vec![0u64; K::words(len)]);
        K::set(faults, at, true);
    }

    fn hole(&mut self, at: usize, len: usize) {
        let holes = self.holes.get_or_insert_with(|| vec![0u64; K::words(len)]);
        K::set(holes, at, true);
    }

    fn of(column: &Column<'x>, def: &ColumnDef, base: usize, len: usize) -> Result<Self, Unsupported> {
        let short = || Unsupported::new(format!("column {} is shorter than its rows", def.path));
        if column.len() < base + len {
            return Err(short());
        }
        let validity = column.validity.map(|(bits, offset)| (bits, offset + base));
        let feed = match (def.ty.kind, column.values) {
            (Kind::Num(_), Values::Scaled { mant, scale }) if scale.len() >= base + len => {
                let feed = Self::new(
                    Data::Num { mant: Cow::Borrowed(&mant[base..base + len]), scale: Scales::Each(Cow::Borrowed(&scale[base..base + len])) },
                    validity,
                );
                feed.scan(len)
            }
            (Kind::Num(_), Values::I64(values)) => {
                let mut feed = Self::new(Data::Num { mant: Cow::Borrowed(&values[base..base + len]), scale: Scales::Fixed(0) }, validity);
                feed.fixed = Some(0);
                feed
            }
            (Kind::Num(_), Values::Dec(values)) => {
                let mut mant = Vec::with_capacity(len);
                let mut scale = Vec::with_capacity(len);
                let mut wide: Vec<(usize, Decimal)> = Vec::new();
                for (i, d) in values[base..base + len].iter().enumerate() {
                    let parts = Dec::from_decimal(d);
                    if parts.is_none() && column.valid(base + i) {
                        wide.push((i, *d));
                    }
                    let p = parts.unwrap_or(Dec::ZERO);
                    mant.push(p.mant);
                    scale.push(p.scale);
                }
                let mut feed = Self::new(Data::Num { mant: Cow::Owned(mant), scale: Scales::Each(Cow::Owned(scale)) }, validity);
                if !wide.is_empty() {
                    let mut bits = vec![0u64; K::words(len)];
                    wide.iter().for_each(|(i, _)| K::set(&mut bits, *i, true));
                    feed.wide = Some((bits, wide));
                }
                feed.scan(len)
            }
            (Kind::Num(_), Values::Dict { keys, values }) => {
                let dict = values.column();
                let Values::Scaled { mant: dm, scale: ds } = dict.values else {
                    return Err(Unsupported::new(format!("column {}: numeric dictionary", def.path)));
                };
                let mut mant = Vec::with_capacity(len);
                let mut scale = Vec::with_capacity(len);
                let mut bad = Vec::new();
                for (i, k) in keys[base..base + len].iter().enumerate() {
                    let entry = usize::try_from(*k).ok().and_then(|k| Some((*dm.get(k)?, *ds.get(k)?, dict.valid(k))));
                    let (m, s, ok) = entry.unwrap_or((0, 0, false));
                    if !ok && column.valid(base + i) {
                        bad.push(i);
                    }
                    mant.push(m);
                    scale.push(s);
                }
                let mut feed = Self::new(Data::Num { mant: Cow::Owned(mant), scale: Scales::Each(Cow::Owned(scale)) }, validity);
                for i in bad {
                    feed.hole(i, len);
                }
                feed.scan(len)
            }
            (Kind::Bool, Values::Bool { bits, offset }) if bits.len() * 64 >= offset + base + len => {
                Self::new(Data::Bool { bits: Cow::Borrowed(bits), offset: offset + base }, validity)
            }
            (Kind::Bool, Values::Dict { keys, values }) => {
                let dict = values.column();
                let mut bits = vec![0u64; K::words(len)];
                let mut bad = Vec::new();
                let mut holes = Vec::new();
                for (i, k) in keys[base..base + len].iter().enumerate() {
                    let entry = usize::try_from(*k).ok().filter(|k| *k < dict.len() && dict.valid(*k));
                    match entry.map(|k| dict.boolean(k)) {
                        Some(Some(b)) => K::set(&mut bits, i, b),
                        Some(None) if column.valid(base + i) => bad.push(i),
                        None if column.valid(base + i) => holes.push(i),
                        _ => {}
                    }
                }
                let mut feed = Self::new(Data::Bool { bits: Cow::Owned(bits), offset: 0 }, validity);
                for i in bad {
                    feed.fault(i, len);
                }
                for i in holes {
                    feed.hole(i, len);
                }
                feed
            }
            (Kind::Text, Values::Text { offsets, data }) if offsets.len() > base + len => {
                Self::new(Data::Text(Text::Str { offsets: &offsets[base..], data: data.as_bytes() }), validity)
            }
            (Kind::Text, Values::Utf8 { offsets, data }) if offsets.len() > base + len => {
                Self::new(Data::Text(Text::Str { offsets: &offsets[base..], data }), validity)
            }
            (Kind::Text, Values::Dict { keys, values }) => {
                let dict = values.column();
                let (offsets, data) = match dict.values {
                    Values::Text { offsets, data } => (offsets, data.as_bytes()),
                    Values::Utf8 { offsets, data } => (offsets, data),
                    _ => return Err(Unsupported::new(format!("column {}: text dictionary", def.path))),
                };
                let entries = offsets.len().saturating_sub(1);
                let keys = &keys[base..base + len];
                let mut feed = Self::new(Data::Text(Text::Dict { keys, offsets, data }), validity);
                let (lo, hi) = keys.iter().fold((i32::MAX, i32::MIN), |(lo, hi), k| (lo.min(*k), hi.max(*k)));
                let clean = keys.is_empty() || (lo >= 0 && (hi as usize) < entries && dict.validity.is_none());
                let bad: Vec<usize> = match clean {
                    true => Vec::new(),
                    false => keys
                        .iter()
                        .enumerate()
                        .filter(|(i, k)| usize::try_from(**k).map_or(true, |k| k >= entries || !dict.valid(k)) && column.valid(base + i))
                        .map(|(i, _)| i)
                        .collect(),
                };
                for i in bad {
                    feed.hole(i, len);
                }
                feed
            }
            (kind, _) => return Err(Unsupported::new(format!("column {}: values do not fit {kind}", def.path))),
        };
        Ok(feed)
    }

    fn scan(mut self, len: usize) -> Self {
        let Data::Num { scale: Scales::Each(each), .. } = &self.data else {
            return self;
        };
        let mut fixed: Option<u8> = None;
        let mut mixed = false;
        let plain = self.validity.is_none() && self.holes.is_none() && self.faults.is_none() && self.wide.is_none();
        if plain && !each.is_empty() {
            let s = each[0];
            fixed = Some(s);
            mixed = each.iter().fold(0u8, |acc, x| acc | (x ^ s)) != 0;
        }
        for (w, chunk) in each.chunks(64).enumerate().filter(|_| !plain) {
            let wide = self.wide.as_ref().map_or(0, |(bits, _)| bits.get(w).copied().unwrap_or(0));
            let mut valid = self.valid_word(w * 64, chunk.len()) & !self.fault_word(w) & !wide;
            if valid == 0 {
                continue;
            }
            if valid == u64::MAX >> (64 - chunk.len()) {
                let s = *fixed.get_or_insert(chunk[0]);
                if chunk.iter().fold(0u8, |acc, x| acc | (x ^ s)) != 0 {
                    mixed = true;
                    break;
                }
                continue;
            }
            while valid != 0 {
                let i = valid.trailing_zeros() as usize;
                valid &= valid - 1;
                let s = *fixed.get_or_insert(chunk[i]);
                if chunk[i] != s {
                    mixed = true;
                    break;
                }
            }
            if mixed {
                break;
            }
        }
        self.fixed = match mixed {
            true => None,
            false => Some(fixed.unwrap_or(0)),
        };
        if len == 0 {
            self.fixed = Some(0);
        }
        self
    }

    fn classify(&self, dict: &Dict, len: usize) -> Feed<'x> {
        let matcher = Matcher::new(dict);
        let codes = match &self.data {
            Data::Text(Text::Dict { keys, offsets, data }) => {
                let entries = offsets.len().saturating_sub(1);
                let remap: Vec<i32> = (0..entries).map(|i| matcher.code(Text::Str { offsets, data }.get(i))).collect();
                keys.iter().map(|k| usize::try_from(*k).ok().and_then(|k| remap.get(k)).copied().unwrap_or(matcher.other)).collect()
            }
            Data::Text(Text::Str { offsets, data }) => match offsets.get(..=len) {
                Some(offsets) => offsets.windows(2).map(|w| matcher.at(data, w[0] as usize, w[1] as usize)).collect(),
                None => vec![matcher.other; len],
            },
            _ => vec![matcher.other; len],
        };
        Feed { data: Data::Codes(codes), validity: self.validity, holes: self.holes.clone(), faults: self.faults.clone(), wide: None, fixed: None }
    }
}

impl<'d> Matcher<'d> {
    pub(crate) fn new(dict: &'d Dict) -> Self {
        let keys = dict.values.iter().map(|v| (v.len(), Self::key(v.as_bytes()))).collect();
        let lengths = dict.values.iter().fold(0u64, |mask, v| mask | 1u64 << v.len().min(63));
        Self { keys, lengths, values: Cow::Borrowed(&dict.values), other: dict.values.len() as i32 }
    }

    pub(crate) fn owned(dict: &Dict) -> Matcher<'static> {
        let Matcher { keys, lengths, values, other } = Matcher::new(dict);
        Matcher { keys, lengths, values: Cow::Owned(values.into_owned()), other }
    }

    #[inline]
    fn key(bytes: &[u8]) -> u64 {
        match bytes.first_chunk::<8>() {
            Some(head) => u64::from_le_bytes(*head),
            None => bytes.iter().enumerate().fold(0u64, |key, (i, b)| key | u64::from(*b) << (8 * i)),
        }
    }

    #[inline]
    fn at(&self, data: &[u8], lo: usize, hi: usize) -> i32 {
        let len = hi.wrapping_sub(lo);
        if hi > data.len() || lo > hi || self.lengths & 1u64 << len.min(63) == 0 {
            return self.other;
        }
        let key = match (len < 8, data.get(lo..).and_then(<[u8]>::first_chunk::<8>)) {
            (true, Some(word)) => u64::from_le_bytes(*word) & ((1u64 << (8 * len)) - 1),
            _ => Self::key(&data[lo..hi]),
        };
        for (i, k) in self.keys.iter().enumerate() {
            if *k == (len, key) && (len <= 8 || self.values[i].as_bytes() == &data[lo..hi]) {
                return i as i32;
            }
        }
        self.other
    }

    #[inline]
    pub(crate) fn code(&self, bytes: Option<&[u8]>) -> i32 {
        let Some(bytes) = bytes else {
            return self.other;
        };
        let len = bytes.len();
        if self.lengths & 1u64 << len.min(63) == 0 {
            return self.other;
        }
        let key = Self::key(bytes);
        for (i, k) in self.keys.iter().enumerate() {
            if *k == (len, key) && (len <= 8 || self.values[i].as_bytes() == bytes) {
                return i as i32;
            }
        }
        self.other
    }
}

impl<'x> Feeds<'x> {
    pub(crate) fn new(hir: &Hir, classes: &[(Origin, u16)], batch: &Batch<'x>) -> Result<Self, Unsupported> {
        let schema = &hir.schema;
        if batch.columns.len() != schema.columns.len() || batch.lists.len() != schema.lists.len() {
            return Err(Unsupported::new("batch does not match the program schema"));
        }
        let rows = batch.rows;
        let mut columns = Vec::with_capacity(batch.columns.len());
        for (column, def) in batch.columns.iter().zip(&schema.columns) {
            columns.push(Feed::of(column, def, 0, rows)?);
        }
        let mut fields = Vec::with_capacity(batch.lists.len());
        let mut lists = Vec::with_capacity(batch.lists.len());
        for (list, def) in batch.lists.iter().zip(&schema.lists) {
            let owners = match def.parent {
                None => rows,
                Some(p) => lists.get(p as usize).map(Lists::len).ok_or_else(|| Unsupported::new(format!("list {} before its parent", def.path)))?,
            };
            if list.fields.len() != def.fields.len() || list.offsets.len() <= owners {
                return Err(Unsupported::new(format!("list {} does not match the program schema", def.path)));
            }
            let base = list.offsets[0];
            let sorted = base >= 0 && list.offsets[..=owners].windows(2).fold(true, |ok, w| ok & (w[0] <= w[1]));
            let total = usize::try_from(list.offsets[owners] as i64 - base as i64).ok().filter(|_| sorted).ok_or_else(|| Unsupported::new("list offsets"))?;
            if list.fields.iter().any(|c| c.len() < base as usize + total) {
                return Err(Unsupported::new(format!("list {} is shorter than its offsets", def.path)));
            }
            let offsets: Vec<i32> = list.offsets[..=owners].iter().map(|o| o - base).collect();
            let mut parent = vec![0u32; total];
            for (r, w) in offsets.windows(2).enumerate() {
                parent[w[0] as usize..w[1] as usize].fill(r as u32);
            }
            let root = match def.parent.and_then(|p| lists.get(p as usize)) {
                None => parent.clone(),
                Some(up) => parent.iter().map(|&o| up.root.get(o as usize).copied().unwrap_or(0)).collect(),
            };
            let base = base as usize;
            let marks = |bits: Option<(&[u64], usize)>, on: bool| -> Option<Vec<u64>> {
                let (bits, offset) = bits?;
                let mut out: Option<Vec<u64>> = None;
                for o in 0..owners {
                    if K::get(bits, offset + o) == on {
                        K::set(out.get_or_insert_with(|| vec![0u64; K::words(owners)]), o, true);
                    }
                }
                out
            };
            let null = marks(list.validity, false);
            if let Some(null) = &null {
                if (0..owners).any(|o| K::get(null, o) && offsets[o] != offsets[o + 1]) {
                    return Err(Unsupported::new(format!("list {} has elements under null entries", def.path)));
                }
            }
            let faulted = marks(list.faults, true);
            let len = parent.len();
            let mut feeds = Vec::with_capacity(list.fields.len());
            for (column, def) in list.fields.iter().zip(&def.fields) {
                feeds.push(Feed::of(column, def, base, len)?);
            }
            fields.push(feeds);
            lists.push(Lists { offsets, parent, root, null, faulted });
        }
        let parents = schema.lists.iter().map(|l| l.parent).collect();
        let mut feeds = Self { rows, columns, fields, classes: Vec::with_capacity(classes.len()), lists, parents, now: Date(None) };
        for (origin, dict) in classes {
            let class = {
                let (source, len) = match *origin {
                    Origin::Col(c) => (feeds.columns.get(c as usize), rows),
                    Origin::Field(l, f) => (feeds.fields.get(l as usize).and_then(|x| x.get(f as usize)), feeds.lists.get(l as usize).map_or(0, Lists::len)),
                };
                let source = source.ok_or_else(|| Unsupported::new("classify source"))?;
                source.classify(hir.dict(*dict), len)
            };
            feeds.classes.push(class);
        }
        Ok(feeds)
    }

    pub(crate) fn prefaults(&self, out: &mut Outputs, select: Option<&[u64]>) {
        let mut rows: Vec<usize> = Vec::new();
        for feed in &self.columns {
            if let Some(faults) = &feed.faults {
                rows.extend((0..self.rows).filter(|&r| K::get(faults, r)));
            }
        }
        for (at, (list, feeds)) in self.lists.iter().zip(&self.fields).enumerate() {
            for feed in feeds {
                if let Some(faults) = &feed.faults {
                    rows.extend((0..list.len()).filter(|&e| K::get(faults, e)).map(|e| list.root[e] as usize));
                }
            }
            if let Some(faulted) = &list.faulted {
                rows.extend((0..list.owners()).filter(|&o| K::get(faulted, o)).map(|o| self.row_of(at, o)));
            }
        }
        rows.retain(|&r| select.is_none_or(|sel| K::get(sel, r)));
        if rows.is_empty() {
            return;
        }
        let (slots, faults) = out.parts();
        for &row in &rows {
            faults.set(row);
            for slot in slots.iter_mut() {
                slot.fault.set(row);
            }
        }
    }

    pub(crate) fn sig(&self, hir: &Hir, into: &mut Vec<Option<u8>>) {
        into.clear();
        for (feed, def) in self.columns.iter().zip(&hir.schema.columns) {
            into.push(Self::fixed(def.ty.kind, feed));
        }
        for (list, def) in self.fields.iter().zip(&hir.schema.lists) {
            for (feed, field) in list.iter().zip(&def.fields) {
                into.push(Self::fixed(field.ty.kind, feed));
            }
        }
    }

    fn fixed(kind: Kind, feed: &Feed) -> Option<u8> {
        match kind {
            Kind::Num(None) => feed.fixed,
            _ => None,
        }
    }

    pub(crate) fn row_of(&self, list: usize, owner: usize) -> usize {
        let up = self.parents.get(list).copied().flatten();
        match up.and_then(|p| self.lists.get(p as usize)) {
            Some(parent) => parent.root.get(owner).map_or(owner, |r| *r as usize),
            None => owner,
        }
    }

    pub(crate) fn len(&self, scope: crate::typed::hir::Scope) -> usize {
        match scope {
            crate::typed::hir::Scope::Row => self.rows,
            crate::typed::hir::Scope::Elem(l) => self.lists.get(l as usize).map_or(0, Lists::len),
        }
    }
}
