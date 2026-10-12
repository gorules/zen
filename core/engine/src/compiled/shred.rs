use crate::compiled::core::list::Frame;
use crate::compiled::leaf::{Array, Bits, Dict, Leaf, Records, Store};
use crate::nodes::input::dates::DeclaredDates;
use std::cell::Ref;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Dictionary, Values};
use zen_expression::typed::{Class, ColumnDef};
use zen_expression::Variable;
use zen_types::variable::Shape;

pub(crate) struct Normalizer;

pub(crate) type Shredded<'a> = (Arc<str>, Leaf<'a>, Option<Rc<[u64]>>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Seen {
    Num,
    Bool,
    Text,
    Null,
    List,
    Mixed,
}

struct Coder<'d> {
    data: &'d [u8],
    slots: [u8; Coder::SLOTS],
    entries: Vec<((u64, u64, usize), usize)>,
}

struct Interner {
    slots: [u8; Coder::SLOTS],
    keys: Vec<(u64, u64, usize)>,
    offsets: Vec<i32>,
    data: String,
}

struct Trie {
    path: Arc<str>,
    children: Vec<(Arc<str>, usize)>,
    column: Option<usize>,
}

pub(crate) struct Shredder {
    rows: usize,
    nodes: Vec<Trie>,
    columns: Vec<(Arc<str>, Vec<Variable>, Vec<u64>)>,
}

pub(crate) struct Listing<'a> {
    pub frame: Frame,
    pub hosted: Option<Rc<[u64]>>,
    pub fields: Vec<(Arc<str>, Leaf<'a>)>,
}

pub(crate) struct Shred {
    pub frame: Frame,
    pub fields: Vec<(Arc<str>, Array)>,
    pub hosted: Rc<[usize]>,
}

impl Shredder {
    pub(crate) fn new(rows: usize) -> Self {
        Self {
            rows,
            nodes: vec![Trie {
                path: Arc::from(""),
                children: Vec::new(),
                column: None,
            }],
            columns: Vec::new(),
        }
    }

    fn child(&mut self, node: usize, key: &str) -> usize {
        if let Some((_, at)) = self.nodes[node].children.iter().find(|(k, _)| k.as_ref() == key) {
            return *at;
        }
        let path: Arc<str> = match self.nodes[node].path.is_empty() {
            true => Arc::from(key),
            false => Arc::from(format!("{}.{key}", self.nodes[node].path)),
        };
        self.nodes.push(Trie {
            path,
            children: Vec::new(),
            column: None,
        });
        let at = self.nodes.len() - 1;
        self.nodes[node].children.push((Arc::from(key), at));
        at
    }

    fn put(&mut self, node: usize, row: usize, value: Variable) {
        let column = match self.nodes[node].column {
            Some(column) => column,
            None => {
                self.columns.push((self.nodes[node].path.clone(), vec![Variable::Null; self.rows], vec![0u64; self.rows.div_ceil(64)]));
                self.nodes[node].column = Some(self.columns.len() - 1);
                self.columns.len() - 1
            }
        };
        let (_, values, present) = &mut self.columns[column];
        values[row] = value;
        Bits::set(present, row, true);
    }

    pub(crate) fn visit(&mut self, value: &Variable, node: usize, row: usize) {
        match value {
            Variable::Object(map) => {
                let map = map.borrow();
                if map.iter().any(|(key, _)| key.is_empty() || key.contains('.')) {
                    self.put(node, row, value.clone());
                    return;
                }
                for (key, child) in map.iter() {
                    let at = self.child(node, key);
                    self.visit(child, at, row);
                }
            }
            Variable::Null => {}
            other => self.put(node, row, other.clone()),
        }
    }

    pub(crate) fn finish<'a>(self) -> Vec<Shredded<'a>> {
        let rows = self.rows;
        self.columns
            .into_iter()
            .map(|(path, values, present)| {
                let mut field = FieldBuilder::new(path.clone(), 0, false);
                for (row, value) in values.iter().enumerate() {
                    match Bits::get(&present, row) {
                        true => field.push(value, rows),
                        false => field.missing(),
                    }
                }
                let (_, array) = field.array();
                (path, Leaf::typed(array), Some(present.into()))
            })
            .collect()
    }
}

impl Normalizer {
    const SAMPLE: usize = 64;

