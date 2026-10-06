use std::cell::OnceCell;
use std::rc::Rc;
use zen_expression::lane::{Column, Dictionary, Kind, Output, Shape, Values};
use zen_expression::Variable;

#[derive(Debug)]
pub(crate) enum Dict {
    Text { offsets: Vec<i32>, data: String },
    Scaled { mant: Vec<i64>, scale: Vec<u8> },
    Bool(Vec<u64>),
    Any(Vec<Variable>),
}

impl Dict {
    fn len(&self) -> Option<usize> {
        match self {
            Dict::Text { offsets, .. } => Some(offsets.len().saturating_sub(1)),
            Dict::Scaled { mant, .. } => Some(mant.len()),
            Dict::Any(values) => Some(values.len()),
            Dict::Bool(_) => None,
        }
    }

    pub fn concat(dicts: &[&Rc<Dict>]) -> Option<(Rc<Dict>, Vec<i32>)> {
        if let [single] = dicts {
            return Some(((*single).clone(), vec![0]));
        }
        let mut bases = Vec::with_capacity(dicts.len());
        let mut total = 0usize;
        for dict in dicts {
            bases.push(i32::try_from(total).ok()?);
            total += dict.len()?;
        }
        i32::try_from(total).ok()?;
        let merged = match dicts.first().map(|d| &***d)? {
            Dict::Text { .. } => {
                let mut offsets = vec![0i32];
                let mut data = String::new();
                for dict in dicts {
                    let Dict::Text { offsets: o, data: d } = &***dict else {
                        return None;
                    };
                    let base = i32::try_from(data.len()).ok()?;
                    offsets.extend(o.iter().skip(1).map(|x| x + base));
                    data.push_str(d);
                }
                i32::try_from(data.len()).ok()?;
                Dict::Text { offsets, data }
            }
            Dict::Scaled { .. } => {
                let (mut mant, mut scale) = (Vec::with_capacity(total), Vec::with_capacity(total));
                for dict in dicts {
                    let Dict::Scaled { mant: m, scale: s } = &***dict else {
                        return None;
                    };
                    mant.extend_from_slice(m);
                    scale.extend_from_slice(s);
                }
                Dict::Scaled { mant, scale }
            }
            Dict::Any(_) => {
                let mut values = Vec::with_capacity(total);
                for dict in dicts {
                    let Dict::Any(v) = &***dict else {
                        return None;
                    };
                    values.extend(v.iter().cloned());
                }
                Dict::Any(values)
            }
            Dict::Bool(_) => return None,
        };
        Some((Rc::new(merged), bases))
    }

