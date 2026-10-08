mod fuse;

pub(super) use fuse::Fusions;
use super::schedule::{Demand, End, Event, Segment};
use super::entity::Entity;
use super::shred::Shred;
use super::table::TableNative;
use super::{Fixed, Native, PolicyPlan, Rows};
use crate::compiled::bound::Outs;
use crate::compiled::columnar::OutputColumn;
use crate::compiled::shred::Shredder;
use crate::compiled::typed::{Array, Bits, ColumnBuilder, Leaf};
use crate::compiled::CompiledGraph;
use crate::policy::blocks::{AssertionIr, MatchIr, MatchSelection, TableSelection};
use crate::policy::evaluator::{Driver, EvalArtifact, Iteration, Pick};
use std::cell::OnceCell;
use crate::workspace::types::{EvaluateRequest, EvaluationError, EvaluationResult};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Binding, Column, Columns, Dictionary, Kind as LaneKind, LaneProgram, LaneRunner, Values};
use zen_expression::{Scope, Variable};

pub struct PolicyColumnarOutput<'a> {
    pub rows: usize,
    pub columns: Vec<(Arc<str>, OutputColumn<'a>)>,
    pub errors: Vec<Option<EvaluationError>>,
    pub hosted: usize,
}

impl PolicyColumnarOutput<'_> {
    pub fn row(&self, row: usize) -> Variable {
        let object = Variable::empty_object();
        for (path, column) in &self.columns {
            let value = column.get(row);
            if !matches!(value, Variable::Null) {
                object.dot_insert(path, value);
            }
        }
        object
    }

    fn from_results<'a>(rows: usize, results: Vec<Result<EvaluationResult, EvaluationError>>) -> PolicyColumnarOutput<'a> {
        let mut shredder = Shredder::new(rows);
        let mut errors = Vec::with_capacity(rows);
        for (row, result) in results.into_iter().enumerate() {
            match result {
                Ok(result) => {
                    shredder.visit(&result.output, 0, row);
                    errors.push(None);
                }
                Err(error) => errors.push(Some(error)),
            }
        }
        PolicyColumnarOutput {
            rows,
            columns: Sheet::columns(shredder.finish()),
            errors,
            hosted: rows,
        }
    }
}

struct Entry<'a> {
    path: Arc<str>,
    leaf: Leaf<'a>,
    present: Option<Rc<[u64]>>,
    input: bool,
    dead: bool,
}

enum Bind {
    Column(usize),
    Absent,
    Row,
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Entry(usize),
    Composed(usize),
}

enum Member {
    Field(usize),
    Root(usize),
    Closure,
    Absent,
    Unsupported,
}

struct Units {
    member: Option<usize>,
    units: Rc<[usize]>,
    owners: Rc<[usize]>,
}

impl Units {
    fn subset(&self, locals: &[usize]) -> Units {
        Units {
            member: self.member,
            units: locals.iter().map(|&local| self.units[local]).collect(),
            owners: locals.iter().map(|&local| self.owners[local]).collect(),
        }
    }
}

struct Sheet<'a> {
    rows: usize,
    entries: Vec<Entry<'a>>,
    inputs: Vec<Column<'a>>,
    entities: Vec<Entity<'a>>,
    refs: Vec<Arc<str>>,
}

impl<'a> Sheet<'a> {
    fn new(columns: &Columns<'a>) -> Option<Self> {
        let rows = columns.rows;
        let mut entries = Vec::with_capacity(columns.columns.len());
        let mut inputs = Vec::with_capacity(columns.columns.len());
        for (path, column) in &columns.columns {
            inputs.push(*column);
            if path.is_empty() || path.contains('[') {
                return None;
            }
            let present = column.validity.map(|(bits, offset)| Rc::from(Bits::window(bits, offset, rows)));
            entries.push(Entry {
                path: Arc::from(*path),
                leaf: Leaf::input(CompiledGraph::validated(*column, rows), rows),
                present,
                input: true,
                dead: false,
            });
        }
        Some(Self {
            rows,
            entries,
            inputs,
            entities: Vec::new(),
            refs: Vec::new(),
        })
    }

    fn related(a: &str, b: &str) -> Option<std::cmp::Ordering> {
        let under = |long: &str, short: &str| long.strip_prefix(short).is_some_and(|rest| rest.starts_with('.'));
        match (a == b, under(a, b), under(b, a)) {
            (true, _, _) => Some(std::cmp::Ordering::Equal),
            (_, true, _) => Some(std::cmp::Ordering::Greater),
            (_, _, true) => Some(std::cmp::Ordering::Less),
            _ => None,
        }
    }

    fn bind(&self, key: &str, positions: &[usize], shared: bool) -> Bind {
        if self.refs.iter().any(|path| Self::related(path, key).is_some()) {
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
        match (Self::related(&entry.path, key), covered, shared && Self::composite(&entry.leaf)) {
            (Some(std::cmp::Ordering::Equal), true, false) => Bind::Column(index),
            _ => Bind::Row,
        }
    }

    fn composite(leaf: &Leaf) -> bool {
        match leaf {
            Leaf::Any(values) => values.iter().any(|v| matches!(v, Variable::Object(_) | Variable::Array(_))),
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
                        values: Dictionary::Scaled { .. } | Dictionary::Text { .. } | Dictionary::Bool { .. },
                        ..
                    }
            ),
        }
    }

    fn present(entry: &Entry, row: usize) -> bool {
        entry.present.as_ref().is_none_or(|present| Bits::get(present, row))
    }

    fn active(&self, index: usize) -> Option<&Entity<'a>> {
        self.entities.iter().find(|entity| entity.active && entity.entry == index)
    }

    fn retire(&mut self, at: usize) {
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
        self.entities[at].active = false;
        self.hosted(path, values, present);
    }

    fn object(&self, row: usize) -> Variable {
        let object = Variable::empty_object();
        for (index, entry) in self.entries.iter().enumerate() {
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

    fn write(&mut self, path: Arc<str>, leaf: Leaf<'a>, positions: &[usize]) {
        let present: Option<Rc<[u64]>> = match positions.len() == self.rows {
            true => None,
            false => {
                let mut bits = vec![0u64; self.rows.div_ceil(64)];
                positions.iter().for_each(|&row| Bits::set(&mut bits, row, true));
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
            .filter(|&at| !self.entries[at].input && self.entries[at].path == path && self.entries[at].present.is_some());
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
                let full = union.iter().map(|w| w.count_ones() as usize).sum::<usize>() == self.rows;
                let union: Rc<[u64]> = union.into();
                (Leaf::typed(builder.finish()), (!full).then_some(union))
            }
            _ => (leaf, present),
        };
        self.entries.push(Entry {
            path,
            leaf,
            present,
            input: false,
            dead: false,
        });
    }

    fn hosted(&mut self, path: Arc<str>, values: Vec<Variable>, present: Vec<u64>) {
        let full = present.iter().map(|w| w.count_ones() as usize).sum::<usize>() == self.rows;
        self.entries.push(Entry {
            path,
            leaf: Leaf::Any(values.into()),
            present: (!full).then(|| present.into()),
            input: false,
            dead: false,
        });
    }

    fn columns(leaves: Vec<(Arc<str>, Leaf<'a>, Option<Rc<[u64]>>)>) -> Vec<(Arc<str>, OutputColumn<'a>)> {
        leaves
            .into_iter()
            .map(|(path, leaf, present)| {
                let leaf = match present {
                    Some(present) => Leaf::masked(leaf, &present),
                    None => leaf,
                };
                (path, OutputColumn(leaf))
            })
            .collect()
    }

    fn output(&self, finished: &[usize]) -> Vec<(Arc<str>, OutputColumn<'a>)> {
        let nested = self.entries.iter().enumerate().filter(|(_, a)| !a.dead).any(|(i, a)| {
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, b)| !b.dead)
                .any(|(j, b)| i != j && matches!(Self::related(&a.path, &b.path), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)))
        });
        match nested {
            false => self
                .entries
                .iter()
                .enumerate()
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
                let mut shredder = Shredder::new(self.rows);
                for &row in finished {
                    shredder.visit(&self.object(row), 0, row);
                }
                Self::columns(shredder.finish())
            }
        }
    }
}

struct Pass<'s, 'a> {
    plan: &'s PolicyPlan,
    artifact: &'s EvalArtifact,
    entry: &'s Arc<str>,
    sheet: Sheet<'a>,
    runner: &'s mut LaneRunner,
    fallback: Vec<bool>,
    arms: Vec<(usize, Vec<Option<usize>>)>,
    stores: &'s [OnceCell<Variable>],
    state: Rows<'s>,
    synced: Vec<usize>,
    shared: bool,
    base: usize,
    chosen: Vec<(usize, Vec<u32>)>,
    members: Vec<(usize, Vec<Option<usize>>)>,
    hosting: Vec<(usize, Vec<bool>)>,
    owners: Vec<Option<Arc<str>>>,
    kin: Vec<(usize, Rc<[usize]>, Rc<[usize]>, bool)>,
    delegated: bool,
    fell: usize,
}

