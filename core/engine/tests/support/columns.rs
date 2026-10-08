#![allow(dead_code)]

use serde_json::Value;
use zen_expression::Variable;

pub enum Buf {
    Dec(Vec<rust_decimal::Decimal>),
    Text(Vec<i32>, Vec<u8>),
    Bool(Vec<u64>),
    Any(Vec<Variable>),
    List(Vec<i32>, Child),
}

pub enum Child {
    Text(Vec<i32>, String),
    Scaled(Vec<i64>, Vec<u8>),
    Bool(Vec<u64>),
    Struct(Vec<(String, Buf, Vec<u64>)>, usize, Vec<Variable>),
}

impl Child {
    pub fn structs(column: &[Option<Value>], items: &[&Value]) -> Option<Child> {
        let mut names: Vec<String> = Vec::new();
        for item in items {
            for key in item.as_object()?.keys() {
                if !names.contains(key) {
                    names.push(key.clone());
                }
            }
        }
        let words = items.len().div_ceil(64).max(1);
        let fields = names
            .into_iter()
            .map(|name| {
                let cells: Vec<Option<Value>> = items.iter().map(|item| item.get(&name).filter(|v| !v.is_null()).cloned()).collect();
                let mut valid = vec![0u64; words];
                cells.iter().enumerate().filter(|(_, c)| c.is_some()).for_each(|(i, _)| valid[i / 64] |= 1 << (i % 64));
                let buf = Buf::scalar(&cells, words)?;
                Some((name, buf, valid))
            })
            .collect::<Option<Vec<_>>>()?;
        let fallback = column
            .iter()
            .map(|c| c.as_ref().map_or(Variable::Null, |v| Variable::from(v.clone())))
            .collect();
        Some(Child::Struct(fields, items.len(), fallback))
    }

    pub fn of(column: &[Option<Value>]) -> Option<(Vec<i32>, Child)> {
        Self::shape(column, false)
    }

    pub fn shape(column: &[Option<Value>], nested: bool) -> Option<(Vec<i32>, Child)> {
        let items: Vec<&Value> = column
            .iter()
            .flatten()
            .map(|v| v.as_array().map(|a| a.iter()))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        let mut offsets = vec![0i32];
        let mut count = 0i32;
        for cell in column {
            count += cell.as_ref().and_then(Value::as_array).map_or(0, |a| a.len() as i32);
            offsets.push(count);
        }
        let child = match items.first() {
            None => Child::Text(vec![0], String::new()),
            Some(Value::String(_)) if items.iter().all(|v| v.is_string()) => {
                let mut child_offsets = vec![0i32];
                let mut data = String::new();
                for item in &items {
                    data.push_str(item.as_str().unwrap_or_default());
                    child_offsets.push(data.len() as i32);
                }
                Child::Text(child_offsets, data)
            }
            Some(Value::Number(_)) if items.iter().all(|v| v.is_number()) => {
                let parts: Option<Vec<(i64, u8)>> = items
                    .iter()
                    .map(|v| {
                        let d = Built::decimal(&v.to_string())?;
                        Some((i64::try_from(d.mantissa()).ok()?, u8::try_from(d.scale()).ok()?))
                    })
                    .collect();
                let parts = parts?;
                Child::Scaled(parts.iter().map(|p| p.0).collect(), parts.iter().map(|p| p.1).collect())
            }
            Some(Value::Bool(_)) if items.iter().all(|v| v.is_boolean()) => {
                let mut bits = vec![0u64; items.len().div_ceil(64).max(1)];
                for (i, item) in items.iter().enumerate() {
                    if item.as_bool() == Some(true) {
                        bits[i / 64] |= 1 << (i % 64);
                    }
                }
                Child::Bool(bits)
            }
            Some(Value::Object(_)) if nested && items.iter().all(|v| v.is_object()) => Self::structs(column, &items)?,
            _ => return None,
        };
        Some((offsets, child))
    }
}

pub struct Built {
    pub rows: usize,
    pub paths: Vec<String>,
    pub bufs: Vec<(Buf, Vec<u64>)>,
}

impl Buf {
    pub fn scalar(column: &[Option<Value>], words: usize) -> Option<Buf> {
        let present: Vec<&Value> = column.iter().flatten().collect();
        if present.is_empty() {
            return Some(Buf::Bool(vec![0u64; words]));
        }
        if present.iter().all(|v| v.as_number().is_some_and(|n| Built::decimal(&n.to_string()).is_some())) {
            return Some(Buf::Dec(
                column
                    .iter()
                    .map(|c| match c {
                        Some(Value::Number(n)) => Built::decimal(&n.to_string()).unwrap_or_default(),
                        _ => rust_decimal::Decimal::ZERO,
                    })
                    .collect(),
            ));
        }
        if present.iter().all(|v| v.is_string()) {
            let mut offsets = vec![0i32];
            let mut data = Vec::new();
            for cell in column {
                if let Some(Value::String(s)) = cell {
                    data.extend_from_slice(s.as_bytes());
                }
                offsets.push(data.len() as i32);
            }
            return Some(Buf::Text(offsets, data));
        }
        if present.iter().all(|v| v.is_boolean()) {
            let mut bits = vec![0u64; words];
            for (row, cell) in column.iter().enumerate() {
                if let Some(Value::Bool(true)) = cell {
                    bits[row / 64] |= 1 << (row % 64);
                }
            }
            return Some(Buf::Bool(bits));
        }
        None
    }