    fn dictionary(&self) -> Dictionary<'_> {
        match self {
            Dict::Text { offsets, data } => Dictionary::Text { offsets, data },
            Dict::Scaled { mant, scale } => Dictionary::Scaled { mant, scale },
            Dict::Bool(bits) => Dictionary::Bool { bits },
            Dict::Any(values) => Dictionary::Any(values),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Store {
    Scaled { mant: Vec<i64>, scale: Vec<u8> },
    Bool(Vec<u64>),
    Text { offsets: Vec<i32>, data: String },
    Coded { codes: Rc<[i32]>, dict: Rc<Dict> },
    Any(Vec<Variable>),
    List { offsets: Vec<i32>, child: Box<Store> },
}

#[derive(Debug)]
pub(crate) struct Array {
    values: Store,
    valid: Option<Vec<u64>>,
    rows: usize,
}

pub(crate) struct Bits;

impl Bits {
    #[inline]
    pub fn get(bits: &[u64], row: usize) -> bool {
        bits.get(row >> 6).is_some_and(|w| w >> (row & 63) & 1 == 1)
    }

    #[inline]
    pub fn set(bits: &mut [u64], row: usize, on: bool) {
        if let Some(word) = bits.get_mut(row >> 6) {
            *word = (*word & !(1 << (row & 63))) | (u64::from(on) << (row & 63));
        }
    }

    pub fn ones(rows: usize) -> Vec<u64> {
        let mut bits = vec![u64::MAX; rows.div_ceil(64)];
        Self::trim(&mut bits, rows);
        bits
    }

    pub fn trim(bits: &mut [u64], rows: usize) {
        if let (Some(last), tail @ 1..) = (bits.last_mut(), rows & 63) {
            *last &= (1u64 << tail) - 1;
        }
    }

    pub fn of(rows: usize, mut f: impl FnMut(usize) -> bool) -> Vec<u64> {
        let mut bits = vec![0u64; rows.div_ceil(64)];
        for (w, word) in bits.iter_mut().enumerate() {
            let base = w << 6;
            let mut value = 0u64;
            for bit in 0..(rows - base).min(64) {
                value |= u64::from(f(base + bit)) << bit;
            }
            *word = value;
        }
        bits
    }

    pub fn window(bits: &[u64], offset: usize, rows: usize) -> Vec<u64> {
        match offset {
            0 if bits.len() >= rows.div_ceil(64) => {
                let mut out = bits[..rows.div_ceil(64)].to_vec();
                Self::trim(&mut out, rows);
                out
            }
            _ => (0..rows.div_ceil(64))
                .map(|w| Column::word(bits, offset + (w << 6), (rows - (w << 6)).min(64)))
                .collect(),
        }
    }

    pub fn gather(bits: &[u64], positions: &[usize]) -> Vec<u64> {
        Self::of(positions.len(), |i| Self::get(bits, positions[i]))
    }

    pub fn fill(bits: &mut [u64], start: usize, len: usize) {
        let end = start + len;
        let mut at = start;
        while at < end {
            let (word, bit) = (at >> 6, at & 63);
            let take = (64 - bit).min(end - at);
            let mask = match take {
                64 => u64::MAX,
                t => ((1u64 << t) - 1) << bit,
            };
            if let Some(w) = bits.get_mut(word) {
                *w |= mask;
            }
            at += take;
        }
    }

    pub fn splice(bits: &mut [u64], start: usize, source: &[u64], len: usize) {
        let mut at = 0;
        while at < len {
            let take = (len - at).min(64);
            let word = Column::word(source, at, take);
            let (dst, bit) = ((start + at) >> 6, (start + at) & 63);
            if let Some(w) = bits.get_mut(dst) {
                *w |= word << bit;
            }
            if bit > 0 {
                if let Some(w) = bits.get_mut(dst + 1) {
                    *w |= word >> (64 - bit);
                }
            }
            at += take;
        }
    }

    pub fn none(bits: &[u64]) -> bool {
        bits.iter().all(|w| *w == 0)
    }
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
                let parts = positions
                    .iter()
                    .map(|&p| match values.get(p) {
                        Some(d) => Some((i64::try_from(d.mantissa()).ok()?, u8::try_from(d.scale()).ok()?)),
                        None => Some((0, 0)),
                    })
                    .collect::<Option<Vec<(i64, u8)>>>();
                match parts {
                    Some(parts) => {
                        let (mant, scale) = parts.into_iter().unzip();
                        Store::Scaled { mant, scale }
                    }
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

#[derive(Debug)]
pub(crate) struct Picked<'a> {
    source: Leaf<'a>,
    positions: Rc<[usize]>,
    dense: OnceCell<Leaf<'a>>,
}

#[derive(Debug)]
pub(crate) struct Stitched<'a> {
    sources: Vec<Leaf<'a>>,
    located: Rc<[(usize, usize)]>,
    dense: OnceCell<Leaf<'a>>,
}

pub(crate) type Passed<'l, 'a> = (&'l Rc<Input<'a>>, Option<&'l Rc<[usize]>>);

#[derive(Debug)]
pub(crate) struct Input<'a> {
    pub column: Column<'a>,
    pub rows: usize,
    valued: OnceCell<Vec<u64>>,
}

#[derive(Debug)]
pub(crate) struct Masked<'a> {
    inner: Leaf<'a>,
    bits: Rc<[u64]>,
}

#[derive(Debug)]
pub(crate) struct Scattered<'a> {
    inner: Leaf<'a>,
    positions: Rc<[usize]>,
    rows: usize,
    dense: OnceCell<Leaf<'a>>,
}

