use std::cell::OnceCell;
use std::rc::Rc;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;
use zen_expression::lane::{Column, Dictionary, Values};
use zen_expression::Variable;
use super::{Bits, Dict, Leaf, Store};

pub(crate) type Field<'a> = (Symbol, Leaf<'a>, Rc<[u64]>);
type Lanes = (Vec<Vec<u64>>, Option<Vec<u64>>);

#[derive(Debug)]
pub(crate) struct Records<'a> {
    pub(super) rows: usize,
    pub(super) offsets: Vec<i32>,
    pub(super) valid: Vec<u64>,
    pub(super) fields: Vec<Field<'a>>,
    pub(super) nulled: Option<Rc<[u64]>>,
    dense: OnceCell<Rc<[Variable]>>,
    lanes: OnceCell<Lanes>,
    maker: OnceCell<Option<Maker>>,
}

#[derive(Debug)]
pub(crate) struct Maker {
    shapes: std::cell::RefCell<Vec<(u64, Option<Rc<zen_types::variable::Shape>>)>>,
    cached: Vec<Option<Decoded>>,
}

type Decoded = (Rc<[i32]>, Vec<Variable>);

impl<'a> Records<'a> {
    pub const LANE_VIEW: bool = true;

    pub fn spread(&self, positions: &[usize], rows: usize) -> Option<Records<'a>> {
        if positions.len() != self.rows || positions.windows(2).any(|w| w[0] >= w[1]) || positions.last().is_some_and(|&last| last >= rows) {
            return None;
        }
        let mut offsets = Vec::with_capacity(rows + 1);
        let mut valid = vec![0u64; rows.div_ceil(64)];
        offsets.push(0i32);
        for (at, &row) in positions.iter().enumerate() {
            let start = *self.offsets.get(at)?;
            offsets.resize(row + 1, start);
            offsets.push(*self.offsets.get(at + 1)?);
            Bits::set(&mut valid, row, Bits::get(&self.valid, at));
        }
        let end = self.offsets.last().copied().unwrap_or(0);
        offsets.resize(rows + 1, end);
        Some(Records::new(rows, offsets, valid, self.fields.clone(), self.nulled.clone()))
    }

    pub fn pick(&self, positions: &[usize]) -> Records<'a> {
        let rows = positions.len();
        let mut offsets = Vec::with_capacity(rows + 1);
        let mut valid = vec![0u64; rows.div_ceil(64)];
        let mut items: Vec<usize> = Vec::new();
        offsets.push(0i32);
        for (at, &row) in positions.iter().enumerate() {
            if let Some((start, end)) = self.range(row) {
                Bits::set(&mut valid, at, true);
                items.extend(start..end);
            }
            offsets.push(i32::try_from(items.len()).unwrap_or(i32::MAX));
        }
        let items: Rc<[usize]> = items.into();
        let fields = self
            .fields
            .iter()
            .map(|(key, leaf, present)| (key.clone(), leaf.pick(&items), Rc::from(Bits::gather(present, &items))))
            .collect();
        let nulled = self.nulled.as_ref().map(|nulled| Rc::from(Bits::gather(nulled, &items)));
        Records::new(rows, offsets, valid, fields, nulled)
    }

    pub fn plain(&self) -> bool {
        self.nulled.is_none()
    }

    pub fn alive(&self, rows: usize) -> bool {
        (0..rows.min(self.rows)).any(|row| self.valid(row))
    }

    pub fn new(rows: usize, offsets: Vec<i32>, valid: Vec<u64>, fields: Vec<Field<'a>>, nulled: Option<Rc<[u64]>>) -> Records<'a> {
        Records {
            rows,
            offsets,
            valid,
            fields,
            nulled,
            dense: OnceCell::new(),
            lanes: OnceCell::new(),
            maker: OnceCell::new(),
        }
    }

    fn maker(&self) -> Option<&Maker> {
        self.maker
            .get_or_init(|| {
                if self.nulled.is_some() || self.fields.len() > 64 || self.fields.iter().any(|(key, _, _)| key.as_str().contains('.')) {
                    return None;
                }
                let cached = self
                    .fields
                    .iter()
                    .map(|(_, leaf, _)| match leaf {
                        Leaf::Typed(array) => match (&array.values, &array.valid) {
                            (Store::Coded { codes, dict }, None) if !matches!(**dict, Dict::Any(_)) => {
                                let values = dict.dictionary();
                                Some((codes.clone(), (0..values.len()).map(|code| values.variable(code)).collect()))
                            }
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect();
                Some(Maker {
                    shapes: std::cell::RefCell::new(Vec::new()),
                    cached,
                })
            })
            .as_ref()
    }

    fn made(&self, maker: &Maker, item: usize) -> Variable {
        let value = |(at, (_, leaf, _)): (usize, &Field<'a>)| match maker.cached.get(at).and_then(Option::as_ref) {
            Some((codes, values)) => codes
                .get(item)
                .and_then(|&code| usize::try_from(code).ok())
                .and_then(|code| values.get(code))
                .cloned()
                .unwrap_or(Variable::Null),
            None => leaf.get(item),
        };
        let pattern = self.fields.iter().enumerate().fold(0u64, |bits, (at, (_, _, present))| bits | u64::from(Bits::get(present, item)) << at);
        let found = maker.shapes.borrow().iter().find(|(p, _)| *p == pattern).map(|(_, shape)| shape.clone());
        let shape = match found {
            Some(shape) => shape,
            None => {
                let mut map = VariableMap::with_capacity(self.fields.len());
                self.fields.iter().enumerate().filter(|(at, _)| pattern >> at & 1 == 1).for_each(|(_, (key, _, _))| {
                    map.insert(key.clone(), Variable::Null);
                });
                let shape = map.shape().filter(|_| map.len() == pattern.count_ones() as usize).cloned();
                maker.shapes.borrow_mut().push((pattern, shape.clone()));
                shape
            }
        };
        let present = self.fields.iter().enumerate().filter(|(at, _)| pattern >> at & 1 == 1);
        match shape {
            Some(shape) => Variable::from_object(VariableMap::from_shape(shape, present.map(value))),
            None => {
                let mut map = VariableMap::with_capacity(self.fields.len());
                for (at, field) in present {
                    map.insert(field.0.clone(), value((at, field)));
                }
                Variable::from_object(map)
            }
        }
    }

    fn lanes(&self) -> &(Vec<Vec<u64>>, Option<Vec<u64>>) {
        self.lanes.get_or_init(|| {
            let count = self.count();
            let fields = self
                .fields
                .iter()
                .map(|(_, leaf, present)| leaf.validity(count).iter().zip(present.iter()).map(|(v, p)| v & p).collect())
                .collect();
            let alive = self.nulled.as_ref().map(|nulled| {
                let mut bits: Vec<u64> = nulled.iter().map(|w| !w).collect();
                bits.resize(count.div_ceil(64), u64::MAX);
                Bits::trim(&mut bits, count);
                bits
            });
            (fields, alive)
        })
    }

    pub fn parts(&self) -> Vec<(&str, Column<'_>)> {
        let (valid, _) = self.lanes();
        self.fields
            .iter()
            .zip(valid)
            .map(|((key, leaf, _), bits)| {
                (
                    key.as_str(),
                    Column {
                        values: leaf.column().values,
                        validity: Some((bits.as_slice(), 0)),
                    },
                )
            })
            .collect()
    }

    pub fn item<'c>(&'c self, fields: &'c [(&'c str, Column<'c>)]) -> Column<'c> {
        let (_, alive) = self.lanes();
        Column {
            values: Values::Struct { fields, len: self.count() },
            validity: alive.as_ref().map(|bits| (bits.as_slice(), 0)),
        }
    }

    pub fn list<'c>(&'c self, item: &'c Column<'c>) -> Column<'c> {
        Column {
            values: Values::List {
                offsets: &self.offsets,
                child: Dictionary::Column(item),
            },
            validity: Some((&self.valid, 0)),
        }
    }

    pub fn valid(&self, row: usize) -> bool {
        Bits::get(&self.valid, row)
    }

    pub fn range(&self, row: usize) -> Option<(usize, usize)> {
        match self.valid(row) {
            true => Some((usize::try_from(*self.offsets.get(row)?).ok()?, usize::try_from(*self.offsets.get(row + 1)?).ok()?)),
            false => None,
        }
    }

    pub fn count(&self) -> usize {
        self.offsets.last().and_then(|o| usize::try_from(*o).ok()).unwrap_or(0)
    }

    pub fn fields(&self) -> &[Field<'a>] {
        &self.fields
    }

    pub fn offsets(&self) -> &[i32] {
        &self.offsets
    }

    pub fn validity(&self) -> &[u64] {
        &self.valid
    }

    pub fn object(fields: &[Field<'a>], item: usize) -> Variable {
        if fields.iter().any(|(key, _, _)| key.as_str().contains('.')) {
            let object = Variable::empty_object();
            for (key, leaf, present) in fields {
                if Bits::get(present, item) {
                    object.dot_insert(key.as_str(), leaf.get(item));
                }
            }
            return object;
        }
        let mut map = VariableMap::with_capacity(fields.len());
        for (key, leaf, present) in fields {
            if Bits::get(present, item) {
                map.insert(key.clone(), leaf.get(item));
            }
        }
        Variable::from_object(map)
    }

    pub fn get(&self, row: usize) -> Variable {
        if let (Some(maker), Some((start, end))) = (self.maker(), self.range(row)) {
            return Variable::from_array((start..end).map(|item| self.made(maker, item)).collect());
        }
        match self.range(row) {
            Some((start, end)) => Variable::from_array(
                (start..end)
                    .map(|item| match self.nulled.as_ref().is_some_and(|n| Bits::get(n, item)) {
                        true => Variable::Null,
                        false => Self::object(&self.fields, item),
                    })
                    .collect(),
            ),
            None => Variable::Null,
        }
    }

    pub(super) fn dense(&self) -> &Rc<[Variable]> {
        self.dense.get_or_init(|| (0..self.rows).map(|row| self.get(row)).collect())
    }
}

pub(crate) enum Items<'a> {
    Input { column: Column<'a>, mask: Option<Rc<[u64]>> },
    Records(Rc<Records<'a>>),
}

impl<'a> Items<'a> {
    pub fn range(&self, row: usize) -> Option<(usize, usize)> {
        match self {
            Items::Input { column, mask } => match mask.as_ref().is_none_or(|m| Bits::get(m, row)) {
                true => column.range(row),
                false => None,
            },
            Items::Records(records) => records.range(row),
        }
    }

    pub fn count(&self) -> usize {
        match self {
            Items::Input { column, .. } => match column.values {
                Values::List { child, .. } => child.column().len(),
                _ => 0,
            },
            Items::Records(records) => records.count(),
        }
    }

    pub fn fields(&self) -> Option<Vec<Field<'a>>> {
        match self {
            Items::Records(records) if records.nulled.is_some() => None,
            Items::Records(records) => Some(records.fields.clone()),
            Items::Input { column, .. } => {
                let Values::List {
                    child: Dictionary::Column(child),
                    ..
                } = column.values
                else {
                    return None;
                };
                let Values::Struct { fields, len } = child.values else {
                    return None;
                };
                let rows = child.validity.map(|(bits, offset)| Bits::window(bits, offset, len));
                Some(
                    fields
                        .iter()
                        .map(|(name, field)| {
                            let mut present = match field.validity {
                                Some((bits, offset)) => Bits::window(bits, offset, len),
                                None => Bits::ones(len),
                            };
                            if let Some(rows) = &rows {
                                present.iter_mut().zip(rows).for_each(|(p, r)| *p &= r);
                            }
                            (Symbol::from(*name), Leaf::input(*field, len), Rc::from(present))
                        })
                        .collect(),
                )
            }
        }
    }
}
