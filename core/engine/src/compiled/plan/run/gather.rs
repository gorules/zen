use crate::compiled::leaf::{Array, Bits, ColumnBuilder, Dict, Leaf, Literal, Store as Buffer};
use std::rc::Rc;
use zen_expression::lane::{Column, Dictionary, Values};
use super::Run;
use super::store::View;

impl<'a> Run<'_, 'a> {
    pub(super) fn coded_gather<'v>(rows: usize, views: &'v [Option<View<'v>>], located: &dyn Fn(usize) -> Option<(usize, &'v Column<'v>, usize)>) -> Option<Array> {
        let mut unique: Vec<&Rc<Dict>> = Vec::new();
        let mut slot = Vec::with_capacity(views.len());
        for view in views {
            match view {
                None => slot.push(0),
                Some((_, _, _, dict)) => {
                    let dict = (*dict)?;
                    let index = match unique.iter().position(|d| Rc::ptr_eq(d, dict)) {
                        Some(index) => index,
                        None => {
                            unique.push(dict);
                            unique.len() - 1
                        }
                    };
                    slot.push(index);
                }
            }
        }
        let (dict, bases) = Dict::concat(&unique)?;
        let mut valid = vec![0u64; rows.div_ceil(64)];
        let codes: Rc<[i32]> = (0..rows)
            .map(|row| {
                let Some((g, view, at)) = located(row) else {
                    return -1;
                };
                match view.code(at) {
                    Some(code) => {
                        valid[row >> 6] |= 1 << (row & 63);
                        code as i32 + bases[slot[g]]
                    }
                    None => -1,
                }
            })
            .collect();
        Some(Array::coded_with(codes, dict, valid))
    }

    pub(super) fn coalesced(column: &Column, value: &Literal, rows: usize) -> Array {
        let typed = match value {
            Literal::Num(n) => Literal::Num(*n).scaled().and_then(|fill| {
                let (mut mant, mut scale) = (vec![0i64; rows], vec![0u8; rows]);
                for row in 0..rows {
                    let (m, s) = match column.valid(row) {
                        false => fill,
                        true => match column.values {
                            Values::Scaled { mant, scale } => mant.get(row).zip(scale.get(row)).map(|(m, s)| (*m, *s))?,
                            Values::I64(values) => (*values.get(row)?, 0),
                            Values::Dec(values) => {
                                let d = values.get(row)?;
                                if d.is_zero() && d.is_sign_negative() {
                                    return None;
                                }
                                (i64::try_from(d.mantissa()).ok()?, u8::try_from(d.scale()).ok()?)
                            }
                            _ => return None,
                        },
                    };
                    mant[row] = m;
                    scale[row] = s;
                }
                Some(Array::parts(Buffer::Scaled { mant, scale }, None, rows))
            }),
            Literal::Str(fill) => {
                let mut offsets = Vec::with_capacity(rows + 1);
                let mut data = String::new();
                offsets.push(0i32);
                let mut ok = true;
                for row in 0..rows {
                    match column.valid(row) {
                        false => data.push_str(fill),
                        true => match column.text(row) {
                            Some(text) => data.push_str(text),
                            None => {
                                ok = false;
                                break;
                            }
                        },
                    }
                    offsets.push(data.len() as i32);
                }
                ok.then(|| Array::parts(Buffer::Text { offsets, data }, None, rows))
            }
            Literal::Bool(fill) => {
                let mut ok = true;
                let bits = Bits::of(rows, |row| match column.valid(row) {
                    false => *fill,
                    true => column.boolean(row).unwrap_or_else(|| {
                        ok = false;
                        false
                    }),
                });
                ok.then(|| Array::parts(Buffer::Bool(bits), None, rows))
            }
            _ => None,
        };
        typed.unwrap_or_else(|| {
            let mut builder = ColumnBuilder::with_capacity(rows);
            for row in 0..rows {
                match column.valid(row) {
                    true => builder.push_cell(column, row),
                    false => builder.push_literal(value),
                }
            }
            builder.finish()
        })
    }

    fn selected(a: &Column, b: &Column, take: &[u64], rows: usize) -> Option<Array> {
        let scaled = |column: &Column, row: usize| -> Option<(i64, u8)> {
            match column.values {
                Values::Scaled { mant, scale } => mant.get(row).zip(scale.get(row)).map(|(m, s)| (*m, *s)),
                Values::I64(values) => values.get(row).map(|v| (*v, 0)),
                Values::Dict {
                    keys,
                    values: Dictionary::Scaled { mant, scale },
                } => keys
                    .get(row)
                    .and_then(|k| usize::try_from(*k).ok())
                    .and_then(|c| mant.get(c).zip(scale.get(c)))
                    .map(|(m, s)| (*m, *s)),
                _ => None,
            }
        };
        let numeric = |column: &Column| {
            matches!(column.values, Values::Scaled { .. } | Values::I64(_) | Values::Dict { values: Dictionary::Scaled { .. }, .. })
                || (0..rows).all(|row| !column.valid(row))
        };
        if !numeric(a) || !numeric(b) {
            return None;
        }
        let (mut mant, mut scale) = (vec![0i64; rows], vec![0u8; rows]);
        let mut valid = vec![0u64; rows.div_ceil(64)];
        for row in 0..rows {
            let source = match Bits::get(take, row) {
                true => b,
                false => a,
            };
            if !source.valid(row) {
                continue;
            }
            let (m, s) = scaled(source, row)?;
            mant[row] = m;
            scale[row] = s;
            valid[row >> 6] |= 1 << (row & 63);
        }
        Some(Array::parts(Buffer::Scaled { mant, scale }, Some(valid), rows))
    }

    pub(super) fn typed_gather<'c>(rows: usize, family: Option<u8>, cell: &dyn Fn(usize) -> Option<(&'c Column<'c>, usize)>) -> Option<Array> {
        let mut valid = vec![0u64; rows.div_ceil(64)];
        match family? {
            0 => {
                let (mut mant, mut scale) = (vec![0i64; rows], vec![0u8; rows]);
                for row in 0..rows {
                    let Some((view, at)) = cell(row) else {
                        continue;
                    };
                    let parts = match view.values {
                        Values::Scaled { mant, scale } => mant.get(at).zip(scale.get(at)).map(|(m, s)| (*m, *s)),
                        Values::I64(values) => values.get(at).map(|v| (*v, 0)),
                        Values::Dict {
                            keys,
                            values: Dictionary::Scaled { mant, scale },
                        } => keys
                            .get(at)
                            .and_then(|k| usize::try_from(*k).ok())
                            .and_then(|c| mant.get(c).zip(scale.get(c)))
                            .map(|(m, s)| (*m, *s)),
                        _ => None,
                    };
                    if let Some((m, s)) = parts {
                        mant[row] = m;
                        scale[row] = s;
                        valid[row >> 6] |= 1 << (row & 63);
                    }
                }
                Some(Array::parts(Buffer::Scaled { mant, scale }, Some(valid), rows))
            }
            1 => {
                let mut bits = vec![0u64; rows.div_ceil(64)];
                for row in 0..rows {
                    if let Some(b) = cell(row).and_then(|(view, at)| view.boolean(at)) {
                        Bits::set(&mut bits, row, b);
                        valid[row >> 6] |= 1 << (row & 63);
                    }
                }
                Some(Array::parts(Buffer::Bool(bits), Some(valid), rows))
            }
            2 => {
                let mut offsets = Vec::with_capacity(rows + 1);
                let mut data = String::new();
                offsets.push(0i32);
                for row in 0..rows {
                    if let Some(text) = cell(row).and_then(|(view, at)| view.text(at)) {
                        data.push_str(text);
                        valid[row >> 6] |= 1 << (row & 63);
                    }
                    offsets.push(data.len() as i32);
                }
                Some(Array::parts(Buffer::Text { offsets, data }, Some(valid), rows))
            }
            _ => None,
        }
    }

    pub(super) fn chosen(a: &Leaf<'a>, b: &Leaf<'a>, take: &[u64], rows: usize) -> Leaf<'a> {
        let (a, b) = (a.column(), b.column());
        let array = Self::selected(&a, &b, take, rows).unwrap_or_else(|| {
            let mut builder = ColumnBuilder::with_capacity(rows);
            for row in 0..rows {
                match Bits::get(take, row) {
                    true => builder.push_cell(&b, row),
                    false => builder.push_cell(&a, row),
                }
            }
            builder.finish()
        });
        Leaf::typed(array)
    }
}
