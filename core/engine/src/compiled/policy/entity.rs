use super::shred::Shred;
use crate::compiled::typed::{Bits, ColumnBuilder, Leaf};
use crate::compiled::CompiledGraph;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Dictionary, Values};
use zen_expression::Variable;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;

pub(super) struct Field<'a> {
    pub name: Arc<str>,
    pub leaf: Leaf<'a>,
    pub present: Option<Rc<[u64]>>,
    pub input: bool,
}

pub(super) struct Entity<'a> {
    pub name: Arc<str>,
    pub path: Arc<str>,
    pub owner: Option<Arc<str>>,
    pub entry: usize,
    pub offsets: Rc<[i32]>,
    pub validity: Option<Rc<[u64]>>,
    pub children: usize,
    pub fields: Vec<Field<'a>>,
    pub active: bool,
    pub hosted: Option<Rc<[usize]>>,
    cache: Option<Vec<Variable>>,
}

impl<'a> Entity<'a> {
    pub fn new(name: Arc<str>, path: Arc<str>, owner: Option<Arc<str>>, entry: usize, list: Column<'a>) -> Option<Self> {
        let Values::List {
            offsets,
            child: Dictionary::Column(child),
        } = list.values
        else {
            return None;
        };
        let Values::Struct { fields, len } = child.values else {
            return None;
        };
        if child.validity.is_some() {
            return None;
        }
        let fields = fields
            .iter()
            .map(|(name, column)| Field {
                name: Arc::from(*name),
                leaf: Leaf::input(CompiledGraph::validated(*column, len), len),
                present: None,
                input: true,
            })
            .collect();
        let rows = offsets.len().saturating_sub(1);
        Some(Self {
            name,
            path,
            owner,
            entry,
            offsets: Rc::from(offsets),
            validity: list.validity.map(|(bits, offset)| Rc::from(Bits::window(bits, offset, rows))),
            children: len,
            fields,
            active: true,
            hosted: None,
            cache: None,
        })
    }

    pub fn shredded(name: Arc<str>, path: Arc<str>, owner: Option<Arc<str>>, entry: usize, shred: Shred) -> Self {
        let fields = shred
            .fields
            .into_iter()
            .map(|(name, array)| Field {
                name,
                leaf: Leaf::typed(array),
                present: None,
                input: true,
            })
            .collect();
        Self {
            name,
            path,
            owner,
            entry,
            offsets: shred.offsets,
            validity: shred.validity,
            children: shred.children,
            fields,
            active: true,
            hosted: Some(shred.hosted),
            cache: None,
        }
    }

    pub fn range(&self, row: usize) -> Option<(usize, usize)> {
        if self.validity.as_ref().is_some_and(|bits| !Bits::get(bits, row)) {
            return None;
        }
        let a = usize::try_from(*self.offsets.get(row)?).unwrap_or(0);
        let b = usize::try_from(*self.offsets.get(row + 1)?).unwrap_or(0).max(a);
        Some((a.min(self.children), b.min(self.children)))
    }

    pub fn field(&self, name: &str) -> Option<usize> {
        self.fields.iter().rposition(|f| f.name.as_ref() == name)
    }

    pub fn flat(&self) -> bool {
        self.fields.iter().all(|f| !f.name.contains('.'))
    }

    pub fn overlaps(&self, name: &str) -> bool {
        let under = |long: &str, short: &str| long.strip_prefix(short).is_some_and(|rest| rest.starts_with('.'));
        self.fields.iter().any(|f| under(&f.name, name) || under(name, &f.name))
    }

    fn present(field: &Field, child: usize) -> bool {
        match field.input {
            true => field.leaf.valid_at(child) || !matches!(field.leaf.get(child), Variable::Null),
            false => field.present.as_ref().is_none_or(|bits| Bits::get(bits, child)),
        }
    }

    fn held(field: &Field, children: usize) -> Vec<u64> {
        let mut bits = field.leaf.validity(children);
        Bits::trim(&mut bits, children);
        let full = bits.iter().map(|w| w.count_ones() as usize).sum::<usize>() == children;
        if !full {
            for child in 0..children {
                if !Bits::get(&bits, child) && !matches!(field.leaf.get(child), Variable::Null) {
                    Bits::set(&mut bits, child, true);
                }
            }
        }
        bits
    }