    pub(crate) fn input<'a>(column: Column<'a>, rows: usize) -> Leaf<'a> {
        let dated = DeclaredDates::dated;
        let nested = match column.values {
            Values::Any(values) => values.iter().take(rows).any(dated),
            Values::Dict { values: Dictionary::Any(values), .. } | Values::List { child: Dictionary::Any(values), .. } => {
                values.iter().any(dated)
            }
            Values::List {
                child: Dictionary::Text { .. } | Dictionary::Scaled { .. } | Dictionary::Bool { .. },
                ..
            } => false,
            Values::List {
                child: Dictionary::Column(child),
                ..
            } if Self::scalar_struct(child) => false,
            Values::Struct { .. } if Self::scalar_struct(&column) => false,
            Values::List { .. } | Values::Dict { values: Dictionary::Column(_), .. } => {
                (0..rows).any(|row| dated(&column.variable(row)))
            }
            _ => false,
        };
        match nested {
            true => Leaf::Any(
                (0..rows)
                    .map(|row| {
                        let value = column.variable(row);
                        DeclaredDates::prepare(&value, None).unwrap_or(value)
                    })
                    .collect(),
            ),
            false => Leaf::input(Self::validated(column, rows), rows),
        }
    }

    fn scalar_struct(column: &Column) -> bool {
        match column.values {
            Values::Struct { fields, .. } => fields.iter().all(|(_, field)| !matches!(field.values, Values::Any(_) | Values::List { .. } | Values::Struct { .. } | Values::Dict { values: Dictionary::Any(_) | Dictionary::Column(_), .. })),
            _ => false,
        }
    }

    fn validated<'a>(column: Column<'a>, rows: usize) -> Column<'a> {
        let Values::Utf8 { offsets, data } = column.values else {
            return column;
        };
        let text = offsets
            .get(rows)
            .and_then(|&end| usize::try_from(end).ok())
            .and_then(|end| data.get(..end))
            .and_then(|bytes| std::str::from_utf8(bytes).ok());
        match text {
            Some(data) => Column {
                values: Values::Text { offsets, data },
                validity: column.validity,
            },
            None => column,
        }
    }

    pub(crate) fn lean<'a>(column: Column<'a>, rows: usize) -> Leaf<'a> {
        let column = Self::validated(column, rows);
        let validity = column.validity.filter(|(bits, offset)| !Bits::full(bits, *offset, rows));
        let column = Column { validity, ..column };
        let unpacked = match column.values {
            Values::Dec(values) => values.get(..rows).and_then(Store::unpacked),
            _ => None,
        };
        match unpacked {
            Some(store) => Leaf::typed(Array::parts(store, validity.map(|(bits, offset)| Bits::window(bits, offset, rows)), rows)),
            None => Leaf::input(column, rows),
        }
    }

    pub(crate) fn seen(leaf: &Leaf, rows: usize) -> Seen {
        if let Some(records) = leaf.records() {
            return match records.alive(rows) {
                true => Seen::List,
                false => Seen::Null,
            };
        }
        let column = leaf.column();
        let dict;
        let values = match column.values {
            Values::Dict { values, .. } => {
                dict = values.column();
                dict.values
            }
            values => values,
        };
        match values {
            Values::Scaled { .. } | Values::I64(_) | Values::Dec(_) => Seen::Num,
            Values::Bool { .. } => Seen::Bool,
            Values::Text { .. } | Values::Utf8 { .. } => Seen::Text,
            Values::Any(values) if !matches!(column.values, Values::Dict { .. }) => {
                let mut counts = [0usize; 5];
                let live = values.iter().take(rows).enumerate().filter(|(row, value)| column.valid(*row) && !matches!(value, Variable::Null));
                for (_, value) in live.take(Self::SAMPLE) {
                    let kind = match value {
                        Variable::Number(_) => 0,
                        Variable::Bool(_) => 1,
                        Variable::String(_) => 2,
                        Variable::Array(_) => 3,
                        _ => 4,
                    };
                    counts[kind] += 1;
                }
                let total: usize = counts.iter().sum();
                let (kind, most) = counts[..3].iter().enumerate().max_by_key(|(_, count)| **count).map_or((3, 0), |(kind, count)| (kind, *count));
                match (total, most * 2 >= total, kind) {
                    (0, ..) => Seen::Null,
                    (_, false, _) if counts[3] * 2 > total => Seen::List,
                    (_, true, 0) => Seen::Num,
                    (_, true, 1) => Seen::Bool,
                    (_, true, 2) => Seen::Text,
                    _ => Seen::Mixed,
                }
            }
            Values::List { .. } => Seen::List,
            _ => Seen::Mixed,
        }
    }

    pub(crate) fn typed<'a>(leaf: &Leaf<'a>, rows: usize, code: bool) -> Option<(Leaf<'a>, Option<Vec<u64>>)> {
        if leaf.records().is_some() {
            return None;
        }
        let column = leaf.column();
        if let Values::Any(values) = column.values {
            return Self::shredded(values.get(..rows)?, &column, Self::seen(leaf, rows), rows);
        }
        let Leaf::Input(input) = leaf else {
            return None;
        };
        let column = input.column;
        let valid = || column.validity.filter(|(bits, offset)| !Bits::full(bits, *offset, rows)).map(|(bits, offset)| Bits::window(bits, offset, rows));
        match column.values {
            Values::Dec(values) => Some((Leaf::typed(Array::parts(Store::unpacked(values.get(..rows)?)?, valid(), rows)), None)),
            Values::Text { offsets, data } if code => {
                let (codes, dict) = Coder::new(data.as_bytes()).codes(offsets.get(..=rows)?)?;
                let array = match valid() {
                    Some(bits) => Array::coded_with(codes.into(), Rc::new(dict), bits),
                    None => Array::coded(codes.into(), Rc::new(dict)),
                };
                Some((Leaf::typed(array), None))
            }
            _ => None,
        }
    }

    fn shredded<'a>(values: &[Variable], column: &zen_expression::lane::Column, seen: Seen, rows: usize) -> Option<(Leaf<'a>, Option<Vec<u64>>)> {
        let mut valid = vec![0u64; rows.div_ceil(64)];
        let mut faults: Option<Vec<u64>> = None;
        let mut fault = |row: usize| Bits::set(faults.get_or_insert_with(|| vec![0u64; rows.div_ceil(64)]), row, true);
        let live = values.iter().enumerate().filter(|(row, value)| column.valid(*row) && !matches!(value, Variable::Null));
        let store = match seen {
            Seen::Num => {
                let (mut mant, mut scale) = (vec![0i64; rows], vec![0u8; rows]);
                for (row, value) in live {
                    match value {
                        Variable::Number(n) => match Store::part(n) {
                            Some((m, s)) => {
                                mant[row] = m;
                                scale[row] = s;
                                Bits::set(&mut valid, row, true);
                            }
                            None => fault(row),
                        },
                        _ => fault(row),
                    }
                }
                Store::Scaled { mant, scale }
            }
            Seen::Bool => {
                let mut bits = vec![0u64; rows.div_ceil(64)];
                for (row, value) in live {
                    match value {
                        Variable::Bool(b) => {
                            Bits::set(&mut bits, row, *b);
                            Bits::set(&mut valid, row, true);
                        }
                        _ => fault(row),
                    }
                }
                Store::Bool(bits)
            }
            Seen::Text => {
                let mut offsets = vec![0i32; rows + 1];
                let mut data = String::new();
                let mut next = 0;
                for (row, value) in live {
                    offsets[next..=row].fill(i32::try_from(data.len()).ok()?);
                    match value {
                        Variable::String(text) => {
                            data.push_str(text);
                            Bits::set(&mut valid, row, true);
                        }
                        _ => fault(row),
                    }
                    next = row + 1;
                }
                offsets[next..].fill(i32::try_from(data.len()).ok()?);
                Store::Text { offsets, data }
            }
            Seen::Null | Seen::List | Seen::Mixed => return None,
        };
        let full = Bits::full(&valid, 0, rows);
        Some((Leaf::typed(Array::parts(store, (!full).then_some(valid), rows)), faults))
    }
}

impl<'d> Coder<'d> {
    const SLOTS: usize = 128;
    const LIMIT: usize = 64;
    const SAMPLE: usize = 64;
    const SAMPLED: usize = 16;

    fn new(data: &'d [u8]) -> Self {
        Self {
            data,
            slots: [u8::MAX; Self::SLOTS],
            entries: Vec::new(),
        }
    }

