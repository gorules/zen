use crate::lane::{Column, Dictionary, Values};
use crate::typed::{Batch, Class, Dec, Hir, List, Resolve, Resolved};
use crate::variable::Variable;
use rust_decimal::Decimal;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Null,
    Num(Dec),
    Bool(bool),
    Str(String),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Row {
    pub(crate) columns: Vec<Value>,
    pub(crate) lists: Vec<Vec<Vec<Value>>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Enc {
    Scaled,
    I64,
    Dec,
    Bool,
    Text,
    Utf8,
    Dict,
}

#[derive(Clone, Debug)]
pub(crate) struct ColSpec {
    pub(crate) path: &'static str,
    pub(crate) class: Class,
    pub(crate) enc: Enc,
}

#[derive(Clone, Debug)]
pub(crate) struct ListSpec {
    pub(crate) path: &'static str,
    pub(crate) alias: &'static str,
    pub(crate) fields: Vec<ColSpec>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Spec {
    pub(crate) columns: Vec<ColSpec>,
    pub(crate) lists: Vec<ListSpec>,
}

pub(crate) struct Mock<'s> {
    pub(crate) spec: &'s Spec,
    pub(crate) member: Option<usize>,
}

enum Store {
    Num { mant: Vec<i64>, scale: Vec<u8> },
    I64(Vec<i64>),
    Dec(Vec<Decimal>),
    Bool(Vec<u64>),
    Text { offsets: Vec<i32>, data: String },
    Utf8 { offsets: Vec<i32>, data: Vec<u8> },
    Dict { keys: Vec<i32>, offsets: Vec<i32>, data: String },
}

pub(crate) struct Owned {
    store: Store,
    validity: Option<Vec<u64>>,
}

pub(crate) struct OwnedList {
    offsets: Vec<i32>,
    fields: Vec<Owned>,
}

pub(crate) struct Frame {
    pub(crate) rows: usize,
    columns: Vec<Owned>,
    lists: Vec<OwnedList>,
}

impl ColSpec {
    pub(crate) fn new(path: &'static str, class: Class, enc: Enc) -> Self {
        Self { path, class, enc }
    }
}

impl Resolve for Mock<'_> {
    fn path(&mut self, path: &str) -> Resolved {
        if let Some(c) = self.spec.columns.iter().find(|c| c.path == path) {
            return Resolved::Column(c.class);
        }
        match self.spec.lists.iter().enumerate().find(|(_, l)| l.path == path) {
            Some((at, _)) if self.member == Some(at) => Resolved::Unsupported,
            Some((_, l)) => Resolved::List { fields: l.fields.iter().map(|f| (Arc::from(f.path), f.class)).collect() },
            None => Resolved::Absent,
        }
    }

    fn alias(&mut self, field: &str) -> Resolved {
        let Some(list) = self.member.and_then(|m| self.spec.lists.get(m)) else {
            return Resolved::Unsupported;
        };
        match list.fields.iter().find(|f| f.path == field) {
            Some(f) => Resolved::Column(f.class),
            None => Resolved::Absent,
        }
    }
}

impl Value {
    pub(crate) fn variable(&self) -> Variable {
        match self {
            Value::Null => Variable::Null,
            Value::Num(d) => Variable::Number(d.decimal()),
            Value::Bool(b) => Variable::Bool(*b),
            Value::Str(s) => Variable::String(s.as_str().into()),
        }
    }
}

impl Row {
    pub(crate) fn variable(&self, spec: &Spec) -> Variable {
        let root = Variable::empty_object();
        for (def, value) in spec.columns.iter().zip(&self.columns) {
            root.dot_insert(def.path, value.variable());
        }
        for (def, records) in spec.lists.iter().zip(&self.lists) {
            let items = records
                .iter()
                .map(|record| {
                    let object = Variable::empty_object();
                    for (field, value) in def.fields.iter().zip(record) {
                        object.dot_insert(field.path, value.variable());
                    }
                    object
                })
                .collect();
            root.dot_insert(def.path, Variable::from_array(items));
        }
        root
    }
}

