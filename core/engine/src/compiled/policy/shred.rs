use super::entity::Entity;
use crate::compiled::typed::{Array, Bits, Store};
use crate::policy::evaluator::EvalArtifact;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns, Dictionary, Values};
use zen_expression::Variable;
use zen_types::variable::Shape;

pub(super) struct Shred {
    pub offsets: Rc<[i32]>,
    pub validity: Option<Rc<[u64]>>,
    pub children: usize,
    pub fields: Vec<(Arc<str>, Array)>,
    pub hosted: Rc<[usize]>,
}

enum Cells {
    Empty,
    Scaled { mant: Vec<i64>, scale: Vec<u8> },
    Text { offsets: Vec<i32>, data: String },
    Bool(Vec<u64>),
    Any(Vec<Variable>),
}

struct Field {
    name: Arc<str>,
    cells: Cells,
    present: Vec<u64>,
    len: usize,
    count: usize,
}

impl Field {
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

    fn scaled(n: &rust_decimal::Decimal) -> Option<(i64, u8)> {
        if n.is_zero() && n.is_sign_negative() {
            return None;
        }
        Some((i64::try_from(n.mantissa()).ok()?, u8::try_from(n.scale()).ok()?))
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
            Variable::Number(n) if Self::scaled(n).is_some() => {
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

    fn push(&mut self, value: &Variable, capacity: usize) {
        if matches!(self.cells, Cells::Empty) {
            self.start(value, capacity);
        }
        let len = self.len;
        let fits = match (&mut self.cells, value) {
            (Cells::Scaled { mant, scale }, Variable::Number(n)) => match Self::scaled(n) {
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

    fn array(self) -> (Arc<str>, Array) {
        let len = self.len;
        let valid = (self.count < len).then_some(self.present);
        let store = match self.cells {
            Cells::Empty => Store::Any(vec![Variable::Null; len]),
            Cells::Scaled { mant, scale } => Store::Scaled { mant, scale },
            Cells::Text { offsets, data } => Store::Text { offsets, data },
            Cells::Bool(mut bits) => {
                bits.resize(len.div_ceil(64), 0);
                Store::Bool(bits)
            }
            Cells::Any(values) => Store::Any(values),
        };
        (self.name, Array::parts(store, valid, len))
    }
}

struct Builder {
    rows: usize,
    offsets: Vec<i32>,
    validity: Vec<u64>,
    children: usize,
    capacity: usize,
    fields: Vec<Field>,
    shapes: Vec<(u64, Vec<usize>)>,
    hosted: Vec<usize>,
}

impl Builder {
    fn new(rows: usize, capacity: usize) -> Self {
        let mut offsets = Vec::with_capacity(rows + 1);
        offsets.push(0);
        Self {
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
        self.fields.push(Field::new(Arc::from(key), self.children, self.capacity));
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

    fn item(&mut self, item: &Variable) -> Option<()> {
        let Variable::Object(map) = item else {
            return None;
        };
        let child = self.children;
        let map = map.borrow();
        let touched = match map.slots() {
            Some((shape, values)) => {
                let layout = self.layout(shape)?;
                let (fields, capacity) = (&mut self.fields, self.capacity);
                for (&at, value) in self.shapes[layout].1.iter().zip(values) {
                    fields[at].push(value, capacity);
                }
                values.len()
            }
            None => {
                for (hint, (key, value)) in map.iter().enumerate() {
                    let at = self.field(hint, key)?;
                    self.fields[at].push(value, self.capacity);
                }
                map.len()
            }
        };
        if touched != self.fields.len() {
            self.fields.iter_mut().filter(|field| field.len == child).for_each(Field::missing);
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
            fields: self.fields.into_iter().map(Field::array).collect(),
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
        let mut builder = Builder::new(rows, capacity);
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

    pub fn sure(artifact: &EvalArtifact, columns: &Columns, entities: &[Entity], goals: &[Arc<str>]) -> Option<Vec<bool>> {
        let shredded: Vec<&Entity> = entities.iter().filter(|entity| entity.hosted.is_some()).collect();
        if shredded.is_empty() {
            return artifact.sure(columns, goals);
        }
        let parts: Vec<Vec<(&str, Column)>> = shredded
            .iter()
            .map(|entity| entity.fields.iter().map(|field| (field.name.as_ref(), field.leaf.column())).collect())
            .collect();
        let items: Vec<Column> = parts
            .iter()
            .zip(&shredded)
            .map(|(fields, entity)| Column::new(Values::Struct { fields, len: entity.children }))
            .collect();
        let lists: Vec<(usize, Column)> = items
            .iter()
            .zip(&shredded)
            .map(|(item, entity)| {
                (
                    entity.entry,
                    Column {
                        values: Values::List {
                            offsets: &entity.offsets,
                            child: Dictionary::Column(item),
                        },
                        validity: entity.validity.as_deref().map(|bits| (bits, 0)),
                    },
                )
            })
            .collect();
        let mut replaced = Columns::new(columns.rows);
        for (index, (path, column)) in columns.columns.iter().enumerate() {
            let column = lists.iter().find(|(entry, _)| *entry == index).map_or(*column, |(_, list)| *list);
            replaced = replaced.column(path, column);
        }
        artifact.sure(&replaced, goals)
    }
}
