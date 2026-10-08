use crate::lane::date::Date;
use crate::lane::program::{Kind, Layout};
use crate::variable::{Variable, VariableMap};
use crate::vm::VMError;
use chrono::DateTime;
use chrono_tz::Tz;
use rust_decimal::Decimal;
use std::sync::Arc;
use zen_types::symbol::Symbol;

#[derive(Debug)]
pub enum Cell<'a> {
    Number(Decimal),
    Bool(bool),
    Str(&'a str),
    Date(Option<DateTime<Tz>>),
    Value(&'a Variable),
    Struct(&'a [(Arc<str>, Output)], usize),
    List(&'a Output, usize, usize),
    Error(&'a VMError),
}

#[derive(Debug, Clone)]
pub(crate) enum Item {
    Num(i64, u8),
    Text(u32, u32),
    Bool(bool),
    Value(Variable),
    Row(u32),
}

impl Item {
    pub(crate) fn variable(&self, arena: &str) -> Variable {
        match self {
            Item::Num(m, s) => Variable::Number(Decimal::new(*m, *s as u32)),
            Item::Text(a, b) => Variable::String(Symbol::from(
                arena.get(*a as usize..*b as usize).unwrap_or_default(),
            )),
            Item::Bool(b) => Variable::Bool(*b),
            Item::Value(v) => v.clone(),
            Item::Row(_) => Variable::Null,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Items {
    tags: Vec<u8>,
    payload: Vec<i64>,
    scales: Vec<u8>,
    values: Vec<Variable>,
}

impl Items {
    const NUM: u8 = 0;
    const TEXT: u8 = 1;
    const BOOL: u8 = 2;
    const VALUE: u8 = 3;
    const ROW: u8 = 4;

    pub(crate) fn len(&self) -> usize {
        self.tags.len()
    }

    pub(crate) fn clear(&mut self) {
        self.tags.clear();
        self.payload.clear();
        self.scales.clear();
        self.values.clear();
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.tags.truncate(len);
        self.payload.truncate(len);
        self.scales.truncate(len);
        let kept = self.tags.iter().filter(|t| **t == Self::VALUE).count();
        self.values.truncate(kept);
    }

    pub(crate) fn push(&mut self, item: Item) {
        let (tag, payload, scale) = match item {
            Item::Num(m, s) => (Self::NUM, m, s),
            Item::Text(a, b) => (Self::TEXT, ((a as i64) << 32) | b as i64, 0),
            Item::Bool(b) => (Self::BOOL, b as i64, 0),
            Item::Value(v) => {
                self.values.push(v);
                (Self::VALUE, self.values.len() as i64 - 1, 0)
            }
            Item::Row(index) => (Self::ROW, index as i64, 0),
        };
        self.tags.push(tag);
        self.payload.push(payload);
        self.scales.push(scale);
    }

    pub(crate) fn extend_nums(&mut self, mant: &[i64], scales: &[u8]) {
        self.tags.resize(self.tags.len() + mant.len(), Self::NUM);
        self.payload.extend_from_slice(mant);
        self.scales.extend_from_slice(scales);
    }

    pub(crate) fn get(&self, index: usize) -> Item {
        let (Some(tag), Some(payload), Some(scale)) = (
            self.tags.get(index),
            self.payload.get(index),
            self.scales.get(index),
        ) else {
            return Item::Value(Variable::Null);
        };
        match *tag {
            Self::NUM => Item::Num(*payload, *scale),
            Self::TEXT => Item::Text((*payload >> 32) as u32, *payload as u32),
            Self::BOOL => Item::Bool(*payload != 0),
            Self::ROW => Item::Row(*payload as u32),
            _ => Item::Value(
                self.values
                    .get(*payload as usize)
                    .cloned()
                    .unwrap_or(Variable::Null),
            ),
        }
    }

    pub(crate) fn row(&self, index: usize) -> Option<u32> {
        match self.tags.get(index) {
            Some(&Self::ROW) => self.payload.get(index).map(|p| *p as u32),
            _ => None,
        }
    }

    pub(crate) fn number(&self, index: usize) -> Option<(i64, u8)> {
        match self.tags.get(index) {
            Some(&Self::NUM) => Some((*self.payload.get(index)?, *self.scales.get(index)?)),
            _ => None,
        }
    }

    pub(crate) fn extend_selected(&mut self, values: &[i64], truths: &[u64], a: usize, b: usize) {
        let (Some(values), Some(words)) = (values.get(a..b), truths.get(a / 64..b.div_ceil(64)))
        else {
            return;
        };
        let start = self.payload.len();
        self.payload.resize(start + values.len() + 1, 0);
        let (out, first) = (&mut self.payload[start..], a / 64);
        let mut k = 0;
        for (i, v) in (a..b).zip(values) {
            out[k] = *v;
            k += ((words[i / 64 - first] >> (i % 64)) & 1) as usize;
        }
        self.payload.truncate(start + k);
        self.tags.resize(start + k, Self::NUM);
        self.scales.resize(start + k, 0);
    }

    pub(crate) fn extend_parts(&mut self, parts: impl Iterator<Item = Option<(i64, u8)>>) -> bool {
        let start = self.payload.len();
        for part in parts {
            let Some((m, s)) = part else {
                self.payload.truncate(start);
                self.scales.truncate(start);
                return false;
            };
            self.payload.push(m);
            self.scales.push(s);
        }
        self.tags.resize(self.payload.len(), Self::NUM);
        true
    }

    pub(crate) fn extend_rows(&mut self, truths: &[u64], a: usize, b: usize, offset: usize) {
        let Some(words) = truths.get(a / 64..b.div_ceil(64)) else {
            return;
        };
        let start = self.payload.len();
        self.payload.resize(start + (b - a) + 1, 0);
        let (out, first) = (&mut self.payload[start..], a / 64);
        let mut k = 0;
        for i in a..b {
            out[k] = (offset + i) as i64;
            k += ((words[i / 64 - first] >> (i % 64)) & 1) as usize;
        }
        self.payload.truncate(start + k);
        self.tags.resize(start + k, Self::ROW);
        self.scales.resize(start + k, 0);
    }

    pub(crate) fn rows(&self, a: usize, b: usize) -> Option<&[i64]> {
        let tags = self.tags.get(a..b)?;
        tags.iter()
            .all(|t| *t == Self::ROW)
            .then(|| self.payload.get(a..b))
            .flatten()
    }

    pub(crate) fn numbers(&self, a: usize, b: usize) -> Option<(&[i64], &[u8])> {
        let tags = self.tags.get(a..b)?;
        tags.iter()
            .all(|t| *t == Self::NUM)
            .then(|| self.payload.get(a..b).zip(self.scales.get(a..b)))
            .flatten()
    }

    pub(crate) fn variable(&self, index: usize, arena: &str) -> Variable {
        self.get(index).variable(arena)
    }
}

#[derive(Debug, Default)]
pub enum Shape {
    #[default]
    Scalar,
    Struct(Vec<(Arc<str>, Output)>),
    List(Box<Output>),
}

#[derive(Debug)]
pub struct Output {
    rows: usize,
    kind: Kind,
    pending: bool,
    pub(crate) shape: Shape,
    pub(crate) mant: Vec<i64>,
    pub(crate) scale: Vec<u8>,
    pub(crate) dates: Vec<Option<DateTime<Tz>>>,
    pub(crate) bits: Vec<u64>,
    pub(crate) boxed: Vec<u64>,
    pub(crate) failed: Vec<u64>,
    pub(crate) values: Vec<Variable>,
    pub(crate) offsets: Vec<u32>,
    pub(crate) data: Vec<u8>,
    pub(crate) extra: Vec<(u32, Variable)>,
    pub(crate) errors: Vec<(u32, Failure)>,
    prefer: bool,
    coding: bool,
    coded: Coded,
}

#[derive(Debug, Default)]
struct Coded {
    keys: Vec<i32>,
    offsets: Vec<i32>,
    data: String,
}

impl Coded {
    const CAP: usize = 64;

    fn clear(&mut self) {
        self.keys.clear();
        self.offsets.clear();
        self.offsets.push(0);
        self.data.clear();
    }

    fn entry(&self, code: usize) -> Option<&str> {
        let (a, b) = (*self.offsets.get(code)? as usize, *self.offsets.get(code + 1)? as usize);
        self.data.get(a..b)
    }

    fn intern(&mut self, text: &str) -> Option<i32> {
        let count = self.offsets.len() - 1;
        if let Some(code) = (0..count).find(|&code| self.entry(code) == Some(text)) {
            return Some(code as i32);
        }
        if count >= Self::CAP {
            return None;
        }
        self.data.push_str(text);
        self.offsets.push(self.data.len() as i32);
        Some(count as i32)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Failure {
    fault: crate::lane::exec::Fault,
    error: std::cell::OnceCell<VMError>,
}

impl Failure {
    pub(crate) fn new(fault: crate::lane::exec::Fault) -> Self {
        Self {
            fault,
            error: std::cell::OnceCell::new(),
        }
    }

    fn error(&self) -> &VMError {
        self.error.get_or_init(|| self.fault.clone().vm())
    }
}

impl Default for Output {
    fn default() -> Self {
        Self {
            rows: 0,
            kind: Kind::Dyn,
            pending: false,
            shape: Shape::Scalar,
            mant: Vec::new(),
            scale: Vec::new(),
            dates: Vec::new(),
            bits: Vec::new(),
            boxed: Vec::new(),
            failed: Vec::new(),
            values: Vec::new(),
            offsets: Vec::new(),
            data: Vec::new(),
            extra: Vec::new(),
            errors: Vec::new(),
            prefer: false,
            coding: false,
            coded: Coded::default(),
        }
    }
}

impl Output {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn reset(&mut self, kind: Kind, rows: usize) {
        let words = rows.div_ceil(64);
        self.rows = rows;
        self.kind = kind;
        self.pending = false;
        let child = match std::mem::take(&mut self.shape) {
            Shape::List(child) => Some(child),
            _ => None,
        };
        self.mant.clear();
        self.scale.clear();
        self.dates.clear();
        self.values.clear();
        self.offsets.clear();
        self.data.clear();
        self.extra.clear();
        self.errors.clear();
        self.coding = self.prefer && kind == Kind::Str;
        if self.coding {
            self.coded.clear();
        }
        match kind {
            Kind::Num => {
                self.mant.resize(rows, 0);
                self.scale.resize(rows, 0);
            }
            Kind::Dyn => self.values.resize(rows, Variable::Null),
            Kind::Str => self.offsets.push(0),
            Kind::Date => self.dates.resize(rows, None),
            Kind::Bool => {}
            Kind::List => {
                self.offsets.push(0);
                let mut child = child.unwrap_or_default();
                child.open();
                self.shape = Shape::List(child);
            }
        }
        for bits in [&mut self.bits, &mut self.boxed, &mut self.failed] {
            bits.clear();
            bits.resize(words, 0);
        }
    }

    pub(crate) fn reset_layout(&mut self, layout: &Layout, kinds: &[Kind], rows: usize) {
        match layout {
            Layout::Value(reg) => self.reset(kinds[*reg as usize], rows),
            Layout::Struct(fields) => {
                self.reset(Kind::Bool, rows);
                let mut outputs = match std::mem::take(&mut self.shape) {
                    Shape::Struct(outputs) if outputs.len() == fields.len() => outputs,
                    _ => fields
                        .iter()
                        .map(|(k, _)| (k.clone(), Output::new()))
                        .collect(),
                };
                for ((key, output), (name, field)) in outputs.iter_mut().zip(fields.iter()) {
                    key.clone_from(name);
                    output.reset_layout(field, kinds, rows);
                }
                self.shape = Shape::Struct(outputs);
            }
            Layout::List(items) => {
                let mut child = match std::mem::take(&mut self.shape) {
                    Shape::List(child) => child,
                    _ => Box::default(),
                };
                self.reset(Kind::Bool, rows);
                let width = items.len();
                match Self::uniform(items, kinds) {
                    Some(kind) => child.reset(kind, rows * width),
                    None => child.reset(Kind::Dyn, rows * width),
                }
                self.offsets.extend((0..=rows).map(|r| (r * width) as u32));
                self.shape = Shape::List(child);
            }
        }
    }

    fn open(&mut self) {
        self.reset(Kind::Dyn, 0);
        self.pending = true;
    }

    fn grow(&mut self) -> usize {
        let row = self.rows;
        self.rows += 1;
        if row.is_multiple_of(64) {
            for bits in [&mut self.bits, &mut self.boxed, &mut self.failed] {
                bits.push(0);
            }
        }
        row
    }

    pub(crate) fn extend_nums(&mut self, mant: &[i64], scales: &[u8]) {
        if self.pending {
            self.pending = false;
            self.kind = Kind::Num;
        }
        if self.kind != Kind::Num {
            for (m, s) in mant.iter().zip(scales) {
                self.push_item(&Item::Num(*m, *s), "");
            }
            return;
        }
        self.rows += mant.len();
        let words = self.rows.div_ceil(64);
        for bits in [&mut self.bits, &mut self.boxed, &mut self.failed] {
            bits.resize(words, 0);
        }
        self.mant.extend_from_slice(mant);
        self.scale.extend_from_slice(scales);
    }

    pub(crate) fn push_item(&mut self, item: &Item, arena: &str) {
        if self.pending {
            self.pending = false;
            self.kind = match item {
                Item::Num(..) => Kind::Num,
                Item::Text(..) => {
                    self.offsets.push(0);
                    Kind::Str
                }
                Item::Bool(_) => Kind::Bool,
                Item::Value(_) | Item::Row(_) => Kind::Dyn,
            };
        }
        let row = self.grow();
        match (self.kind, item) {
            (Kind::Num, Item::Num(m, s)) => {
                self.mant.push(*m);
                self.scale.push(*s);
            }
            (Kind::Str, Item::Text(a, b)) => {
                self.push_text(arena.get(*a as usize..*b as usize).unwrap_or_default())
            }
            (Kind::Bool, Item::Bool(b)) => self.set_bit(row, *b),
            (Kind::Dyn, item) => self.values.push(item.variable(arena)),
            (kind, item) => {
                match kind {
                    Kind::Num => {
                        self.mant.push(0);
                        self.scale.push(0);
                    }
                    Kind::Date => self.dates.push(None),
                    _ => {}
                }
                self.box_at(row, item.variable(arena));
            }
        }
    }

    pub(crate) fn extend_lists(
        &mut self,
        mant: &[i64],
        scales: &[u8],
        ends: impl Iterator<Item = u32>,
    ) -> bool {
        let Shape::List(child) = &mut self.shape else {
            return false;
        };
        let base = child.rows as u32;
        child.extend_nums(mant, scales);
        self.offsets.extend(ends.map(|end| base + end));
        true
    }

    pub(crate) fn close(&mut self) {
        if let Shape::List(child) = &self.shape {
            self.offsets.push(child.rows as u32);
        }
    }

    pub(crate) fn uniform(items: &[Layout], kinds: &[Kind]) -> Option<Kind> {
        let mut kinds = items.iter().map(|item| match item {
            Layout::Value(reg) => Some(kinds[*reg as usize]),
            _ => None,
        });
        let first = kinds.next().flatten()?;
        kinds.all(|k| k == Some(first)).then_some(first)
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn shape(&self) -> &Shape {
        &self.shape
    }

    pub fn fields(&self) -> &[(Arc<str>, Output)] {
        match &self.shape {
            Shape::Struct(fields) => fields,
            _ => &[],
        }
    }

    pub fn child(&self) -> Option<&Output> {
        match &self.shape {
            Shape::List(child) => Some(child),
            _ => None,
        }
    }

    pub fn mantissas(&self) -> &[i64] {
        &self.mant
    }

    pub fn scales(&self) -> &[u8] {
        &self.scale
    }

    pub fn dates(&self) -> &[Option<DateTime<Tz>>] {
        &self.dates
    }

    pub fn bools(&self) -> &[u64] {
        &self.bits
    }

    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn prefer_codes(&mut self) {
        self.prefer = true;
    }

    pub fn prefer_text(&mut self) {
        self.prefer = false;
    }

    pub fn codes(&self) -> Option<(&[i32], &[i32], &str)> {
        (self.coding && self.kind == Kind::Str).then_some((self.coded.keys.as_slice(), self.coded.offsets.as_slice(), self.coded.data.as_str()))
    }

    pub(crate) fn coding(&self) -> bool {
        self.coding
    }

    pub(crate) fn push_code(&mut self, code: i32) {
        self.coded.keys.push(code);
    }

    pub(crate) fn last_code(&self) -> Option<i32> {
        self.coded.keys.last().copied().filter(|_| self.coding)
    }

    fn spill(&mut self) {
        self.coding = false;
        let keys = std::mem::take(&mut self.coded.keys);
        for key in &keys {
            let text = usize::try_from(*key).ok().and_then(|code| self.coded.entry(code)).unwrap_or_default();
            self.data.extend_from_slice(text.as_bytes());
            self.offsets.push(self.data.len() as u32);
        }
        self.coded.keys = keys;
    }

    pub(crate) fn push_empty(&mut self) {
        match self.coding {
            true => self.coded.keys.push(-1),
            false => self.offsets.push(self.data.len() as u32),
        }
    }

    pub fn text(&self, row: usize) -> Option<&str> {
        if self.coding {
            return match *self.coded.keys.get(row)? {
                code if code < 0 => Some(""),
                code => self.coded.entry(code as usize),
            };
        }
        let (a, b) = (
            *self.offsets.get(row)? as usize,
            *self.offsets.get(row + 1)? as usize,
        );
        std::str::from_utf8(self.data.get(a..b)?).ok()
    }

    pub fn boxed(&self) -> &[u64] {
        &self.boxed
    }

    pub fn failed(&self) -> &[u64] {
        &self.failed
    }

    pub fn boxed_values(&self) -> &[(u32, Variable)] {
        &self.extra
    }

    pub fn take_numbers(&mut self) -> (Vec<i64>, Vec<u8>) {
        (std::mem::take(&mut self.mant), std::mem::take(&mut self.scale))
    }

    pub fn take_values(&mut self) -> Vec<Variable> {
        std::mem::take(&mut self.values)
    }

    pub fn take_bits(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.bits)
    }

    pub fn take_text(&mut self) -> (Vec<u32>, Vec<u8>) {
        if self.coding {
            self.spill();
        }
        (std::mem::take(&mut self.offsets), std::mem::take(&mut self.data))
    }

    pub(crate) fn fail_at(&mut self, at: usize) {
        if let Some(w) = self.failed.get_mut(at / 64) {
            *w |= 1 << (at % 64);
        }
        match (&self.shape, self.kind) {
            (Shape::Scalar, Kind::Str) => self.push_empty(),
            (Shape::List(_), Kind::List) => self.close(),
            _ => {}
        }
    }

    pub(crate) fn put(&mut self, at: usize, value: Variable) {
        match (self.kind, value) {
            (Kind::Dyn, value) => {
                if let Some(slot) = self.values.get_mut(at) {
                    *slot = value;
                }
            }
            (Kind::Num, Variable::Number(n)) if crate::lane::scaled::Scaled::parts(&n).is_some() => {
                if let (Some((m, s)), Some(mant), Some(scale)) =
                    (crate::lane::scaled::Scaled::parts(&n), self.mant.get_mut(at), self.scale.get_mut(at))
                {
                    *mant = m;
                    *scale = s;
                }
            }
            (Kind::Bool, Variable::Bool(b)) => self.set_bit(at, b),
            (Kind::Str, Variable::String(text)) => self.push_text(text.as_str()),
            (Kind::Date, value) if !crate::lane::date::Date::sourced(&value) && crate::lane::date::Date::of(&value).is_some() => {
                if let (Some(date), Some(slot)) = (crate::lane::date::Date::of(&value), self.dates.get_mut(at)) {
                    *slot = date.0;
                }
            }
            (_, value) => self.box_at(at, value),
        }
    }

    pub(crate) fn box_at(&mut self, at: usize, value: Variable) {
        if let Some(w) = self.boxed.get_mut(at / 64) {
            *w |= 1 << (at % 64);
        }
        match self.kind {
            Kind::Str => self.push_empty(),
            Kind::List => self.close(),
            _ => {}
        }
        self.extra.push((at as u32, value));
    }

    pub(crate) fn set_bit(&mut self, at: usize, on: bool) {
        if let Some(w) = self.bits.get_mut(at / 64) {
            *w = (*w & !(1 << (at % 64))) | ((on as u64) << (at % 64));
        }
    }

    pub(crate) fn push_text(&mut self, text: &str) {
        if self.coding {
            match self.coded.intern(text) {
                Some(code) => return self.coded.keys.push(code),
                None => self.spill(),
            }
        }
        self.data.extend_from_slice(text.as_bytes());
        self.offsets.push(self.data.len() as u32);
    }

    fn bit(bits: &[u64], row: usize) -> bool {
        bits.get(row / 64).is_some_and(|w| w >> (row % 64) & 1 == 1)
    }

    fn find<T>(entries: &[(u32, T)], row: usize) -> Option<&T> {
        entries
            .binary_search_by_key(&(row as u32), |(r, _)| *r)
            .ok()
            .map(|i| &entries[i].1)
    }

    pub fn get(&self, row: usize) -> Option<Cell<'_>> {
        if row >= self.rows {
            return None;
        }
        if Self::bit(&self.failed, row) {
            return Self::find(&self.errors, row).map(|failure| Cell::Error(failure.error()));
        }
        if self.kind != Kind::Dyn && Self::bit(&self.boxed, row) {
            return Self::find(&self.extra, row).map(Cell::Value);
        }
        match &self.shape {
            Shape::Struct(fields) => return Some(Cell::Struct(fields, row)),
            Shape::List(child) => {
                let (a, b) = (
                    *self.offsets.get(row)? as usize,
                    *self.offsets.get(row + 1)? as usize,
                );
                return Some(Cell::List(child, a, b));
            }
            Shape::Scalar => {}
        }
        if self.kind == Kind::Dyn {
            return self.values.get(row).map(Cell::Value);
        }
        if Self::bit(&self.boxed, row) {
            return Self::find(&self.extra, row).map(Cell::Value);
        }
        Some(match self.kind {
            Kind::Num => Cell::Number(Decimal::new(self.mant[row], self.scale[row] as u32)),
            Kind::Str => Cell::Str(self.text(row)?),
            Kind::Date => Cell::Date(*self.dates.get(row)?),
            _ => Cell::Bool(Self::bit(&self.bits, row)),
        })
    }

    pub fn variable(&self, row: usize) -> Option<Result<Variable, VMError>> {
        Some(match self.get(row)? {
            Cell::Number(n) => Ok(Variable::Number(n)),
            Cell::Bool(b) => Ok(Variable::Bool(b)),
            Cell::Str(s) => Ok(Variable::String(s.into())),
            Cell::Date(d) => Ok(Date(d).variable()),
            Cell::Value(v) => Ok(v.clone()),
            Cell::Struct(fields, row) => {
                let mut map = VariableMap::with_capacity(fields.len());
                for (key, field) in fields.iter().rev() {
                    map.insert(Symbol::from(key.as_ref()), field.variable(row)?.ok()?);
                }
                Ok(Variable::from_object(map))
            }
            Cell::List(child, a, b) => Ok(Variable::from_array(
                (a..b)
                    .map(|i| child.variable(i)?.ok())
                    .collect::<Option<Vec<_>>>()?,
            )),
            Cell::Error(e) => Err(e.clone()),
        })
    }
}