    fn values(&self) -> zen_expression::lane::Values<'_> {
        use zen_expression::lane::Values;
        match self {
            Buf::Dec(v) => Values::Dec(v),
            Buf::Text(offsets, data) => Values::Utf8 { offsets, data },
            Buf::Bool(bits) => Values::Bool { bits, offset: 0 },
            Buf::Any(v) => Values::Any(v),
            Buf::List(..) => Values::Any(&[]),
        }
    }
}

pub type Fields<'b> = Vec<Vec<(&'b str, zen_expression::lane::Column<'b>)>>;

impl Built {
    pub fn decimal(text: &str) -> Option<rust_decimal::Decimal> {
        text.parse()
            .ok()
            .or_else(|| rust_decimal::Decimal::from_scientific(text).ok())
    }

    pub fn flatten(value: &Value, prefix: &str, out: &mut Vec<(String, Value)>) -> bool {
        match value {
            Value::Object(map) => map.iter().all(|(key, child)| {
                !key.contains('.')
                    && Self::flatten(
                        child,
                        &match prefix.is_empty() {
                            true => key.clone(),
                            false => format!("{prefix}.{key}"),
                        },
                        out,
                    )
            }),
            Value::Null => true,
            other if !prefix.is_empty() => {
                out.push((prefix.to_string(), other.clone()));
                true
            }
            _ => false,
        }
    }

    pub fn new(rows: &[Value]) -> Option<Built> {
        Self::shaped(rows, false)
    }

    pub fn lists(rows: &[Value]) -> Option<Built> {
        Self::shaped(rows, true).filter(|built| built.bufs.iter().any(|(b, _)| matches!(b, Buf::List(..))))
    }

    pub fn nested(rows: &[Value]) -> Option<Built> {
        Self::build(rows, true, true).filter(|built| built.bufs.iter().any(|(b, _)| matches!(b, Buf::List(_, Child::Struct(..)))))
    }

    pub fn shaped(rows: &[Value], lists: bool) -> Option<Built> {
        Self::build(rows, lists, false)
    }