impl Owned {
    fn build(spec: &ColSpec, values: &[&Value], shift: usize) -> Self {
        let n = values.len() + shift;
        let mut validity = vec![u64::MAX; n.div_ceil(64)];
        let mut nulls = false;
        let lead = std::iter::repeat_n(&Value::Null, shift);
        let all: Vec<&Value> = lead.chain(values.iter().copied()).collect();
        for (i, v) in all.iter().enumerate() {
            if matches!(v, Value::Null) {
                validity[i / 64] &= !(1u64 << (i % 64));
                nulls = true;
            }
        }
        let num = |v: &Value| match v {
            Value::Num(d) => *d,
            _ => Dec::new(0, 0),
        };
        let text = |v: &Value| match v {
            Value::Str(s) => s.clone(),
            _ => String::new(),
        };
        let store = match spec.enc {
            Enc::Scaled => Store::Num { mant: all.iter().map(|v| num(v).mant).collect(), scale: all.iter().map(|v| num(v).scale).collect() },
            Enc::I64 => Store::I64(all.iter().map(|v| num(v).mant).collect()),
            Enc::Dec => Store::Dec(all.iter().map(|v| num(v).decimal()).collect()),
            Enc::Bool => {
                let mut bits = vec![0u64; n.div_ceil(64)];
                for (i, v) in all.iter().enumerate() {
                    if matches!(v, Value::Bool(true)) {
                        bits[i / 64] |= 1u64 << (i % 64);
                    }
                }
                Store::Bool(bits)
            }
            Enc::Text | Enc::Utf8 => {
                let mut offsets = vec![0i32];
                let mut data = String::new();
                for v in &all {
                    data.push_str(&text(v));
                    offsets.push(data.len() as i32);
                }
                match spec.enc {
                    Enc::Text => Store::Text { offsets, data },
                    _ => Store::Utf8 { offsets, data: data.into_bytes() },
                }
            }
            Enc::Dict => {
                let mut entries: Vec<String> = Vec::new();
                let mut keys = Vec::with_capacity(n);
                for v in &all {
                    let t = text(v);
                    let k = match entries.iter().position(|e| *e == t) {
                        Some(k) => k,
                        None => {
                            entries.push(t);
                            entries.len() - 1
                        }
                    };
                    keys.push(k as i32);
                }
                entries.reverse();
                let last = entries.len() as i32 - 1;
                let keys = keys.into_iter().map(|k| last - k).collect();
                let mut offsets = vec![0i32];
                let mut data = String::new();
                for e in &entries {
                    data.push_str(e);
                    offsets.push(data.len() as i32);
                }
                Store::Dict { keys, offsets, data }
            }
        };
        Self { store, validity: nulls.then_some(validity) }
    }

    pub(crate) fn column(&self) -> Column<'_> {
        let values = match &self.store {
            Store::Num { mant, scale } => Values::Scaled { mant, scale },
            Store::I64(v) => Values::I64(v),
            Store::Dec(v) => Values::Dec(v),
            Store::Bool(bits) => Values::Bool { bits, offset: 0 },
            Store::Text { offsets, data } => Values::Text { offsets, data },
            Store::Utf8 { offsets, data } => Values::Utf8 { offsets, data },
            Store::Dict { keys, offsets, data } => Values::Dict { keys, values: Dictionary::Text { offsets, data } },
        };
        match &self.validity {
            Some(bits) => Column::with_validity(values, bits, 0),
            None => Column::new(values),
        }
    }
}

impl Frame {
    pub(crate) fn new(hir: &Hir, spec: &Spec, rows: &[Row], shift: usize) -> Self {
        let mut columns = Vec::with_capacity(hir.schema.columns.len());
        for def in &hir.schema.columns {
            let at = spec.columns.iter().position(|c| c.path == def.path).expect("spec column");
            let values: Vec<&Value> = rows.iter().map(|r| r.columns.get(at).unwrap_or(&Value::Null)).collect();
            columns.push(Owned::build(&spec.columns[at], &values, 0));
        }
        let mut lists = Vec::with_capacity(hir.schema.lists.len());
        for def in &hir.schema.lists {
            let l = spec.lists.iter().position(|x| x.path == def.path).expect("spec list");
            let mut offsets = vec![shift as i32];
            for row in rows {
                let count = row.lists.get(l).map_or(0, Vec::len);
                offsets.push(offsets[offsets.len() - 1] + count as i32);
            }
            let mut fields = Vec::with_capacity(def.fields.len());
            for field in &def.fields {
                let f = spec.lists[l].fields.iter().position(|x| x.path == field.path).expect("spec field");
                let values: Vec<&Value> =
                    rows.iter().flat_map(|r| r.lists.get(l).into_iter().flatten()).map(|record| record.get(f).unwrap_or(&Value::Null)).collect();
                fields.push(Owned::build(&spec.lists[l].fields[f], &values, shift));
            }
            lists.push(OwnedList { offsets, fields });
        }
        Self { rows: rows.len(), columns, lists }
    }

    pub(crate) fn batch(&self) -> Batch<'_> {
        let mut batch = Batch::new(self.rows);
        for c in &self.columns {
            batch.push_column(c.column());
        }
        for l in &self.lists {
            batch.push_list(List::new(&l.offsets, None, l.fields.iter().map(Owned::column).collect()));
        }
        batch
    }

    pub(crate) fn range(&self, list: usize, row: usize) -> std::ops::Range<usize> {
        let l = &self.lists[list];
        let base = l.offsets[0] as usize;
        (l.offsets[row] as usize - base)..(l.offsets[row + 1] as usize - base)
    }
}