    #[inline]
    fn word(&self, at: usize, len: usize) -> u64 {
        match (len < 8, self.data.get(at..).and_then(<[u8]>::first_chunk::<8>)) {
            (_, _) if len == 0 => 0,
            (true, Some(word)) => u64::from_le_bytes(*word) & ((1u64 << (8 * len)) - 1),
            (false, Some(word)) => u64::from_le_bytes(*word),
            (_, None) => self.data.get(at..at + len.min(8)).map_or(0, |bytes| bytes.iter().enumerate().fold(0u64, |key, (i, b)| key | u64::from(*b) << (8 * i))),
        }
    }

    #[inline]
    fn code(&mut self, lo: usize, hi: usize) -> Option<i32> {
        let len = hi.checked_sub(lo)?;
        let key = (self.word(lo, len), self.word(lo + 8, len.saturating_sub(8)), len);
        let mut slot = Self::slot(key);
        loop {
            match self.slots[slot] {
                u8::MAX => {
                    if self.entries.len() >= Self::LIMIT || hi > self.data.len() {
                        return None;
                    }
                    self.slots[slot] = self.entries.len() as u8;
                    self.entries.push((key, lo));
                    return i32::try_from(self.entries.len() - 1).ok();
                }
                at => {
                    let (k, a) = self.entries[at as usize];
                    if k == key && (len <= 16 || self.data.get(a..a + len) == self.data.get(lo..hi)) {
                        return Some(i32::from(at));
                    }
                    slot = (slot + 1) % Self::SLOTS;
                }
            }
        }
    }

    #[inline]
    fn slot((low, high, len): (u64, u64, usize)) -> usize {
        let hash = (low ^ high.rotate_left(29) ^ (len as u64).rotate_right(8)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        (hash >> 57) as usize
    }

    fn codes(mut self, offsets: &[i32]) -> Option<(Vec<i32>, Dict)> {
        let mut codes = Vec::with_capacity(offsets.len().saturating_sub(1));
        for (at, w) in offsets.windows(2).enumerate() {
            if at == Self::SAMPLE && self.entries.len() > Self::SAMPLED {
                return None;
            }
            codes.push(self.code(usize::try_from(w[0]).ok()?, usize::try_from(w[1]).ok()?)?);
        }
        let mut dict_offsets = Vec::with_capacity(self.entries.len() + 1);
        let mut text = String::new();
        dict_offsets.push(0);
        for &((_, _, len), lo) in &self.entries {
            text.push_str(std::str::from_utf8(self.data.get(lo..lo + len)?).ok()?);
            dict_offsets.push(i32::try_from(text.len()).ok()?);
        }
        Some((codes, Dict::Text { offsets: dict_offsets, data: text }))
    }
}

impl Interner {
    fn new() -> Self {
        Self {
            slots: [u8::MAX; Coder::SLOTS],
            keys: Vec::new(),
            offsets: vec![0],
            data: String::new(),
        }
    }

    #[inline(always)]
    fn key(bytes: &[u8]) -> (u64, u64, usize) {
        let len = bytes.len();
        let at = |lo: usize| bytes.get(lo..).and_then(<[u8]>::first_chunk::<8>).map_or(0, |word| u64::from_le_bytes(*word));
        let half = |lo: usize| bytes.get(lo..).and_then(<[u8]>::first_chunk::<4>).map_or(0, |word| u64::from(u32::from_le_bytes(*word)));
        let byte = |lo: usize| bytes.get(lo).map_or(0, |&byte| u64::from(byte));
        let words = match len {
            0 => (0, 0),
            1..=3 => (byte(0) | byte(len / 2) << 8 | byte(len - 1) << 16, 0),
            4..=8 => (half(0) | half(len - 4) << 32, 0),
            9..=16 => (at(0), at(len - 8)),
            _ => (at(0), at(8)),
        };
        (words.0, words.1, len)
    }

    fn text(&self, code: i32) -> &str {
        let at = usize::try_from(code).unwrap_or(usize::MAX);
        let range = self.offsets.get(at).zip(self.offsets.get(at.wrapping_add(1)));
        range.and_then(|(&lo, &hi)| self.data.get(usize::try_from(lo).ok()?..usize::try_from(hi).ok()?)).unwrap_or_default()
    }

    #[inline(always)]
    fn found(&self, text: &str) -> Option<i32> {
        let key = Self::key(text.as_bytes());
        let mut slot = Coder::slot(key);
        loop {
            let at = self.slots[slot];
            let code = i32::from(at);
            match self.keys.get(usize::from(at)) {
                None => return None,
                Some(known) if *known == key && (key.2 <= 16 || self.text(code) == text) => return Some(code),
                Some(_) => slot = (slot + 1) % Coder::SLOTS,
            }
        }
    }

    fn code(&mut self, text: &str) -> Option<i32> {
        let bytes = text.as_bytes();
        let len = bytes.len();
        let key = Self::key(bytes);
        let mut slot = Coder::slot(key);
        loop {
            match self.slots[slot] {
                u8::MAX => {
                    if self.keys.len() >= Coder::LIMIT {
                        return None;
                    }
                    let code = i32::try_from(self.keys.len()).ok()?;
                    let end = i32::try_from(self.data.len() + len).ok()?;
                    self.slots[slot] = self.keys.len() as u8;
                    self.keys.push(key);
                    self.data.push_str(text);
                    self.offsets.push(end);
                    return Some(code);
                }
                at => {
                    let code = i32::from(at);
                    if self.keys[usize::from(at)] == key && (len <= 16 || self.text(code) == text) {
                        return Some(code);
                    }
                    slot = (slot + 1) % Coder::SLOTS;
                }
            }
        }
    }

    fn dict(self) -> Dict {
        Dict::Text { offsets: self.offsets, data: self.data }
    }
}

enum Cells {
    Empty,
    Scaled { mant: Vec<i64>, scale: Vec<u8> },
    Text { offsets: Vec<i32>, data: String },
    Coded { codes: Vec<i32>, words: Interner, bytes: usize },
    Bool(Vec<u64>),
    Any(Vec<Variable>),
}

struct FieldBuilder {
    name: Arc<str>,
    coded: bool,
    cells: Cells,
    gaps: Option<Vec<u64>>,
    len: usize,
}

impl FieldBuilder {
    fn new(name: Arc<str>, len: usize, coded: bool) -> Self {
        let mut field = Self {
            name,
            coded,
            cells: Cells::Empty,
            gaps: None,
            len: 0,
        };
        (0..len).for_each(|_| field.missing());
        field
    }