impl<'s, 'a> Pass<'s, 'a> {
    fn scope(&mut self, row: usize) -> Scope {
        if !self.shared {
            return Scope::new(self.sheet.object(row));
        }
        match self.driver(row) {
            Some(driver) => {
                driver.clear_dollar();
                Scope::new(driver.env().shallow_clone())
            }
            None => Scope::default(),
        }
    }

    fn scopes(&mut self, positions: &[usize]) -> Vec<Scope> {
        let mut scopes: Vec<Scope> = (0..self.sheet.rows).map(|_| Scope::default()).collect();
        for &row in positions {
            scopes[row] = self.scope(row);
        }
        scopes
    }

    fn run(&mut self, program: &LaneProgram, positions: &[usize]) -> (Leaf<'a>, Vec<bool>) {
        let (mut leaves, failed) = self.run_outs(program, positions);
        let leaf = match leaves.is_empty() {
            true => Leaf::nulls(positions.len()),
            false => leaves.swap_remove(0),
        };
        (leaf, failed)
    }

    fn run_outs(&mut self, program: &LaneProgram, positions: &[usize]) -> (Vec<Leaf<'a>>, Vec<bool>) {
        let p = program.program();
        let nested = p.site_keys.iter().flatten().any(|key| {
            self.sheet.entities.iter().any(|e| e.active && e.path.as_ref() == key.as_str() && !e.flat())
        });
        let cached = nested || !PolicyPlan::scalar(program);
        if cached {
            for key in p.site_keys.iter().flatten() {
                if let Some(entity) = self.sheet.entities.iter_mut().find(|e| e.active && e.path.as_ref() == key.as_str()) {
                    entity.materialize();
                }
            }
        }
        let mut failed = vec![false; positions.len()];
        if p.writes_env || p.chain || p.opaque() || p.rows {
            let scopes: Vec<Scope> = positions.iter().map(|&row| self.scope(row)).collect();
            let mut values = vec![Variable::Null; positions.len()];
            self.runner.evaluate_with(program, &scopes, |at, result| match result {
                Ok(value) => values[at] = value,
                Err(_) => failed[at] = true,
            });
            return (vec![Leaf::Any(values.into())], failed);
        }
        for key in p.site_keys.iter().flatten() {
            let stale: Vec<usize> = self
                .sheet
                .entities
                .iter()
                .enumerate()
                .filter(|(_, entity)| entity.active && Sheet::related(key, &entity.path).is_some_and(|o| o != std::cmp::Ordering::Equal))
                .map(|(at, _)| at)
                .collect();
            stale.into_iter().for_each(|at| self.sheet.retire(at));
        }
        let mut columns = Columns::new(self.sheet.rows);
        let mut used: Vec<Slot> = Vec::new();
        let mut rows = false;
        let bound: Vec<Binding> = p
            .site_keys
            .iter()
            .map(|key| {
                let slot = match key.as_deref() {
                    None => return Binding::Row,
                    Some(key) => match self.sheet.entities.iter().position(|e| e.active && e.path.as_ref() == key) {
                        Some(entity) => Slot::Composed(entity),
                        None => match self.sheet.bind(key, positions, self.shared) {
                            Bind::Absent => return Binding::Absent,
                            Bind::Row => {
                                rows = true;
                                return Binding::Row;
                            }
                            Bind::Column(entry) => Slot::Entry(entry),
                        },
                    },
                };
                match used.iter().position(|e| *e == slot) {
                    Some(at) => Binding::Column(at),
                    None => {
                        used.push(slot);
                        Binding::Column(used.len() - 1)
                    }
                }
            })
            .collect();
        let scopes = match rows {
            true => crate::compiled::bound::Scopes::Owned(self.scopes(positions)),
            false => crate::compiled::bound::Bound::blank(self.sheet.rows),
        };
        let views: Vec<(Arc<str>, Leaf<'a>)> = used
            .iter()
            .filter_map(|slot| match slot {
                Slot::Entry(entry) => Some((self.sheet.entries[*entry].path.clone(), self.sheet.entries[*entry].leaf.clone())),
                Slot::Composed(at) if cached => {
                    let entity = &self.sheet.entities[*at];
                    let values: Vec<Variable> = (0..self.sheet.rows).map(|row| entity.compose(row).unwrap_or(Variable::Null)).collect();
                    Some((entity.path.clone(), Leaf::Any(values.into())))
                }
                Slot::Composed(_) => None,
            })
            .collect();
        let parts: Vec<(usize, Vec<(Arc<str>, Leaf<'a>)>)> = used
            .iter()
            .filter_map(|slot| match slot {
                Slot::Composed(at) if !cached => {
                    let entity = &self.sheet.entities[*at];
                    Some((*at, (0..entity.fields.len()).map(|f| (entity.fields[f].name.clone(), entity.view(f))).collect()))
                }
                _ => None,
            })
            .collect();
        let shapes: Vec<(usize, &[i32], Option<(&[u64], usize)>)> = parts
            .iter()
            .map(|(at, _)| {
                let entity = &self.sheet.entities[*at];
                (entity.children, entity.offsets.as_ref(), entity.validity.as_deref().map(|bits| (bits, 0)))
            })
            .collect();
        let fields: Vec<Vec<(&str, Column)>> = parts.iter().map(|(_, leaves)| leaves.iter().map(|(n, l)| (n.as_ref(), l.column())).collect()).collect();
        let structs: Vec<Column> = fields
            .iter()
            .zip(&shapes)
            .map(|(f, (len, _, _))| Column::new(Values::Struct { fields: f, len: *len }))
            .collect();
        let lists: Vec<(&str, Column)> = structs
            .iter()
            .zip(&shapes)
            .zip(&parts)
            .map(|((child, (_, offsets, validity)), (at, _))| {
                (
                    self.sheet.entities[*at].path.as_ref(),
                    Column {
                        values: Values::List {
                            offsets,
                            child: Dictionary::Column(child),
                        },
                        validity: *validity,
                    },
                )
            })
            .collect();
        let (mut next_view, mut next_list) = (views.iter(), lists.iter());
        for slot in &used {
            match (slot, cached) {
                (Slot::Entry(_), _) | (Slot::Composed(_), true) => {
                    if let Some((path, view)) = next_view.next() {
                        columns = columns.column(path.as_ref(), view.column());
                    }
                }
                (Slot::Composed(_), false) => {
                    if let Some((path, list)) = next_list.next() {
                        columns = columns.column(path, *list);
                    }
                }
            }
        }
        let mut outs = Outs::take();
        let subset = (positions.len() != self.sheet.rows).then_some(positions);
        let special = self.plan.special(program, &columns);
        let program = special.as_deref().unwrap_or(program);
        Self::prefer_codes(program, &mut outs);
        self.runner.evaluate_sites(program, scopes.slice(), &columns, &bound, subset, &mut outs, |at, _, _| {
            if let Some(f) = failed.get_mut(at) {
                *f = true;
            }
        });
        let leaves = outs.iter_mut().map(|out| Leaf::typed(Array::from_output(out))).collect();
        outs.iter_mut().for_each(zen_expression::lane::Output::prefer_text);
        (leaves, failed)
    }

    fn prefer_codes(program: &LaneProgram, outs: &mut Outs) {
        let p = program.program();
        outs.resize_with(p.outputs.len().max(1), zen_expression::lane::Output::new);
        outs.iter_mut().for_each(zen_expression::lane::Output::prefer_codes);
    }

    fn fall(&mut self, row: usize) {
        if !self.fallback[row] {
            self.fallback[row] = true;
            self.fell += 1;
        }
    }

    fn fail(&mut self, positions: &[usize], failed: &[bool]) {
        if !failed.contains(&true) {
            return;
        }
        for (&row, &f) in positions.iter().zip(failed) {
            if f {
                self.fall(row);
            }
        }
    }

    fn write(&mut self, key: &Arc<str>, leaf: Leaf<'a>, positions: &[usize]) {
        if !key.is_empty() {
            self.sheet.write(key.clone(), leaf, positions);
        }
    }

    fn expression(&mut self, key: &Arc<str>, program: &LaneProgram, positions: &[usize]) {
        let (leaf, failed) = self.run(program, positions);
        self.fail(positions, &failed);
        self.write(key, leaf, positions);
    }

    fn assertion(&mut self, ir: &AssertionIr, conditions: &[LaneProgram], positions: &[usize]) {
        let mut truths: Vec<Vec<u64>> = Vec::with_capacity(conditions.len());
        for program in conditions {
            let (leaf, failed) = self.run(program, positions);
            self.fail(positions, &failed);
            truths.push(leaf.truths(positions.len()));
        }
        let bits = Self::folded(ir, &truths, positions.len());
        let leaf = Leaf::typed(Array::parts(crate::compiled::typed::Store::Bool(bits), None, positions.len()));
        self.write(&ir.output, leaf, positions);
    }

    fn folded(ir: &AssertionIr, truths: &[Vec<u64>], count: usize) -> Vec<u64> {
        let width = truths.len();
        if width > 6 {
            let mut results = vec![false; width];
            return Bits::of(count, |at| {
                results.iter_mut().zip(truths).for_each(|(r, t)| *r = Bits::get(t, at));
                ir.fold(&results)
            });
        }
        let mut results = vec![false; width];
        let table = (0..1usize << width).fold(0u64, |table, combo| {
            results.iter_mut().enumerate().for_each(|(c, r)| *r = combo >> c & 1 == 1);
            table | u64::from(ir.fold(&results)) << combo
        });
        match table {
            0 => vec![0; count.div_ceil(64)],
            _ => Bits::of(count, |at| {
                let combo = truths.iter().enumerate().fold(0usize, |combo, (c, t)| combo | usize::from(Bits::get(t, at)) << c);
                table >> combo & 1 == 1
            }),
        }
    }

    fn dict(values: &[Variable]) -> crate::compiled::typed::Dict {
        use crate::compiled::typed::Dict;
        if values.iter().all(|v| matches!(v, Variable::String(_))) {
            let mut offsets = vec![0i32];
            let mut data = String::new();
            for value in values {
                data.push_str(value.as_str().unwrap_or_default());
                offsets.push(data.len() as i32);
            }
            return Dict::Text { offsets, data };
        }
        let scaled: Option<Vec<(i64, u8)>> = values
            .iter()
            .map(|v| match v {
                Variable::Number(n) => Some((i64::try_from(n.mantissa()).ok()?, u8::try_from(n.scale()).ok()?)),
                _ => None,
            })
            .collect();
        if let Some(parts) = scaled {
            return Dict::Scaled {
                mant: parts.iter().map(|p| p.0).collect(),
                scale: parts.iter().map(|p| p.1).collect(),
            };
        }
        if values.iter().all(|v| matches!(v, Variable::Bool(_))) {
            return Dict::Bool(Bits::of(values.len(), |i| matches!(values[i], Variable::Bool(true))));
        }
        Dict::Any(values.to_vec())
    }

    fn coded(values: &[Variable], codes: Vec<i32>) -> Leaf<'a> {
        let mut valid: Vec<u64> = codes
            .chunks(64)
            .map(|chunk| chunk.iter().enumerate().fold(0u64, |word, (at, code)| word | u64::from(*code >= 0) << at))
            .collect();
        Bits::trim(&mut valid, codes.len());
        Leaf::typed(Array::coded_with(codes.into(), Rc::new(Self::dict(values)), valid))
    }

    fn arms_of(leaf: &Leaf, count: usize) -> Vec<Option<usize>> {
        let column = leaf.column();
        if let Values::Scaled { mant, scale } = column.values {
            if mant.len() >= count && scale.len() >= count && scale[..count].iter().all(|s| *s == 0) {
                return mant[..count].iter().map(|m| usize::try_from(*m).ok()).collect();
            }
        }
        (0..count)
            .map(|at| column.number(at).and_then(|n| i64::try_from(n).ok()).and_then(|n| usize::try_from(n).ok()))
            .collect()
    }

    fn select(&mut self, block: usize, conditions: &[Option<LaneProgram>], pick: Option<&LaneProgram>, positions: &[usize]) {
        let mut chosen: Vec<Option<usize>> = vec![None; self.sheet.rows];
        if let Some(pick) = pick {
            let (leaf, failed) = self.run(pick, positions);
            self.fail(positions, &failed);
            for (&row, arm) in positions.iter().zip(Self::arms_of(&leaf, positions.len())) {
                chosen[row] = arm;
            }
            match self.arms.iter_mut().find(|(b, _)| *b == block) {
                Some((_, arms)) => positions.iter().for_each(|&row| arms[row] = chosen[row]),
                None => self.arms.push((block, chosen)),
            }
            return;
        }
        let mut open: Vec<usize> = positions.to_vec();
        for (arm, condition) in conditions.iter().enumerate() {
            if open.is_empty() {
                break;
            }
            let Some(program) = condition else {
                open.drain(..).for_each(|row| chosen[row] = Some(arm));
                break;
            };
            let (leaf, failed) = self.run(program, &open);
            self.fail(&open, &failed);
            let mut next = Vec::with_capacity(open.len());
            for (at, &row) in open.iter().enumerate() {
                match leaf.truthy(at) {
                    true => chosen[row] = Some(arm),
                    false => next.push(row),
                }
            }
            open = next;
        }
        match self.arms.iter_mut().find(|(b, _)| *b == block) {
            Some((_, arms)) => positions.iter().for_each(|&row| arms[row] = chosen[row]),
            None => self.arms.push((block, chosen)),
        }
    }

    fn arm(&self, block: usize, row: usize) -> Option<usize> {
        self.arms.iter().find(|(b, _)| *b == block).and_then(|(_, arms)| arms[row])
    }

    fn constant(values: &[Option<LaneProgram>], constants: &[Option<Fixed>]) -> bool {
        values.iter().enumerate().all(|(arm, value)| value.is_none() || constants.get(arm).is_some_and(Option::is_some))
    }

    fn fixed(constants: &[Option<Fixed>]) -> Vec<Variable> {
        constants.iter().map(|c| c.as_ref().map_or(Variable::Null, Fixed::variable)).collect()
    }

    fn code(arm: Option<usize>, values: &[Option<LaneProgram>]) -> i32 {
        arm.filter(|&arm| values.get(arm).is_some_and(Option::is_some)).map_or(-1, |arm| arm as i32)
    }

    fn commit(&mut self, block: usize, ir: &MatchIr, values: &[Option<LaneProgram>], constants: &[Option<Fixed>], positions: &[usize]) {
        if Self::constant(values, constants) {
            let codes: Vec<i32> = match self.arms.iter().find(|(b, _)| *b == block) {
                Some((_, arms)) => positions.iter().map(|&row| Self::code(arms[row], values)).collect(),
                None => vec![-1; positions.len()],
            };
            self.write(&ir.key, Self::coded(&Self::fixed(constants), codes), positions);
            return;
        }
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); ir.arms.len()];
        for (local, &row) in positions.iter().enumerate() {
            if let Some(arm) = self.arm(block, row).filter(|&arm| values[arm].is_some()) {
                groups[arm].push(local);
            }
        }
        if groups.iter().enumerate().all(|(arm, locals)| locals.is_empty() || constants.get(arm).is_some_and(Option::is_some)) {
            let values: Vec<Variable> = constants.iter().map(|c| c.as_ref().map_or(Variable::Null, Fixed::variable)).collect();
            let mut codes = vec![-1i32; positions.len()];
            for (arm, locals) in groups.iter().enumerate() {
                locals.iter().for_each(|&local| codes[local] = arm as i32);
            }
            self.write(&ir.key, Self::coded(&values, codes), positions);
            return;
        }
        let mut out: Vec<Variable> = vec![Variable::Null; positions.len()];
        for (arm, locals) in groups.iter_mut().enumerate() {
            if let Some(Some(value)) = constants.get(arm) {
                let value = value.variable();
                locals.drain(..).for_each(|local| out[local] = value.clone());
            }
        }
        for (arm, locals) in groups.into_iter().enumerate() {
            let Some(program) = values[arm].as_ref().filter(|_| !locals.is_empty()) else {
                continue;
            };
            let members: Vec<usize> = locals.iter().map(|&local| positions[local]).collect();
            let (leaf, failed) = self.run(program, &members);
            self.fail(&members, &failed);
            for (i, &local) in locals.iter().enumerate() {
                out[local] = leaf.get(i);
            }
        }
        let mut builder = ColumnBuilder::with_capacity(out.len());
        out.into_iter().for_each(|value| builder.push_variable(value));
        self.write(&ir.key, Leaf::typed(builder.finish()), positions);
    }

