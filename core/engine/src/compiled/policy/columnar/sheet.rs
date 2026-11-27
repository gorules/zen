use crate::compiled::leaf::{Bits, ColumnBuilder, Dated, Leaf};
use crate::compiled::output::OutputColumn;
use crate::compiled::policy::entity::Entity;
use crate::compiled::policy::validator::{DateTexts, InputSchema};
use crate::compiled::shred::{Normalizer, Shredder};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns, Dictionary, Values};
use zen_expression::Variable;
use zen_types::path::DotPath;

pub(super) struct Entry<'a> {
    pub(super) path: Arc<str>,
    pub(super) leaf: Leaf<'a>,
    present: Option<Rc<[u64]>>,
    pub(super) input: bool,
    dead: bool,
}

pub(super) enum Bind {
    Column(usize),
    Absent,
    Row,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Slot {
    Entry(usize),
    Composed(usize),
}

pub(super) struct Units {
    pub(super) member: Option<usize>,
    pub(super) units: Rc<[usize]>,
    pub(super) owners: Rc<[usize]>,
}

impl<'a> Entry<'a> {
    pub(super) fn declared(&self) -> Option<Leaf<'a>> {
        self.leaf.declared()
    }
}

impl Units {
    pub(super) fn subset(&self, locals: &[usize]) -> Units {
        Units {
            member: self.member,
            units: locals.iter().map(|&local| self.units[local]).collect(),
            owners: locals.iter().map(|&local| self.owners[local]).collect(),
        }
    }
}

pub(super) struct Sheet<'a> {
    pub(super) rows: usize,
    pub(super) entries: Vec<Entry<'a>>,
    pub(super) inputs: Vec<Column<'a>>,
    pub(super) entities: Vec<Entity<'a>>,
    pub(super) refs: Vec<Arc<str>>,
    order: Vec<usize>,
}

impl<'a> Sheet<'a> {
    pub(super) fn new(columns: &Columns<'a>) -> Option<Self> {
        let rows = columns.rows;
        let mut entries = Vec::with_capacity(columns.columns.len());
        let mut inputs = Vec::with_capacity(columns.columns.len());
        for (path, column) in &columns.columns {
            inputs.push(*column);
            if path.is_empty() || path.contains('[') {
                return None;
            }
            let present = Bits::validity(column.validity, rows).map(Rc::from);
            entries.push(Entry {
                path: Arc::from(*path),
                leaf: Normalizer::lean(*column, rows),
                present,
                input: true,
                dead: false,
            });
        }
        Some(Self {
            rows,
            order: (0..entries.len()).collect(),
            entries,
            inputs,
            entities: Vec::new(),
            refs: Vec::new(),
        })
    }