    #[inline]
    fn bit(&mut self, on: bool) {
        let len = self.len;
        let gaps = match (&mut self.gaps, on) {
            (None, true) => return,
            (Some(gaps), _) => gaps,
            (gaps @ None, false) => gaps.insert(Bits::ones(len)),
        };
        if len & 63 == 0 {
            gaps.push(0);
        }
        if on {
            Bits::set(gaps, len, true);
        }
    }

    fn present(&self, at: usize) -> bool {
        self.gaps.as_ref().is_none_or(|gaps| Bits::get(gaps, at))
    }

    fn missing(&mut self) {
        self.bit(false);
        match &mut self.cells {
            Cells::Empty | Cells::Bool(_) => {}
            Cells::Scaled { mant, scale } => {
                mant.push(0);
                scale.push(0);
            }
            Cells::Text { offsets, data } => offsets.push(data.len() as i32),
            Cells::Coded { .. } => {
                self.text("");
            }
            Cells::Any(values) => values.push(Variable::Null),
        }
        self.len += 1;
    }

    #[inline]
    fn text(&mut self, text: &str) -> bool {
        match &mut self.cells {
            Cells::Coded { codes, words, bytes } => {
                let end = bytes.checked_add(text.len()).filter(|&end| i32::try_from(end).is_ok());
                if let Some((end, code)) = end.zip(words.found(text)).filter(|_| codes.len() != Coder::SAMPLE) {
                    codes.push(code);
                    *bytes = end;
                    return true;
                }
                self.spell(text)
            }
            Cells::Text { offsets, data } => Self::spelt(offsets, data, text),
            _ => false,
        }
    }

    #[inline]
    fn spelt(offsets: &mut Vec<i32>, data: &mut String, text: &str) -> bool {
        match i32::try_from(data.len() + text.len()) {
            Ok(end) => {
                data.push_str(text);
                offsets.push(end);
                true
            }
            Err(_) => false,
        }
    }

    #[cold]
    #[inline(never)]
    fn spell(&mut self, text: &str) -> bool {
        if let Cells::Coded { codes, words, bytes } = &mut self.cells {
            let Some(end) = bytes.checked_add(text.len()).filter(|&end| i32::try_from(end).is_ok()) else {
                return false;
            };
            let sampled = codes.len() != Coder::SAMPLE || words.keys.len() <= Coder::SAMPLED;
            if let Some(code) = sampled.then(|| words.code(text)).flatten() {
                codes.push(code);
                *bytes = end;
                return true;
            }
            self.cells = self.spelled();
        }
        match &mut self.cells {
            Cells::Text { offsets, data } => Self::spelt(offsets, data, text),
            _ => false,
        }
    }

    fn spelled(&self) -> Cells {
        let Cells::Coded { codes, words, bytes } = &self.cells else {
            return Cells::Empty;
        };
        let mut offsets = Vec::with_capacity(codes.capacity() + 1);
        let mut data = String::with_capacity(*bytes);
        offsets.push(0);
        for &code in codes {
            data.push_str(words.text(code));
            offsets.push(data.len() as i32);
        }
        Cells::Text { offsets, data }
    }

    fn variables(&self) -> Vec<Variable> {
        let mut values = Vec::with_capacity(self.len + 1);
        for at in 0..self.len {
            let value = match (&self.cells, self.present(at)) {
                (_, false) | (Cells::Empty, _) => Variable::Null,
                (Cells::Scaled { mant, scale }, true) => Variable::Number(rust_decimal::Decimal::new(mant[at], u32::from(scale[at]))),
                (Cells::Text { offsets, data }, true) => Variable::String(data[offsets[at] as usize..offsets[at + 1] as usize].into()),
                (Cells::Coded { codes, words, .. }, true) => Variable::String(words.text(codes[at]).into()),
                (Cells::Bool(bits), true) => Variable::Bool(Bits::get(bits, at)),
                (Cells::Any(values), true) => values[at].clone(),
            };
            values.push(value);
        }
        values
    }

    fn start(&mut self, value: &Variable, capacity: usize) {
        let len = self.len;
        self.cells = match value {
            Variable::Number(n) if Store::part(n).is_some() => {
                let (mut mant, mut scale) = (Vec::with_capacity(capacity), Vec::with_capacity(capacity));
                mant.resize(len, 0);
                scale.resize(len, 0);
                Cells::Scaled { mant, scale }
            }
            Variable::String(_) if self.coded => Cells::Coded {
                codes: Vec::with_capacity(capacity),
                words: Interner::new(),
                bytes: 0,
            },
            Variable::String(_) => {
                let mut offsets = Vec::with_capacity(capacity + 1);
                offsets.resize(len + 1, 0);
                Cells::Text { offsets, data: String::new() }
            }
            Variable::Bool(_) => Cells::Bool(Vec::with_capacity(capacity.div_ceil(64))),
            _ => {
                let mut values = Vec::with_capacity(capacity);
                values.resize(len, Variable::Null);
                Cells::Any(values)
            }
        };
        if matches!(self.cells, Cells::Coded { .. }) {
            (0..len).for_each(|_| {
                self.text("");
            });
        }
    }

    #[inline(always)]
    fn push(&mut self, value: &Variable, capacity: usize) {
        if matches!(self.cells, Cells::Empty) {
            self.start(value, capacity);
        }
        let len = self.len;
        let fits = match (&mut self.cells, value) {
            (Cells::Scaled { mant, scale }, Variable::Number(n)) => match Store::part(n) {
                Some((m, s)) => {
                    mant.push(m);
                    scale.push(s);
                    true
                }
                None => false,
            },
            (Cells::Text { .. } | Cells::Coded { .. }, Variable::String(text)) => self.text(text),
            (Cells::Bool(bits), Variable::Bool(on)) => {
                let words = (len + 1).div_ceil(64);
                if bits.len() < words {
                    bits.resize(words, 0);
                }
                Bits::set(bits, len, *on);
                true
            }
            (Cells::Any(values), value) => {
                values.push(value.clone());
                true
            }
            _ => false,
        };
        if !fits {
            let mut values = self.variables();
            values.push(value.clone());
            self.cells = Cells::Any(values);
        }
        self.bit(true);
        self.len += 1;
    }

    fn run<'v>(&mut self, mut values: impl Iterator<Item = &'v Variable>, capacity: usize) {
        while let Some(value) = self.quick(&mut values) {
            self.push(value, capacity);
        }
    }

