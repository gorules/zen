use std::rc::Rc;
use zen_expression::lane::{Column, Dictionary, Kind, Output, Shape, Values};
use zen_expression::Variable;
use super::{Bits, Dict};

#[derive(Debug)]
pub(crate) enum Store {
    Scaled { mant: Vec<i64>, scale: Vec<u8> },
    Bool(Vec<u64>),
    Text { offsets: Vec<i32>, data: String },
    Coded { codes: Rc<[i32]>, dict: Rc<Dict> },
    Any(Vec<Variable>),
    List { offsets: Vec<i32>, child: Box<Store> },
}

impl Store {
    #[inline(always)]
    pub fn part(n: &rust_decimal::Decimal) -> Option<(i64, u8)> {
        let unpacked = n.unpack();
        let wide = u64::from(unpacked.mid) << 32 | u64::from(unpacked.lo);
        let scale = u8::try_from(unpacked.scale).ok()?;
        match (unpacked.hi, unpacked.negative) {
            (0, false) => Some((i64::try_from(wide).ok()?, scale)),
            (0, true) if wide != 0 => Some((0i64.checked_sub_unsigned(wide)?, scale)),
            _ => None,
        }
    }

    pub fn unpacked(values: &[rust_decimal::Decimal]) -> Option<Store> {
        let mut mant = Vec::with_capacity(values.len());
        let mut scale = Vec::with_capacity(values.len());
        for value in values {
            let (m, s) = Self::part(value)?;
            mant.push(m);
            scale.push(s);
        }
        Some(Store::Scaled { mant, scale })
    }
}

#[derive(Debug)]
pub(crate) struct Array {
    pub(super) values: Store,
    pub(super) valid: Option<Vec<u64>>,
    pub(super) rows: usize,
}

impl Array {
    fn any(out: &Output) -> Array {
        Array::from_values(
            (0..out.len())
                .map(|row| out.variable(row).and_then(Result::ok).unwrap_or(Variable::Null))
                .collect(),
        )
    }

    pub fn from_output(out: &mut Output) -> Array {
        let rows = out.len();
        let mut valid: Option<Vec<u64>> = None;
        if out.kind() != Kind::Dyn {
            for (row, value) in out.boxed_values() {
                match value {
                    Variable::Null => Bits::set(valid.get_or_insert_with(|| Bits::ones(rows)), *row as usize, false),
                    _ => return Self::any(out),
                }
            }
        }
        if out.failed().iter().any(|w| *w != 0) {
            let bits = valid.get_or_insert_with(|| Bits::ones(rows));
            bits.iter_mut().zip(out.failed()).for_each(|(v, failed)| *v &= !failed);
        }
        let scalar = matches!(out.shape(), Shape::Scalar);
        if let (true, Some((keys, offsets, data))) = (scalar, out.codes()) {
            let mut valid = valid.unwrap_or_else(|| Bits::ones(rows));
            keys.iter().enumerate().filter(|(_, code)| **code < 0).for_each(|(row, _)| Bits::set(&mut valid, row, false));
            let dict = Dict::Text {
                offsets: offsets.to_vec(),
                data: data.to_string(),
            };
            return Array::coded_with(keys.into(), Rc::new(dict), valid);
        }
        let values = match out.kind() {
            Kind::Num if scalar => {
                let (mant, scale) = out.take_numbers();
                Store::Scaled { mant, scale }
            }
            Kind::Bool if scalar => Store::Bool(out.take_bits()),
            Kind::Str if i32::try_from(out.data().len()).is_ok() && std::str::from_utf8(out.data()).is_ok() => {
                let (offsets, data) = out.take_text();
                Store::Text {
                    offsets: offsets.into_iter().map(|o| o as i32).collect(),
                    data: String::from_utf8(data).unwrap_or_default(),
                }
            }
            Kind::Dyn if scalar && out.boxed_values().is_empty() => {
                let failed = out.failed().to_vec();
                let mut values = out.take_values();
                values.resize(rows, Variable::Null);
                for (row, value) in values.iter_mut().enumerate() {
                    if Bits::get(&failed, row) {
                        *value = Variable::Null;
                    }
                }
                return Array::from_values(values);
            }
            _ => return Self::any(out),
        };
        Array { values, valid, rows }
    }

