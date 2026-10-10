use std::cell::OnceCell;
use std::rc::Rc;
use zen_expression::lane::{Column, Dictionary, Values};
use zen_expression::Variable;
use super::{Array, Bits, ColumnBuilder, Dict, Input, Items, Literal, Masked, Passed, Picked, Records, Scattered, Stitched, Store, Templated};

#[derive(Clone, Debug)]
pub(crate) enum Leaf<'a> {
    Scattered(Rc<Scattered<'a>>),
    Masked(Rc<Masked<'a>>),
    Input(Rc<Input<'a>>),
    Any(Rc<[Variable]>),
    Typed(Rc<Array>),
    Picked(Rc<Picked<'a>>),
    Stitched(Rc<Stitched<'a>>),
    Records(Rc<Records<'a>>),
    Templated(Rc<Templated>),
}

impl<'a> Leaf<'a> {
    pub fn items(&self) -> Option<Items<'a>> {
        match self {
            Leaf::Records(records) => Some(Items::Records(records.clone())),
            Leaf::Input(input) => Self::struct_list(input.column).then(|| Items::Input { column: input.column, mask: None }),
            Leaf::Masked(masked) => match &masked.inner {
                Leaf::Input(input) if Self::struct_list(input.column) => Some(Items::Input {
                    column: input.column,
                    mask: Some(masked.bits().clone()),
                }),
                Leaf::Records(records) => {
                    let valid: Vec<u64> = records.valid.iter().zip(masked.bits().iter()).map(|(v, m)| v & m).collect();
                    Some(Items::Records(Rc::new(Records::new(records.rows, records.offsets.clone(), valid, records.fields.clone(), records.nulled.clone()))))
                }
                _ => None,
            },
            _ => None,
        }
    }

    pub fn records(&self) -> Option<Rc<Records<'a>>> {
        match self {
            Leaf::Records(records) => Some(records.clone()),
            Leaf::Picked(picked) => {
                let records = picked.source.records()?;
                match picked.dense.get_or_init(|| Leaf::Records(Rc::new(records.pick(&picked.positions)))) {
                    Leaf::Records(records) => Some(records.clone()),
                    _ => None,
                }
            }
            Leaf::Scattered(scattered) if scattered.inner.records().is_some() => match scattered.dense.get_or_init(|| Self::spread(scattered)) {
                Leaf::Records(records) => Some(records.clone()),
                _ => None,
            },
            Leaf::Masked(masked) if matches!(masked.inner, Leaf::Records(_)) => match self.items() {
                Some(Items::Records(records)) => Some(records),
                _ => None,
            },
            _ => None,
        }
    }

    fn struct_list(column: Column) -> bool {
        matches!(column.values, Values::List { child: Dictionary::Column(child), .. } if matches!(child.values, Values::Struct { .. }))
    }

    pub fn typed(array: Array) -> Leaf<'a> {
        Leaf::Typed(Rc::new(array))
    }

    pub fn masked(inner: Leaf<'a>, mask: &Rc<[u64]>) -> Leaf<'a> {
        let rows = inner.len();
        let trimmed = mask.last().is_none_or(|w| rows.is_multiple_of(64) || w >> (rows % 64) == 0);
        let bits = OnceCell::new();
        if inner.all_valid() && trimmed && mask.len() == rows.div_ceil(64) {
            let _ = bits.set(mask.clone());
        }
        Leaf::Masked(Rc::new(Masked { inner, mask: mask.clone(), bits }))
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
            (Leaf::Records(a), Leaf::Records(b)) => Rc::ptr_eq(a, b),
            (Leaf::Templated(a), Leaf::Templated(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    fn spread(scattered: &Scattered<'a>) -> Leaf<'a> {
        let (rows, positions) = (scattered.rows, &scattered.positions);
        if let Some(spread) = scattered.inner.records().and_then(|records| records.spread(positions, rows)) {
            return Leaf::Records(Rc::new(spread));
        }
        if let Leaf::Templated(templated) = &scattered.inner {
            let mut codes = vec![-1i32; rows];
            for (at, &row) in positions.iter().enumerate() {
                if let (Some(slot), Some(code)) = (codes.get_mut(row), templated.codes.get(at)) {
                    *slot = *code;
                }
            }
            return Templated::leaf(codes.into(), templated.templates.clone());
        }
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
            Values::Text { .. } | Values::Utf8 { .. } => {
                return Self::spread_text(&column, positions, rows).map_or_else(|| Self::spread_cells(&column, positions, rows), Leaf::typed)
            }
            _ => return Self::spread_cells(&column, positions, rows),
        };
        Leaf::typed(Array {
            values,
            valid: Some(valid),
            rows,
        })
    }

    fn spread_text(column: &Column, positions: &[usize], rows: usize) -> Option<Array> {
        let mut offsets = Vec::with_capacity(rows + 1);
        let mut data = String::new();
        let mut valid = vec![0u64; rows.div_ceil(64)];
        let (mut nulls, mut filled) = (false, false);
        let mut next = positions.iter().enumerate().peekable();
        offsets.push(0i32);
        for row in 0..rows {
            let text = match next.peek() {
                Some(&(i, &at)) if at == row => {
                    next.next();
                    match column.valid(i) {
                        true => column.text(i),
                        false => None,
                    }
                }
                _ => None,
            };
            match text {
                Some(text) => {
                    data.push_str(text);
                    valid[row >> 6] |= 1 << (row & 63);
                    filled = true;
                }
                None => nulls = true,
            }
            offsets.push(data.len() as i32);
        }
        filled.then_some(Array {
            values: Store::Text { offsets, data },
            valid: nulls.then_some(valid),
            rows,
        })
    }

    fn spread_cells(column: &Column, positions: &[usize], rows: usize) -> Leaf<'a> {
        let mut builder = ColumnBuilder::with_capacity(rows);
        let mut next = positions.iter().enumerate().peekable();
        for row in 0..rows {
            match next.peek() {
                Some(&(i, &at)) if at == row => {
                    builder.push_cell(column, i);
                    next.next();
                }
                _ => builder.push_null(),
            }
        }
        Leaf::typed(builder.finish())
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
            Leaf::Records(records) => records.get(row),
            Leaf::Templated(templated) => templated.get(row),
            _ => self.column().variable(row),
        }
    }

    pub fn objects(&self) -> bool {
        if let Leaf::Masked(_) | Leaf::Scattered(_) = self {
            if let Some(found) = self.plain_objects() {
                return found;
            }
        }
        match self {
            Leaf::Any(values) => values.iter().any(|v| matches!(v, Variable::Object(_))),
            Leaf::Picked(picked) => picked.source.objects(),
            Leaf::Stitched(stitched) => stitched.sources.iter().any(Leaf::objects),
            Leaf::Records(records) => records.fields.iter().any(|(key, leaf, _)| key.as_str().starts_with('$') || leaf.objects()),
            Leaf::Templated(templated) => templated.templates.iter().any(|literal| matches!(literal, Literal::Object(_))),
            Leaf::Masked(masked) if matches!(masked.inner, Leaf::Records(_)) => masked.inner.objects(),
            Leaf::Scattered(scattered) if matches!(scattered.inner, Leaf::Records(_)) => scattered.inner.objects(),
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

    fn plain_objects(&self) -> Option<bool> {
        match self {
            _ if self.objectless() => Some(false),
            Leaf::Typed(_) | Leaf::Input(_) | Leaf::Any(_) => Some(self.objects()),
            Leaf::Masked(masked) => masked.inner.plain_objects(),
            _ => None,
        }
    }

    fn objectless(&self) -> bool {
        let plain = |values: &[Variable]| !values.iter().any(|v| matches!(v, Variable::Object(_)));
        match self {
            _ if self.scalar() => true,
            Leaf::Any(values) => plain(values),
            Leaf::Typed(typed) => match &typed.values {
                Store::Any(values) => plain(values),
                Store::Coded { dict, .. } => matches!(&**dict, Dict::Any(values) if plain(values)),
                _ => false,
            },
            Leaf::Input(input) => match input.column.values {
                Values::Any(values) | Values::Dict { values: Dictionary::Any(values), .. } => plain(values),
                _ => false,
            },
            Leaf::Templated(templated) => !templated.templates.iter().any(|literal| matches!(literal, Literal::Object(_))),
            Leaf::Masked(masked) => masked.inner.objectless(),
            Leaf::Scattered(scattered) => scattered.inner.objectless(),
            Leaf::Picked(picked) => picked.source.objectless(),
            Leaf::Stitched(stitched) => stitched.sources.iter().all(Leaf::objectless),
            Leaf::Records(_) => false,
        }
    }

    fn scalar(&self) -> bool {
        match self {
            Leaf::Typed(typed) => match &typed.values {
                Store::Scaled { .. } | Store::Bool(_) | Store::Text { .. } => true,
                Store::Coded { dict, .. } => !matches!(**dict, Dict::Any(_)),
                Store::Any(_) | Store::List { .. } => false,
            },
            Leaf::Masked(masked) => masked.inner.scalar(),
            Leaf::Scattered(scattered) => scattered.inner.scalar(),
            Leaf::Picked(picked) => picked.source.scalar(),
            Leaf::Stitched(stitched) => stitched.sources.iter().all(Leaf::scalar),
            Leaf::Input(input) => match input.column.values {
                Values::Dict { values, .. } => !matches!(values, Dictionary::Any(_) | Dictionary::Column(_)),
                Values::List { .. } | Values::Struct { .. } | Values::Any(_) => false,
                _ => true,
            },
            _ => false,
        }
    }

    fn dense(&self, positions: &[usize]) -> Leaf<'a> {
        match self {
            Leaf::Any(values) => Leaf::Any(positions.iter().map(|&i| values[i].clone()).collect()),
            Leaf::Typed(typed) => Leaf::typed(typed.pick(positions)),
            Leaf::Input(input) => Leaf::typed(Array::gather(&input.column, positions)),
            Leaf::Masked(_) | Leaf::Scattered(_) => Leaf::typed(Array::gather(&self.column(), positions)),
            Leaf::Records(records) => Leaf::Records(Rc::new(records.pick(positions))),
            Leaf::Templated(templated) => templated.mapped(positions.iter().copied()),
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
            Leaf::Templated(templated) => templated.mapped(ids.iter().map(|&id| id as usize)),
            other => {
                let column = other.column();
                Leaf::typed(Array::from_values((0..other.len()).map(|row| column.variable(row)).collect()).expand(ids))
            }
        }
    }

    pub fn regroup(&self, positions: &Rc<[usize]>) -> Leaf<'a> {
        match self.records() {
            Some(records) => Leaf::Records(Rc::new(records.pick(positions))),
            None => self.pick(positions),
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
                validity: Some((masked.bits(), 0)),
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
            Leaf::Records(records) => Column::new(Values::Any(records.dense())),
            Leaf::Templated(templated) => Column::with_validity(Values::Any(templated.dense()), &templated.valid, 0),
        }
    }

    pub fn null(&self, row: usize) -> bool {
        match self {
            Leaf::Any(values) => values.get(row).is_none_or(|v| matches!(v, Variable::Null)),
            Leaf::Picked(_) | Leaf::Stitched(_) => self.source(row).is_none_or(|(leaf, at)| leaf.null(at)),
            Leaf::Records(records) => !records.valid(row),
            Leaf::Templated(templated) => templated.template(row).is_none(),
            _ => ColumnBuilder::null_at(&self.column(), row),
        }
    }

    pub fn valid_at(&self, row: usize) -> bool {
        match self {
            Leaf::Input(input) => input.column.valid(row),
            Leaf::Masked(masked) => Bits::get(masked.bits(), row),
            Leaf::Scattered(_) => self.column().valid(row),
            Leaf::Typed(typed) => typed.valid(row),
            Leaf::Any(_) | Leaf::Records(_) => true,
            Leaf::Templated(templated) => templated.template(row).is_some(),
            Leaf::Picked(_) => self.source(row).is_some_and(|(leaf, at)| leaf.valid_at(at)),
            Leaf::Stitched(_) => self.column().valid(row),
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
            Leaf::Any(_) | Leaf::Records(_) => Bits::ones(rows),
            Leaf::Templated(templated) => Bits::window(&templated.valid, 0, rows),
            Leaf::Masked(masked) => Bits::window(masked.bits(), 0, rows),
            Leaf::Scattered(_) => match self.column().validity {
                Some((bits, offset)) => Bits::window(bits, offset, rows),
                None => Bits::ones(rows),
            },
            Leaf::Picked(picked) => match picked.dense.get() {
                Some(dense) => dense.validity(rows),
                None => Bits::gather(&picked.source.validity(picked.source.len()), &picked.positions),
            },
            Leaf::Stitched(_) => match self.column().validity {
                Some((bits, offset)) => Bits::window(bits, offset, rows),
                None => Bits::ones(rows),
            },
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
            Leaf::Stitched(stitched) => {
                let parts: Vec<Vec<u64>> = stitched.sources.iter().map(|leaf| leaf.valued(leaf.len())).collect();
                Bits::of(rows, |row| stitched.located.get(row).is_some_and(|&(piece, at)| parts.get(piece).is_some_and(|bits| Bits::get(bits, at))))
            }
            Leaf::Records(_) | Leaf::Templated(_) => Bits::of(rows, |row| !self.null(row)),
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
            Leaf::Records(_) => false,
            Leaf::Templated(templated) => matches!(templated.template(row), Some(Literal::Bool(true))),
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
            Leaf::Records(records) => records.rows,
            Leaf::Templated(templated) => templated.codes.len(),
        }
    }
}