#[derive(Clone, Debug)]
pub(crate) enum Leaf<'a> {
    Scattered(Rc<Scattered<'a>>),
    Masked(Rc<Masked<'a>>),
    Input(Rc<Input<'a>>),
    Any(Rc<[Variable]>),
    Typed(Rc<Array>),
    Picked(Rc<Picked<'a>>),
    Stitched(Rc<Stitched<'a>>),
}

impl<'a> Leaf<'a> {
    pub fn typed(array: Array) -> Leaf<'a> {
        Leaf::Typed(Rc::new(array))
    }

    pub fn masked(inner: Leaf<'a>, mask: &Rc<[u64]>) -> Leaf<'a> {
        let rows = inner.len();
        let trimmed = mask.last().is_none_or(|w| rows.is_multiple_of(64) || w >> (rows % 64) == 0);
        if inner.all_valid() && trimmed && mask.len() == rows.div_ceil(64) {
            return Leaf::Masked(Rc::new(Masked { inner, bits: mask.clone() }));
        }
        let mut bits = inner.validity(rows);
        bits.iter_mut().zip(mask.iter()).for_each(|(b, m)| *b &= m);
        bits.iter_mut().skip(mask.len()).for_each(|b| *b = 0);
        Leaf::Masked(Rc::new(Masked { inner, bits: bits.into() }))
    }

    fn all_valid(&self) -> bool {
        match self {
            Leaf::Input(input) => input.column.validity.is_none(),
            Leaf::Typed(typed) => typed.valid.is_none(),
            Leaf::Any(_) => true,
            _ => false,
        }
    }

    pub fn scattered(inner: Leaf<'a>, positions: Rc<[usize]>, rows: usize) -> Leaf<'a> {
        Leaf::Scattered(Rc::new(Scattered {
            inner,
            positions,
            rows,
            dense: OnceCell::new(),
        }))
    }

    pub fn coded_dict(&self) -> Option<&Rc<Dict>> {
        match self {
            Leaf::Typed(typed) => match &typed.values {
                Store::Coded { dict, .. } => Some(dict),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn scatter_parts(&self) -> Option<(&Leaf<'a>, &Rc<[usize]>)> {
        match self {
            Leaf::Scattered(scattered) if scattered.dense.get().is_none() => Some((&scattered.inner, &scattered.positions)),
            _ => None,
        }
    }

    pub fn same(&self, other: &Leaf<'a>) -> bool {
        match (self, other) {
            (Leaf::Scattered(a), Leaf::Scattered(b)) => Rc::ptr_eq(a, b),
            (Leaf::Masked(a), Leaf::Masked(b)) => Rc::ptr_eq(a, b),
            (Leaf::Input(a), Leaf::Input(b)) => Rc::ptr_eq(a, b),
            (Leaf::Any(a), Leaf::Any(b)) => Rc::ptr_eq(a, b),
            (Leaf::Typed(a), Leaf::Typed(b)) => Rc::ptr_eq(a, b),
            (Leaf::Picked(a), Leaf::Picked(b)) => Rc::ptr_eq(a, b),
            (Leaf::Stitched(a), Leaf::Stitched(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    fn spread(scattered: &Scattered<'a>) -> Leaf<'a> {
        let (rows, positions) = (scattered.rows, &scattered.positions);
        if let Leaf::Typed(typed) = &scattered.inner {
            if let Store::Coded { codes, dict } = &typed.values {
                let mut full = vec![-1i32; rows];
                for (i, &row) in positions.iter().enumerate() {
                    if let (Some(slot), Some(code)) = (full.get_mut(row), codes.get(i)) {
                        *slot = match typed.valid(i) {
                            true => *code,
                            false => -1,
                        };
                    }
                }
                return Leaf::typed(Array::coded(full.into(), dict.clone()));
            }
        }
        let column = scattered.inner.column();
        let mut valid = vec![0u64; rows.div_ceil(64)];
        for (i, &row) in positions.iter().enumerate() {
            if column.valid(i) {
                Bits::set(&mut valid, row, true);
            }
        }
        let values = match column.values {
            Values::Scaled { mant, scale } => {
                let (mut m, mut s) = (vec![0i64; rows], vec![0u8; rows]);
                for (i, &row) in positions.iter().enumerate() {
                    m[row] = mant.get(i).copied().unwrap_or(0);
                    s[row] = scale.get(i).copied().unwrap_or(0);
                }
                Store::Scaled { mant: m, scale: s }
            }
            Values::Bool { bits, offset } => {
                let mut out = vec![0u64; rows.div_ceil(64)];
                for (i, &row) in positions.iter().enumerate() {
                    Bits::set(&mut out, row, Bits::get(bits, offset + i));
                }
                Store::Bool(out)
            }
            Values::Any(values) => {
                let mut out = vec![Variable::Null; rows];
                for (i, &row) in positions.iter().enumerate() {
                    out[row] = values.get(i).cloned().unwrap_or(Variable::Null);
                }
                Store::Any(out)
            }
            _ => {
                let mut builder = ColumnBuilder::with_capacity(rows);
                let mut next = positions.iter().enumerate().peekable();
                for row in 0..rows {
                    match next.peek() {
                        Some(&(i, &at)) if at == row => {
                            builder.push_cell(&column, i);
                            next.next();
                        }
                        _ => builder.push_null(),
                    }
                }
                return Leaf::typed(builder.finish());
            }
        };
        Leaf::typed(Array {
            values,
            valid: Some(valid),
            rows,
        })
    }

    pub fn nulls(rows: usize) -> Leaf<'a> {
        Leaf::Any((0..rows).map(|_| Variable::Null).collect())
    }

    pub fn input(column: Column<'a>, rows: usize) -> Leaf<'a> {
        Leaf::Input(Rc::new(Input {
            column,
            rows,
            valued: OnceCell::new(),
        }))
    }

    pub fn passed(&self) -> Option<Passed<'_, 'a>> {
        match self {
            Leaf::Input(input) => Some((input, None)),
            Leaf::Picked(picked) => match &picked.source {
                Leaf::Input(input) => Some((input, Some(&picked.positions))),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn stitched(sources: Vec<Leaf<'a>>, located: Rc<[(usize, usize)]>) -> Leaf<'a> {
        Leaf::Stitched(Rc::new(Stitched {
            sources,
            located,
            dense: OnceCell::new(),
        }))
    }

    fn source(&self, row: usize) -> Option<(&Leaf<'a>, usize)> {
        match self {
            Leaf::Stitched(stitched) => {
                let &(piece, at) = stitched.located.get(row)?;
                Some((stitched.sources.get(piece)?, at))
            }
            Leaf::Picked(picked) => Some((&picked.source, *picked.positions.get(row)?)),
            _ => Some((self, row)),
        }
    }

    pub fn get(&self, row: usize) -> Variable {
        match self {
            Leaf::Any(values) => values.get(row).cloned().unwrap_or(Variable::Null),
            Leaf::Picked(_) | Leaf::Stitched(_) => self.source(row).map_or(Variable::Null, |(leaf, at)| leaf.get(at)),
            _ => self.column().variable(row),
        }
    }

    pub fn objects(&self) -> bool {
        match self {
            Leaf::Any(values) => values.iter().any(|v| matches!(v, Variable::Object(_))),
            Leaf::Picked(picked) => picked.source.objects(),
            Leaf::Stitched(stitched) => stitched.sources.iter().any(Leaf::objects),
            _ => match self.column().values {
                Values::Any(values) => values.iter().any(|v| matches!(v, Variable::Object(_))),
                Values::Dict {
                    values: Dictionary::Any(values),
                    ..
                } => values.iter().any(|v| matches!(v, Variable::Object(_))),
                _ => false,
            },
        }
    }

    fn dense(&self, positions: &[usize]) -> Leaf<'a> {
        match self {
            Leaf::Any(values) => Leaf::Any(positions.iter().map(|&i| values[i].clone()).collect()),
            Leaf::Typed(typed) => Leaf::typed(typed.pick(positions)),
            Leaf::Input(input) => Leaf::typed(Array::gather(&input.column, positions)),
            Leaf::Masked(_) | Leaf::Scattered(_) => Leaf::typed(Array::gather(&self.column(), positions)),
            Leaf::Picked(picked) => {
                let composed: Vec<usize> = positions.iter().map(|&i| picked.positions[i]).collect();
                picked.source.dense(&composed)
            }
            Leaf::Stitched(stitched) => {
                let views: Vec<Column> = stitched.sources.iter().map(Leaf::column).collect();
                let mut builder = ColumnBuilder::with_capacity(positions.len());
                for &row in positions {
                    match stitched.located.get(row).and_then(|&(piece, at)| Some((views.get(piece)?, at))) {
                        Some((view, at)) => builder.push_cell(view, at),
                        None => builder.push_null(),
                    }
                }
                Leaf::typed(builder.finish())
            }
        }
    }

    pub fn expand(&self, ids: &[u32]) -> Leaf<'a> {
        match self {
            Leaf::Typed(typed) => Leaf::typed(typed.expand(ids)),
            Leaf::Any(values) => Leaf::typed(Array::from_values(values.to_vec()).expand(ids)),
            other => {
                let column = other.column();
                Leaf::typed(Array::from_values((0..other.len()).map(|row| column.variable(row)).collect()).expand(ids))
            }
        }
    }

    pub fn pick(&self, positions: &Rc<[usize]>) -> Leaf<'a> {
        match self {
            Leaf::Picked(picked) => Leaf::Picked(Rc::new(Picked {
                source: picked.source.clone(),
                positions: positions.iter().map(|&i| picked.positions[i]).collect(),
                dense: OnceCell::new(),
            })),
            other => Leaf::Picked(Rc::new(Picked {
                source: other.clone(),
                positions: positions.clone(),
                dense: OnceCell::new(),
            })),
        }
    }

    pub fn column(&self) -> Column<'_> {
        match self {
            Leaf::Input(input) => input.column,
            Leaf::Scattered(scattered) => scattered.dense.get_or_init(|| Self::spread(scattered)).column(),
            Leaf::Masked(masked) => Column {
                values: masked.inner.column().values,
                validity: Some((&masked.bits, 0)),
            },
            Leaf::Any(values) => Column::new(Values::Any(values)),
            Leaf::Typed(typed) => typed.column(),
            Leaf::Picked(picked) => picked
                .dense
                .get_or_init(|| picked.source.dense(&picked.positions))
                .column(),
            Leaf::Stitched(stitched) => stitched
                .dense
                .get_or_init(|| self.dense(&(0..stitched.located.len()).collect::<Vec<_>>()))
                .column(),
        }
    }

    pub fn null(&self, row: usize) -> bool {
        match self {
            Leaf::Any(values) => values.get(row).is_none_or(|v| matches!(v, Variable::Null)),
            Leaf::Picked(_) | Leaf::Stitched(_) => self.source(row).is_none_or(|(leaf, at)| leaf.null(at)),
            _ => ColumnBuilder::null_at(&self.column(), row),
        }
    }

    pub fn valid_at(&self, row: usize) -> bool {
        match self {
            Leaf::Input(input) => input.column.valid(row),
            Leaf::Masked(masked) => Bits::get(&masked.bits, row),
            Leaf::Scattered(_) => self.column().valid(row),
            Leaf::Typed(typed) => typed.valid(row),
            Leaf::Any(_) => true,
            Leaf::Picked(_) | Leaf::Stitched(_) => self.source(row).is_some_and(|(leaf, at)| leaf.valid_at(at)),
        }
    }

    pub fn validity(&self, rows: usize) -> Vec<u64> {
        match self {
            Leaf::Input(input) => match input.column.validity {
                Some((bits, offset)) => Bits::window(bits, offset, rows),
                None => Bits::ones(rows),
            },
            Leaf::Typed(typed) => match &typed.valid {
                Some(bits) => Bits::window(bits, 0, rows),
                None => Bits::ones(rows),
            },
            Leaf::Any(_) => Bits::ones(rows),
            Leaf::Masked(masked) => Bits::window(&masked.bits, 0, rows),
            Leaf::Scattered(_) => match self.column().validity {
                Some((bits, offset)) => Bits::window(bits, offset, rows),
                None => Bits::ones(rows),
            },
            Leaf::Picked(picked) => match picked.dense.get() {
                Some(dense) => dense.validity(rows),
                None => Bits::gather(&picked.source.validity(picked.source.len()), &picked.positions),
            },
            Leaf::Stitched(_) => Bits::of(rows, |row| self.valid_at(row)),
        }
    }

    pub fn valued(&self, rows: usize) -> Vec<u64> {
        match self {
            Leaf::Input(input) if input.rows == rows => input
                .valued
                .get_or_init(|| Self::column_valued(input.column, rows))
                .clone(),
            Leaf::Any(values) => Bits::of(rows, |row| values.get(row).is_some_and(|v| !matches!(v, Variable::Null))),
            Leaf::Picked(picked) => match picked.dense.get() {
                Some(dense) => dense.valued(rows),
                None => Bits::gather(&picked.source.valued(picked.source.len()), &picked.positions),
            },
            Leaf::Stitched(_) => Bits::of(rows, |row| !self.null(row)),
            Leaf::Typed(typed) if matches!(typed.values, Store::Coded { ref dict, .. } if !matches!(**dict, Dict::Any(_))) => {
                let Store::Coded { codes, .. } = &typed.values else {
                    return Bits::of(rows, |row| !self.null(row));
                };
                let mut bits: Vec<u64> = codes
                    .get(..rows)
                    .unwrap_or(codes)
                    .chunks(64)
                    .map(|chunk| chunk.iter().enumerate().fold(0u64, |word, (at, c)| word | (u64::from(*c >= 0) << at)))
                    .collect();
                bits.resize(rows.div_ceil(64), 0);
                if let Some(valid) = &typed.valid {
                    bits.iter_mut().zip(valid).for_each(|(b, v)| *b &= v);
                }
                bits
            }
            _ => Self::column_valued(self.column(), rows),
        }
    }

    fn column_valued(column: Column, rows: usize) -> Vec<u64> {
        match column.values {
            Values::Dict { keys, values } => {
                let size = values.column().len();
                if size > rows {
                    return Bits::of(rows, |row| !ColumnBuilder::null_at(&column, row));
                }
                let present: Vec<bool> = (0..size)
                    .map(|code| {
                        values.valid(code)
                            && !matches!(values, Dictionary::Any(v) if v.get(code).is_none_or(|v| matches!(v, Variable::Null)))
                    })
                    .collect();
                let mut bits = Bits::of(rows, |row| {
                    keys.get(row)
                        .and_then(|k| usize::try_from(*k).ok())
                        .and_then(|code| present.get(code))
                        .is_some_and(|p| *p)
                });
                if let Some((valid, offset)) = column.validity {
                    bits.iter_mut().zip(Bits::window(valid, offset, rows)).for_each(|(b, v)| *b &= v);
                }
                bits
            }
            Values::Text { offsets, data } => {
                let bytes = data.as_bytes();
                let boundary = |o: &i32| usize::try_from(*o).is_ok_and(|o| o == bytes.len() || bytes.get(o).is_some_and(|b| (*b as i8) >= -0x40));
                let formed = offsets
                    .get(..=rows)
                    .is_some_and(|window| window.windows(2).all(|w| w[0] <= w[1]) && window.iter().all(boundary));
                if formed {
                    return match column.validity {
                        None => Bits::ones(rows),
                        Some((bits, offset)) => Bits::window(bits, offset, rows),
                    };
                }
                let mut bits = Bits::of(rows, |row| {
                    offsets
                        .get(row)
                        .zip(offsets.get(row + 1))
                        .is_some_and(|(a, b)| data.get(*a as usize..*b as usize).is_some())
                });
                if let Some((valid, offset)) = column.validity {
                    bits.iter_mut().zip(Bits::window(valid, offset, rows)).for_each(|(b, v)| *b &= v);
                }
                bits
            }
            Values::Any(_) | Values::List { .. } | Values::Strs(_) | Values::Utf8 { .. } | Values::LargeUtf8 { .. } => {
                Bits::of(rows, |row| !ColumnBuilder::null_at(&column, row))
            }
            _ => match column.validity {
                None => Bits::ones(rows),
                Some((bits, offset)) => Bits::window(bits, offset, rows),
            },
        }
    }

    pub fn truthy(&self, row: usize) -> bool {
        match self {
            Leaf::Any(values) => matches!(values.get(row), Some(Variable::Bool(true))),
            Leaf::Picked(_) | Leaf::Stitched(_) => self.source(row).is_some_and(|(leaf, at)| leaf.truthy(at)),
            _ => {
                let column = self.column();
                match column.values {
                    Values::Any(values) => column.valid(row) && matches!(values.get(row), Some(Variable::Bool(true))),
                    _ => column.valid(row) && column.boolean(row) == Some(true),
                }
            }
        }
    }

    pub fn truths(&self, rows: usize) -> Vec<u64> {
        if let Leaf::Scattered(scattered) = self {
            if scattered.dense.get().is_none() {
                let inner = scattered.inner.truths(scattered.positions.len());
                let mut truths = vec![0u64; rows.div_ceil(64)];
                scattered
                    .positions
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| Bits::get(&inner, *i))
                    .for_each(|(_, &row)| Bits::set(&mut truths, row, true));
                return truths;
            }
        }
        if let Leaf::Typed(_) | Leaf::Input(..) | Leaf::Masked(_) | Leaf::Scattered(_) = self {
            let column = self.column();
            if let Values::Bool { bits, offset } = column.values {
                let mut truths = Bits::window(bits, offset, rows);
                if let Some((valid, at)) = column.validity {
                    truths.iter_mut().zip(Bits::window(valid, at, rows)).for_each(|(t, v)| *t &= v);
                }
                return truths;
            }
        }
        Bits::of(rows, |row| self.truthy(row))
    }

    pub fn len(&self) -> usize {
        match self {
            Leaf::Input(input) => input.rows,
            Leaf::Masked(masked) => masked.inner.len(),
            Leaf::Scattered(scattered) => scattered.rows,
            Leaf::Any(values) => values.len(),
            Leaf::Typed(typed) => typed.len(),
            Leaf::Picked(picked) => picked.positions.len(),
            Leaf::Stitched(stitched) => stitched.located.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Literal {
    Null,
    Bool(bool),
    Num(rust_decimal::Decimal),
    Str(std::sync::Arc<str>),
    Array(Vec<Literal>),
    Object(Vec<(std::sync::Arc<str>, Literal)>),
}

impl Literal {
    pub fn variable(&self) -> Variable {
        match self {
            Literal::Null => Variable::Null,
            Literal::Bool(b) => Variable::Bool(*b),
            Literal::Num(n) => Variable::Number(*n),
            Literal::Str(s) => Variable::String(s.as_ref().into()),
            Literal::Array(items) => Variable::from_array(items.iter().map(Literal::variable).collect()),
            Literal::Object(fields) => {
                let mut map = zen_types::variable::VariableMap::with_capacity(fields.len());
                for (key, value) in fields {
                    map.insert(key.as_ref().into(), value.variable());
                }
                Variable::from_object(map)
            }
        }
    }

    pub fn deep(&self) -> bool {
        match self {
            Literal::Object(_) => true,
            Literal::Array(items) => items.iter().any(Literal::deep),
            _ => false,
        }
    }

    pub fn scaled(&self) -> Option<(i64, u8)> {
        match self {
            Literal::Num(n) => Some((i64::try_from(n.mantissa()).ok()?, u8::try_from(n.scale()).ok()?)),
            _ => None,
        }
    }

    pub fn of(value: &Variable) -> Option<Literal> {
        match value {
            Variable::Null => Some(Literal::Null),
            Variable::Bool(b) => Some(Literal::Bool(*b)),
            Variable::Number(n) => Some(Literal::Num(*n)),
            Variable::String(s) => Some(Literal::Str(std::sync::Arc::from(s.as_ref() as &str))),
            Variable::Array(items) => items.borrow().iter().map(Literal::of).collect::<Option<Vec<_>>>().map(Literal::Array),
            Variable::Object(fields) => fields
                .borrow()
                .iter()
                .map(|(key, value)| Some((std::sync::Arc::from(key.as_ref() as &str), Literal::of(value)?)))
                .collect::<Option<Vec<_>>>()
                .map(Literal::Object),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(crate) struct ColumnBuilder {
    kind: Option<u8>,
    mant: Vec<i64>,
    scale: Vec<u8>,
    bits: Vec<u64>,
    offsets: Vec<i32>,
    data: String,
    any: Vec<Variable>,
    valid: Vec<u64>,
    nulls: bool,
    rows: usize,
    capacity: usize,
}

impl ColumnBuilder {
    const NUM: u8 = 0;
    const BOOL: u8 = 1;
    const TEXT: u8 = 2;
    const ANY: u8 = 3;

    pub fn with_capacity(rows: usize) -> Self {
        Self {
            valid: Vec::with_capacity(rows.div_ceil(64)),
            capacity: rows,
            ..Default::default()
        }
    }

    pub fn null_at(column: &Column, row: usize) -> bool {
        if !column.valid(row) {
            return true;
        }
        match column.values {
            Values::Any(values) => values.get(row).is_none_or(|v| matches!(v, Variable::Null)),
            Values::Dict { values, .. } => column.code(row).is_none_or(|code| {
                !values.valid(code)
                    || matches!(values, Dictionary::Any(v) if v.get(code).is_none_or(|v| matches!(v, Variable::Null)))
            }),
            Values::Dec(_) | Values::Scaled { .. } | Values::I64(_) | Values::F64(_) => column.number(row).is_none(),
            Values::Bool { .. } | Values::List { .. } => false,
            _ => column.text(row).is_none(),
        }
    }

    fn open(&mut self) -> usize {
        let row = self.rows;
        self.rows += 1;
        if row.is_multiple_of(64) {
            self.valid.push(0);
            if self.kind == Some(Self::BOOL) {
                self.bits.push(0);
            }
        }
        row
    }

    fn fill(&mut self) {
        match self.kind {
            Some(Self::NUM) => {
                self.mant.push(0);
                self.scale.push(0);
            }
            Some(Self::TEXT) => self.offsets.push(self.data.len() as i32),
            Some(Self::ANY) => self.any.push(Variable::Null),
            _ => {}
        }
    }

    fn start(&mut self, kind: u8) {
        let rows = self.rows;
        let capacity = self.capacity.max(rows);
        self.kind = Some(kind);
        match kind {
            Self::NUM => {
                self.mant = Vec::with_capacity(capacity);
                self.mant.resize(rows, 0);
                self.scale = Vec::with_capacity(capacity);
                self.scale.resize(rows, 0);
            }
            Self::BOOL => self.bits = vec![0; rows.div_ceil(64)],
            Self::TEXT => self.offsets = vec![0; rows + 1],
            _ => self.any = vec![Variable::Null; rows],
        }
    }

    fn degrade(&mut self) {
        let typed = Array {
            values: match self.kind {
                Some(Self::NUM) => Store::Scaled {
                    mant: std::mem::take(&mut self.mant),
                    scale: std::mem::take(&mut self.scale),
                },
                Some(Self::BOOL) => Store::Bool(std::mem::take(&mut self.bits)),
                Some(Self::TEXT) => Store::Text {
                    offsets: std::mem::take(&mut self.offsets),
                    data: std::mem::take(&mut self.data),
                },
                _ => Store::Any(Vec::new()),
            },
            valid: Some(self.valid.clone()),
            rows: self.rows,
        };
        let column = typed.column();
        self.any = (0..self.rows).map(|row| column.variable(row)).collect();
        self.kind = Some(Self::ANY);
    }

    pub fn push_cell(&mut self, column: &Column, row: usize) {
        if !column.valid(row) {
            return self.push_null();
        }
        match column.values {
            Values::Scaled { mant, scale } => match (mant.get(row), scale.get(row)) {
                (Some(m), Some(s)) => self.push_scaled(*m, *s),
                _ => self.push_null(),
            },
            Values::I64(values) => match values.get(row) {
                Some(v) => self.push_scaled(*v, 0),
                None => self.push_null(),
            },
            Values::Dec(values) => match values.get(row) {
                Some(d) => match (i64::try_from(d.mantissa()), u8::try_from(d.scale())) {
                    (Ok(m), Ok(s)) => self.push_scaled(m, s),
                    _ => self.push_variable(Variable::Number(*d)),
                },
                None => self.push_null(),
            },
            Values::Bool { .. } => match column.boolean(row) {
                Some(b) => self.push_bool(b),
                None => self.push_null(),
            },
            Values::Text { .. } | Values::Utf8 { .. } => match column.text(row) {
                Some(text) => self.push_text(text),
                None => self.push_null(),
            },
            Values::Dict { keys, values } => match keys.get(row).and_then(|k| usize::try_from(*k).ok()) {
                None => self.push_null(),
                Some(code) => match values {
                    Dictionary::Scaled { mant, scale } => match (mant.get(code), scale.get(code)) {
                        (Some(m), Some(s)) => self.push_scaled(*m, *s),
                        _ => self.push_null(),
                    },
                    Dictionary::Text { offsets, data } => {
                        let text = offsets
                            .get(code)
                            .zip(offsets.get(code + 1))
                            .and_then(|(a, b)| data.get(*a as usize..*b as usize));
                        match text {
                            Some(text) => self.push_text(text),
                            None => self.push_null(),
                        }
                    }
                    Dictionary::Bool { bits } => self.push_bool(Bits::get(bits, code)),
                    Dictionary::Any(values) => self.push_variable(values.get(code).cloned().unwrap_or(Variable::Null)),
                    Dictionary::Column(inner) => self.push_cell(inner, code),
                },
            },
            Values::Any(values) => self.push_variable(values.get(row).cloned().unwrap_or(Variable::Null)),
            _ => self.push_variable(column.variable(row)),
        }
    }

    pub fn push_scaled(&mut self, mant: i64, scale: u8) {
        if self.kind.is_none() {
            self.start(Self::NUM);
        }
        if self.kind != Some(Self::NUM) {
            return self.push_variable(Variable::Number(rust_decimal::Decimal::new(mant, scale as u32)));
        }
        let row = self.open();
        self.mark(row);
        self.mant.push(mant);
        self.scale.push(scale);
    }

    pub fn push_text(&mut self, text: &str) {
        if self.kind.is_none() {
            self.start(Self::TEXT);
        }
        if self.kind != Some(Self::TEXT) {
            return self.push_variable(Variable::String(text.into()));
        }
        let row = self.open();
        self.mark(row);
        self.data.push_str(text);
        self.offsets.push(self.data.len() as i32);
    }

    pub fn push_null(&mut self) {
        self.open();
        self.nulls = true;
        self.fill();
    }

    fn mark(&mut self, row: usize) {
        if let Some(word) = self.valid.get_mut(row / 64) {
            *word |= 1 << (row % 64);
        }
    }

    pub fn push_literal(&mut self, literal: &Literal) {
        match literal {
            Literal::Null => self.push_null(),
            Literal::Array(_) | Literal::Object(_) => self.push_any(literal.variable()),
            Literal::Num(n) => match (i64::try_from(n.mantissa()), u8::try_from(n.scale())) {
                (Ok(m), Ok(s)) => self.push_scaled(m, s),
                _ => self.push_any(Variable::Number(*n)),
            },
            Literal::Str(text) => self.push_text(text),
            Literal::Bool(b) => self.push_bool(*b),
        }
    }

    pub fn push_any(&mut self, value: Variable) {
        match self.kind {
            None => self.start(Self::ANY),
            Some(Self::ANY) => {}
            Some(_) => self.degrade(),
        }
        let row = self.open();
        self.mark(row);
        self.any.push(value);
    }

    pub fn push_variable(&mut self, value: Variable) {
        let typed = |kind: u8| self.kind.is_none_or(|k| k == kind);
        match &value {
            Variable::Null => return self.push_null(),
            Variable::Number(n) if typed(Self::NUM) => {
                if let (Ok(m), Ok(s)) = (i64::try_from(n.mantissa()), u8::try_from(n.scale())) {
                    return self.push_scaled(m, s);
                }
            }
            Variable::String(text) if typed(Self::TEXT) => return self.push_text(text),
            Variable::Bool(b) if typed(Self::BOOL) => return self.push_bool(*b),
            _ => {}
        }
        self.push_any(value)
    }

    pub fn push_bool(&mut self, b: bool) {
        if self.kind.is_none() {
            self.start(Self::BOOL);
        }
        if self.kind != Some(Self::BOOL) {
            return self.push_any(Variable::Bool(b));
        }
        let row = self.open();
        self.mark(row);
        if b {
            if let Some(word) = self.bits.get_mut(row / 64) {
                *word |= 1 << (row % 64);
            }
        }
    }

    pub fn finish(mut self) -> Array {
        let values = match self.kind {
            Some(Self::NUM) => Store::Scaled {
                mant: self.mant,
                scale: self.scale,
            },
            Some(Self::BOOL) => Store::Bool(self.bits),
            Some(Self::TEXT) => Store::Text {
                offsets: self.offsets,
                data: self.data,
            },
            Some(_) => Store::Any(self.any),
            None => {
                self.nulls = true;
                Store::Any(vec![Variable::Null; self.rows])
            }
        };
        Array {
            values,
            valid: self.nulls.then_some(self.valid),
            rows: self.rows,
        }
    }
}