    fn driver(&mut self, row: usize) -> Option<&mut Driver<'s>> {
        self.state.ensure();
        if self.state.drivers[row].is_none() {
            let prepared = self.stores[row].get().is_some();
            let store = self.stores[row].get_or_init(|| self.sheet.object(row));
            self.state.drivers[row] = Some(Driver::new(self.artifact, store, self.entry, false, false));
            self.synced[row] = match prepared {
                true => self.base,
                false => self.sheet.entries.len(),
            };
        }
        let from = self.synced[row];
        self.synced[row] = self.sheet.entries.len();
        let driver = self.state.drivers[row].as_mut()?;
        for entry in &self.sheet.entries[from..] {
            if Sheet::present(entry, row) {
                driver.write(&entry.path, entry.leaf.get(row));
            }
        }
        Some(driver)
    }

    fn readback(&self, block: usize) -> Vec<Arc<str>> {
        let rule = &self.plan.blocks.rules[block];
        let top = |path: &str| -> Arc<str> { Arc::from(path.split(['.', '[']).next().unwrap_or(path)) };
        let mut paths: Vec<Arc<str>> = match &self.plan.iterations[block] {
            Some((_, path, _)) => vec![top(path)],
            None => rule
                .kind
                .write_sites()
                .into_iter()
                .map(|site| match site.path.contains('[') {
                    true => top(&site.path),
                    false => site.path,
                })
                .collect(),
        };
        paths.sort();
        paths.dedup();
        let nested: Vec<Arc<str>> = paths
            .iter()
            .filter(|p| paths.iter().any(|q| Sheet::related(p, q) == Some(std::cmp::Ordering::Greater)))
            .cloned()
            .collect();
        paths.retain(|p| !nested.contains(p));
        paths
    }

    fn delegate(&mut self, event: Event, positions: &[usize]) {
        let (block, commit) = match event {
            Event::Select(block) => (block, false),
            Event::Commit(block) => (block, true),
        };
        self.state.ensure();
        self.delegated = true;
        for &row in positions {
            self.driver(row);
        }
        match commit {
            false => self.plan.select(block, positions, &mut self.state),
            true => self.plan.commit(block, positions, &mut self.state),
        }
        let mut done: Vec<usize> = Vec::with_capacity(positions.len());
        for &row in positions {
            match self.state.failures[row].is_some() {
                true => self.fall(row),
                false => done.push(row),
            }
        }
        if !commit || done.is_empty() {
            return;
        }
        for path in self.readback(block) {
            let mut values = vec![Variable::Null; self.sheet.rows];
            let mut present = vec![0u64; self.sheet.rows.div_ceil(64)];
            for &row in &done {
                if let Some(value) = self.stores[row].get().and_then(|store| store.dot(&path)) {
                    values[row] = value;
                    Bits::set(&mut present, row, true);
                }
            }
            self.sheet.hosted(path, values, present);
        }
        for &row in &done {
            self.synced[row] = self.sheet.entries.len();
        }
    }

    fn quiet(&self, block: usize) -> bool {
        use crate::policy::blocks::ConditionalReads;
        match self.artifact.read_plans.get(&self.plan.blocks.refs[block]).map(|plan| &plan.conditional) {
            None | Some(ConditionalReads::None) => true,
            Some(ConditionalReads::Match(arms)) => arms.iter().all(|arm| arm.value_reads.is_empty()),
            Some(ConditionalReads::DecisionTable(cells)) => cells.iter().all(|cell| cell.cell_reads.is_empty()),
        }
    }

    fn selection(&self, block: usize, row: usize) -> Option<Vec<u32>> {
        let code = |arm: Option<usize>| arm.map_or(0, |arm| arm as u32 + 1);
        if let Some(at) = self.entity_of(block) {
            if let (Some((_, arms)), Some(Native::Match { .. })) = (self.members.iter().find(|(b, _)| *b == block), Self::native(self.plan, block)) {
                let (a, b) = self.sheet.entities[at].range(row).unwrap_or((0, 0));
                let mut key: Vec<u32> = (a..b).map(|child| code(arms.get(child).copied().flatten())).collect();
                key.sort_unstable();
                key.dedup();
                key.insert(0, 1);
                return Some(key);
            }
        }
        if let (Some(table), false) = (&self.plan.tables[block], self.hosted(block, row)) {
            if let Some((_, chosen)) = self.chosen.iter().find(|(b, _)| *b == block) {
                let outputs = table.outputs.len().max(1);
                let unit = |unit: usize| -> Vec<u32> { (0..outputs).map(|c| chosen.get(unit * outputs + c).copied().unwrap_or(u32::MAX)).collect() };
                let mut units: Vec<Vec<u32>> = match self.entity_of(block) {
                    None => vec![unit(row)],
                    Some(at) => {
                        let (a, b) = self.sheet.entities[at].range(row).unwrap_or((0, 0));
                        (a..b).map(unit).collect()
                    }
                };
                units.sort_unstable();
                units.dedup();
                let mut key = vec![2];
                key.extend(units.into_iter().flatten());
                return Some(key);
            }
        }
        if self.state.picked(row, block).is_some() {
            return None;
        }
        match &self.plan.natives[block] {
            Some(Native::Match { .. }) => Some(vec![3, code(self.arm(block, row))]),
            _ => None,
        }
    }

    fn demand(&self, block: usize, row: usize) -> Demand {
        if let Some(demand) = self.member_demand(block, row).or_else(|| self.table_demand(block, row)) {
            return demand;
        }
        let owner = &self.plan.blocks.refs[block];
        if let (Some(picked), Some(driver)) = (self.state.picked(row, block), self.state.drivers.get(row).and_then(Option::as_ref)) {
            return driver.demanded(owner, picked).into();
        }
        let Some(plan) = self.artifact.read_plans.get(owner) else {
            return Arc::from([]);
        };
        let Some(Native::Match { ir, .. }) = &self.plan.natives[block] else {
            return Arc::from([]);
        };
        let pick = Pick::Match(MatchSelection {
            matched_arm: self.arm(block, row).and_then(|arm| ir.arms.get(arm)).map(|arm| arm.id.clone()),
            arms: Vec::new(),
        });
        let mut demanded = Vec::new();
        pick.collect_reads(plan, &mut demanded);
        demanded.into()
    }

    fn event(&mut self, event: Event, positions: &[usize]) {
        let plan = self.plan;
        let block = match event {
            Event::Select(block) | Event::Commit(block) => block,
        };
        if self.owners[block].is_some() {
            let selected = self.members.iter().any(|(b, _)| *b == block) || self.chosen.iter().any(|(b, _)| *b == block);
            if let Some(at) = self.entity_of(block) {
                if self.supported(at, block, positions) {
                    return self.member(at, block, event, positions);
                }
                if selected || self.sheet.entities[at].cached() {
                    positions.iter().for_each(|&row| self.fall(row));
                    return;
                }
                self.sheet.retire(at);
            }
            return self.delegate(event, positions);
        }
        if let Some(table) = &plan.tables[block] {
            let units = |rows: &[usize]| Units {
                member: None,
                units: rows.into(),
                owners: rows.into(),
            };
            return match event {
                Event::Select(block) => self.table_select(block, table, &units(positions)),
                Event::Commit(block) if !self.hosting.iter().any(|(b, _)| *b == block) => self.table_commit(block, table, &units(positions)),
                Event::Commit(block) => {
                    let (hosted, native): (Vec<usize>, Vec<usize>) = positions.iter().partition(|&&row| self.hosted(block, row));
                    if !native.is_empty() {
                        self.table_commit(block, table, &units(&native));
                    }
                    if !hosted.is_empty() {
                        self.delegate(event, &hosted);
                    }
                }
            };
        }
        match (event, &plan.natives[block]) {
            (Event::Select(block), Some(Native::Match { conditions, pick, .. })) => self.select(block, conditions, pick.as_deref(), positions),
            (Event::Commit(block), Some(Native::Match { ir, values, constants, .. })) => self.commit(block, ir, values, constants, positions),
            (Event::Commit(_), Some(Native::Expression { key, program })) => self.expression(key, program, positions),
            (Event::Commit(_), Some(Native::Assertion { ir, conditions })) => self.assertion(ir, conditions, positions),
            _ => self.delegate(event, positions),
        }
    }

    fn walk(&mut self, root: Arc<Segment>, live: Vec<usize>) {
        let mut stack: Vec<(Arc<Segment>, Vec<usize>)> = vec![(root, live)];
        while let Some((segment, mut rows)) = stack.pop() {
            let segment = self.events(segment, &mut rows);
            let End::Branch { block, .. } = &segment.end else {
                continue;
            };
            if rows.is_empty() {
                continue;
            }
            if self.quiet(*block) {
                stack.push((self.plan.child(self.artifact, &segment, Arc::from([])), rows));
                continue;
            }
            let mut groups: Vec<(Demand, Vec<usize>)> = Vec::new();
            let mut memo: ahash::HashMap<Vec<u32>, usize> = ahash::HashMap::default();
            for &row in &rows {
                let key = self.selection(*block, row);
                if let Some(&at) = key.as_ref().and_then(|key| memo.get(key)) {
                    groups[at].1.push(row);
                    continue;
                }
                let demand = self.demand(*block, row);
                let at = match groups.iter().position(|(d, _)| *d == demand) {
                    Some(at) => {
                        groups[at].1.push(row);
                        at
                    }
                    None => {
                        groups.push((demand, vec![row]));
                        groups.len() - 1
                    }
                };
                if let Some(key) = key {
                    memo.insert(key, at);
                }
            }
            for (demand, members) in groups.into_iter().rev() {
                stack.push((self.plan.child(self.artifact, &segment, demand), members));
            }
        }
    }
}