    #[inline]
    fn quick<'v>(&mut self, values: &mut impl Iterator<Item = &'v Variable>) -> Option<&'v Variable> {
        if self.gaps.is_some() {
            return values.next();
        }
        let len = self.len;
        let mut taken = 0;
        let stray = match &mut self.cells {
            Cells::Scaled { mant, scale } => values.find(|value| {
                let part = match value {
                    Variable::Number(n) => Store::part(n),
                    _ => None,
                };
                part.inspect(|&(m, s)| {
                    mant.push(m);
                    scale.push(s);
                    taken += 1;
                })
                .is_none()
            }),
            Cells::Bool(bits) => values.find(|value| {
                let Variable::Bool(on) = value else {
                    return true;
                };
                let at = len + taken;
                let words = (at + 1).div_ceil(64);
                if bits.len() < words {
                    bits.resize(words, 0);
                }
                Bits::set(bits, at, *on);
                taken += 1;
                false
            }),
            Cells::Coded { codes, words, bytes } => values.find(|value| {
                let Variable::String(text) = value else {
                    return true;
                };
                let end = bytes.checked_add(text.len()).filter(|&end| i32::try_from(end).is_ok());
                match end.zip(words.found(text)).filter(|_| codes.len() != Coder::SAMPLE) {
                    Some((end, code)) => {
                        codes.push(code);
                        *bytes = end;
                        taken += 1;
                        false
                    }
                    None => true,
                }
            }),
            _ => values.next(),
        };
        self.len += taken;
        stray
    }

    fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }
        let mut values = self.variables();
        values.truncate(len);
        self.cells = Cells::Any(values);
        if let Some(gaps) = &mut self.gaps {
            gaps.truncate(len.div_ceil(64));
            Bits::trim(gaps, len);
        }
        self.len = len;
    }

    fn array(self) -> (Arc<str>, Array) {
        let len = self.len;
        let valid = self.gaps.filter(|gaps| !Bits::full(gaps, 0, len));
        let store = match self.cells {
            Cells::Empty => Store::Any(vec![Variable::Null; len]),
            Cells::Scaled { mant, scale } => Store::Scaled { mant, scale },
            Cells::Text { offsets, data } => Store::Text { offsets, data },
            Cells::Coded { codes, words, .. } => Store::Coded { codes: codes.into(), dict: Rc::new(words.dict()) },
            Cells::Bool(mut bits) => {
                bits.resize(len.div_ceil(64), 0);
                Store::Bool(bits)
            }
            Cells::Any(values) => Store::Any(values),
        };
        (self.name, Array::parts(store, valid, len))
    }
}

struct ListBuilder {
    nulls: bool,
    primitive: bool,
    rows: usize,
    offsets: Vec<i32>,
    validity: Vec<u64>,
    children: usize,
    capacity: usize,
    fields: Vec<FieldBuilder>,
    shapes: Vec<(u64, Vec<usize>)>,
    hosted: Vec<usize>,
}

impl ListBuilder {
    const CHUNK: usize = 64;

    fn new(rows: usize, capacity: usize) -> Self {
        let mut offsets = Vec::with_capacity(rows + 1);
        offsets.push(0);
        Self {
            nulls: false,
            primitive: false,
            rows,
            offsets,
            validity: Bits::ones(rows),
            children: 0,
            capacity,
            fields: Vec::new(),
            shapes: Vec::new(),
            hosted: Vec::new(),
        }
    }

    fn field(&mut self, hint: usize, key: &str) -> Option<usize> {
        if self.fields.get(hint).is_some_and(|field| field.name.as_ref() == key) {
            return Some(hint);
        }
        if let Some(at) = self.fields.iter().position(|field| field.name.as_ref() == key) {
            return Some(at);
        }
        if key.is_empty() || key.contains('.') {
            return None;
        }
        self.fields.push(FieldBuilder::new(Arc::from(key), self.children, true));
        Some(self.fields.len() - 1)
    }

    fn layout(&mut self, shape: &Shape) -> Option<usize> {
        if let Some(at) = self.shapes.iter().position(|(id, _)| *id == shape.id()) {
            return Some(at);
        }
        let slots = shape
            .keys()
            .iter()
            .enumerate()
            .map(|(hint, key)| self.field(hint, key))
            .collect::<Option<Vec<usize>>>()?;
        self.shapes.push((shape.id(), slots));
        Some(self.shapes.len() - 1)
    }

    #[inline(always)]
    fn item(&mut self, item: &Variable) -> Option<()> {
        if self.primitive {
            let field = self.fields.first_mut()?;
            match item {
                Variable::Object(_) | Variable::Array(_) => return None,
                Variable::Null => field.missing(),
                value => field.push(value, self.capacity),
            }
            self.children += 1;
            return Some(());
        }
        let Variable::Object(map) = item else {
            return None;
        };
        let child = self.children;
        let map = map.borrow();
        let touched = match map.slots() {
            Some((shape, values)) => {
                let layout = self.layout(shape)?;
                let (fields, capacity, nulls) = (&mut self.fields, self.capacity, self.nulls);
                for (&at, value) in self.shapes[layout].1.iter().zip(values) {
                    match (nulls, value) {
                        (true, Variable::Null) => fields[at].missing(),
                        _ => fields[at].push(value, capacity),
                    }
                }
                values.len()
            }
            None => {
                for (hint, (key, value)) in map.iter().enumerate() {
                    let at = self.field(hint, key)?;
                    match (self.nulls, value) {
                        (true, Variable::Null) => self.fields[at].missing(),
                        _ => self.fields[at].push(value, self.capacity),
                    }
                }
                map.len()
            }
        };
        if touched != self.fields.len() {
            self.fields.iter_mut().filter(|field| field.len == child).for_each(FieldBuilder::missing);
        }
        self.children += 1;
        Some(())
    }

    fn host(&mut self, row: usize) {
        self.hosted.push(row);
        Bits::set(&mut self.validity, row, false);
    }

    fn row(&mut self, row: usize, items: &[Variable]) {
        let start = self.children;
        if items.iter().try_for_each(|item| self.item(item)).is_none() {
            self.fields.iter_mut().for_each(|field| field.truncate(start));
            self.children = start;
            self.host(row);
        }
    }

    fn close(&mut self) -> Option<()> {
        self.offsets.push(i32::try_from(self.children).ok()?);
        Some(())
    }

