use crate::compiled::core::list::Frame;
use crate::compiled::shred::{Normalizer, Shred};
use crate::policy::evaluator::EvalArtifact;
use crate::compiled::leaf::{Bits, ColumnBuilder, Field, Leaf, Records};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns, Dictionary, Values};
use zen_expression::Variable;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;

pub(super) struct Entity<'a> {
    pub name: Arc<str>,
    pub path: Arc<str>,
    pub owner: Option<Arc<str>>,
    pub entry: usize,
    pub frame: Frame,
    pub fields: Vec<Field<'a>>,
    masked: Vec<bool>,
    pub active: bool,
    pub hosted: Option<Rc<[usize]>>,
    cache: Option<Vec<Variable>>,
}

impl<'a> Entity<'a> {
    pub fn sure(artifact: &EvalArtifact, columns: &Columns, entities: &[Entity], goals: &[Arc<str>]) -> Option<Vec<bool>> {
        let shredded: Vec<&Entity> = entities.iter().filter(|entity| entity.hosted.is_some()).collect();
        if shredded.is_empty() {
            return artifact.sure(columns, goals);
        }
        let parts: Vec<Vec<(&str, Column)>> = shredded
            .iter()
            .map(|entity| entity.fields.iter().map(|(name, leaf, _)| (name.as_str(), leaf.column())).collect())
            .collect();
        let items: Vec<Column> = parts
            .iter()
            .zip(&shredded)
            .map(|(fields, entity)| Column::new(Values::Struct { fields, len: entity.frame.children }))
            .collect();
        let lists: Vec<(usize, Column)> = items
            .iter()
            .zip(&shredded)
            .map(|(item, entity)| {
                (
                    entity.entry,
                    Column {
                        values: Values::List {
                            offsets: &entity.frame.offsets,
                            child: Dictionary::Column(item),
                        },
                        validity: entity.frame.validity.as_deref().map(|bits| (bits, 0)),
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
        if child.validity.is_some_and(|(bits, offset)| !Bits::full(bits, offset, len)) {
            return None;
        }
        let fields: Vec<Field<'a>> = fields.iter().map(|(name, column)| Self::input(Symbol::from(*name), Normalizer::lean(*column, len), len)).collect();
        let rows = offsets.len().saturating_sub(1);
        let validity = list.validity.filter(|(bits, offset)| !Bits::full(bits, *offset, rows));
        Some(Self {
            name,
            path,
            owner,
            entry,
            frame: Frame {
                offsets: Rc::from(offsets),
                validity: validity.map(|(bits, offset)| Rc::from(Bits::window(bits, offset, rows))),
                children: len,
            },
            masked: vec![false; fields.len()],
            fields,
            active: true,
            hosted: None,
            cache: None,
        })
    }

    pub fn shredded(name: Arc<str>, path: Arc<str>, owner: Option<Arc<str>>, entry: usize, shred: Shred) -> Self {
        let children = shred.frame.children;
        let fields: Vec<Field<'a>> = shred.fields.into_iter().map(|(name, array)| Self::input(Symbol::from(name.as_ref()), Leaf::typed(array), children)).collect();
        Self {
            name,
            path,
            owner,
            entry,
            frame: shred.frame,
            masked: vec![false; fields.len()],
            fields,
            active: true,
            hosted: Some(shred.hosted),
            cache: None,
        }
    }

    fn input(name: Symbol, leaf: Leaf<'a>, children: usize) -> Field<'a> {
        let mut bits = leaf.validity(children);
        Bits::trim(&mut bits, children);
        let full = bits.iter().map(|w| w.count_ones() as usize).sum::<usize>() == children;
        if !full {
            for child in 0..children {
                if !Bits::get(&bits, child) && !matches!(leaf.get(child), Variable::Null) {
                    Bits::set(&mut bits, child, true);
                }
            }
        }
        (name, leaf, bits.into())
    }

    pub fn field(&self, name: &str) -> Option<usize> {
        self.fields.iter().rposition(|(f, _, _)| f.as_str() == name)
    }

    pub fn flat(&self) -> bool {
        self.fields.iter().all(|(f, _, _)| !f.as_str().contains('.'))
    }

    pub fn overlaps(&self, name: &str) -> bool {
        let under = |long: &str, short: &str| long.strip_prefix(short).is_some_and(|rest| rest.starts_with('.'));
        self.fields.iter().any(|(f, _, _)| under(f.as_str(), name) || under(name, f.as_str()))
    }

    pub fn view(&self, field: usize) -> Leaf<'a> {
        let (_, leaf, present) = &self.fields[field];
        match self.masked[field] {
            true => Leaf::masked(leaf.clone(), present),
            false => leaf.clone(),
        }
    }

    fn object(&self, child: usize) -> Variable {
        let mut map = VariableMap::with_capacity(self.fields.len());
        let mut nested = Vec::new();
        for (name, leaf, present) in &self.fields {
            if Bits::get(present, child) {
                let value = match leaf.get(child) {
                    value @ (Variable::Object(_) | Variable::Array(_)) => value.depth_clone(usize::MAX),
                    other => other,
                };
                match name.as_str().contains('.') {
                    true => nested.push((name.as_str(), value)),
                    false => {
                        map.insert(name.clone(), value);
                    }
                }
            }
        }
        let object = Variable::from_object(map);
        for (name, value) in nested {
            object.dot_insert(name, value);
        }
        object
    }

    pub fn materialize(&mut self) {
        if self.cache.is_none() {
            self.cache = Some((0..self.frame.children).map(|child| self.object(child)).collect());
        }
    }

    pub fn cached(&self) -> bool {
        self.cache.is_some()
    }

    pub fn records(&self, rows: usize) -> Leaf<'a> {
        let offsets: Vec<i32> = self.frame.offsets.get(..=rows).map_or_else(Vec::new, <[i32]>::to_vec);
        let valid = match &self.frame.validity {
            Some(bits) => Bits::window(bits, 0, rows),
            None => Bits::ones(rows),
        };
        Leaf::Records(Rc::new(Records::new(rows, offsets, valid, self.fields.clone(), None)))
    }

    pub fn compose(&self, row: usize) -> Option<Variable> {
        let (a, b) = self.frame.range(row)?;
        Some(Variable::from_array(match &self.cache {
            Some(cache) => cache[a..b].iter().map(Variable::shallow_clone).collect(),
            None => (a..b).map(|child| self.object(child)).collect(),
        }))
    }

    pub fn write(&mut self, name: Arc<str>, leaf: Leaf<'a>, kids: &Rc<[usize]>) {
        let key = Symbol::from(name.as_ref());
        if let Some(cache) = &self.cache {
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
        let total = self.frame.children;
        let (leaf, full, bits) = match kids.len() == total {
            true => (leaf, true, Bits::ones(total)),
            false => {
                let mut bits = vec![0u64; total.div_ceil(64)];
                kids.iter().for_each(|&child| Bits::set(&mut bits, child, true));
                (Leaf::scattered(leaf, kids.clone(), total), false, bits)
            }
        };
        let fresh: Field<'a> = (key, leaf, bits.into());
        match (self.field(&name), full) {
            (None, _) => {
                self.fields.push(fresh);
                self.masked.push(!full);
            }
            (Some(at), true) => {
                self.fields[at] = fresh;
                self.masked[at] = false;
            }
            (Some(at), false) => {
                let (old, new) = (&self.fields[at], &fresh);
                let (new_column, old_column) = (new.1.column(), old.1.column());
                let mut builder = ColumnBuilder::with_capacity(total);
                let mut bits = vec![0u64; total.div_ceil(64)];
                for child in 0..total {
                    match (Bits::get(&new.2, child), Bits::get(&old.2, child)) {
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
                self.fields[at] = (fresh.0, Leaf::typed(builder.finish()), bits.into());
                self.masked[at] = true;
            }
        }
    }
}