impl<'s, 'a> Pass<'s, 'a> {
    fn entity_of(&self, block: usize) -> Option<usize> {
        let path = self.owners.get(block)?.as_ref()?;
        self.sheet.entities.iter().position(|e| e.active && e.path == *path)
    }

    fn kids(&mut self, at: usize, positions: &[usize]) -> (Rc<[usize]>, Rc<[usize]>) {
        let full = positions.len() == self.sheet.rows;
        if let Some((_, kids, parents, _)) = self.kin.iter().find(|(a, ..)| full && *a == at) {
            return (kids.clone(), parents.clone());
        }
        let (kids, parents) = self.gather(at, positions);
        if full {
            let identity = kids.len() == self.sheet.entities[at].children && kids.iter().enumerate().all(|(i, &k)| i == k);
            self.kin.push((at, kids.clone(), parents.clone(), identity));
        }
        (kids, parents)
    }

    fn identity(&self, at: usize, kids: &Rc<[usize]>) -> bool {
        match self.kin.iter().find(|(a, k, ..)| *a == at && Rc::ptr_eq(k, kids)) {
            Some((.., identity)) => *identity,
            None => kids.len() == self.sheet.entities[at].children && kids.iter().enumerate().all(|(i, &k)| i == k),
        }
    }

    fn gather(&self, at: usize, positions: &[usize]) -> (Rc<[usize]>, Rc<[usize]>) {
        let entity = &self.sheet.entities[at];
        let mut kids = Vec::with_capacity(entity.children);
        let mut parents = Vec::with_capacity(entity.children);
        for &row in positions {
            if let Some((a, b)) = entity.range(row) {
                kids.extend(a..b);
                parents.extend(std::iter::repeat_n(row, b.saturating_sub(a)));
            }
        }
        (kids.into(), parents.into())
    }