    fn each(mut self, column: &Column) -> Option<Shred> {
        for row in 0..self.rows {
            if !column.valid(row) {
                Bits::set(&mut self.validity, row, false);
                self.close()?;
                continue;
            }
            match column.values {
                Values::Any(values) => match values.get(row) {
                    Some(Variable::Array(items)) => self.row(row, &items.borrow()),
                    _ => self.host(row),
                },
                Values::List {
                    child: Dictionary::Any(items),
                    ..
                } => match column.range(row).and_then(|(a, b)| items.get(a..b)) {
                    Some(slice) => self.row(row, slice),
                    None => self.host(row),
                },
                _ => return None,
            }
            self.close()?;
        }
        Some(self.finish())
    }

    fn uniform(mut self, column: &Column, values: &[Variable]) -> Option<Option<Shred>> {
        let mut lists = Vec::with_capacity(self.rows);
        for row in 0..self.rows {
            match values.get(row) {
                _ if !column.valid(row) => Bits::set(&mut self.validity, row, false),
                Some(Variable::Array(items)) => {
                    let list = items.borrow();
                    self.children += list.len();
                    lists.push(list);
                }
                _ => self.host(row),
            }
            if self.close().is_none() {
                return Some(None);
            }
        }
        self.capacity = self.children;
        self.children = 0;
        let mut items = Vec::with_capacity(self.capacity.min(Self::CHUNK));
        let mut last = (0, 0, 0);
        let mut all = lists.iter().flat_map(|list| list.iter()).peekable();
        while all.peek().is_some() {
            for item in all.by_ref().take(Self::CHUNK) {
                let Variable::Object(map) = item else {
                    return None;
                };
                let map = map.borrow();
                let (shape, values) = map.slots()?;
                if shape.id() != last.0 || self.children == 0 {
                    let known = self.shapes.len();
                    let layout = self.layout(shape)?;
                    let slots = &self.shapes[layout].1;
                    if layout >= known && slots.iter().enumerate().any(|(at, slot)| slots[..at].contains(slot)) {
                        return None;
                    }
                    last = (shape.id(), layout, slots.len());
                }
                if last.2 != values.len() {
                    return None;
                }
                items.push((last.1, Ref::map(map, |map| map.slots().map_or(&[][..], |(_, values)| values))));
                self.children += 1;
            }
            self.spread(&items);
            items.clear();
        }
        Some(Some(self.finish()))
    }