    pub fn fields(&self) -> Fields<'_> {
        use zen_expression::lane::Column;
        self.bufs
            .iter()
            .filter_map(|(buf, _)| match buf {
                Buf::List(_, Child::Struct(fields, _, _)) => Some(
                    fields
                        .iter()
                        .map(|(name, buf, valid)| (name.as_str(), Column::with_validity(buf.values(), valid, 0)))
                        .collect(),
                ),
                _ => None,
            })
            .collect()
    }

    pub fn structs<'b>(&self, fields: &'b Fields<'b>) -> Vec<zen_expression::lane::Column<'b>> {
        use zen_expression::lane::{Column, Values};
        let lens = self.bufs.iter().filter_map(|(buf, _)| match buf {
            Buf::List(_, Child::Struct(_, len, _)) => Some(*len),
            _ => None,
        });
        fields.iter().zip(lens).map(|(f, len)| Column::new(Values::Struct { fields: f, len })).collect()
    }

    pub fn columns_with<'c>(&'c self, structs: &'c [zen_expression::lane::Column<'c>]) -> zen_expression::lane::Columns<'c> {
        use zen_expression::lane::{Column, Columns, Dictionary, Values};
        let mut columns = Columns::new(self.rows);
        let mut next = structs.iter();
        for (path, (buf, valid)) in self.paths.iter().zip(&self.bufs) {
            let values = match buf {
                Buf::List(offsets, Child::Struct(_, len, fallback)) => match next.next() {
                    Some(child) if child.len() == *len || *len == 0 => Values::List {
                        offsets,
                        child: Dictionary::Column(child),
                    },
                    _ => Values::Any(fallback),
                },
                other => match Self::plain(other) {
                    Some(values) => values,
                    None => continue,
                },
            };
            columns = columns.column(path, Column::with_validity(values, valid, 0));
        }
        columns
    }

    fn build(rows: &[Value], lists: bool, nested: bool) -> Option<Built> {
        let conflicts = |a: &str, b: &str| {
            a.strip_prefix(b).is_some_and(|r| r.starts_with('.')) || b.strip_prefix(a).is_some_and(|r| r.starts_with('.'))
        };
        let mut kept: Vec<Vec<(String, Value)>> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for value in rows {
            let mut entries = Vec::new();
            if !Self::flatten(value, "", &mut entries) {
                continue;
            }
            if entries.iter().any(|(p, _)| seen.iter().any(|s| conflicts(p, s))) {
                continue;
            }
            for (p, _) in &entries {
                if !seen.contains(p) {
                    seen.push(p.clone());
                }
            }
            kept.push(entries);
        }
        if kept.is_empty() {
            return None;
        }
        let rows: Vec<()> = vec![(); kept.len()];
        let mut paths: Vec<String> = Vec::new();
        let mut cells: Vec<Vec<Option<Value>>> = Vec::new();
        for (row, entries) in kept.into_iter().enumerate() {
            for (path, leaf) in entries {
                let at = match paths.iter().position(|p| *p == path) {
                    Some(at) => at,
                    None => {
                        paths.push(path);
                        cells.push(vec![None; rows.len()]);
                        cells.len() - 1
                    }
                };
                cells[at][row] = Some(leaf);
            }
        }
        for (i, a) in paths.iter().enumerate() {
            if paths.iter().enumerate().any(|(j, b)| i != j && b.starts_with(&format!("{a}."))) {
                return None;
            }
        }
        let words = rows.len().div_ceil(64);
        let bufs = cells
            .into_iter()
            .map(|column| {
                let mut valid = vec![0u64; words];
                for (row, cell) in column.iter().enumerate() {
                    if cell.is_some() {
                        valid[row / 64] |= 1 << (row % 64);
                    }
                }
                let present: Vec<&Value> = column.iter().flatten().collect();
                let buf = if !present.is_empty() && present.iter().all(|v| v.as_number().is_some_and(|n| Self::decimal(&n.to_string()).is_some())) {
                    Buf::Dec(
                        column
                            .iter()
                            .map(|c| match c {
                                Some(Value::Number(n)) => Self::decimal(&n.to_string()).unwrap_or_default(),
                                _ => rust_decimal::Decimal::ZERO,
                            })
                            .collect(),
                    )
                } else if !present.is_empty() && present.iter().all(|v| v.is_string()) {
                    let mut offsets = vec![0i32];
                    let mut data = Vec::new();
                    for cell in &column {
                        if let Some(Value::String(s)) = cell {
                            data.extend_from_slice(s.as_bytes());
                        }
                        offsets.push(data.len() as i32);
                    }
                    Buf::Text(offsets, data)
                } else if !present.is_empty() && present.iter().all(|v| v.is_boolean()) {
                    let mut bits = vec![0u64; words];
                    for (row, cell) in column.iter().enumerate() {
                        if let Some(Value::Bool(true)) = cell {
                            bits[row / 64] |= 1 << (row % 64);
                        }
                    }
                    Buf::Bool(bits)
                } else if let Some((offsets, child)) = lists.then(|| Child::shape(&column, nested)).flatten() {
                    Buf::List(offsets, child)
                } else {
                    Buf::Any(
                        column
                            .iter()
                            .map(|c| c.as_ref().map_or(Variable::Null, |v| Variable::from(v.clone())))
                            .collect(),
                    )
                };
                (buf, valid)
            })
            .collect();
        Some(Built {
            rows: rows.len(),
            paths,
            bufs,
        })
    }

    fn plain(buf: &Buf) -> Option<zen_expression::lane::Values<'_>> {
        use zen_expression::lane::{Dictionary, Values};
        Some(match buf {
            Buf::List(offsets, child) => Values::List {
                offsets,
                child: match child {
                    Child::Text(o, d) => Dictionary::Text { offsets: o, data: d },
                    Child::Scaled(m, sc) => Dictionary::Scaled { mant: m, scale: sc },
                    Child::Bool(bits) => Dictionary::Bool { bits },
                    Child::Struct(_, _, fallback) => return Some(Values::Any(fallback)),
                },
            },
            other => other.values(),
        })
    }

    pub fn columns(&self) -> zen_expression::lane::Columns<'_> {
        use zen_expression::lane::{Column, Columns};
        let mut columns = Columns::new(self.rows);
        for (path, (buf, valid)) in self.paths.iter().zip(&self.bufs) {
            if let Some(values) = Self::plain(buf) {
                columns = columns.column(path, Column::with_validity(values, valid, 0));
            }
        }
        columns
    }

    pub fn normalized(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let cleaned: serde_json::Map<String, Value> = map
                    .into_iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, v)| (k, Self::normalized(v)))
                    .filter(|(_, v)| !matches!(v, Value::Object(m) if m.is_empty()))
                    .collect();
                Value::Object(cleaned)
            }
            other => other,
        }
    }
}