    pub fn parts(values: Store, valid: Option<Vec<u64>>, rows: usize) -> Array {
        Array { values, valid, rows }
    }

    pub fn from_values(values: Vec<Variable>) -> Array {
        Array {
            rows: values.len(),
            values: Store::Any(values),
            valid: None,
        }
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn store(&self) -> &Store {
        &self.values
    }

    pub fn valid(&self, row: usize) -> bool {
        self.valid.as_deref().is_none_or(|bits| Bits::get(bits, row))
    }

    pub fn validity_bits(&self) -> Option<&[u64]> {
        self.valid.as_deref()
    }

    pub fn column(&self) -> Column<'_> {
        let values = match &self.values {
            Store::Scaled { mant, scale } => Values::Scaled { mant, scale },
            Store::Bool(bits) => Values::Bool { bits, offset: 0 },
            Store::Text { offsets, data } => Values::Text { offsets, data },
            Store::Coded { codes, dict } => Values::Dict {
                keys: codes,
                values: dict.dictionary(),
            },
            Store::Any(values) => Values::Any(values),
            Store::List { offsets, child } => Values::List {
                offsets,
                child: match child.as_ref() {
                    Store::Text { offsets, data } => Dictionary::Text { offsets, data },
                    Store::Scaled { mant, scale } => Dictionary::Scaled { mant, scale },
                    Store::Bool(bits) => Dictionary::Bool { bits },
                    _ => Dictionary::Any(&[]),
                },
            },
        };
        match &self.valid {
            Some(bits) => Column::with_validity(values, bits, 0),
            None => Column::new(values),
        }
    }

    fn listed(offsets: &[i32], child: Dictionary, valid: Option<(&[u64], usize)>, positions: &[usize]) -> Option<Store> {
        let range = |p: usize| -> (usize, usize) {
            let live = valid.is_none_or(|(bits, at)| Bits::get(bits, at + p));
            match (live, offsets.get(p), offsets.get(p + 1)) {
                (true, Some(a), Some(b)) => (*a as usize, (*b as usize).max(*a as usize)),
                _ => (0, 0),
            }
        };
        let mut out = Vec::with_capacity(positions.len() + 1);
        out.push(0i32);
        let child = match child {
            Dictionary::Text { offsets: co, data } => {
                let (mut offsets, mut text) = (vec![0i32], String::new());
                for &p in positions {
                    let (a, b) = range(p);
                    for i in a..b {
                        text.push_str(data.get(*co.get(i)? as usize..*co.get(i + 1)? as usize)?);
                        offsets.push(i32::try_from(text.len()).ok()?);
                    }
                    out.push(i32::try_from(offsets.len() - 1).ok()?);
                }
                Store::Text { offsets, data: text }
            }
            Dictionary::Scaled { mant, scale } => {
                let (mut m, mut sc) = (Vec::new(), Vec::new());
                for &p in positions {
                    let (a, b) = range(p);
                    m.extend_from_slice(mant.get(a..b)?);
                    sc.extend_from_slice(scale.get(a..b)?);
                    out.push(i32::try_from(m.len()).ok()?);
                }
                Store::Scaled { mant: m, scale: sc }
            }
            Dictionary::Bool { bits } => {
                let mut flat: Vec<bool> = Vec::new();
                for &p in positions {
                    let (a, b) = range(p);
                    flat.extend((a..b).map(|i| Bits::get(bits, i)));
                    out.push(i32::try_from(flat.len()).ok()?);
                }
                Store::Bool(Bits::of(flat.len(), |i| flat[i]))
            }
            _ => return None,
        };
        Some(Store::List { offsets: out, child: Box::new(child) })
    }

    fn coded_gather(keys: &[i32], values: Dictionary, valid: Option<(&[u64], usize)>, positions: &[usize]) -> Option<Array> {
        let size = values.column().len();
        if size > positions.len().max(64) {
            return None;
        }
        let dict = match values {
            Dictionary::Text { offsets, data } => Dict::Text {
                offsets: offsets.get(..=size)?.to_vec(),
                data: data.to_string(),
            },
            Dictionary::Scaled { mant, scale } => Dict::Scaled {
                mant: mant.get(..size)?.to_vec(),
                scale: scale.get(..size)?.to_vec(),
            },
            Dictionary::Bool { bits } => Dict::Bool(bits.to_vec()),
            _ => return None,
        };
        let codes: Rc<[i32]> = positions
            .iter()
            .map(|&p| match valid.is_none_or(|(bits, at)| Bits::get(bits, at + p)) {
                true => keys.get(p).copied().unwrap_or(-1),
                false => -1,
            })
            .collect();
        Some(Array::coded(codes, Rc::new(dict)))
    }