    pub fn view(&self, field: usize) -> Leaf<'a> {
        let field = &self.fields[field];
        match &field.present {
            Some(bits) => Leaf::masked(field.leaf.clone(), bits),
            None => field.leaf.clone(),
        }
    }

    fn object(&self, child: usize) -> Variable {
        let mut map = VariableMap::with_capacity(self.fields.len());
        let mut nested = Vec::new();
        for field in &self.fields {
            if Self::present(field, child) {
                let value = field.leaf.get(child);
                let value = match value {
                    Variable::Object(_) | Variable::Array(_) => value.depth_clone(usize::MAX),
                    other => other,
                };
                match field.name.contains('.') {
                    true => nested.push((field.name.clone(), value)),
                    false => {
                        map.insert(Symbol::from(field.name.as_ref()), value);
                    }
                }
            }
        }
        let object = Variable::from_object(map);
        for (name, value) in nested {
            object.dot_insert(&name, value);
        }
        object
    }

    pub fn materialize(&mut self) {
        if self.cache.is_none() {
            self.cache = Some((0..self.children).map(|child| self.object(child)).collect());
        }
    }

    pub fn cached(&self) -> bool {
        self.cache.is_some()
    }

    pub fn records(&self, rows: usize) -> Leaf<'a> {
        let offsets: Vec<i32> = self.offsets.get(..=rows).map_or_else(Vec::new, <[i32]>::to_vec);
        let valid = match &self.validity {
            Some(bits) => Bits::window(bits, 0, rows),
            None => Bits::ones(rows),
        };
        let fields = self
            .fields
            .iter()
            .map(|field| {
                let present: Rc<[u64]> = match (&field.present, field.input) {
                    (Some(bits), _) => bits.clone(),
                    (None, false) => Bits::ones(self.children).into(),
                    (None, true) => Self::held(field, self.children).into(),
                };
                (Symbol::from(field.name.as_ref()), field.leaf.clone(), present)
            })
            .collect();
        Leaf::Records(Rc::new(crate::compiled::typed::Records::new(rows, offsets, valid, fields, None)))
    }

    pub fn compose(&self, row: usize) -> Option<Variable> {
        let (a, b) = self.range(row)?;
        Some(Variable::from_array(match &self.cache {
            Some(cache) => cache[a..b].iter().map(Variable::shallow_clone).collect(),
            None => (a..b).map(|child| self.object(child)).collect(),
        }))
    }

    pub fn write(&mut self, name: Arc<str>, leaf: Leaf<'a>, kids: &Rc<[usize]>) {
        if let Some(cache) = &self.cache {
            let key = Symbol::from(name.as_ref());
            for (local, &child) in kids.iter().enumerate() {
                match (cache.get(child), name.contains('.')) {
                    (Some(object), true) => {
                        object.dot_insert(&name, leaf.get(local));
                    }
                    (Some(Variable::Object(map)), false) => {
                        map.borrow_mut().insert(key.clone(), leaf.get(local));
                    }
                    _ => {}
                }
            }
        }
        let total = self.children;
        let full = kids.len() == total;
        let (leaf, present) = match full {
            true => (leaf, None),
            false => {
                let mut bits = vec![0u64; total.div_ceil(64)];
                kids.iter().for_each(|&child| Bits::set(&mut bits, child, true));
                (Leaf::scattered(leaf, kids.clone(), total), Some(Rc::from(bits)))
            }
        };
        let fresh = Field {
            name,
            leaf,
            present,
            input: false,
        };
        match (self.field(&fresh.name), full) {
            (None, _) => self.fields.push(fresh),
            (Some(at), true) => self.fields[at] = fresh,
            (Some(at), false) => {
                let old = &self.fields[at];
                let (new_column, old_column) = (fresh.leaf.column(), old.leaf.column());
                let mut builder = ColumnBuilder::with_capacity(total);
                let mut bits = vec![0u64; total.div_ceil(64)];
                for child in 0..total {
                    match (Self::present(&fresh, child), Self::present(old, child)) {
                        (true, _) => {
                            builder.push_cell(&new_column, child);
                            Bits::set(&mut bits, child, true);
                        }
                        (false, true) => {
                            builder.push_cell(&old_column, child);
                            Bits::set(&mut bits, child, true);
                        }
                        (false, false) => builder.push_null(),
                    }
                }
                let merged = Field {
                    name: fresh.name.clone(),
                    leaf: Leaf::typed(builder.finish()),
                    present: Some(bits.into()),
                    input: false,
                };
                self.fields[at] = merged;
            }
        }
    }
}