    fn site(&self, at: usize, key: Option<&str>, rows: &[usize]) -> Member {
        let Some(key) = key else {
            return Member::Closure;
        };
        let entity = &self.sheet.entities[at];
        let owned = |field: &str| {
            entity
                .owner
                .as_deref()
                .is_some_and(|owner| field == owner || field.strip_prefix(owner).is_some_and(|rest| rest.starts_with('.')))
        };
        match key.strip_prefix(entity.name.as_ref()).and_then(|rest| rest.strip_prefix('.')) {
            Some(field) if owned(field) || entity.overlaps(field) => Member::Unsupported,
            Some(field) => entity.field(field).map_or(Member::Absent, Member::Field),
            None if key == entity.name.as_ref() || key.starts_with('$') => Member::Unsupported,
            None if self.sheet.entities.iter().any(|e| e.active && Sheet::related(key, &e.path).is_some()) => Member::Unsupported,
            None => match self.sheet.bind(key, rows, self.shared) {
                Bind::Column(entry) => Member::Root(entry),
                Bind::Absent => Member::Absent,
                Bind::Row => Member::Unsupported,
            },
        }
    }

    fn field_of(&self, at: usize, key: &Arc<str>) -> Option<Option<Arc<str>>> {
        if key.is_empty() {
            return Some(None);
        }
        let entity = &self.sheet.entities[at];
        let field = key.strip_prefix(entity.name.as_ref())?.strip_prefix('.')?;
        (!field.is_empty() && !entity.overlaps(field)).then(|| Some(Arc::from(field)))
    }

    fn native(plan: &PolicyPlan, block: usize) -> Option<&Native> {
        plan.natives[block].as_ref().or(plan.members[block].as_ref())
    }

    fn block(plan: &PolicyPlan, block: usize) -> Option<(Vec<&LaneProgram>, Vec<&Arc<str>>)> {
        match (Self::native(plan, block), &plan.tables[block]) {
            (Some(native), _) => {
                let key = match native {
                    Native::Expression { key, .. } => key,
                    Native::Assertion { ir, .. } => &ir.output,
                    Native::Match { ir, .. } => &ir.key,
                };
                Some((Self::programs(native), vec![key]))
            }
            (None, Some(table)) => Some((table.programs(), table.outputs.iter().map(|o| &o.field).collect())),
            (None, None) => None,
        }
    }

    fn programs(native: &Native) -> Vec<&LaneProgram> {
        match native {
            Native::Expression { program, .. } => vec![program.as_ref()],
            Native::Assertion { conditions, .. } => conditions.iter().collect(),
            Native::Match { conditions, values, .. } => conditions.iter().chain(values.iter()).flatten().collect(),
        }
    }

    fn supported(&self, at: usize, block: usize, rows: &[usize]) -> bool {
        let Some((programs, keys)) = Self::block(self.plan, block) else {
            return false;
        };
        let iterated = self.plan.members[block].is_some() || self.plan.tables[block].is_some();
        iterated
            && keys.iter().all(|key| self.field_of(at, key).is_some())
            && programs.into_iter().all(|program| {
                let p = program.program();
                !(p.writes_env || p.chain || p.opaque() || p.rows)
                    && p.site_keys.iter().all(|k| !matches!(self.site(at, k.as_deref(), rows), Member::Unsupported))
            })
    }

    fn member_run(&mut self, at: usize, program: &LaneProgram, kids: &Rc<[usize]>, parents: &Rc<[usize]>) -> (Leaf<'a>, Vec<bool>) {
        let (mut leaves, failed) = self.member_outs(at, program, kids, parents);
        let leaf = match leaves.is_empty() {
            true => Leaf::nulls(kids.len()),
            false => leaves.swap_remove(0),
        };
        (leaf, failed)
    }