    pub fn gather(column: &Column, positions: &[usize]) -> Array {
        let rows = positions.len();
        if let Values::Dict { keys, values } = column.values {
            if let Some(array) = Self::coded_gather(keys, values, column.validity, positions) {
                return array;
            }
        }
        if let Values::List { offsets, child } = column.values {
            if let Some(values) = Self::listed(offsets, child, column.validity, positions) {
                let valid = column
                    .validity
                    .map(|(bits, offset)| Bits::of(rows, |i| Bits::get(bits, offset + positions[i])));
                return Array { values, valid, rows };
            }
        }
        let valid = column
            .validity
            .filter(|(bits, offset)| !Bits::full(bits, *offset, positions.iter().max().map_or(0, |m| m + 1)))
            .map(|(bits, offset)| Bits::of(rows, |i| Bits::get(bits, offset + positions[i])));
        let values = match column.values {
            Values::Scaled { mant, scale } => Store::Scaled {
                mant: positions.iter().map(|&p| mant.get(p).copied().unwrap_or(0)).collect(),
                scale: positions.iter().map(|&p| scale.get(p).copied().unwrap_or(0)).collect(),
            },
            Values::I64(values) => Store::Scaled {
                mant: positions.iter().map(|&p| values.get(p).copied().unwrap_or(0)).collect(),
                scale: vec![0; rows],
            },
            Values::Dec(values) => {
                let mut mant = Vec::with_capacity(rows);
                let mut scale = Vec::with_capacity(rows);
                let parts = positions.iter().try_for_each(|&p| {
                    let (m, s) = match values.get(p) {
                        Some(d) => (i64::try_from(d.mantissa()).ok()?, u8::try_from(d.scale()).ok()?),
                        None => (0, 0),
                    };
                    mant.push(m);
                    scale.push(s);
                    Some(())
                });
                match parts {
                    Some(()) => Store::Scaled { mant, scale },
                    None => {
                        return Array {
                            values: Store::Any(positions.iter().map(|&p| column.variable(p)).collect()),
                            valid: None,
                            rows,
                        }
                    }
                }
            }
            Values::Bool { bits, offset } => Store::Bool(Bits::of(rows, |i| Bits::get(bits, offset + positions[i]))),
            Values::Text { .. } | Values::Utf8 { .. } => {
                let mut offsets = Vec::with_capacity(rows + 1);
                let mut data = String::new();
                offsets.push(0);
                for &p in positions {
                    match column.text(p) {
                        Some(text) => data.push_str(text),
                        None if !column.valid(p) => {}
                        None => {
                            return Array {
                                values: Store::Any(positions.iter().map(|&p| column.variable(p)).collect()),
                                valid: None,
                                rows,
                            }
                        }
                    }
                    offsets.push(data.len() as i32);
                }
                Store::Text { offsets, data }
            }
            _ => {
                return Array {
                    values: Store::Any(positions.iter().map(|&p| column.variable(p)).collect()),
                    valid: None,
                    rows,
                }
            }
        };
        Array { values, valid, rows }
    }

    pub fn coded(codes: Rc<[i32]>, dict: Rc<Dict>) -> Array {
        Array {
            rows: codes.len(),
            values: Store::Coded { codes, dict },
            valid: None,
        }
    }

