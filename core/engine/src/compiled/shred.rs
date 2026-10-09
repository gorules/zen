use crate::compiled::typed::{Array, Bits, Dict, Leaf, Store};
use crate::nodes::input::dates::DeclaredDates;
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
    pub offsets: Rc<[i32]>,
    pub validity: Option<Rc<[u64]>>,
    pub hosted: Option<Rc<[u64]>>,
    pub children: usize,
    pub fields: Vec<(Arc<str>, Leaf<'a>)>,
}

pub(crate) struct Shred {
    pub offsets: Rc<[i32]>,
    pub validity: Option<Rc<[u64]>>,
    pub children: usize,
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
                let mut field = FieldBuilder::new(path.clone(), 0, rows);
                for (row, value) in values.iter().enumerate() {
                    match Bits::get(&present, row) {
                        true => field.push(value, rows),
                        false => field.missing(),
                    }
                }
                let (_, array) = field.array(false);
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
        let hash = (key.0 ^ key.1.rotate_left(29) ^ (len as u64).rotate_right(8)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut slot = (hash >> 57) as usize;
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

enum Cells {
    Empty,
    Scaled { mant: Vec<i64>, scale: Vec<u8> },
    Text { offsets: Vec<i32>, data: String },
    Bool(Vec<u64>),
    Any(Vec<Variable>),
}

struct FieldBuilder {
    name: Arc<str>,
    cells: Cells,
    present: Vec<u64>,
    len: usize,
    count: usize,
}

impl FieldBuilder {
    fn new(name: Arc<str>, len: usize, capacity: usize) -> Self {
        let mut field = Self {
            name,
            cells: Cells::Empty,
            present: Vec::with_capacity(capacity.div_ceil(64)),
            len: 0,
            count: 0,
        };
        (0..len).for_each(|_| field.missing());
        field
    }


    fn bit(&mut self, on: bool) {
        if self.len & 63 == 0 {
            self.present.push(0);
        }
        if on {
            Bits::set(&mut self.present, self.len, true);
        }
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
            Cells::Any(values) => values.push(Variable::Null),
        }
        self.len += 1;
    }

    fn variables(&self) -> Vec<Variable> {
        let mut values = Vec::with_capacity(self.len + 1);
        for at in 0..self.len {
            let value = match (&self.cells, Bits::get(&self.present, at)) {
                (_, false) | (Cells::Empty, _) => Variable::Null,
                (Cells::Scaled { mant, scale }, true) => Variable::Number(rust_decimal::Decimal::new(mant[at], u32::from(scale[at]))),
                (Cells::Text { offsets, data }, true) => Variable::String(data[offsets[at] as usize..offsets[at + 1] as usize].into()),
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
            (Cells::Text { offsets, data }, Variable::String(text)) => match i32::try_from(data.len() + text.len()) {
                Ok(end) => {
                    data.push_str(text);
                    offsets.push(end);
                    true
                }
                Err(_) => false,
            },
            (Cells::Bool(bits), Variable::Bool(on)) => {
                bits.resize((len + 1).div_ceil(64), 0);
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
        self.count += 1;
    }

    fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }
        let mut values = self.variables();
        values.truncate(len);
        self.cells = Cells::Any(values);
        self.present.truncate(len.div_ceil(64));
        Bits::trim(&mut self.present, len);
        self.count = (0..len).filter(|&at| Bits::get(&self.present, at)).count();
        self.len = len;
    }

    fn array(self, code: bool) -> (Arc<str>, Array) {
        let len = self.len;
        let valid = (self.count < len).then_some(self.present);
        let store = match self.cells {
            Cells::Empty => Store::Any(vec![Variable::Null; len]),
            Cells::Scaled { mant, scale } => Store::Scaled { mant, scale },
            Cells::Text { offsets, data } => match code.then(|| Coder::new(data.as_bytes()).codes(&offsets)).flatten() {
                Some((codes, dict)) => Store::Coded { codes: codes.into(), dict: Rc::new(dict) },
                None => Store::Text { offsets, data },
            },
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
        self.fields.push(FieldBuilder::new(Arc::from(key), self.children, self.capacity));
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

    fn finish(self) -> Shred {
        let validity = match Bits::ones(self.rows) == self.validity {
            true => None,
            false => Some(Rc::from(self.validity)),
        };
        Shred {
            offsets: self.offsets.into(),
            validity,
            children: self.children,
            fields: self.fields.into_iter().map(|field| field.array(true)).collect(),
            hosted: self.hosted.into(),
        }
    }
}

impl Shred {
    pub fn of(column: &Column, rows: usize) -> Option<Shred> {
        let capacity = match column.values {
            Values::Any(values) => values
                .iter()
                .take(rows)
                .map(|value| match value {
                    Variable::Array(items) => items.borrow().len(),
                    _ => 0,
                })
                .sum(),
            Values::List { child, .. } => child.len(),
            _ => return None,
        };
        let mut builder = ListBuilder::new(rows, capacity);
        for row in 0..rows {
            if !column.valid(row) {
                Bits::set(&mut builder.validity, row, false);
                builder.close()?;
                continue;
            }
            match column.values {
                Values::Any(values) => match values.get(row) {
                    Some(Variable::Array(items)) => builder.row(row, &items.borrow()),
                    _ => builder.host(row),
                },
                Values::List {
                    child: Dictionary::Any(items),
                    ..
                } => match column.range(row).and_then(|(a, b)| items.get(a..b)) {
                    Some(slice) => builder.row(row, slice),
                    None => builder.host(row),
                },
                _ => return None,
            }
            builder.close()?;
        }
        Some(builder.finish())
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
                            Some(Listing { offsets, validity, hosted: None, children: len, fields })
                        }
                        Values::Any(_) | Values::List { .. } | Values::Dict { .. } => None,
                        _ => {
                            let len = item.len();
                            Some(Listing { offsets, validity, hosted: None, children: len, fields: vec![(Arc::from(""), Normalizer::lean(*item, len))] })
                        }
                    },
                    scalar => {
                        let len = scalar.len();
                        Some(Listing { offsets, validity, hosted: None, children: len, fields: vec![(Arc::from(""), Normalizer::lean(scalar.column(), len))] })
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
            other => Self::copied(&other.column(), owners),
        }
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
            offsets: offsets.get(..=owners)?.into(),
            validity,
            hosted: None,
            children: len,
            fields: vec![(Arc::from(""), Leaf::typed(Array::parts(store, None, len)))],
        })
    }

    pub fn nested(&self, name: &str) -> Option<Listing<'a>> {
        let (_, leaf) = self.fields.iter().rev().find(|(key, _)| key.as_ref() == name)?;
        Listing::leaf(leaf, self.children)
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
            builder.fields.push(FieldBuilder::new(Arc::from(""), 0, capacity));
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
            offsets: shred.offsets,
            validity: shred.validity,
            hosted,
            children: shred.children,
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