    fn member_outs(&mut self, at: usize, program: &LaneProgram, kids: &Rc<[usize]>, parents: &Rc<[usize]>) -> (Vec<Leaf<'a>>, Vec<bool>) {
        let p = program.program();
        let count = kids.len();
        let mut failed = vec![false; count];
        if count == 0 {
            return (Vec::new(), failed);
        }
        let mut views: Vec<(Arc<str>, Leaf<'a>)> = Vec::new();
        let identity = self.identity(at, kids);
        let bound: Vec<Binding> = p
            .site_keys
            .iter()
            .map(|key| {
                let name: Arc<str> = key.as_deref().map_or_else(|| Arc::from(""), Arc::from);
                if let Some(position) = views.iter().position(|(k, _)| *k == name) {
                    return Binding::Column(position);
                }
                let leaf = match self.site(at, key.as_deref(), parents) {
                    Member::Field(field) if identity => self.sheet.entities[at].view(field),
                    Member::Field(field) => self.sheet.entities[at].view(field).pick(kids),
                    Member::Root(entry) => self.sheet.entries[entry].leaf.pick(parents),
                    Member::Closure => return Binding::Row,
                    Member::Absent | Member::Unsupported => return Binding::Absent,
                };
                views.push((name, leaf));
                Binding::Column(views.len() - 1)
            })
            .collect();
        let mut columns = Columns::new(count);
        for (name, view) in &views {
            columns = columns.column(name.as_ref(), view.column());
        }
        let scopes = crate::compiled::bound::Bound::blank(count);
        let mut outs = Outs::take();
        let special = self.plan.special(program, &columns);
        let program = special.as_deref().unwrap_or(program);
        Self::prefer_codes(program, &mut outs);
        self.runner.evaluate_sites(program, scopes.slice(), &columns, &bound, None, &mut outs, |child, _, _| {
            if let Some(f) = failed.get_mut(child) {
                *f = true;
            }
        });
        let leaves = outs.iter_mut().map(|out| Leaf::typed(Array::from_output(out))).collect();
        outs.iter_mut().for_each(zen_expression::lane::Output::prefer_text);
        (leaves, failed)
    }

    fn member_fail(&mut self, parents: &[usize], failed: &[bool]) {
        if !failed.contains(&true) {
            return;
        }
        for (&row, &f) in parents.iter().zip(failed) {
            if f {
                self.fall(row);
            }
        }
    }

    fn member_write(&mut self, at: usize, key: &Arc<str>, leaf: Leaf<'a>, kids: &Rc<[usize]>) {
        if let Some(Some(field)) = self.field_of(at, key) {
            self.sheet.entities[at].write(field, leaf, kids);
        }
    }

    fn member(&mut self, at: usize, block: usize, event: Event, positions: &[usize]) {
        let plan = self.plan;
        let (kids, parents) = self.kids(at, positions);
        if let (None, Some(table)) = (Self::native(plan, block), &plan.tables[block]) {
            let units = Units {
                member: Some(at),
                units: kids,
                owners: parents,
            };
            return match event {
                Event::Select(block) => self.table_select(block, table, &units),
                Event::Commit(block) => self.table_commit(block, table, &units),
            };
        }
        match (event, Self::native(plan, block)) {
            (Event::Commit(_), Some(Native::Expression { key, program })) => {
                let (leaf, failed) = self.member_run(at, program, &kids, &parents);
                self.member_fail(&parents, &failed);
                self.member_write(at, key, leaf, &kids);
            }
            (Event::Commit(_), Some(Native::Assertion { ir, conditions })) => {
                let mut truths: Vec<Vec<u64>> = Vec::with_capacity(conditions.len());
                for program in conditions {
                    let (leaf, failed) = self.member_run(at, program, &kids, &parents);
                    self.member_fail(&parents, &failed);
                    truths.push(leaf.truths(kids.len()));
                }
                let bits = Self::folded(ir, &truths, kids.len());
                let leaf = Leaf::typed(Array::parts(crate::compiled::typed::Store::Bool(bits), None, kids.len()));
                self.member_write(at, &ir.output, leaf, &kids);
            }
            (Event::Select(_), Some(Native::Match { pick: Some(pick), .. })) => {
                let total = self.sheet.entities[at].children;
                let (leaf, failed) = self.member_run(at, pick, &kids, &parents);
                self.member_fail(&parents, &failed);
                let picked = Self::arms_of(&leaf, kids.len());
                let slot = self.members.iter().position(|(b, _)| *b == block);
                match slot {
                    None if self.identity(at, &kids) => self.members.push((block, picked)),
                    None => {
                        let mut chosen: Vec<Option<usize>> = vec![None; total];
                        kids.iter().zip(picked).for_each(|(&child, arm)| chosen[child] = arm);
                        self.members.push((block, chosen));
                    }
                    Some(at) => {
                        let arms = &mut self.members[at].1;
                        kids.iter().zip(picked).for_each(|(&child, arm)| arms[child] = arm);
                    }
                }
            }
            (Event::Select(_), Some(Native::Match { conditions, .. })) => {
                let total = self.sheet.entities[at].children;
                let mut chosen: Vec<Option<usize>> = vec![None; total];
                let mut open: Vec<usize> = (0..kids.len()).collect();
                for (arm, condition) in conditions.iter().enumerate() {
                    if open.is_empty() {
                        break;
                    }
                    let Some(program) = condition else {
                        open.drain(..).for_each(|local| chosen[kids[local]] = Some(arm));
                        break;
                    };
                    let subset: Rc<[usize]> = open.iter().map(|&local| kids[local]).collect();
                    let owners: Rc<[usize]> = open.iter().map(|&local| parents[local]).collect();
                    let (leaf, failed) = self.member_run(at, program, &subset, &owners);
                    self.member_fail(&owners, &failed);
                    let mut next = Vec::with_capacity(open.len());
                    for (i, &local) in open.iter().enumerate() {
                        match leaf.truthy(i) {
                            true => chosen[kids[local]] = Some(arm),
                            false => next.push(local),
                        }
                    }
                    open = next;
                }
                match self.members.iter_mut().find(|(b, _)| *b == block) {
                    Some((_, arms)) => kids.iter().for_each(|&child| arms[child] = chosen[child]),
                    None => self.members.push((block, chosen)),
                }
            }
            (Event::Commit(_), Some(Native::Match { ir, values, constants, .. })) if Self::constant(values, constants) => {
                let codes: Vec<i32> = match self.members.iter().find(|(b, _)| *b == block) {
                    Some((_, arms)) => kids.iter().map(|&child| Self::code(arms.get(child).copied().flatten(), values)).collect(),
                    None => vec![-1; kids.len()],
                };
                self.member_write(at, &ir.key, Self::coded(&Self::fixed(constants), codes), &kids);
            }
            (Event::Commit(_), Some(Native::Match { ir, values, constants, .. })) => {
                let arms = self.members.iter().find(|(b, _)| *b == block).map(|(_, arms)| arms.clone()).unwrap_or_default();
                let mut out: Vec<Option<(usize, usize)>> = vec![None; kids.len()];
                let mut leaves: Vec<Leaf<'a>> = Vec::new();
                let mut groups: Vec<Vec<usize>> = vec![Vec::new(); ir.arms.len()];
                for (local, &child) in kids.iter().enumerate() {
                    if let Some(arm) = arms.get(child).copied().flatten().filter(|&arm| values[arm].is_some()) {
                        groups[arm].push(local);
                    }
                }
                if groups.iter().enumerate().all(|(arm, locals)| locals.is_empty() || constants.get(arm).is_some_and(Option::is_some)) {
                    let values: Vec<Variable> = constants.iter().map(|c| c.as_ref().map_or(Variable::Null, Fixed::variable)).collect();
                    let mut codes = vec![-1i32; kids.len()];
                    for (arm, locals) in groups.iter().enumerate() {
                        locals.iter().for_each(|&local| codes[local] = arm as i32);
                    }
                    self.member_write(at, &ir.key, Self::coded(&values, codes), &kids);
                    return;
                }
                let fixed: Vec<(usize, Variable)> = constants.iter().enumerate().filter_map(|(arm, c)| Some((arm, c.as_ref()?.variable()))).collect();
                if !fixed.is_empty() {
                    let mut builder = ColumnBuilder::with_capacity(fixed.len());
                    fixed.iter().for_each(|(_, value)| builder.push_variable(value.clone()));
                    let table = Leaf::typed(builder.finish());
                    for (slot, (arm, _)) in fixed.iter().enumerate() {
                        groups[*arm].drain(..).for_each(|local| out[local] = Some((leaves.len(), slot)));
                    }
                    leaves.push(table);
                }
                for (arm, locals) in groups.into_iter().enumerate() {
                    let Some(program) = values[arm].as_ref().filter(|_| !locals.is_empty()) else {
                        continue;
                    };
                    let subset: Rc<[usize]> = locals.iter().map(|&local| kids[local]).collect();
                    let owners: Rc<[usize]> = locals.iter().map(|&local| parents[local]).collect();
                    let (leaf, failed) = self.member_run(at, program, &subset, &owners);
                    self.member_fail(&owners, &failed);
                    for (i, &local) in locals.iter().enumerate() {
                        out[local] = Some((leaves.len(), i));
                    }
                    leaves.push(leaf);
                }
                let columns: Vec<Column> = leaves.iter().map(Leaf::column).collect();
                let mut builder = ColumnBuilder::with_capacity(out.len());
                for cell in &out {
                    match cell {
                        Some((leaf, i)) => builder.push_cell(&columns[*leaf], *i),
                        None => builder.push_null(),
                    }
                }
                self.member_write(at, &ir.key, Leaf::typed(builder.finish()), &kids);
            }
            _ => {}
        }
    }

    fn member_demand(&self, block: usize, row: usize) -> Option<Demand> {
        let at = self.entity_of(block)?;
        let (_, arms) = self.members.iter().find(|(b, _)| *b == block)?;
        let Some(Native::Match { ir, .. }) = Self::native(self.plan, block) else {
            return None;
        };
        let plan = self.artifact.read_plans.get(&self.plan.blocks.refs[block])?;
        let (a, b) = self.sheet.entities[at].range(row).unwrap_or((0, 0));
        let mut demanded: Vec<Arc<str>> = Vec::new();
        for child in a..b {
            let pick = Pick::Match(MatchSelection {
                matched_arm: arms.get(child).copied().flatten().and_then(|arm| ir.arms.get(arm)).map(|arm| arm.id.clone()),
                arms: Vec::new(),
            });
            pick.collect_reads(plan, &mut demanded);
        }
        demanded.sort();
        demanded.dedup();
        Some(demanded.into())
    }

    fn plain(leaf: &Leaf) -> bool {
        matches!(
            leaf,
            Leaf::Input(_) | Leaf::Typed(_) | Leaf::Masked(_) | Leaf::Scattered(_) | Leaf::Picked(_)
        ) && matches!(
            leaf.column().values,
            Values::Scaled { .. }
                | Values::Dec(_)
                | Values::I64(_)
                | Values::Bool { .. }
                | Values::Utf8 { .. }
                | Values::Text { .. }
                | Values::Dict {
                    values: Dictionary::Scaled { .. } | Dictionary::Text { .. } | Dictionary::Bool { .. },
                    ..
                }
        )
    }

    fn direct(&self, units: &Units, program: &LaneProgram) -> Option<Leaf<'a>> {
        let key = program.path()?;
        let leaf = match units.member {
            None => {
                if self.sheet.entities.iter().any(|e| e.active && Sheet::related(key, &e.path).is_some()) {
                    return None;
                }
                let Bind::Column(entry) = self.sheet.bind(key, &units.units, self.shared) else {
                    return None;
                };
                let leaf = &self.sheet.entries[entry].leaf;
                match units.units.len() == self.sheet.rows {
                    true => leaf.clone(),
                    false => leaf.pick(&units.units),
                }
            }
            Some(at) => match self.site(at, Some(key), &units.owners) {
                Member::Field(field) if self.identity(at, &units.units) => self.sheet.entities[at].view(field),
                Member::Field(field) => self.sheet.entities[at].view(field).pick(&units.units),
                Member::Root(entry) => self.sheet.entries[entry].leaf.pick(&units.owners),
                _ => return None,
            },
        };
        Self::plain(&leaf).then_some(leaf)
    }