    pub fn broadcast(&self, rows: usize) -> Option<Array> {
        let valid = self.valid.as_deref().is_none_or(|bits| Bits::get(bits, 0));
        let (dict, code) = match &self.values {
            Store::List { .. } => return Some(Self::gather(&self.column(), &vec![0; rows])),
            Store::Scaled { mant, scale } => (
                Rc::new(Dict::Scaled {
                    mant: vec![*mant.first()?],
                    scale: vec![*scale.first()?],
                }),
                0,
            ),
            Store::Bool(bits) => (Rc::new(Dict::Bool(vec![bits.first()? & 1])), 0),
            Store::Text { offsets, data } => {
                let text = data.get(*offsets.first()? as usize..*offsets.get(1)? as usize)?;
                (
                    Rc::new(Dict::Text {
                        offsets: vec![0, i32::try_from(text.len()).ok()?],
                        data: text.to_string(),
                    }),
                    0,
                )
            }
            Store::Coded { codes, dict } => (dict.clone(), *codes.first()?),
            Store::Any(values) => match values.first()? {
                Variable::Array(items)
                    if items
                        .borrow()
                        .iter()
                        .all(|item| matches!(item, Variable::Null | Variable::Bool(_) | Variable::Number(_) | Variable::String(_))) =>
                {
                    (Rc::new(Dict::Any(vec![values.first()?.clone()])), 0)
                }
                value @ (Variable::Array(_) | Variable::Object(_) | Variable::Dynamic(_)) => {
                    return Some(Array {
                        rows,
                        values: Store::Any((0..rows).map(|_| if valid { value.deep_clone() } else { Variable::Null }).collect()),
                        valid: None,
                    })
                }
                value => (Rc::new(Dict::Any(vec![value.clone()])), 0),
            },
        };
        let code = if valid { code } else { -1 };
        Some(Array {
            rows,
            values: Store::Coded {
                codes: vec![code; rows].into(),
                dict,
            },
            valid: None,
        })
    }

    pub fn coded_with(codes: Rc<[i32]>, dict: Rc<Dict>, valid: Vec<u64>) -> Array {
        Array {
            rows: codes.len(),
            values: Store::Coded { codes, dict },
            valid: Some(valid),
        }
    }

    pub fn expand(&self, ids: &[u32]) -> Array {
        let code = |id: u32| match self.valid(id as usize) {
            true => id as i32,
            false => -1,
        };
        let shared = |value: &Variable| match value {
            Variable::Array(items) => items
                .borrow()
                .iter()
                .all(|item| matches!(item, Variable::Null | Variable::Bool(_) | Variable::Number(_) | Variable::String(_))),
            Variable::Object(_) | Variable::Dynamic(_) => false,
            _ => true,
        };
        let dict = match &self.values {
            Store::Scaled { mant, scale } => Dict::Scaled {
                mant: mant.clone(),
                scale: scale.clone(),
            },
            Store::Text { offsets, data } => Dict::Text {
                offsets: offsets.clone(),
                data: data.clone(),
            },
            Store::Coded { codes, dict } => {
                let codes = ids
                    .iter()
                    .map(|&id| match self.valid(id as usize) {
                        true => codes.get(id as usize).copied().unwrap_or(-1),
                        false => -1,
                    })
                    .collect();
                return Array::coded(codes, dict.clone());
            }
            Store::Any(values) if values.iter().all(shared) => Dict::Any(values.clone()),
            Store::Any(values) => {
                let values = ids
                    .iter()
                    .map(|&id| match (self.valid(id as usize), values.get(id as usize)) {
                        (true, Some(value)) => value.deep_clone(),
                        _ => Variable::Null,
                    })
                    .collect();
                return Array::from_values(values);
            }
            Store::Bool(_) | Store::List { .. } => {
                let positions: Vec<usize> = ids.iter().map(|&id| id as usize).collect();
                return self.pick(&positions);
            }
        };
        Array::coded(ids.iter().map(|&id| code(id)).collect(), Rc::new(dict))
    }

    pub fn pick(&self, positions: &[usize]) -> Array {
        match &self.values {
            Store::Coded { codes, dict } => Array {
                values: Store::Coded {
                    codes: positions.iter().map(|&p| codes[p]).collect(),
                    dict: dict.clone(),
                },
                valid: self.valid.as_deref().map(|bits| Bits::gather(bits, positions)),
                rows: positions.len(),
            },
            Store::Any(values) => Array {
                values: Store::Any(positions.iter().map(|&p| values[p].clone()).collect()),
                valid: self.valid.as_deref().map(|bits| Bits::gather(bits, positions)),
                rows: positions.len(),
            },
            _ => Self::gather(&self.column(), positions),
        }
    }
}
