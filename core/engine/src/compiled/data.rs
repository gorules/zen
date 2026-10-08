use crate::compiled::typed::{Bits, ColumnBuilder, Leaf};
use crate::decision_graph::walker::GraphWalker;
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::Binding;
use zen_expression::Variable;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;

pub(crate) type Col = Rc<[Variable]>;
type Parts<'a> = (Option<Col>, Vec<Layer<'a>>, Vec<(Arc<str>, Leaf<'a>)>);

#[derive(Clone, Debug)]
pub(crate) enum Mask<'a> {
    Bits(Rc<[u64]>),
    Valid(Leaf<'a>),
    Picked(Rc<[u64]>, Rc<[usize]>),
}

impl<'a> Mask<'a> {
    pub fn get(&self, row: usize) -> bool {
        match self {
            Mask::Bits(bits) => Bits::get(bits, row),
            Mask::Valid(leaf) => leaf.valid_at(row),
            Mask::Picked(bits, positions) => positions.get(row).is_some_and(|&at| Bits::get(bits, at)),
        }
    }

    pub fn pick(&self, positions: &Rc<[usize]>) -> Mask<'a> {
        match self {
            Mask::Bits(bits) => Mask::Picked(bits.clone(), positions.clone()),
            Mask::Valid(leaf) => Mask::Valid(leaf.pick(positions)),
            Mask::Picked(bits, inner) => Mask::Picked(bits.clone(), positions.iter().map(|&i| inner[i]).collect()),
        }
    }

    pub fn dense(&self, rows: usize) -> Rc<[u64]> {
        match self {
            Mask::Bits(bits) => bits.clone(),
            Mask::Valid(leaf) => leaf.validity(rows).into(),
            Mask::Picked(..) => Bits::of(rows, |row| self.get(row)).into(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Layer<'a> {
    pub paths: Arc<[Arc<str>]>,
    keys: Rc<[Symbol]>,
    segments: Rc<[Box<[Symbol]>]>,
    pub columns: Rc<[Leaf<'a>]>,
    pub present: Option<Rc<[Mask<'a>]>>,
    pub flat: bool,
}

impl<'a> Layer<'a> {
    pub fn new(paths: Arc<[Arc<str>]>, columns: Vec<Leaf<'a>>, present: Option<Vec<Mask<'a>>>) -> Layer<'a> {
        let flat = paths.iter().all(|p| !p.contains('.'));
        let keys = paths.iter().map(|p| Symbol::from(p.as_ref())).collect();
        let segments = paths.iter().map(|p| p.split('.').map(Symbol::from).collect()).collect();
        Layer {
            paths,
            keys,
            segments,
            columns: columns.into(),
            present: present.map(Rc::from),
            flat,
        }
    }

    fn has(&self, index: usize, row: usize) -> bool {
        self.present.as_ref().is_none_or(|p| p[index].get(row))
    }

    fn object(&self, row: usize) -> Variable {
        match self.flat {
            true => {
                let mut map = VariableMap::with_capacity(self.keys.len());
                for (i, (key, column)) in self.keys.iter().zip(self.columns.iter()).enumerate() {
                    if self.has(i, row) {
                        map.insert(key.clone(), column.get(row));
                    }
                }
                Variable::from_object(map)
            }
            false => {
                let object = Variable::empty_object();
                for (i, (segments, column)) in self.segments.iter().zip(self.columns.iter()).enumerate() {
                    if self.has(i, row) {
                        Self::insert(&object, segments, column.get(row));
                    }
                }
                object
            }
        }
    }

    fn insert(target: &Variable, segments: &[Symbol], value: Variable) {
        let Some((last, head)) = segments.split_last() else {
            return;
        };
        let mut current = target.shallow_clone();
        for part in head {
            let Variable::Object(object) = &current else {
                return;
            };
            let existing = object.borrow().get(part).map(Variable::shallow_clone);
            let next = match existing {
                Some(existing) => existing,
                None => {
                    let created = Variable::empty_object();
                    object.borrow_mut().insert(part.clone(), created.shallow_clone());
                    created
                }
            };
            current = next;
        }
        if let Variable::Object(object) = current {
            object.borrow_mut().insert(last.clone(), value);
        }
    }

    fn put(current: &mut VariableMap, key: &Symbol, value: &Variable) {
        match value {
            Variable::Null => {
                current.remove(key);
            }
            Variable::Object(_) => {
                let merged = match current.get(key) {
                    Some(existing @ Variable::Object(_)) => existing.clone().merge_clone(value),
                    _ => value.clone(),
                };
                current.insert(key.clone(), merged);
            }
            _ => {
                current.insert(key.clone(), value.clone());
            }
        }
    }

    fn under(&self, top: &str) -> Vec<usize> {
        self.paths
            .iter()
            .enumerate()
            .filter(|(_, path)| path.as_ref() == top || path.strip_prefix(top).is_some_and(|r| r.starts_with('.')))
            .map(|(i, _)| i)
            .collect()
    }

    fn object_key(&self, row: usize, top: &str, under: &[usize]) -> Option<Variable> {
        if let [index] = under {
            if self.paths[*index].as_ref() == top {
                return self.has(*index, row).then(|| self.columns[*index].get(row));
            }
        }
        let object = Variable::empty_object();
        let mut any = false;
        for &i in under {
            if self.has(i, row) {
                Self::insert(&object, &self.segments[i], self.columns[i].get(row));
                any = true;
            }
        }
        match any {
            true => object.as_object().and_then(|m| m.borrow().get_str(top).cloned()),
            false => None,
        }
    }

    fn apply(&self, row: usize, current: &mut VariableMap) {
        match self.flat {
            true => {
                for (i, (key, column)) in self.keys.iter().zip(self.columns.iter()).enumerate() {
                    if self.has(i, row) {
                        Self::put(current, key, &column.get(row));
                    }
                }
            }
            false => {
                if let Variable::Object(map) = self.object(row) {
                    for (key, value) in map.borrow().iter() {
                        Self::put(current, key, value);
                    }
                }
            }
        }
    }

    fn gather(&self, positions: &Rc<[usize]>) -> Layer<'a> {
        Layer {
            paths: self.paths.clone(),
            keys: self.keys.clone(),
            segments: self.segments.clone(),
            columns: self.columns.iter().map(|c| c.pick(positions)).collect(),
            present: self.present.as_ref().map(|masks| masks.iter().map(|m| m.pick(positions)).collect()),
            flat: self.flat,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Presence<'a> {
    Valued,
    All,
    Mask(Mask<'a>),
}

impl<'a> Presence<'a> {
    fn at(&self, leaf: &Leaf, row: usize) -> bool {
        match self {
            Presence::Valued => !leaf.null(row),
            Presence::All => true,
            Presence::Mask(mask) => mask.get(row),
        }
    }

    fn pick(&self, positions: &Rc<[usize]>) -> Presence<'a> {
        match self {
            Presence::Mask(mask) => Presence::Mask(mask.pick(positions)),
            other => other.clone(),
        }
    }

    fn mask(&self, leaf: &Leaf, rows: usize) -> Rc<[u64]> {
        match self {
            Presence::Mask(mask) => mask.dense(rows),
            Presence::All => Bits::ones(rows).into(),
            Presence::Valued => leaf.valued(rows).into(),
        }
    }
}

#[derive(Clone)]
pub(crate) enum Shape<'a> {
    Plain(Col),
    Patched {
        base: Option<Col>,
        layers: Rc<[Layer<'a>]>,
        leaves: Rc<[(Arc<str>, Leaf<'a>)]>,
        nulls: Option<Rc<[u64]>>,
        presence: Option<Rc<[Presence<'a>]>>,
    },
}

pub(crate) enum Read {
    Leaf(usize),
    Base,
    Whole,
}

pub(crate) struct Data<'a> {
    pub rows: Rc<[usize]>,
    pub shape: Shape<'a>,
    whole: OnceCell<Col>,
    tops: RefCell<Vec<(Arc<str>, Col)>>,
}

impl<'a> Data<'a> {
    pub fn plain(rows: Rc<[usize]>, values: Col) -> Data<'a> {
        Data {
            rows,
            shape: Shape::Plain(values),
            whole: OnceCell::new(),
            tops: RefCell::new(Vec::new()),
        }
    }

    fn with_shape(rows: Rc<[usize]>, shape: Shape<'a>) -> Data<'a> {
        Data {
            rows,
            shape,
            whole: OnceCell::new(),
            tops: RefCell::new(Vec::new()),
        }
    }

    pub fn pick(column: &Col, positions: &[usize]) -> Col {
        positions.iter().map(|&i| column[i].clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn gather(pieces: &[Rc<Data<'a>>], rows: &Rc<[usize]>) -> Data<'a> {
        if let [single] = pieces {
            if Rc::ptr_eq(&single.rows, rows) || single.rows == *rows {
                return Data::with_shape(rows.clone(), single.shape.clone());
            }
        }
        let span = pieces
            .iter()
            .filter_map(|piece| piece.rows.last())
            .chain(rows.last())
            .max()
            .map_or(0, |m| m + 1);
        let mut table: Vec<(usize, usize)> = vec![(usize::MAX, 0); span];
        for (p, piece) in pieces.iter().enumerate() {
            for (i, &row) in piece.rows.iter().enumerate() {
                if table[row].0 == usize::MAX {
                    table[row] = (p, i);
                }
            }
        }
        let located: Vec<(usize, usize)> = rows.iter().map(|&row| table[row]).collect();
        let single = located.iter().all(|(p, _)| *p == located.first().map_or(0, |l| l.0));
        match (single, located.first()) {
            (true, Some(&(piece, _))) if piece != usize::MAX => {
                let positions: Vec<usize> = located.iter().map(|(_, i)| *i).collect();
                Data::with_shape(rows.clone(), pieces[piece].shape_at(&positions))
            }
            _ => Self::concat(pieces, &located, rows).unwrap_or_else(|| {
                let values: Col = located
                    .iter()
                    .map(|&(p, i)| match pieces.get(p) {
                        Some(piece) => piece.materialize_row(i),
                        None => Variable::Null,
                    })
                    .collect();
                Data::plain(rows.clone(), values)
            }),
        }
    }

    fn concat(pieces: &[Rc<Data<'a>>], located: &[(usize, usize)], rows: &Rc<[usize]>) -> Option<Data<'a>> {
        let shapes = pieces
            .iter()
            .map(|piece| match &piece.shape {
                Shape::Patched {
                    base: None,
                    leaves,
                    presence: Some(presence),
                    nulls,
                    ..
                } if piece.patchable() => Some((leaves, presence, nulls)),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let (first, _, _) = shapes.first()?;
        let same = shapes.iter().all(|(leaves, _, _)| {
            leaves.len() == first.len() && leaves.iter().zip(first.iter()).all(|((a, _), (b, _))| a == b)
        });
        if !same {
            return None;
        }
        let count = located.len();
        let nulls = Bits::of(count, |row| {
            let (p, i) = located[row];
            match shapes.get(p) {
                Some((_, _, nulls)) => nulls.as_ref().is_some_and(|n| Bits::get(n, i)),
                None => true,
            }
        });
        let mut columns = Vec::with_capacity(first.len());
        let mut present = Vec::with_capacity(first.len());
        let shared: Rc<[(usize, usize)]> = located.iter().copied().collect();
        for k in 0..first.len() {
            let mask = Bits::of(count, |row| {
                let (p, i) = located[row];
                shapes.get(p).is_some_and(|(leaves, presence, _)| presence[k].at(&leaves[k].1, i))
            });
            let sources = shapes.iter().map(|(leaves, _, _)| leaves[k].1.clone()).collect();
            columns.push(Leaf::stitched(sources, shared.clone()));
            present.push(Mask::Bits(mask.into()));
        }
        let layer = Layer::new(first.iter().map(|(path, _)| path.clone()).collect(), columns, Some(present));
        let nulls = (!Bits::none(&nulls)).then(|| Rc::from(nulls));
        Some(Data::record(rows, layer, nulls))
    }

    fn shape_at(&self, positions: &[usize]) -> Shape<'a> {
        let positions: Rc<[usize]> = positions.into();
        let positions = &positions;
        match &self.shape {
            Shape::Plain(values) => Shape::Plain(Self::pick(values, positions)),
            Shape::Patched {
                base,
                layers,
                leaves,
                nulls,
                presence,
            } => Shape::Patched {
                base: base.as_ref().map(|b| Self::pick(b, positions)),
                layers: layers.iter().map(|l| l.gather(positions)).collect(),
                leaves: leaves
                    .iter()
                    .map(|(p, c)| (p.clone(), c.pick(positions)))
                    .collect(),
                nulls: nulls.as_ref().map(|n| Bits::gather(n, positions).into()),
                presence: presence.as_ref().map(|p| p.iter().map(|x| x.pick(positions)).collect()),
            },
        }
    }

    fn nulled(&self, row: usize) -> bool {
        matches!(&self.shape, Shape::Patched { nulls: Some(nulls), .. } if Bits::get(nulls, row))
    }

    pub fn materialize_row(&self, row: usize) -> Variable {
        if let Some(whole) = self.whole.get() {
            return whole[row].clone();
        }
        if self.nulled(row) {
            return Variable::Null;
        }
        match &self.shape {
            Shape::Plain(values) => values[row].clone(),
            Shape::Patched { base, layers, .. } => {
                let mut layers = layers.iter();
                let mut current = match base.as_ref().map(|b| &b[row]) {
                    Some(Variable::Object(map)) => map.borrow().clone(),
                    _ => match layers.next() {
                        Some(first) => match first.object(row) {
                            Variable::Object(map) => map.borrow().clone(),
                            _ => VariableMap::default(),
                        },
                        None => VariableMap::default(),
                    },
                };
                current.reserve(layers.as_slice().iter().map(|l| l.keys.len()).sum());
                for layer in layers {
                    layer.apply(row, &mut current);
                }
                Variable::from_object(current)
            }
        }
    }

    pub fn partial(&self, path: &str) -> Leaf<'a> {
        Leaf::Any(self.subtree(path))
    }

    pub fn subtree(&self, path: &str) -> Col {
        let top = path.split('.').next().unwrap_or(path);
        let rest = path.strip_prefix(top).and_then(|r| r.strip_prefix('.'));
        let column = self.top(top);
        match rest {
            None => column,
            Some(rest) => column
                .iter()
                .map(|value| Self::lookup(value, rest).unwrap_or(Variable::Null))
                .collect(),
        }
    }

    fn top(&self, top: &str) -> Col {
        if let Some((_, column)) = self.tops.borrow().iter().find(|(key, _)| key.as_ref() == top) {
            return column.clone();
        }
        let column: Col = match (self.whole.get(), &self.shape) {
            (Some(whole), _) => whole.iter().map(|v| Self::lookup(v, top).unwrap_or(Variable::Null)).collect(),
            (None, Shape::Plain(values)) => values.iter().map(|v| Self::lookup(v, top).unwrap_or(Variable::Null)).collect(),
            (None, Shape::Patched { layers, .. }) => {
                let under: Vec<Vec<usize>> = layers.iter().map(|layer| layer.under(top)).collect();
                (0..self.len())
                    .map(|row| self.materialize_key(row, top, &under).unwrap_or(Variable::Null))
                    .collect()
            }
        };
        self.tops.borrow_mut().push((Arc::from(top), column.clone()));
        column
    }

    fn materialize_key(&self, row: usize, top: &str, under: &[Vec<usize>]) -> Option<Variable> {
        if self.nulled(row) {
            return None;
        }
        match &self.shape {
            Shape::Plain(values) => Self::lookup(&values[row], top),
            Shape::Patched { base, layers, .. } => {
                let mut layers = layers.iter().zip(under);
                let mut current = match base.as_ref().map(|b| &b[row]) {
                    Some(value @ Variable::Object(_)) => Self::lookup(value, top),
                    _ => layers.next().and_then(|(first, under)| first.object_key(row, top, under)),
                };
                for (layer, under) in layers {
                    if under.is_empty() {
                        continue;
                    }
                    let Some(value) = layer.object_key(row, top, under) else {
                        continue;
                    };
                    current = match value {
                        Variable::Null => None,
                        value @ Variable::Object(_) => match current {
                            Some(mut existing @ Variable::Object(_)) => Some(existing.merge_clone(&value)),
                            _ => Some(value),
                        },
                        value => Some(value),
                    };
                }
                current
            }
        }
    }

    pub fn materialized(&self) -> Col {
        self.whole
            .get_or_init(|| (0..self.len()).map(|r| self.materialize_row(r)).collect())
            .clone()
    }

    fn read(leaves: &[(Arc<str>, Leaf)], path: &str) -> Read {
        let overlaps = |leaf: &str| {
            leaf.strip_prefix(path).is_some_and(|r| r.starts_with('.'))
                || path.strip_prefix(leaf).is_some_and(|r| r.starts_with('.'))
        };
        if leaves.iter().any(|(leaf, _)| overlaps(leaf)) {
            return Read::Whole;
        }
        match leaves.iter().rposition(|(leaf, _)| leaf.as_ref() == path) {
            Some(index) => Read::Leaf(index),
            None => Read::Base,
        }
    }

    pub fn lookup(value: &Variable, path: &str) -> Option<Variable> {
        path.split('.').try_fold(value.clone(), |current, key| match current {
            Variable::Object(map) => map.borrow().get_str(key).cloned(),
            _ => None,
        })
    }

    fn resolve(&self, path: &str, row: usize) -> Variable {
        match &self.shape {
            Shape::Plain(values) => Self::lookup(&values[row], path).unwrap_or(Variable::Null),
            Shape::Patched { base, leaves, .. } => match Self::read(leaves, path) {
                Read::Leaf(index) => leaves[index].1.get(row),
                Read::Base => base
                    .as_ref()
                    .and_then(|b| Self::lookup(&b[row], path))
                    .unwrap_or(Variable::Null),
                Read::Whole => Self::lookup(&self.materialize_row(row), path).unwrap_or(Variable::Null),
            },
        }
    }

    pub fn merged(rows: &Rc<[usize]>, parents: Vec<Data<'a>>) -> Data<'a> {
        match parents.len() {
            0 => Data::plain(rows.clone(), (0..rows.len()).map(|_| Variable::empty_object()).collect()),
            1 => {
                let Some(single) = parents.into_iter().next() else {
                    return Data::plain(rows.clone(), Rc::from(Vec::new()));
                };
                match &single.shape {
                    Shape::Plain(values) => {
                        let containers = values
                            .iter()
                            .all(|v| matches!(v, Variable::Object(_) | Variable::Array(_)));
                        match containers {
                            true => single,
                            false => Data::plain(
                                rows.clone(),
                                values
                                    .iter()
                                    .map(|v| match v {
                                        Variable::Object(_) | Variable::Array(_) => v.clone(),
                                        _ => Variable::empty_object(),
                                    })
                                    .collect(),
                            ),
                        }
                    }
                    Shape::Patched { nulls: None, .. } => single,
                    Shape::Patched {
                        base,
                        layers,
                        leaves,
                        presence,
                        ..
                    } => Data::with_shape(
                        rows.clone(),
                        Shape::Patched {
                            base: base.clone(),
                            layers: layers.clone(),
                            leaves: leaves.clone(),
                            nulls: None,
                            presence: presence.clone(),
                        },
                    ),
                }
            }
            _ if parents.iter().all(Data::patchable) => {
                let mut parents = parents.into_iter();
                let mut head = match parents.next() {
                    Some(first) => Data::merged(rows, vec![first]),
                    None => Data::empty(rows),
                };
                for parent in parents {
                    if let Some(layer) = parent.patch() {
                        head = head.layered(layer);
                    }
                }
                head
            }
            _ => {
                let materialized: Vec<Col> = parents.iter().map(Data::materialized).collect();
                let values: Col = (0..rows.len())
                    .map(|row| GraphWalker::merge_values(materialized.iter().map(|m| &m[row])))
                    .collect();
                Data::plain(rows.clone(), values)
            }
        }
    }

    pub fn empty(rows: &Rc<[usize]>) -> Data<'a> {
        Data::with_shape(
            rows.clone(),
            Shape::Patched {
                base: None,
                layers: Rc::from(Vec::new()),
                leaves: Rc::from(Vec::new()),
                nulls: None,
                presence: Some(Rc::from(Vec::new())),
            },
        )
    }

    pub fn patchable(&self) -> bool {
        match &self.shape {
            Shape::Patched {
                base: None,
                leaves,
                presence: Some(_),
                ..
            } => !leaves.iter().any(|(a, _)| {
                leaves
                    .iter()
                    .any(|(b, _)| b.strip_prefix(a.as_ref()).is_some_and(|rest| rest.starts_with('.')))
            }),
            _ => false,
        }
    }

    pub fn patch(&self) -> Option<Layer<'a>> {
        let Shape::Patched {
            base: None,
            leaves,
            presence: Some(presence),
            nulls,
            ..
        } = &self.shape
        else {
            return None;
        };
        let rows = self.len();
        let masks: Vec<Mask<'a>> = leaves
            .iter()
            .zip(presence.iter())
            .map(|((_, leaf), p)| {
                let mask = p.mask(leaf, rows);
                Mask::Bits(match nulls {
                    Some(nulls) => mask.iter().zip(nulls.iter()).map(|(m, n)| m & !n).collect(),
                    None => mask,
                })
            })
            .collect();
        Some(Layer::new(
            leaves.iter().map(|(path, _)| path.clone()).collect(),
            leaves.iter().map(|(_, leaf)| leaf.clone()).collect(),
            Some(masks),
        ))
    }

    pub fn record(rows: &Rc<[usize]>, layer: Layer<'a>, nulls: Option<Rc<[u64]>>) -> Data<'a> {
        let leaves: Rc<[(Arc<str>, Leaf)]> = layer
            .paths
            .iter()
            .cloned()
            .zip(layer.columns.iter().cloned())
            .collect();
        let count = rows.len();
        let presence: Rc<[Presence]> = (0..layer.paths.len())
            .map(|index| match (&layer.present, &nulls) {
                (None, None) => Presence::All,
                (Some(masks), None) => Presence::Mask(masks[index].clone()),
                (present, Some(nulls)) => Presence::Mask(Mask::Bits(match present {
                    Some(masks) => masks[index].dense(count).iter().zip(nulls.iter()).map(|(p, n)| p & !n).collect(),
                    None => {
                        let mut bits: Vec<u64> = nulls.iter().map(|n| !n).collect();
                        Bits::trim(&mut bits, count);
                        bits.into()
                    }
                })),
            })
            .collect();
        Data::with_shape(
            rows.clone(),
            Shape::Patched {
                base: None,
                layers: Rc::from(vec![layer]),
                leaves,
                nulls,
                presence: Some(presence),
            },
        )
    }

    pub fn layered(&self, layer: Layer<'a>) -> Data<'a> {
        let rows = &self.rows;
        let count = rows.len();
        let mut presence: Option<Vec<Presence<'a>>> = match &self.shape {
            Shape::Patched {
                base: None,
                presence: Some(p),
                ..
            } => Some(p.to_vec()),
            _ => None,
        };
        let (base, layers, leaves): Parts<'a> = match &self.shape {
            Shape::Plain(values) => {
                let objects = values.iter().all(|v| matches!(v, Variable::Object(_)));
                match objects {
                    true => (Some(values.clone()), Vec::new(), Vec::new()),
                    false => {
                        let merged: Col = (0..count)
                            .map(|row| values[row].clone().merge_clone(&layer.object(row)))
                            .collect();
                        return Data::plain(rows.clone(), merged);
                    }
                }
            }
            Shape::Patched {
                base,
                layers,
                leaves,
                ..
            } => (base.clone(), layers.to_vec(), leaves.to_vec()),
        };
        let mut next = leaves;
        for (index, (path, column)) in layer.paths.iter().zip(layer.columns.iter()).enumerate() {
            let valued = match layer.present.is_some() || base.is_none() {
                true => column.valued(count),
                false => Vec::new(),
            };
            let removed = base.is_none() && layer.present.is_none() && Bits::none(&valued);
            if removed {
                let gone = |leaf: &Arc<str>| {
                    leaf == path || leaf.strip_prefix(path.as_ref()).is_some_and(|r| r.starts_with('.'))
                };
                let keep: Vec<bool> = next.iter().map(|(leaf, _)| !gone(leaf)).collect();
                if let Some(tracked) = presence.as_mut() {
                    let mut flags = keep.iter();
                    tracked.retain(|_| flags.next().copied().unwrap_or(true));
                }
                let mut flags = keep.iter();
                next.retain(|_| flags.next().copied().unwrap_or(true));
                continue;
            }
            let fresh = base.is_none()
                && !next.iter().any(|(leaf, _)| {
                    leaf == path
                        || leaf.strip_prefix(path.as_ref()).is_some_and(|r| r.starts_with('.'))
                        || path.strip_prefix(leaf.as_ref()).is_some_and(|r| r.starts_with('.'))
                });
            let objects = column.objects();
            let needs = !fresh && (layer.present.is_some() || objects);
            let exact = match (needs, objects) {
                (true, false) => match Self::read(self.leaves(), path) {
                    Read::Leaf(at) => self.leaves().get(at).map(|(_, leaf)| leaf),
                    _ => None,
                },
                _ => None,
            };
            let has = |row: usize| layer.has(index, row);
            let (resolved, valued): (Leaf<'a>, Vec<u64>) = match (needs, exact) {
                (false, _) => (column.clone(), valued),
                (true, Some(old)) => {
                    let mut builder = ColumnBuilder::with_capacity(count);
                    let (fresh, before) = (column.column(), old.column());
                    for row in 0..count {
                        match has(row) {
                            true => builder.push_cell(&fresh, row),
                            false => builder.push_cell(&before, row),
                        }
                    }
                    let leaf = Leaf::typed(builder.finish());
                    let valued = match layer.present.is_some() {
                        true => leaf.valued(count),
                        false => Vec::new(),
                    };
                    (leaf, valued)
                }
                (true, None) => {
                    let leaf = Leaf::Any(
                        (0..count)
                            .map(|row| match (has(row), column.get(row)) {
                                (false, _) => self.resolve(path, row),
                                (true, value @ Variable::Object(_)) => match self.resolve(path, row) {
                                    mut existing @ Variable::Object(_) => existing.merge_clone(&value),
                                    _ => value,
                                },
                                (true, value) => value,
                            })
                            .collect(),
                    );
                    let valued = match layer.present.is_some() {
                        true => leaf.valued(count),
                        false => Vec::new(),
                    };
                    (leaf, valued)
                }
            };
            let existing = next.iter().position(|(leaf, _)| leaf == path);
            if let Some(tracked) = presence.as_mut() {
                let present = match &layer.present {
                    None => Presence::Valued,
                    Some(masks) => {
                        let has = masks[index].dense(count);
                        let before = existing.map(|i| tracked[i].mask(&next[i].1, count));
                        Presence::Mask(Mask::Bits(match before {
                            Some(before) => has
                                .iter()
                                .zip(valued.iter())
                                .zip(before.iter())
                                .map(|((h, v), b)| (h & v) | (!h & b))
                                .collect(),
                            None => has.iter().zip(valued.iter()).map(|(h, v)| h & v).collect(),
                        }))
                    }
                };
                match existing {
                    Some(i) => tracked[i] = present,
                    None => tracked.push(present),
                }
            }
            match existing {
                Some(i) => next[i].1 = resolved,
                None => next.push((path.clone(), resolved)),
            }
        }
        let mut all = layers;
        all.push(layer);
        Data::with_shape(
            rows.clone(),
            Shape::Patched {
                base,
                layers: all.into(),
                leaves: next.into(),
                nulls: None,
                presence: presence.map(Rc::from),
            },
        )
    }

    pub fn presence_bits(&self, index: usize) -> Option<Vec<u64>> {
        let Shape::Patched {
            presence: Some(presence),
            leaves,
            nulls,
            ..
        } = &self.shape
        else {
            return None;
        };
        let rows = self.len();
        let mut bits = presence.get(index)?.mask(&leaves.get(index)?.1, rows).to_vec();
        if let Some(nulls) = nulls {
            bits.iter_mut().zip(nulls.iter()).for_each(|(b, n)| *b &= !n);
        }
        Some(bits)
    }

    pub fn present(&self, path: &str) -> (Leaf<'a>, Vec<u64>) {
        let rows = self.len();
        if let (Binding::Column(index), Shape::Patched { leaves, nulls, .. }) = (self.binding(path).0, &self.shape) {
            let bits = self.presence_bits(index).unwrap_or_else(|| {
                let mut bits = Bits::ones(rows).to_vec();
                if let Some(nulls) = nulls {
                    bits.iter_mut().zip(nulls.iter()).for_each(|(b, n)| *b &= !n);
                }
                bits
            });
            return (leaves[index].1.clone(), bits);
        }
        let mut bits = vec![0u64; rows.div_ceil(64)];
        let values: Col = (0..rows)
            .map(|row| match Self::lookup(&self.materialize_row(row), path) {
                Some(value) => {
                    Bits::set(&mut bits, row, true);
                    value
                }
                None => Variable::Null,
            })
            .collect();
        (Leaf::Any(values), bits)
    }

    pub fn leaves(&self) -> &[(Arc<str>, Leaf<'a>)] {
        match &self.shape {
            Shape::Plain(_) => &[],
            Shape::Patched { leaves, .. } => leaves,
        }
    }

    pub fn column_at(&self, path: &str) -> Leaf<'a> {
        match self.binding(path).0 {
            Binding::Column(index) => self.leaves()[index].1.clone(),
            Binding::Absent => Leaf::nulls(self.len()),
            Binding::Row => Leaf::Any((0..self.len()).map(|row| self.resolve(path, row)).collect()),
        }
    }

    pub fn binding(&self, path: &str) -> (Binding, bool) {
        if path == "$" || path.starts_with("$.") || path.starts_with("$nodes") {
            return (Binding::Row, false);
        }
        match &self.shape {
            Shape::Plain(_) => (Binding::Row, false),
            Shape::Patched { base, leaves, .. } => match Self::read(leaves, path) {
                Read::Leaf(index) => (Binding::Column(index), false),
                Read::Base => match base {
                    None => (Binding::Absent, false),
                    Some(_) => (Binding::Row, false),
                },
                Read::Whole => (Binding::Row, true),
            },
        }
    }

    pub fn base(&self) -> Col {
        match &self.shape {
            Shape::Plain(values) => values.clone(),
            Shape::Patched { base: Some(base), .. } => base.clone(),
            Shape::Patched { base: None, .. } => {
                let shared = Variable::empty_object();
                (0..self.len()).map(|_| shared.clone()).collect()
            }
        }
    }
}