    fn evaluate(&mut self, units: &Units, program: &LaneProgram) -> Leaf<'a> {
        match units.member {
            None => {
                let (leaf, failed) = self.run(program, &units.units);
                self.fail(&units.units, &failed);
                leaf
            }
            Some(at) => {
                let (leaf, failed) = self.member_run(at, program, &units.units, &units.owners);
                self.member_fail(&units.owners, &failed);
                leaf
            }
        }
    }

    fn table_select(&mut self, block: usize, table: &TableNative, units: &Units) {
        let count = units.units.len();
        let words = table.words;
        let tail = match (table.rules, table.rules % 64) {
            (0, _) => 0,
            (_, 0) => u64::MAX,
            (_, t) => (1u64 << t) - 1,
        };
        let mut all = vec![u64::MAX; count * words];
        all.chunks_exact_mut(words).for_each(|row| {
            if let Some(last) = row.last_mut() {
                *last &= tail;
            }
        });
        let mut bits = Vec::new();
        let mut host = vec![false; count];
        for input in &table.inputs {
            let leaf = match self.direct(units, &input.field) {
                Some(leaf) => leaf,
                None => self.evaluate(units, &input.field),
            };
            let column = leaf.column();
            if input.ordered {
                for local in 0..count {
                    if !column.valid(local) || column.number(local).is_none() {
                        host[local] = true;
                    }
                }
            }
            input.cells.indexed_column(&column, count, &mut bits);
            for other in 0..input.cells.others() {
                let Some((program, rules)) = input.cells.other(other) else {
                    continue;
                };
                let locals: Vec<usize> = (0..count)
                    .filter(|&local| all[local * words..(local + 1) * words].iter().zip(rules).any(|(a, r)| a & r != 0))
                    .collect();
                if locals.is_empty() {
                    continue;
                }
                let scopes: Vec<Scope> = locals
                    .iter()
                    .map(|&local| {
                        let mut scope = Scope::new(Variable::Null);
                        scope.set_local(Variable::dollar_key(), leaf.get(local));
                        scope
                    })
                    .collect();
                let mut passed: Vec<Option<bool>> = vec![None; locals.len()];
                self.runner.evaluate_bound(program, &scopes, &Columns::new(locals.len()), &|_| Binding::Row, None, |at, result| {
                    if let Some(slot) = passed.get_mut(at) {
                        *slot = result.ok().map(|values| matches!(values.as_slice(), [Variable::Bool(true)]));
                    }
                });
                for (&local, pass) in locals.iter().zip(passed) {
                    match pass {
                        None => host[local] = true,
                        Some(true) => bits[local * words..(local + 1) * words].iter_mut().zip(rules).for_each(|(b, r)| *b |= r),
                        Some(false) => {}
                    }
                }
            }
            all.iter_mut().zip(&bits).for_each(|(a, b)| *a &= b);
        }
        let outputs = table.outputs.len();
        let total = match units.member {
            Some(at) => self.sheet.entities[at].children,
            None => self.sheet.rows,
        };
        let at = match self.chosen.iter().position(|(b, _)| *b == block) {
            Some(at) => at,
            None => {
                self.chosen.push((block, vec![u32::MAX; total * outputs]));
                self.chosen.len() - 1
            }
        };
        for (local, &unit) in units.units.iter().enumerate() {
            let matched = &all[local * words..(local + 1) * words];
            for (c, output) in table.outputs.iter().enumerate() {
                let first = matched
                    .iter()
                    .zip(&output.filled)
                    .enumerate()
                    .find_map(|(w, (m, f))| (m & f != 0).then(|| (w * 64) as u32 + (m & f).trailing_zeros()));
                self.chosen[at].1[unit * outputs + c] = first.unwrap_or(u32::MAX);
            }
        }
        let hosted: Vec<usize> = (0..count).filter(|&local| host[local]).map(|local| units.owners[local]).collect();
        if hosted.is_empty() {
            return;
        }
        if units.member.is_some() {
            hosted.into_iter().for_each(|row| self.fall(row));
            return;
        }
        let at = match self.hosting.iter().position(|(b, _)| *b == block) {
            Some(at) => at,
            None => {
                self.hosting.push((block, vec![false; self.sheet.rows]));
                self.hosting.len() - 1
            }
        };
        hosted.iter().for_each(|&row| self.hosting[at].1[row] = true);
        self.delegate(Event::Select(block), &hosted);
    }

    fn hosted(&self, block: usize, row: usize) -> bool {
        self.hosting.iter().any(|(b, rows)| *b == block && rows[row])
    }

    fn table_commit(&mut self, block: usize, table: &TableNative, units: &Units) {
        let outputs = table.outputs.len();
        let slot = self.chosen.iter().position(|(b, _)| *b == block);
        let chosen = slot.map(|at| std::mem::take(&mut self.chosen[at].1)).unwrap_or_default();
        for (c, output) in table.outputs.iter().enumerate() {
            if let Some((values, remap)) = Self::table_constants(output) {
                let codes: Vec<i32> = units
                    .units
                    .iter()
                    .map(|&unit| {
                        let rule = chosen.get(unit * outputs + c).copied().unwrap_or(u32::MAX);
                        remap.get(rule as usize).copied().unwrap_or(-1)
                    })
                    .collect();
                let leaf = Self::coded(&values, codes);
                match units.member {
                    Some(at) => self.member_write(at, &output.field, leaf, &units.units),
                    None => self.write(&output.field, leaf, &units.units),
                }
                continue;
            }
            let mut values: Vec<Variable> = vec![Variable::Null; units.units.len()];
            let mut groups: Vec<(u32, Vec<usize>)> = Vec::new();
            for (local, &unit) in units.units.iter().enumerate() {
                let rule = chosen.get(unit * outputs + c).copied().unwrap_or(u32::MAX);
                if rule == u32::MAX {
                    continue;
                }
                match groups.iter_mut().find(|(r, _)| *r == rule) {
                    Some((_, locals)) => locals.push(local),
                    None => groups.push((rule, vec![local])),
                }
            }
            if groups.iter().all(|(rule, _)| output.constants.get(*rule as usize).is_some_and(Option::is_some)) {
                let mut values: Vec<Variable> = Vec::with_capacity(groups.len());
                let mut codes = vec![-1i32; units.units.len()];
                for (rule, locals) in &groups {
                    let code = values.len() as i32;
                    values.push(output.constants[*rule as usize].as_ref().map_or(Variable::Null, Fixed::variable));
                    locals.iter().for_each(|&local| codes[local] = code);
                }
                let leaf = Self::coded(&values, codes);
                match units.member {
                    Some(at) => self.member_write(at, &output.field, leaf, &units.units),
                    None => self.write(&output.field, leaf, &units.units),
                }
                continue;
            }
            for (rule, locals) in groups {
                if let Some(Some(value)) = output.constants.get(rule as usize) {
                    let value = value.variable();
                    locals.iter().for_each(|&local| values[local] = value.clone());
                    continue;
                }
                let Some(Some(program)) = output.cells.get(rule as usize) else {
                    continue;
                };
                let leaf = self.evaluate(&units.subset(&locals), program);
                for (i, &local) in locals.iter().enumerate() {
                    values[local] = leaf.get(i);
                }
            }
            let mut builder = ColumnBuilder::with_capacity(values.len());
            values.into_iter().for_each(|value| builder.push_variable(value));
            let leaf = Leaf::typed(builder.finish());
            match units.member {
                Some(at) => self.member_write(at, &output.field, leaf, &units.units),
                None => self.write(&output.field, leaf, &units.units),
            }
        }
        if let Some(at) = slot {
            self.chosen[at].1 = chosen;
        }
    }

    fn table_constants(output: &super::table::TableOutput) -> Option<(Vec<Variable>, Vec<i32>)> {
        let mut values = Vec::new();
        let mut remap = vec![-1i32; output.constants.len()];
        for (rule, constant) in output.constants.iter().enumerate() {
            if !Bits::get(&output.filled, rule) {
                continue;
            }
            remap[rule] = values.len() as i32;
            values.push(constant.as_ref()?.variable());
        }
        Some((values, remap))
    }

    fn table_cells(table: &TableNative, chosen: &[u32], unit: usize) -> Vec<(u32, Arc<str>)> {
        let outputs = table.outputs.len();
        let mut used: Vec<(u32, usize)> = (0..outputs)
            .filter_map(|c| {
                let rule = chosen.get(unit * outputs + c).copied()?;
                (rule != u32::MAX).then_some((rule, c))
            })
            .collect();
        used.sort();
        used.into_iter().map(|(rule, c)| (rule, table.outputs[c].id.clone())).collect()
    }

    fn table_demand(&self, block: usize, row: usize) -> Option<Demand> {
        if self.hosted(block, row) {
            return None;
        }
        let table = self.plan.tables[block].as_ref()?;
        let (_, chosen) = self.chosen.iter().find(|(b, _)| *b == block)?;
        let plan = self.artifact.read_plans.get(&self.plan.blocks.refs[block])?;
        let mut demanded = Vec::new();
        match self.entity_of(block) {
            None => Pick::Table(TableSelection::used(Self::table_cells(table, chosen, row))).collect_reads(plan, &mut demanded),
            Some(at) => {
                let (a, b) = self.sheet.entities[at].range(row).unwrap_or((0, 0));
                for child in a..b {
                    Pick::Table(TableSelection::used(Self::table_cells(table, chosen, child))).collect_reads(plan, &mut demanded);
                }
                demanded.sort();
                demanded.dedup();
            }
        }
        Some(demanded.into())
    }
}

impl PolicyPlan {
    fn scalar(program: &LaneProgram) -> bool {
        let p = program.program();
        let regs = match p.outputs.is_empty() {
            true => std::slice::from_ref(&p.out),
            false => p.outputs.as_slice(),
        };
        regs.iter().all(|r| matches!(p.kinds.get(*r as usize), Some(LaneKind::Num | LaneKind::Bool | LaneKind::Str)))
    }