    pub(super) fn dates(&mut self, schema: &InputSchema, tz: zen_expression::context::Timezone) {
        if !schema.has_dates() {
            return;
        }
        let texts = Rc::new(DateTexts::remembered(tz));
        let owned: Vec<usize> = self
            .entities
            .iter()
            .filter(|entity| entity.active)
            .map(|entity| entity.entry)
            .collect();
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if !entry.input || owned.contains(&index) {
                continue;
            }
            if let Some(at) = schema.dates_at(&entry.path) {
                entry.leaf = Dated::leaf(entry.leaf.clone(), self.rows, schema.dating(at, &texts));
            }
        }
        for entity in self.entities.iter_mut() {
            let children = entity.frame.children;
            for field in entity.fields.iter_mut().filter(|field| field.input) {
                if let Some(at) = schema.dates_at(&format!("{}.{}", entity.path, field.name)) {
                    field.leaf =
                        Dated::leaf(field.leaf.clone(), children, schema.dating(at, &texts));
                }
            }
        }
    }

    pub(super) fn dated(&self) -> Vec<bool> {
        let mut dated = vec![false; self.rows];
        for entry in &self.entries {
            if let Leaf::Dated(leaf) = &entry.leaf {
                let marks = leaf.marks();
                dated
                    .iter_mut()
                    .enumerate()
                    .for_each(|(row, dated)| *dated |= Bits::get(marks, row));
            }
        }
        for entity in &self.entities {
            for field in &entity.fields {
                let Leaf::Dated(leaf) = &field.leaf else {
                    continue;
                };
                let marks = leaf.marks();
                for (row, dated) in dated.iter_mut().enumerate() {
                    if let Some((start, end)) = entity.frame.range(row) {
                        *dated |= (start..end).any(|child| Bits::get(marks, child));
                    }
                }
            }
        }
        dated
    }

    pub(super) fn related(a: &str, b: &str) -> Option<std::cmp::Ordering> {
        match (a == b, DotPath::under(a, b), DotPath::under(b, a)) {
            (true, _, _) => Some(std::cmp::Ordering::Equal),
            (_, true, _) => Some(std::cmp::Ordering::Greater),
            (_, _, true) => Some(std::cmp::Ordering::Less),
            _ => None,
        }
    }

    pub(super) fn bind(&self, key: &str, positions: &[usize], shared: bool) -> Bind {
        if self
            .refs
            .iter()
            .any(|path| Self::related(path, key).is_some())
        {
            return Bind::Row;
        }
        let Some((index, entry)) = self
            .entries
            .iter()
            .enumerate()
            .rev()
            .find(|(_, entry)| !entry.dead && Self::related(&entry.path, key).is_some())
        else {
            return Bind::Absent;
        };
        let covered = entry.input
            || entry
                .present
                .as_ref()
                .is_none_or(|present| positions.iter().all(|&row| Bits::get(present, row)));
        match (
            Self::related(&entry.path, key),
            covered,
            shared && Self::composite(&entry.leaf),
        ) {
            (Some(std::cmp::Ordering::Equal), true, false) => Bind::Column(index),
            _ => Bind::Row,
        }
    }

    fn composite(leaf: &Leaf) -> bool {
        match leaf {
            Leaf::Any(values) => values
                .iter()
                .any(|v| matches!(v, Variable::Object(_) | Variable::Array(_))),
            _ => !matches!(
                leaf.column().values,
                Values::Scaled { .. }
                    | Values::I64(_)
                    | Values::Dec(_)
                    | Values::F64(_)
                    | Values::Bool { .. }
                    | Values::Utf8 { .. }
                    | Values::Text { .. }
                    | Values::LargeUtf8 { .. }
                    | Values::Strs(_)
                    | Values::Dict {
                        values: Dictionary::Scaled { .. }
                            | Dictionary::Text { .. }
                            | Dictionary::Bool { .. },
                        ..
                    }
            ),
        }
    }

    pub(super) fn resolve(
        &mut self,
        keys: &[Arc<str>],
        fields: Vec<(Arc<str>, Leaf<'a>, Option<Rc<[u64]>>)>,
    ) {
        for entry in self.entries.iter_mut() {
            if keys.iter().any(|key| *key == entry.path) {
                entry.dead = true;
            }
        }
        for (path, leaf, present) in fields {
            self.push(
                Entry {
                    path,
                    leaf,
                    present,
                    input: true,
                    dead: false,
                },
                None,
            );
        }
    }

    pub(super) fn present(entry: &Entry, row: usize) -> bool {
        entry
            .present
            .as_ref()
            .is_none_or(|present| Bits::get(present, row))
    }

    pub(super) fn relink(&mut self, pool: usize) {
        for at in 0..self.entities.len() {
            let Some(index) = self.entities[at]
                .link
                .as_ref()
                .filter(|(source, _)| *source == pool)
                .map(|(_, index)| index.clone())
            else {
                continue;
            };
            let fields = self.entities[pool].gathered(&index);
            self.entities[at].relinked(fields);
        }
    }

    pub(super) fn tied(&self, at: usize) -> bool {
        self.entities.get(at).is_some_and(|entity| {
            entity.active
                && (entity.link.is_some()
                    || self.entities.iter().any(|other| {
                        other.active && other.link.as_ref().is_some_and(|(pool, _)| *pool == at)
                    }))
        })
    }

    pub(super) fn live(entry: &Entry) -> bool {
        !entry.dead
    }

    pub(super) fn solid(entry: &Entry) -> bool {
        entry.present.is_none()
    }

    fn active(&self, index: usize) -> Option<&Entity<'a>> {
        self.entities
            .iter()
            .find(|entity| entity.active && entity.entry == index)
    }

    pub(super) fn retire(&mut self, at: usize) {
        let Some(entity) = self.entities.get(at).filter(|entity| entity.active) else {
            return;
        };
        let mut values = vec![Variable::Null; self.rows];
        let mut present = vec![0u64; self.rows.div_ceil(64)];
        for (row, value) in values.iter_mut().enumerate() {
            if let Some(composed) = entity.compose(row) {
                *value = composed;
                Bits::set(&mut present, row, true);
            }
        }
        let path = entity.path.clone();
        if crate::compiled::census::Census::on() {
            crate::compiled::census::Census::record(
                "policy rows | entity composed per row".into(),
                self.rows,
                0,
            );
        }
        self.entities[at].active = false;
        self.hosted(path, values, present);
    }

    pub(super) fn object(&self, row: usize) -> Variable {
        let object = Variable::empty_object();
        for &index in &self.order {
            let entry = &self.entries[index];
            if entry.dead {
                continue;
            }
            if let Some(entity) = self.active(index) {
                if let Some(composed) = entity.compose(row) {
                    object.dot_insert(&entry.path, composed);
                }
                continue;
            }
            let value = entry.leaf.get(row);
            let keep = match entry.input {
                true => !matches!(value, Variable::Null) || Self::present(entry, row),
                false => Self::present(entry, row),
            };
            if keep {
                object.dot_insert(&entry.path, value.depth_clone(usize::MAX));
            }
        }
        object
    }

    pub(super) fn write(&mut self, path: Arc<str>, leaf: Leaf<'a>, positions: &[usize]) {
        let present: Option<Rc<[u64]>> = match positions.len() == self.rows {
            true => None,
            false => {
                let mut bits = vec![0u64; self.rows.div_ceil(64)];
                positions
                    .iter()
                    .for_each(|&row| Bits::set(&mut bits, row, true));
                Some(bits.into())
            }
        };
        let leaf = match present {
            None => leaf,
            Some(_) => Leaf::scattered(leaf, positions.iter().copied().collect(), self.rows),
        };
        let previous = self
            .entries
            .iter()
            .rposition(|entry| !entry.dead && Self::related(&entry.path, &path).is_some())
            .filter(|&at| {
                !self.entries[at].input
                    && self.entries[at].path == path
                    && self.entries[at].present.is_some()
            });
        let (leaf, present) = match (previous, &present) {
            (Some(at), Some(bits)) => {
                self.entries[at].dead = true;
                let old = &self.entries[at];
                let (fresh, stale) = (leaf.column(), old.leaf.column());
                let mut builder = ColumnBuilder::with_capacity(self.rows);
                let mut union = vec![0u64; self.rows.div_ceil(64)];
                for row in 0..self.rows {
                    match (Bits::get(bits, row), Self::present(old, row)) {
                        (true, _) => {
                            builder.push_cell(&fresh, row);
                            Bits::set(&mut union, row, true);
                        }
                        (false, true) => {
                            builder.push_cell(&stale, row);
                            Bits::set(&mut union, row, true);
                        }
                        (false, false) => builder.push_null(),
                    }
                }
                let full =
                    union.iter().map(|w| w.count_ones() as usize).sum::<usize>() == self.rows;
                let union: Rc<[u64]> = union.into();
                (Leaf::typed(builder.finish()), (!full).then_some(union))
            }
            _ => (leaf, present),
        };
        self.push(
            Entry {
                path,
                leaf,
                present,
                input: false,
                dead: false,
            },
            previous.filter(|&at| self.entries[at].dead),
        );
    }

    fn push(&mut self, entry: Entry<'a>, replaces: Option<usize>) {
        let index = self.entries.len();
        self.entries.push(entry);
        match replaces.and_then(|at| self.order.iter().position(|&slot| slot == at)) {
            Some(slot) => self.order[slot] = index,
            None => self.order.push(index),
        }
    }

    pub(super) fn hosted(&mut self, path: Arc<str>, values: Vec<Variable>, present: Vec<u64>) {
        let full = present
            .iter()
            .map(|w| w.count_ones() as usize)
            .sum::<usize>()
            == self.rows;
        self.push(
            Entry {
                path,
                leaf: Leaf::Any(values.into()),
                present: (!full).then(|| present.into()),
                input: false,
                dead: false,
            },
            None,
        );
    }

    pub(super) fn output(&self, finished: &[usize]) -> Vec<(Arc<str>, OutputColumn<'a>)> {
        let nested = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.dead)
            .any(|(i, a)| {
                self.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| !b.dead)
                    .any(|(j, b)| {
                        i != j
                            && matches!(
                                Self::related(&a.path, &b.path),
                                Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                            )
                    })
            });
        match nested {
            false => self
                .order
                .iter()
                .map(|&index| (index, &self.entries[index]))
                .filter(|(_, entry)| !entry.dead)
                .map(|(index, entry)| {
                    if let Some(entity) = self.active(index) {
                        return (entry.path.clone(), OutputColumn(entity.records(self.rows)));
                    }
                    let leaf = match &entry.present {
                        Some(present) => Leaf::masked(entry.leaf.clone(), present),
                        None => entry.leaf.clone(),
                    };
                    (entry.path.clone(), OutputColumn(leaf))
                })
                .collect(),
            true => {
                if crate::compiled::census::Census::on() {
                    crate::compiled::census::Census::record(
                        "policy rows | output assembled per row".into(),
                        finished.len(),
                        0,
                    );
                }
                let mut shredder = Shredder::new(self.rows);
                for &row in finished {
                    shredder.visit(&self.object(row), 0, row);
                }
                shredder.output()
            }
        }
    }
}