    fn spread(&mut self, items: &[(usize, Ref<'_, [Variable]>)]) {
        let (capacity, start) = (self.capacity, self.children - items.len());
        let shapes = &self.shapes;
        for (index, field) in self.fields.iter_mut().enumerate() {
            let slot = |layout: usize| shapes.get(layout).and_then(|(_, slots)| slots.iter().position(|&at| at == index));
            let fixed = (0..shapes.len()).map(slot).reduce(|a, b| a.filter(|&a| Some(a) == b)).flatten();
            let items = items.get(field.len.saturating_sub(start)..).unwrap_or_default();
            match fixed {
                Some(at) => field.run(items.iter().filter_map(|(_, values)| values.get(at)), capacity),
                None => items.iter().for_each(|(layout, values)| match slot(*layout).and_then(|at| values.get(at)) {
                    Some(value) => field.push(value, capacity),
                    None => field.missing(),
                }),
            }
        }
    }

    fn finish(self) -> Shred {
        let validity = match Bits::ones(self.rows) == self.validity {
            true => None,
            false => Some(Rc::from(self.validity)),
        };
        Shred {
            frame: Frame {
                offsets: self.offsets.into(),
                validity,
                children: self.children,
            },
            fields: self.fields.into_iter().map(FieldBuilder::array).collect(),
            hosted: self.hosted.into(),
        }
    }
}

impl Shred {
    pub fn of(column: &Column, rows: usize) -> Option<Shred> {
        let capacity = match column.values {
            Values::Any(values) => {
                if let Some(shred) = ListBuilder::new(rows, 0).uniform(column, values) {
                    return shred;
                }
                values
                    .iter()
                    .take(rows)
                    .map(|value| match value {
                        Variable::Array(items) => items.borrow().len(),
                        _ => 0,
                    })
                    .sum()
            }
            Values::List { child, .. } => child.len(),
            _ => return None,
        };
        ListBuilder::new(rows, capacity).each(column)
    }
}

impl<'a> Listing<'a> {
    pub fn of(column: Column<'a>, owners: usize) -> Option<Listing<'a>> {
        let validity = column
            .validity
            .filter(|(bits, offset)| !Bits::full(bits, *offset, owners))
            .map(|(bits, offset)| Rc::from(Bits::window(bits, offset, owners)));
        match column.values {
            Values::List { offsets, child } => {
                let offsets: Rc<[i32]> = offsets.get(..=owners)?.into();
                match child {
                    Dictionary::Any(_) => Self::shredded(&column, owners),
                    Dictionary::Column(item) => match item.values {
                        Values::Struct { fields, len } => {
                            if item.validity.is_some_and(|(bits, offset)| !Bits::full(bits, offset, len)) {
                                return None;
                            }
                            let fields = fields.iter().map(|(name, field)| (Arc::from(*name), Normalizer::lean(*field, len))).collect();
                            Some(Listing { frame: Frame { offsets, validity, children: len }, hosted: None, fields })
                        }
                        Values::Any(_) | Values::List { .. } | Values::Dict { .. } => None,
                        _ => {
                            let len = item.len();
                            Some(Listing { frame: Frame { offsets, validity, children: len }, hosted: None, fields: vec![(Arc::from(""), Normalizer::lean(*item, len))] })
                        }
                    },
                    scalar => {
                        let len = scalar.len();
                        Some(Listing { frame: Frame { offsets, validity, children: len }, hosted: None, fields: vec![(Arc::from(""), Normalizer::lean(scalar.column(), len))] })
                    }
                }
            }
            Values::Any(_) => Self::shredded(&column, owners),
            _ => None,
        }
    }

    pub fn leaf(leaf: &Leaf<'a>, owners: usize) -> Option<Listing<'a>> {
        match leaf {
            Leaf::Input(input) => Listing::of(input.column, owners),
            other => match other.records().and_then(|records| Self::recorded(&records, owners)) {
                Some(listing) => Some(listing),
                None => Self::copied(&other.column(), owners),
            },
        }
    }

    fn recorded(records: &Records<'a>, owners: usize) -> Option<Listing<'a>> {
        if !records.plain() {
            return None;
        }
        let count = records.count();
        let mut fields = Vec::with_capacity(records.fields().len());
        for (key, leaf, present) in records.fields() {
            Normalizer::class(&leaf.column().values)?;
            let leaf = match Bits::full(present, 0, count) {
                true => leaf.clone(),
                false => Leaf::masked(leaf.clone(), present),
            };
            fields.push((Arc::from(key.as_str()), leaf));
        }
        let validity = records.validity();
        Some(Listing {
            frame: Frame {
                offsets: records.offsets().get(..=owners)?.into(),
                validity: (!Bits::full(validity, 0, owners)).then(|| Rc::from(Bits::window(validity, 0, owners))),
                children: count,
            },
            hosted: None,
            fields,
        })
    }

    fn copied(column: &Column<'_>, owners: usize) -> Option<Listing<'a>> {
        let validity = column
            .validity
            .filter(|(bits, offset)| !Bits::full(bits, *offset, owners))
            .map(|(bits, offset)| Rc::from(Bits::window(bits, offset, owners)));
        let Values::List { offsets, child } = column.values else {
            return match column.values {
                Values::Any(_) => Self::shredded(column, owners),
                _ => None,
            };
        };
        let store = match child {
            Dictionary::Any(_) => return Self::shredded(column, owners),
            Dictionary::Scaled { mant, scale } => Store::Scaled { mant: mant.to_vec(), scale: scale.to_vec() },
            Dictionary::Text { offsets, data } => Store::Text { offsets: offsets.to_vec(), data: data.to_string() },
            Dictionary::Bool { bits } => Store::Bool(bits.to_vec()),
            Dictionary::Column(_) => return None,
        };
        let len = child.len();
        Some(Listing {
            frame: Frame {
                offsets: offsets.get(..=owners)?.into(),
                validity,
                children: len,
            },
            hosted: None,
            fields: vec![(Arc::from(""), Leaf::typed(Array::parts(store, None, len)))],
        })
    }

    pub fn nested(&self, name: &str) -> Option<Listing<'a>> {
        let (_, leaf) = self.fields.iter().rev().find(|(key, _)| key.as_ref() == name)?;
        Listing::leaf(leaf, self.frame.children)
    }

    pub fn columns<'s>(&'s self, names: &[ColumnDef]) -> Option<Vec<Column<'s>>> {
        names.iter().map(|def| self.fields.iter().rev().find(|(name, _)| name.as_ref() == def.path).map(|(_, leaf)| leaf.column())).collect()
    }

    pub fn classes(&self) -> Vec<(Arc<str>, Class)> {
        self.fields.iter().filter_map(|(name, leaf)| Some((name.clone(), Normalizer::class(&leaf.column().values)?))).collect()
    }

    fn items<'v>(column: &Column<'v>, row: usize) -> Option<Option<std::borrow::Cow<'v, [Variable]>>> {
        if !column.valid(row) {
            return Some(None);
        }
        match column.values {
            Values::Any(values) => match values.get(row)? {
                Variable::Null => Some(None),
                Variable::Array(items) => Some(Some(std::borrow::Cow::Owned(items.borrow().clone()))),
                _ => None,
            },
            Values::List { child: Dictionary::Any(items), .. } => column.range(row).and_then(|(a, b)| items.get(a..b)).map(|slice| Some(std::borrow::Cow::Borrowed(slice))),
            _ => None,
        }
    }

    fn sort(value: &Variable) -> Option<usize> {
        match value {
            Variable::Null => None,
            Variable::Number(n) if Store::part(n).is_some() => Some(0),
            Variable::String(_) => Some(1),
            Variable::Bool(_) => Some(2),
            _ => Some(3),
        }
    }

    fn odd(items: &[Variable], objects: bool, sorts: &[(Arc<str>, usize)]) -> bool {
        let wanted = |key: &str| sorts.iter().find(|(k, _)| k.as_ref() == key).map(|(_, sort)| *sort);
        items.iter().any(|item| match (objects, item) {
            (false, item) => Self::sort(item).is_some_and(|sort| Some(sort) != wanted("")),
            (true, Variable::Object(map)) => map.borrow().iter().any(|(key, value)| Self::sort(value).is_some_and(|sort| Some(sort) != wanted(key))),
            (true, _) => false,
        })
    }

    fn shredded(column: &Column<'_>, owners: usize) -> Option<Listing<'a>> {
        let mut capacity = 0;
        let mut arrays = 0;
        let mut objects = false;
        let mut counts: Vec<(Arc<str>, [usize; 4])> = Vec::new();
        let mut count = |key: &str, value: &Variable| {
            let Some(sort) = Self::sort(value) else {
                return;
            };
            match counts.iter_mut().find(|(k, _)| k.as_ref() == key) {
                Some((_, c)) => c[sort] += 1,
                None => {
                    let mut c = [0usize; 4];
                    c[sort] = 1;
                    counts.push((Arc::from(key), c));
                }
            }
        };
        for row in 0..owners {
            if let Some(Some(items)) = Self::items(column, row) {
                arrays += 1;
                capacity += items.len();
                objects |= items.iter().any(|item| matches!(item, Variable::Object(_)));
            }
        }
        if arrays == 0 {
            return None;
        }
        for row in 0..owners {
            if let Some(Some(items)) = Self::items(column, row) {
                for item in items.iter() {
                    match (objects, item) {
                        (true, Variable::Object(map)) => map.borrow().iter().for_each(|(key, value)| count(key, value)),
                        (true, _) => {}
                        (false, item) => count("", item),
                    }
                }
            }
        }
        let sorts: Vec<(Arc<str>, usize)> =
            counts.into_iter().map(|(key, c)| (key, (0..4).max_by_key(|&sort| (c[sort], std::cmp::Reverse(sort))).unwrap_or(3))).collect();
        let mut builder = ListBuilder::new(owners, capacity);
        builder.nulls = true;
        builder.primitive = !objects;
        if builder.primitive {
            builder.fields.push(FieldBuilder::new(Arc::from(""), 0, true));
        }
        for row in 0..owners {
            match Self::items(column, row) {
                Some(Some(items)) if Self::odd(&items, objects, &sorts) => builder.host(row),
                Some(Some(items)) => builder.row(row, &items),
                Some(None) => Bits::set(&mut builder.validity, row, false),
                None => builder.host(row),
            }
            builder.close()?;
        }
        let hosted = (!builder.hosted.is_empty()).then(|| {
            let mut bits = vec![0u64; owners.div_ceil(64)];
            builder.hosted.iter().for_each(|&row| Bits::set(&mut bits, row, true));
            Rc::from(bits)
        });
        let shred = builder.finish();
        Some(Listing {
            frame: shred.frame,
            hosted,
            fields: shred.fields.into_iter().map(|(name, array)| (name, Leaf::typed(array))).collect(),
        })
    }
}

impl Normalizer {
    pub(crate) fn class(values: &Values) -> Option<Class> {
        match values {
            Values::Scaled { .. } | Values::I64(_) | Values::Dec(_) => Some(Class::Num),
            Values::Bool { .. } => Some(Class::Bool),
            Values::Text { .. } | Values::Utf8 { .. } => Some(Class::Text),
            Values::Dict { values, .. } => match values {
                Dictionary::Scaled { .. } => Some(Class::Num),
                Dictionary::Bool { .. } => Some(Class::Bool),
                Dictionary::Text { .. } => Some(Class::Text),
                Dictionary::Column(column) => match column.values {
                    Values::Scaled { .. } => Some(Class::Num),
                    Values::Bool { .. } => Some(Class::Bool),
                    Values::Text { .. } | Values::Utf8 { .. } => Some(Class::Text),
                    _ => None,
                },
                Dictionary::Any(_) => None,
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    struct Random(u64);

    impl Random {
        fn next(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n.max(1)
        }

        fn text(&mut self, wide: u64) -> Value {
            let pool = ["", "a", "ab", "abc", "loan", "saving", "savings", "checking", "currency", "a much longer text value", "a much longer text valuf", "ü€x"];
            match wide {
                0 => json!(pool[self.next(pool.len() as u64) as usize]),
                spread => json!(format!("w{}", self.next(spread))),
            }
        }

        fn value(&mut self, kind: u64, wide: u64) -> Value {
            match kind {
                0 => json!(self.next(5000)),
                1 => json!(self.next(5000) as f64 / 8.0),
                2 => self.text(wide),
                3 => json!(self.next(2) == 0),
                4 => Value::Null,
                5 => json!([self.next(3)]),
                6 => json!({"x": 1}),
                _ => serde_json::from_str("123456789012345678901234567890").unwrap_or(Value::Null),
            }
        }

        fn rows(&mut self, mixed: u64) -> Vec<Variable> {
            let keys = ["balance", "kind", "active", "rate", "note"];
            let kinds: Vec<u64> = (0..keys.len()).map(|_| self.next(4)).collect();
            let wide = [0, 0, 0, 8, 16, 17, 18, 40, 64, 65, 90][self.next(11) as usize];
            let many = self.next(4) == 0;
            (0..self.next(70) + 1)
                .map(|_| {
                    if self.next(12) == 0 {
                        let kind = self.next(3);
                        return Variable::from(self.value(kind, wide));
                    }
                    let items: Vec<Value> = (0..self.next(if many { 40 } else { 6 }))
                        .map(|_| {
                            if mixed > 0 && self.next(200 / mixed) == 0 {
                                let kind = self.next(8);
                                return self.value(kind, wide);
                            }
                            let mut map = serde_json::Map::new();
                            for (at, key) in keys.iter().enumerate() {
                                if mixed > 0 && self.next(12) == 0 {
                                    continue;
                                }
                                let kind = match mixed > 0 && self.next(40) == 0 {
                                    true => self.next(8),
                                    false => kinds[at],
                                };
                                map.insert(key.to_string(), self.value(kind, wide));
                            }
                            let odd = if mixed == 2 { self.next(300) } else { 9 };
                            match odd {
                                0 => {
                                    map.insert("a.b".to_string(), json!(1));
                                }
                                1 => {
                                    (0..40).for_each(|at| {
                                        map.insert(format!("k{at}"), json!(at));
                                    });
                                }
                                _ => {}
                            }
                            Value::Object(map)
                        })
                        .collect();
                    Variable::from(Value::Array(items))
                })
                .collect()
        }
    }

    fn render(shred: Option<Shred>) -> String {
        let Some(shred) = shred else {
            return "none".to_string();
        };
        let fields: Vec<String> = shred.fields.iter().map(|(name, array)| format!("{name}={array:?}")).collect();
        format!("{:?} {:?} {} {:?} {fields:?}", shred.frame.offsets, shred.frame.validity, shred.frame.children, shred.hosted)
    }

    fn recoded(shred: &Option<Shred>) -> usize {
        let fields = shred.iter().flat_map(|shred| shred.fields.iter());
        fields
            .map(|(_, array)| match array.store() {
                Store::Coded { codes, dict } => {
                    let Dict::Text { offsets, data } = dict.as_ref() else {
                        panic!("coded text dictionary");
                    };
                    let mut spelled = vec![0i32];
                    let mut text = String::new();
                    for &code in codes.iter() {
                        let at = usize::try_from(code).unwrap_or(usize::MAX);
                        text.push_str(&data[offsets[at] as usize..offsets[at + 1] as usize]);
                        spelled.push(text.len() as i32);
                    }
                    let (again, dict_again) = Coder::new(text.as_bytes()).codes(&spelled).expect("old coder codes");
                    assert_eq!(again.as_slice(), codes.as_ref());
                    assert_eq!(format!("{dict_again:?}"), format!("{dict:?}"));
                    1
                }
                Store::Text { offsets, data } => {
                    assert!(Coder::new(data.as_bytes()).codes(offsets).is_none());
                    0
                }
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn uniform_lists_shred_like_row_by_row() {
        let mut random = Random(0x9E37_79B9_7F4A_7C15);
        let (mut uniform, mut coded) = (0, 0);
        for round in 0..3000u64 {
            let rows = random.rows(round % 3);
            let count = rows.len();
            let valid = Bits::of(count, |_| random.next(9) != 0);
            let column = match round % 2 {
                0 => Column::new(Values::Any(&rows)),
                _ => Column::with_validity(Values::Any(&rows), &valid, 0),
            };
            let capacity = rows
                .iter()
                .map(|value| match value {
                    Variable::Array(items) => items.borrow().len(),
                    _ => 0,
                })
                .sum();
            let generic = ListBuilder::new(count, capacity).each(&column);
            coded += recoded(&generic);
            let expected = render(generic);
            if let Some(shred) = ListBuilder::new(count, capacity).uniform(&column, &rows) {
                uniform += 1;
                assert_eq!(render(shred), expected, "round {round}");
            }
            assert_eq!(render(Shred::of(&column, count)), expected, "round {round}");
        }
        assert!(uniform > 1000 && coded > 1000, "{uniform} {coded}");
    }
}