    fn safe(&self, artifact: &EvalArtifact) -> &[Iteration] {
        self.safe.get_or_init(|| {
            let mut found: Vec<Iteration> = Vec::new();
            for (name, path, owner) in self.iterations.iter().flatten() {
                if !found.iter().any(|(_, p, _)| p == path) {
                    found.push((name.clone(), path.clone(), owner.clone()));
                }
            }
            found.retain(|(name, path, _)| self.members_safe(name, path) && self.readers_safe(artifact, path));
            found
        })
    }

    fn entities<'a>(&self, artifact: &EvalArtifact, sheet: &Sheet<'a>) -> Vec<Entity<'a>> {
        if !artifact.reference_fields.is_empty() {
            return Vec::new();
        }
        self.safe(artifact)
            .iter()
            .filter_map(|(name, path, owner)| {
                let (name, path, owner) = (name.clone(), path.clone(), owner.clone());
                let entry = sheet.entries.iter().position(|e| e.input && e.path == path)?;
                if sheet.entries.iter().any(|e| Sheet::related(&e.path, &path) == Some(std::cmp::Ordering::Greater)) {
                    return None;
                }
                Entity::new(name.clone(), path.clone(), owner.clone(), entry, sheet.inputs[entry])
                    .or_else(|| Shred::of(&sheet.inputs[entry], sheet.rows).map(|shred| Entity::shredded(name, path, owner, entry, shred)))
            })
            .collect()
    }

    fn members_safe(&self, name: &str, path: &str) -> bool {
        let iterations = self.iterations.iter();
        let prefix = format!("{name}.");
        iterations.enumerate().all(|(block, iteration)| {
            let Some((_, own, _)) = iteration else {
                return true;
            };
            if Sheet::related(own, path) == Some(std::cmp::Ordering::Greater) {
                return false;
            }
            if own.as_ref() != path {
                return true;
            }
            if self.members[block].is_none() && self.tables[block].is_none() {
                return false;
            }
            let Some((programs, keys)) = Pass::block(self, block) else {
                return false;
            };
            let keyed = keys.iter().all(|key| key.is_empty() || key.strip_prefix(prefix.as_str()).is_some_and(|f| !f.is_empty()));
            keyed
                && programs.into_iter().all(|program| {
                    program.program().site_keys.iter().all(|k| match k.as_deref() {
                        None => true,
                        Some(k) if k == name || k.starts_with('$') => false,
                        Some(k) => match k.strip_prefix(prefix.as_str()) {
                            Some(field) => !field.is_empty(),
                            None => Sheet::related(k, path).is_none(),
                        },
                    })
                })
        })
    }

    fn readers_safe(&self, artifact: &EvalArtifact, path: &str) -> bool {
        self.blocks.rules.iter().enumerate().all(|(block, rule)| {
            if self.iterations[block].as_ref().is_some_and(|(_, own, _)| own.as_ref() == path) {
                return true;
            }
            let writes = rule.kind.write_sites().iter().any(|site| Sheet::related(&site.path, path).is_some());
            let reads = artifact.reads.get(&self.blocks.refs[block]).map_or(Vec::new(), |reads| {
                reads.iter().filter_map(|read| Sheet::related(&read.path, path)).collect::<Vec<_>>()
            });
            if writes || reads.contains(&std::cmp::Ordering::Less) {
                return false;
            }
            reads.is_empty()
                || matches!(
                    (&self.natives[block], self.iterated[block], &self.tables[block], &self.iterations[block]),
                    (Some(_), false, _, None) | (None, _, Some(_), None)
                )
        })
    }

    pub(crate) fn evaluate_columns<'a>(
        &self,
        artifact: &EvalArtifact,
        policy_path: &Arc<str>,
        goals: &[Arc<str>],
        columns: &'a Columns<'a>,
    ) -> PolicyColumnarOutput<'a> {
        let rows = columns.rows;
        let request = |row: usize| EvaluateRequest {
            policy_path: policy_path.clone(),
            input: columns.row(row),
            goals: goals.to_vec(),
            trace: false,
        };
        let sheet = Sheet::new(columns);
        let Some(mut sheet) = sheet else {
            let requests: Vec<EvaluateRequest> = (0..rows).map(request).collect();
            return PolicyColumnarOutput::from_results(rows, self.evaluate(artifact, &requests));
        };
        let mut errors: Vec<Option<EvaluationError>> = (0..rows).map(|_| None).collect();
        let stores: Vec<OnceCell<Variable>> = (0..rows).map(|_| OnceCell::new()).collect();
        sheet.entities = self.entities(artifact, &sheet);
        sheet.refs = artifact.reference_fields.iter().map(|field| field.path.clone()).collect();
        let shared = !artifact.reference_fields.is_empty()
            || self.iterations.iter().enumerate().any(|(block, iteration)| match iteration {
                Some((_, path, _)) => !sheet.entities.iter().any(|e| e.path == *path),
                None => self.natives[block].is_none() && self.tables[block].is_none(),
            });
        let mut fallback = vec![false; rows];
        let mut erred = false;
        let sure = match shared {
            true => None,
            false => Shred::sure(artifact, columns, &sheet.entities, goals),
        };
        sheet.entities.iter().filter_map(|entity| entity.hosted.as_deref()).flatten().for_each(|&row| fallback[row] = true);
        for row in 0..rows {
            if sure.as_ref().is_some_and(|sure| sure[row]) {
                continue;
            }
            let req = request(row);
            if artifact.input_schema.convert_dates(&req.input).is_some() {
                fallback[row] = true;
                continue;
            }
            match artifact.prepare(&req) {
                Err(error) => {
                    errors[row] = Some(error);
                    erred = true;
                }
                Ok((store, _)) if shared => {
                    let _ = stores[row].set(store);
                }
                Ok(_) => {}
            }
        }
        let base = sheet.entries.len();
        let live: Vec<usize> = match erred {
            true => (0..rows).filter(|&row| errors[row].is_none() && !fallback[row]).collect(),
            false => (0..rows).filter(|&row| !fallback[row]).collect(),
        };
        let mut runner = Self::RUNNER.with_borrow_mut(std::mem::take);
        let (sheet, fallback, errors, owned) = {
            let runner = &mut runner;
            let mut pass = Pass {
                plan: self,
                artifact,
                entry: policy_path,
                sheet,
                runner,
                fallback,
                arms: Vec::new(),
                stores: &stores,
                state: Rows {
                    drivers: Vec::new(),
                    picks: Vec::new(),
                    failures: errors,
                },
                synced: vec![0; rows],
                shared,
                base,
                chosen: Vec::new(),
                members: Vec::new(),
                hosting: Vec::new(),
                kin: Vec::new(),
                delegated: false,
                fell: 0,
                owners: self.iterations.iter().map(|iteration| iteration.as_ref().map(|(_, path, _)| path.clone())).collect(),
            };
            pass.walk(self.root.clone(), live);
            let owning = |row: usize| pass.state.drivers.get(row).is_some_and(Option::is_some) || pass.stores[row].get().is_some();
            let owns = (0..rows).any(owning);
            if owns {
                (0..pass.sheet.entities.len()).for_each(|at| pass.sheet.retire(at));
            }
            let owned: Vec<Option<Variable>> = match owns {
                false => Vec::new(),
                true => (0..rows)
                    .map(|row| match pass.state.drivers.get(row).is_some_and(Option::is_some) || pass.stores[row].get().is_some() {
                        true => pass.driver(row).map(|driver| driver.env().shallow_clone()).and(pass.stores[row].get().cloned()),
                        false => None,
                    })
                    .collect(),
            };
            erred |= pass.delegated;
            (pass.sheet, pass.fallback, pass.state.failures, owned)
        };
        Self::RUNNER.with_borrow_mut(|slot| *slot = runner);
        let mut errors = errors;
        let clean = |row: usize| !erred || errors[row].is_none();
        let hosted: Vec<usize> = (0..rows).filter(|&row| fallback[row] && clean(row)).collect();
        let finished: Vec<usize> = (0..rows).filter(|&row| !fallback[row] && clean(row)).collect();
        if hosted.is_empty() && owned.iter().all(Option::is_none) {
            let columns = sheet.output(&finished);
            return PolicyColumnarOutput {
                rows,
                columns,
                errors,
                hosted: 0,
            };
        }
        let requests: Vec<EvaluateRequest> = hosted.iter().map(|&row| request(row)).collect();
        let mut shredder = Shredder::new(rows);
        for &row in &finished {
            match owned.get(row) {
                Some(Some(store)) => shredder.visit(store, 0, row),
                _ => shredder.visit(&sheet.object(row), 0, row),
            }
        }
        for (&row, result) in hosted.iter().zip(self.evaluate(artifact, &requests)) {
            match result {
                Ok(result) => shredder.visit(&result.output, 0, row),
                Err(error) => errors[row] = Some(error),
            }
        }
        PolicyColumnarOutput {
            rows,
            columns: Sheet::columns(shredder.finish()),
            errors,
            hosted: hosted.len(),
        }
    }
}
