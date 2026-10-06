use crate::compiled::bound::{Bound, Outs};
use crate::compiled::data::Col;
use crate::compiled::data::Mask;
use crate::compiled::typed::{Array, Bits, ColumnBuilder, Dict, Leaf, Literal, Store};
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Binding, CellEnv, CellSet, Column as LaneColumn, LaneProgram, LaneRunner, Op, Pieces, SourceInfo, Values};
use zen_expression::Variable;
use zen_types::decision::{DecisionTableContent, DecisionTableHitPolicy};

pub(crate) enum Column {
    Field {
        field: Box<LaneProgram>,
        cells: Box<CellSet>,
        empty: Vec<u64>,
    },
    Plain {
        cells: Vec<(Option<LaneProgram>, Vec<u64>)>,
        empty: Vec<u64>,
        owners: Vec<Option<usize>>,
        optimistic: Vec<u64>,
    },
}

enum Deferred {
    Field {
        column: usize,
        reference: Option<String>,
        values: Vec<Variable>,
        scopes: Option<Vec<zen_expression::Scope>>,
        memo: Vec<Option<bool>>,
    },
    Plain {
        column: usize,
        memo: Vec<Option<bool>>,
    },
}

pub(crate) enum OutSource {
    Literal(Literal),
    Path(Arc<str>),
}

pub(crate) struct RuleOutputs {
    full: Option<LaneProgram>,
    literals: Option<Vec<Literal>>,
    sources: Option<Vec<OutSource>>,
    collect: Option<LaneProgram>,
    paths: Vec<(Arc<str>, bool, usize)>,
    slots: Arc<[usize]>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Num,
    Text,
    Bool,
    Any,
}

impl Kind {
    fn of_literal(literal: &Literal) -> Option<Kind> {
        match literal {
            Literal::Null => None,
            Literal::Num(_) if literal.scaled().is_some() => Some(Kind::Num),
            Literal::Str(_) => Some(Kind::Text),
            Literal::Bool(_) => Some(Kind::Bool),
            _ => Some(Kind::Any),
        }
    }

    fn of_array(array: &Array) -> Option<Kind> {
        match array.store() {
            Store::Scaled { .. } => Some(Kind::Num),
            Store::Text { .. } => Some(Kind::Text),
            Store::Bool(_) => Some(Kind::Bool),
            Store::Coded { .. } | Store::List { .. } => Some(Kind::Any),
            Store::Any(values) => values.iter().any(|v| !matches!(v, Variable::Null)).then_some(Kind::Any),
        }
    }

    fn join(a: Option<Kind>, b: Option<Kind>) -> Option<Kind> {
        match (a, b) {
            (None, k) | (k, None) => k,
            (Some(a), Some(b)) if a == b => Some(a),
            _ => Some(Kind::Any),
        }
    }
}

pub(crate) struct Batch {
    rule: usize,
    outputs: Vec<Array>,
}

pub(crate) struct First {
    rules: usize,
    codes: Vec<i32>,
    batches: Vec<Batch>,
    starts: Vec<usize>,
}

impl First {
    fn entries(&self) -> usize {
        self.rules + self.starts.last().copied().unwrap_or(0)
    }

    fn computed(&self, code: i32) -> Option<(usize, usize)> {
        let entry = usize::try_from(code).ok()?.checked_sub(self.rules)?;
        let batch = self.starts.partition_point(|&start| start <= entry).checked_sub(1)?;
        Some((batch, entry - self.starts[batch]))
    }

    fn rule(&self, code: i32) -> Option<usize> {
        let code = usize::try_from(code).ok()?;
        match code < self.rules {
            true => Some(code),
            false => self.computed(code as i32).and_then(|(batch, _)| self.batches.get(batch)).map(|b| b.rule),
        }
    }

    fn chosen(&self, row: usize) -> Option<usize> {
        self.codes.get(row).and_then(|&code| self.rule(code))
    }

    fn value(&self, row: usize, index: usize) -> Variable {
        self.codes
            .get(row)
            .and_then(|&code| self.computed(code))
            .and_then(|(batch, pos)| Some(self.batches.get(batch)?.outputs.get(index)?.column().variable(pos).deep_clone()))
            .unwrap_or(Variable::Null)
    }
}

pub(crate) enum TableOut {
    Leaves(Arc<[usize]>, Vec<Variable>),
    Value(Variable),
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Hit {
    First,
    Collect,
    FirstCollect,
}

pub(crate) struct Dispatch {
    pieces: Vec<Pieces>,
    strides: Vec<usize>,
    lut: Option<Vec<u32>>,
    candidates: Vec<Vec<u64>>,
    firsts: Vec<i32>,
    chosen: Option<Vec<i32>>,
}

impl Dispatch {
    const LUT: usize = 1 << 16;

    fn build(columns: &[Column], rules: usize, words: usize) -> Option<Dispatch> {
        let pieces = columns
            .iter()
            .map(|column| match column {
                Column::Field { cells, empty, .. } => cells.pieces(empty),
                Column::Plain { .. } => None,
            })
            .collect::<Option<Vec<Pieces>>>()?;
        let mut strides = Vec::with_capacity(pieces.len());
        let mut size = 1usize;
        for p in &pieces {
            strides.push(size);
            size = size.saturating_mul(p.len().max(1));
        }
        let mut candidates: Vec<Vec<u64>> = Vec::new();
        let lut = (size <= Self::LUT).then(|| {
            (0..size)
                .map(|index| {
                    let mut bits = vec![u64::MAX; words];
                    if let Some(last) = bits.last_mut() {
                        *last = match (rules, rules % 64) {
                            (0, _) => 0,
                            (_, 0) => u64::MAX,
                            (_, t) => (1u64 << t) - 1,
                        };
                    }
                    for (p, stride) in pieces.iter().zip(&strides) {
                        let piece = (index / stride) % p.len().max(1);
                        if let Some(b) = p.bits.get(piece) {
                            bits.iter_mut().zip(b).for_each(|(a, b)| *a &= b);
                        }
                    }
                    match candidates.iter().position(|c| *c == bits) {
                        Some(at) => at as u32,
                        None => {
                            candidates.push(bits);
                            (candidates.len() - 1) as u32
                        }
                    }
                })
                .collect()
        });
        let firsts: Vec<i32> = candidates
            .iter()
            .map(|bits| {
                bits.iter()
                    .enumerate()
                    .find(|(_, w)| **w != 0)
                    .map_or(-1, |(i, w)| (i * 64 + w.trailing_zeros() as usize) as i32)
            })
            .collect();
        let chosen = lut.as_ref().map(|lut: &Vec<u32>| {
            lut.iter()
                .map(|&at| firsts.get(at as usize).copied().unwrap_or(-1))
                .collect()
        });
        Some(Dispatch {
            pieces,
            strides,
            lut,
            candidates,
            firsts,
            chosen,
        })
    }
}

enum Frozen {
    Text { offsets: Vec<i32>, data: String },
    Scaled { mant: Vec<i64>, scale: Vec<u8> },
    Bool(Vec<u64>),
}

impl Frozen {
    fn of(dict: Dict) -> Option<Frozen> {
        match dict {
            Dict::Text { offsets, data } => Some(Frozen::Text { offsets, data }),
            Dict::Scaled { mant, scale } => Some(Frozen::Scaled { mant, scale }),
            Dict::Bool(bits) => Some(Frozen::Bool(bits)),
            Dict::Any(_) => None,
        }
    }

    fn thaw(&self) -> Dict {
        match self {
            Frozen::Text { offsets, data } => Dict::Text {
                offsets: offsets.clone(),
                data: data.clone(),
            },
            Frozen::Scaled { mant, scale } => Dict::Scaled {
                mant: mant.clone(),
                scale: scale.clone(),
            },
            Frozen::Bool(bits) => Dict::Bool(bits.clone()),
        }
    }
}

type Static = Option<(Frozen, Vec<u64>, Vec<u64>)>;
type Keyed<'k, T> = (Cow<'k, [u64]>, Rc<[T]>);
type Found<'k> = (Rc<Dict>, Cow<'k, [u64]>, Cow<'k, [u64]>);

struct FusedCells {
    program: LaneProgram,
    cells: Vec<(usize, usize)>,
}

pub(crate) struct Table {
    id: u64,
    fused: Option<FusedCells>,
    positions: Vec<Option<u32>>,
    statics: Vec<Static>,
    dispatch: Option<Dispatch>,
    all_paths: Arc<[Arc<str>]>,
    hit: Hit,
    rules: usize,
    words: usize,
    columns: Vec<Column>,
    outputs: Vec<RuleOutputs>,
    collect_columns: Vec<(usize, Arc<str>)>,
    split: Option<Arc<[Arc<str>]>>,
    literal: bool,
    pub stable: bool,
}

enum Part {
    Value(usize),
    Child(Built),
}

struct Built {
    shape: Rc<zen_types::variable::Shape>,
    parts: Vec<Part>,
}

impl Built {
    fn of(paths: &[(Vec<&str>, usize)]) -> Option<Built> {
        let mut keys: Vec<&str> = Vec::new();
        for (segments, _) in paths {
            let head = *segments.first()?;
            if !keys.contains(&head) {
                keys.push(head);
            }
        }
        let mut map = zen_types::variable::VariableMap::with_capacity(keys.len());
        let mut parts = Vec::with_capacity(keys.len());
        for key in &keys {
            let group: Vec<&(Vec<&str>, usize)> = paths.iter().filter(|(segments, _)| segments.first() == Some(key)).collect();
            let part = match group.as_slice() {
                [(segments, index)] if segments.len() == 1 => Part::Value(*index),
                _ if group.iter().all(|(segments, _)| segments.len() > 1) => {
                    let rest: Vec<(Vec<&str>, usize)> = group.iter().map(|(segments, index)| (segments[1..].to_vec(), *index)).collect();
                    Part::Child(Built::of(&rest)?)
                }
                _ => return None,
            };
            map.insert(zen_types::symbol::Symbol::from(*key), Variable::Null);
            parts.push(part);
        }
        let shape = map.shape().filter(|_| map.len() == keys.len())?.clone();
        Some(Built { shape, parts })
    }

    fn make(&self, values: &[Variable]) -> Variable {
        let values = self.parts.iter().map(|part| match part {
            Part::Value(index) => values.get(*index).map_or(Variable::Null, Variable::deep_clone),
            Part::Child(child) => child.make(values),
        });
        Variable::from_object(zen_types::variable::VariableMap::from_shape(self.shape.clone(), values))
    }
}

pub(crate) struct Collected<'a> {
    pub paths: Arc<[Arc<str>]>,
    pub columns: Vec<Leaf<'a>>,
    pub present: Vec<Mask<'a>>,
    pub unmatched: Vec<u64>,
}

impl Table {
    pub fn compile(content: &DecisionTableContent) -> Result<Self, String> {
        let rules = content.rules.len();
        let words = rules.div_ceil(64).max(1);
        let bit = |bits: &mut Vec<u64>, rule: usize| bits[rule / 64] |= 1 << (rule % 64);
        let mut columns = Vec::with_capacity(content.inputs.len());
        let mut plain_sources: Vec<(usize, usize, Arc<str>)> = Vec::new();
        for input in content.inputs.iter() {
            let cells: Vec<Option<&str>> = content
                .rules
                .iter()
                .map(|rule| rule.get(&input.id).map(|c| c.as_ref()).filter(|c| !c.is_empty()))
                .collect();
            let mut empty = vec![0u64; words];
            for (rule, cell) in cells.iter().enumerate() {
                if cell.is_none() {
                    bit(&mut empty, rule);
                }
            }
            match &input.field {
                Some(field) => {
                    let program = LaneProgram::standard(field).map_err(|e| e.to_string())?;
                    let set = CellSet::compile(&cells).map_err(|e| e.to_string())?;
                    columns.push(Column::Field {
                        field: Box::new(program),
                        cells: Box::new(set),
                        empty,
                    });
                }
                None => {
                    let mut distinct: Vec<(Arc<str>, Option<LaneProgram>, Vec<u64>)> = Vec::new();
                    for (rule, cell) in cells.iter().enumerate() {
                        let Some(cell) = cell else {
                            continue;
                        };
                        if SourceInfo::reads_dollar(cell) {
                            return Err("table cell without a field reads `$`".into());
                        }
                        match distinct.iter_mut().find(|(s, _, _)| s.as_ref() == *cell) {
                            Some((_, _, bits)) => bit(bits, rule),
                            None => {
                                let mut bits = vec![0u64; words];
                                bit(&mut bits, rule);
                                distinct.push((Arc::from(*cell), LaneProgram::standard(cell).ok(), bits));
                            }
                        }
                    }
                    let mut owners = vec![None; rules];
                    let mut optimistic = vec![0u64; words];
                    for (index, (_, program, bits)) in distinct.iter().enumerate() {
                        if program.is_none() {
                            continue;
                        }
                        optimistic.iter_mut().zip(bits).for_each(|(a, b)| *a |= b);
                        for (rule, owner) in owners.iter_mut().enumerate() {
                            if bits[rule / 64] >> (rule % 64) & 1 == 1 {
                                *owner = Some(index);
                            }
                        }
                    }
                    for (cell, (source, program, _)) in distinct.iter().enumerate() {
                        if program.is_some() {
                            plain_sources.push((columns.len(), cell, source.clone()));
                        }
                    }
                    columns.push(Column::Plain {
                        cells: distinct.into_iter().map(|(_, p, b)| (p, b)).collect(),
                        empty,
                        owners,
                        optimistic,
                    });
                }
            }
        }

        let mut all_paths: Vec<Arc<str>> = Vec::new();
        for output in content.outputs.iter() {
            let (path, _) = output.write_path();
            if !path.is_empty() && !all_paths.iter().any(|p| p.as_ref() == path) {
                all_paths.push(Arc::from(path));
            }
        }
        let mut outputs = Vec::with_capacity(rules);
        for rule in content.rules.iter() {
            let mut paths = Vec::new();
            let mut full: Vec<(String, &str)> = Vec::new();
            let mut collect: Vec<(String, &str)> = Vec::new();
            for (column, output) in content.outputs.iter().enumerate() {
                let (path, is_collect) = output.write_path();
                if path.is_empty() {
                    continue;
                }
                let Some(cell) = rule.get(&output.id).filter(|c| !c.is_empty()) else {
                    continue;
                };
                if SourceInfo::reads_dollar(cell) {
                    return Err("table output cell reads `$`".into());
                }
                let key = format!("o{}", full.len());
                full.push((key, cell.as_ref()));
                if is_collect {
                    collect.push((format!("o{}", collect.len()), cell.as_ref()));
                }
                paths.push((Arc::from(path), is_collect, column));
            }
            let many = |entries: &[(String, &str)]| {
                let refs: Vec<(&str, &str)> = entries.iter().map(|(k, v)| (k.as_str(), *v)).collect();
                LaneProgram::compile_many(&refs, false).ok()
            };
            let literals = full
                .iter()
                .map(|(_, cell)| {
                    let program = LaneProgram::standard(cell).ok()?;
                    let value = program.literal().or_else(|| {
                        let p = program.program();
                        match p.site_keys.is_empty() && p.timeless() && !p.opaque() {
                            true => LaneRunner::new().evaluate_one(&program, &zen_expression::Scope::default()).ok(),
                            false => None,
                        }
                    })?;
                    Literal::of(&value)
                })
                .collect::<Option<Vec<_>>>();
            let sources = match literals {
                Some(_) => None,
                None => full
                    .iter()
                    .map(|(_, cell)| {
                        let program = LaneProgram::standard(cell).ok()?;
                        match program.literal().as_ref().and_then(Literal::of) {
                            Some(literal) => Some(OutSource::Literal(literal)),
                            None => program
                                .path()
                                .filter(|path| !path.starts_with('$'))
                                .map(|path| OutSource::Path(Arc::from(path))),
                        }
                    })
                    .collect::<Option<Vec<_>>>(),
            };
            outputs.push(RuleOutputs {
                literals,
                sources,
                full: many(&full),
                collect: many(&collect),
                slots: paths
                    .iter()
                    .map(|(p, _, _): &(Arc<str>, bool, usize)| {
                        all_paths.iter().position(|a| a == p).unwrap_or_default()
                    })
                    .collect(),
                paths,
            });
        }

        let has_collect = content.outputs.iter().any(|o| o.write_path().1);
        let hit = match (&content.hit_policy, has_collect) {
            (DecisionTableHitPolicy::First, true) => Hit::FirstCollect,
            (DecisionTableHitPolicy::First, false) => Hit::First,
            (DecisionTableHitPolicy::Collect, _) => Hit::Collect,
        };
        let collect_columns = content
            .outputs
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                let (path, collect) = o.write_path();
                (collect && !path.is_empty()).then(|| (i, Arc::from(path)))
            })
            .collect::<Vec<(usize, Arc<str>)>>();
        let collected: Vec<&Arc<str>> = collect_columns.iter().map(|(_, p)| p).collect();
        let distinct = collected.iter().enumerate().all(|(i, p)| !collected[..i].contains(p));
        let scalars: Vec<Arc<str>> = all_paths.iter().filter(|p| !collected.contains(p)).cloned().collect();
        let disjoint = content.outputs.iter().all(|o| {
            let (path, collect) = o.write_path();
            collect || path.is_empty() || !collected.iter().any(|c| c.as_ref() == path)
        });
        let split = (hit == Hit::FirstCollect && distinct && disjoint)
            .then(|| scalars.into_iter().chain(collected.into_iter().cloned()).collect());

        let literal = outputs.iter().all(|o| o.paths.is_empty() || o.literals.is_some());
        let slots = all_paths.len();
        let mut positions: Vec<Option<u32>> = vec![None; outputs.len() * slots];
        for (rule, rule_outputs) in outputs.iter().enumerate() {
            for (index, &slot) in rule_outputs.slots.iter().enumerate() {
                if slot < slots {
                    positions[rule * slots + slot] = Some(index as u32);
                }
            }
        }
        let fused = Self::fused(&columns, &plain_sources, hit == Hit::First);
        let clocked = |text: &str| {
            text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|token| matches!(token, "rand" | "date" | "time" | "d" | "now" | "dateString" | "isToday"))
        };
        let stable = content.rules.iter().all(|rule| rule.values().all(|cell| !clocked(cell.as_ref())));
        let mut table = Self {
            id: Self::next_id(),
            stable,
            fused,
            positions,
            statics: Vec::new(),
            dispatch: Dispatch::build(&columns, rules, words),
            literal,
            split,
            all_paths: all_paths.into(),
            hit,
            rules,
            words,
            columns,
            outputs,
            collect_columns,
        };
        let empty = First {
            rules: table.outputs.len(),
            codes: Vec::new(),
            batches: Vec::new(),
            starts: vec![0],
        };
        let all = Bits::ones(table.outputs.len());
        table.statics = (0..slots)
            .map(|slot| {
                let computed = (0..table.outputs.len()).any(|rule| {
                    table.positions[rule * slots + slot].is_some() && table.outputs[rule].literals.is_none()
                });
                match computed {
                    true => None,
                    false => table
                        .slot(&empty, slot, slots, &table.positions, &all)
                        .and_then(|(dict, valid, written)| Some((Frozen::of(dict)?, valid, written))),
                }
            })
            .collect();
        Ok(table)
    }

    pub fn programs(&self) -> Vec<&LaneProgram> {
        let mut programs: Vec<&LaneProgram> = Vec::new();
        for column in &self.columns {
            match column {
                Column::Field { field, cells, .. } => {
                    programs.push(field);
                    programs.extend(cells.programs());
                }
                Column::Plain { cells, .. } => programs.extend(cells.iter().filter_map(|(p, _)| p.as_ref())),
            }
        }
        for outputs in &self.outputs {
            programs.extend(outputs.full.iter());
            programs.extend(outputs.collect.iter());
        }
        programs
    }

    pub fn paths(&self) -> &Arc<[Arc<str>]> {
        &self.all_paths
    }

    pub fn slot_kind(&self, path: &str) -> Option<u8> {
        let slot = self.all_paths.iter().position(|p| p.as_ref() == path)?;
        match self.statics.get(slot)?.as_ref()?.0 {
            Frozen::Scaled { .. } => Some(0),
            Frozen::Bool(_) => Some(1),
            Frozen::Text { .. } => Some(2),
        }
    }

    pub fn output_paths(&self) -> Arc<[Arc<str>]> {
        match &self.split {
            Some(split) if !self.first_hit() => split.clone(),
            _ => self.all_paths.clone(),
        }
    }

    pub fn value(&self, out: TableOut) -> Variable {
        match out {
            TableOut::Value(v) => v,
            TableOut::Leaves(slots, values) => {
                let object = Variable::empty_object();
                for (&slot, value) in slots.iter().zip(values) {
                    if let Some(path) = self.all_paths.get(slot) {
                        object.dot_insert(path, value);
                    }
                }
                object
            }
        }
    }

    fn field_column<'b>(runner: &mut LaneRunner, bound: &'b Bound, field: &LaneProgram, failed: &mut [bool]) -> (Option<LaneColumn<'b>>, Option<Array>) {
        let direct = field.path().and_then(|path| match (bound.bind)(path) {
            Binding::Column(index) => bound.columns.columns.get(index).map(|(_, c)| *c),
            Binding::Absent => Some(LaneColumn::new(Values::Any(&[]))),
            Binding::Row => None,
        });
        match direct {
            Some(column) => (Some(column), None),
            None => {
                let mut outs = Outs::take();
                bound.export(runner, field, None, &mut outs, |row, _, _| {
                    if let Some(f) = failed.get_mut(row) {
                        *f = true;
                    }
                });
                (None, outs.first_mut().map(Array::from_output))
            }
        }
    }

    fn keyed(&self, dispatch: &Dispatch, runner: &mut LaneRunner, bound: &Bound) -> Option<Vec<u32>> {
        dispatch.lut.as_ref()?;
        let rows = bound.rows();
        let mut keys = vec![0u32; rows];
        let mut pieces = vec![0u16; rows];
        let mut failed = vec![false; rows];
        for ((column, p), stride) in self.columns.iter().zip(&dispatch.pieces).zip(&dispatch.strides) {
            let Column::Field { field, cells, .. } = column else {
                continue;
            };
            failed.iter_mut().for_each(|f| *f = false);
            let (direct, typed) = Self::field_column(runner, bound, field, &mut failed);
            let view = direct.or_else(|| typed.as_ref().map(Array::column));
            match view {
                Some(view) => cells.classify(p, &view, rows, &mut pieces),
                None => pieces.iter_mut().for_each(|x| *x = p.failed),
            }
            let stride = *stride as u32;
            match failed.iter().any(|f| *f) {
                false => keys.iter_mut().zip(&pieces).for_each(|(key, piece)| *key += *piece as u32 * stride),
                true => {
                    for ((key, piece), failed) in keys.iter_mut().zip(&pieces).zip(&failed) {
                        *key += match failed {
                            true => p.failed as u32,
                            false => *piece as u32,
                        } * stride;
                    }
                }
            }
        }
        Some(keys)
    }

    fn dispatched(&self, dispatch: &Dispatch, runner: &mut LaneRunner, bound: &Bound) -> Vec<u64> {
        if let (Some(keys), Some(lut), 1) = (self.keyed(dispatch, runner, bound), &dispatch.lut, self.words) {
            return keys
                .iter()
                .map(|key| {
                    lut.get(*key as usize)
                        .and_then(|i| dispatch.candidates.get(*i as usize))
                        .and_then(|bits| bits.first().copied())
                        .unwrap_or(0)
                })
                .collect();
        }
        let (rows, words) = (bound.rows(), self.words);
        let mut keys = vec![0u32; rows];
        let mut pieces = vec![0u16; rows];
        let mut failed = vec![false; rows];
        let tail = match (self.rules, self.rules % 64) {
            (0, _) => 0,
            (_, 0) => u64::MAX,
            (_, t) => (1u64 << t) - 1,
        };
        let mut all: Vec<u64> = match dispatch.lut {
            Some(_) => Vec::new(),
            None => {
                let mut all = vec![u64::MAX; rows * words];
                for row in 0..rows {
                    all[row * words + words - 1] &= tail;
                }
                all
            }
        };
        for ((column, p), stride) in self.columns.iter().zip(&dispatch.pieces).zip(&dispatch.strides) {
            let Column::Field { field, cells, .. } = column else {
                continue;
            };
            failed.iter_mut().for_each(|f| *f = false);
            let (direct, typed) = Self::field_column(runner, bound, field, &mut failed);
            let view = direct.or_else(|| typed.as_ref().map(Array::column));
            match view {
                Some(view) => cells.classify(p, &view, rows, &mut pieces),
                None => pieces.iter_mut().for_each(|x| *x = p.failed),
            }
            for (piece, failed) in pieces.iter_mut().zip(&failed) {
                if *failed {
                    *piece = p.failed;
                }
            }
            match dispatch.lut {
                Some(_) => keys.iter_mut().zip(&pieces).for_each(|(k, piece)| *k += *piece as u32 * *stride as u32),
                None => {
                    for (row, piece) in pieces.iter().enumerate() {
                        if let Some(bits) = p.bits.get(*piece as usize) {
                            all[row * words..(row + 1) * words].iter_mut().zip(bits).for_each(|(a, b)| *a &= b);
                        }
                    }
                }
            }
        }
        if let Some(lut) = &dispatch.lut {
            all = Vec::with_capacity(rows * words);
            for key in &keys {
                let index = lut.get(*key as usize).copied().unwrap_or(0) as usize;
                match dispatch.candidates.get(index) {
                    Some(bits) => all.extend(bits.iter().enumerate().map(|(w, b)| if w + 1 == words { b & tail } else { *b })),
                    None => all.extend(std::iter::repeat_n(0, words)),
                }
            }
        }
        all
    }

    fn candidates(&self, runner: &mut LaneRunner, bound: &Bound, lazy: bool) -> (Vec<u64>, Vec<Deferred>) {
        if let Some(dispatch) = &self.dispatch {
            return (self.dispatched(dispatch, runner, bound), Vec::new());
        }
        let (rows, words) = (bound.rows(), self.words);
        let scopes = bound.scopes();
        let mut all = vec![u64::MAX; rows * words];
        let tail = match (self.rules, self.rules % 64) {
            (0, _) => 0,
            (_, 0) => u64::MAX,
            (_, t) => (1u64 << t) - 1,
        };
        for row in 0..rows {
            all[row * words + words - 1] &= tail;
        }
        let mut bits = Vec::new();
        let mut deferred = Vec::new();
        let fused = self.fused_truths(runner, bound);
        for (index, column) in self.columns.iter().enumerate() {
            match column {
                Column::Field { field, cells, empty } => {
                    let mut failed = vec![false; rows];
                    let direct = field.path().and_then(|path| match (bound.bind)(path) {
                        Binding::Column(index) => bound.columns.columns.get(index).map(|(_, c)| *c),
                        Binding::Absent => Some(LaneColumn::new(Values::Any(&[]))),
                        Binding::Row => None,
                    });
                    let typed = match direct {
                        Some(_) => None,
                        None => {
                            let mut outs = Outs::take();
                            bound.export(runner, field, None, &mut outs, |row, _, _| failed[row] = true);
                            outs.first_mut().map(Array::from_output)
                        }
                    };
                    let column = direct.or_else(|| typed.as_ref().map(Array::column));
                    if let (Some(truths), Some(view), true) = (&fused, direct, self.fuses(index)) {
                        cells.indexed_column(&view, rows, &mut bits);
                        for other in 0..cells.others() {
                            let (Some((_, rules)), Some(truth)) = (cells.other(other), self.fused_at(index, other).and_then(|at| truths.get(at))) else {
                                continue;
                            };
                            Self::spread_rules(&mut bits, words, truth, rules, rows);
                        }
                        all.iter_mut().zip(&bits).for_each(|(a, b)| *a &= b);
                        continue;
                    }
                    let values = |_: &Option<Array>| -> Vec<Variable> {
                        (0..rows)
                            .map(|row| column.map_or(Variable::Null, |c| c.variable(row)))
                            .collect()
                    };
                    let referenced = direct
                        .and(field.path())
                        .filter(|reference| Self::referenced(bound, cells, reference));
                    let peeled = cells.others() > 0
                        && cells.peelable()
                        && column.is_some_and(|column| Self::peeled(runner, cells, &column, rows, &mut bits));
                    match (lazy && !peeled, cells.others() > 0 && !peeled, column, referenced) {
                        _ if peeled => {}
                        (_, false, Some(column), _) => cells.indexed_column(&column, rows, &mut bits),
                        (true, true, Some(column), _) => cells.evaluate_indexed_column(&column, rows, &mut bits),
                        (false, true, Some(column), Some(reference)) => {
                            cells.indexed_column(&column, rows, &mut bits);
                            for other in 0..cells.others() {
                                let Some((_, rules)) = cells.other(other) else {
                                    continue;
                                };
                                let subset: Vec<usize> = (0..rows)
                                    .filter(|&row| all[row * words..(row + 1) * words].iter().zip(rules).any(|(a, r)| a & r != 0))
                                    .collect();
                                Self::referenced_truths(runner, bound, cells, other, reference, &subset, |row, pass| {
                                    if pass {
                                        bits[row * words..(row + 1) * words]
                                            .iter_mut()
                                            .zip(rules)
                                            .for_each(|(a, r)| *a |= r);
                                    }
                                });
                            }
                        }
                        _ => {
                            let env = CellEnv {
                                scopes,
                                columns: &bound.columns,
                                bind: &*bound.bind,
                            };
                            cells.evaluate_within(runner, &values(&typed), &env, Some(&all), &mut bits)
                        }
                    }
                    all.iter_mut().zip(&bits).for_each(|(a, b)| *a &= b);
                    for row in (0..rows).filter(|&row| failed[row]) {
                        all[row * words..(row + 1) * words]
                            .iter_mut()
                            .zip(empty)
                            .for_each(|(a, e)| *a &= e);
                    }
                    if lazy && cells.others() > 0 && !peeled {
                        deferred.push(Deferred::Field {
                            column: index,
                            reference: referenced.map(str::to_string),
                            values: match referenced {
                                Some(_) => Vec::new(),
                                None => values(&typed),
                            },
                            scopes: None,
                            memo: vec![None; rows * cells.others()],
                        });
                    }
                }
                Column::Plain { cells, empty, .. } if fused.is_some() && self.fuses(index) => {
                    let truths = fused.as_ref();
                    let mut passing = match words {
                        1 => vec![empty[0]; rows],
                        _ => {
                            let mut passing = vec![0u64; rows * words];
                            passing.chunks_exact_mut(words).for_each(|row| row.copy_from_slice(empty));
                            passing
                        }
                    };
                    for (cell, (_, rules)) in cells.iter().enumerate() {
                        let found = self.fused_at(index, cell).and_then(|at| truths.and_then(|t| t.get(at)));
                        let Some(truth) = found else {
                            continue;
                        };
                        Self::spread_rules(&mut passing, words, truth, rules, rows);
                    }
                    all.iter_mut().zip(&passing).for_each(|(a, p)| *a &= p);
                }
                Column::Plain {
                    cells,
                    empty,
                    optimistic,
                    ..
                } if lazy => {
                    for row in 0..rows {
                        all[row * words..(row + 1) * words]
                            .iter_mut()
                            .zip(empty.iter().zip(optimistic))
                            .for_each(|(a, (e, o))| *a &= e | o);
                    }
                    deferred.push(Deferred::Plain {
                        column: index,
                        memo: vec![None; rows * cells.len()],
                    });
                }
                Column::Plain { cells, empty, .. } => {
                    let mut passing = vec![0u64; rows * words];
                    for row in 0..rows {
                        passing[row * words..(row + 1) * words].copy_from_slice(empty);
                    }
                    for (program, rules) in cells {
                        let Some(program) = program else {
                            continue;
                        };
                        let subset: Vec<usize> = (0..rows)
                            .filter(|&row| {
                                all[row * words..(row + 1) * words]
                                    .iter()
                                    .zip(rules)
                                    .any(|(a, r)| a & r != 0)
                            })
                            .collect();
                        if subset.is_empty() {
                            continue;
                        }
                        let within = (subset.len() < rows).then_some(subset.as_slice());
                        Self::truths(runner, bound, program, within, |row, pass| {
                            if pass {
                                passing[row * words..(row + 1) * words]
                                    .iter_mut()
                                    .zip(rules)
                                    .for_each(|(a, b)| *a |= b);
                            }
                        });
                    }
                    all.iter_mut().zip(&passing).for_each(|(a, p)| *a &= p);
                }
            }
        }
        (all, deferred)
    }

    const FUSED: usize = 64;
    const THAWS: usize = 4096;
    fn next_id() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    thread_local! {
        static THAWED: std::cell::RefCell<ahash::HashMap<(u64, usize), Rc<Dict>>> = std::cell::RefCell::new(ahash::HashMap::default());
    }

    fn thawed(&self, slot: usize, frozen: &Frozen) -> Rc<Dict> {
        Self::THAWED.with_borrow_mut(|cache| {
            if cache.len() >= Self::THAWS {
                cache.clear();
            }
            cache.entry((self.id, slot)).or_insert_with(|| Rc::new(frozen.thaw())).clone()
        })
    }

    fn fused(columns: &[Column], plain: &[(usize, usize, Arc<str>)], lazy: bool) -> Option<FusedCells> {
        let mut entries: Vec<(Arc<str>, Option<Arc<str>>)> = Vec::new();
        let mut cells: Vec<(usize, usize)> = Vec::new();
        for (index, column) in columns.iter().enumerate() {
            match column {
                Column::Field { field, cells: set, .. } if !lazy && set.others() > 0 && !set.peelable() && field.path().is_some() => {
                    let Some(target) = field.source() else {
                        continue;
                    };
                    let sources: Option<Vec<Arc<str>>> = (0..set.others())
                        .map(|other| set.other(other).and_then(|(program, _)| program.source()).map(Arc::from))
                        .collect();
                    let Some(sources) = sources else {
                        continue;
                    };
                    for (other, source) in sources.into_iter().enumerate() {
                        entries.push((source, Some(Arc::from(target))));
                        cells.push((index, other));
                    }
                }
                Column::Plain { cells: programs, .. } if !lazy || plain.len() <= 8 || !programs.iter().flat_map(|(p, _)| p).any(Self::heavy) => {
                    for (_, cell, source) in plain.iter().filter(|(column, _, _)| *column == index) {
                        entries.push((source.clone(), None));
                        cells.push((index, *cell));
                    }
                }
                Column::Field { .. } | Column::Plain { .. } => {}
            }
        }
        if entries.is_empty() || entries.len() > Self::FUSED {
            return None;
        }
        let refs: Vec<(&str, Option<&str>)> = entries.iter().map(|(s, f)| (s.as_ref(), f.as_deref())).collect();
        LaneProgram::compile_cells(&refs).ok().map(|program| FusedCells { program, cells })
    }

    fn heavy(program: &LaneProgram) -> bool {
        program
            .program()
            .steps
            .iter()
            .any(|step| matches!(step.op, Op::Call { .. } | Op::Method { .. } | Op::Closure(_) | Op::LoadCall(_)))
    }

    fn fused_truths(&self, runner: &mut LaneRunner, bound: &Bound) -> Option<Vec<Vec<u64>>> {
        let fused = self.fused.as_ref()?;
        let p = fused.program.program();
        let bound_only = p
            .site_keys
            .iter()
            .all(|key| key.as_deref().is_some_and(|key| !matches!((bound.bind)(key), Binding::Row)));
        if p.opaque() || !bound_only {
            return None;
        }
        let rows = bound.rows();
        let mut outs = Outs::take();
        let mut failed: Vec<(usize, usize)> = Vec::new();
        bound.export(runner, &fused.program, None, &mut outs, |row, stage, _| failed.push((row, stage)));
        let mut truths: Vec<Vec<u64>> = outs
            .iter_mut()
            .map(|out| Leaf::typed(Array::from_output(out)).truths(rows))
            .collect();
        truths.resize(fused.cells.len(), vec![0u64; rows.div_ceil(64)]);
        for (row, stage) in failed {
            if let Some(truth) = truths.get_mut(stage) {
                Bits::set(truth, row, false);
            }
        }
        Some(truths)
    }

    fn fused_at(&self, column: usize, cell: usize) -> Option<usize> {
        self.fused.as_ref()?.cells.iter().position(|c| *c == (column, cell))
    }

    fn fuses(&self, column: usize) -> bool {
        self.fused.as_ref().is_some_and(|fused| fused.cells.iter().any(|(c, _)| *c == column))
    }

    fn peeled(runner: &mut LaneRunner, cells: &CellSet, column: &LaneColumn, rows: usize, out: &mut Vec<u64>) -> bool {
        let mut index: Vec<u32> = Vec::with_capacity(rows);
        let mut values: Vec<Variable> = Vec::new();
        let limit = rows / 2;
        let text = matches!(column.values, Values::Text { .. } | Values::Utf8 { .. } | Values::Strs(_));
        match text {
            true => {
                let mut seen: ahash::HashMap<&[u8], u32> = ahash::HashMap::default();
                let mut null: Option<u32> = None;
                for row in 0..rows {
                    let at = match column.valid(row).then(|| column.bytes(row)).flatten() {
                        Some(bytes) => match seen.get(bytes) {
                            Some(at) => *at,
                            None => {
                                let at = values.len() as u32;
                                values.push(std::str::from_utf8(bytes).map_or(Variable::Null, |t| Variable::String(t.into())));
                                seen.insert(bytes, at);
                                at
                            }
                        },
                        None => *null.get_or_insert_with(|| {
                            values.push(Variable::Null);
                            (values.len() - 1) as u32
                        }),
                    };
                    if values.len() > limit {
                        return false;
                    }
                    index.push(at);
                }
            }
            false => {
                let mut seen: Vec<Variable> = Vec::new();
                for row in 0..rows {
                    let value = column.variable(row);
                    if !matches!(value, Variable::Null | Variable::Bool(_) | Variable::Number(_) | Variable::String(_)) {
                        return false;
                    }
                    let same = |a: &Variable, b: &Variable| match (a, b) {
                        (Variable::Number(x), Variable::Number(y)) => x.serialize() == y.serialize(),
                        (x, y) => std::mem::discriminant(x) == std::mem::discriminant(y) && x == y,
                    };
                    let at = match seen.iter().position(|v| same(v, &value)) {
                        Some(at) => at,
                        None => {
                            seen.push(value);
                            seen.len() - 1
                        }
                    };
                    if seen.len() > limit.min(64) {
                        return false;
                    }
                    index.push(at as u32);
                }
                values = seen;
            }
        }
        let words = cells.words();
        let scopes = Bound::blank(values.len());
        let columns = zen_expression::lane::Columns::new(values.len());
        let env = CellEnv {
            scopes: scopes.slice(),
            columns: &columns,
            bind: &|_| Binding::Row,
        };
        let mut distinct = Vec::new();
        cells.evaluate_within(runner, &values, &env, None, &mut distinct);
        out.clear();
        out.reserve(rows * words);
        for at in &index {
            let at = *at as usize * words;
            out.extend_from_slice(&distinct[at..at + words]);
        }
        true
    }

    fn verify(
        &self,
        runner: &mut LaneRunner,
        bound: &Bound,
        deferred: &mut [Deferred],
        rule: usize,
        lanes: &mut Vec<usize>,
    ) {
        for entry in deferred.iter_mut() {
            if lanes.is_empty() {
                return;
            }
            match entry {
                Deferred::Field {
                    column,
                    reference,
                    values,
                    scopes,
                    memo,
                } => {
                    let Some(Column::Field { cells, .. }) = self.columns.get(*column) else {
                        continue;
                    };
                    let Some(other) = cells.owner(rule) else {
                        continue;
                    };
                    let stride = cells.others();
                    let unknown: Vec<usize> = lanes
                        .iter()
                        .copied()
                        .filter(|&row| memo[row * stride + other].is_none())
                        .collect();
                    match (unknown.is_empty(), reference) {
                        (true, _) => {}
                        (false, Some(reference)) => {
                            Self::referenced_truths(runner, bound, cells, other, reference, &unknown, |row, pass| {
                                memo[row * stride + other] = Some(pass)
                            })
                        }
                        (false, None) => {
                            let env = CellEnv {
                                scopes: bound.scopes(),
                                columns: &bound.columns,
                                bind: &*bound.bind,
                            };
                            let scopes = scopes.get_or_insert_with(|| CellSet::scopes(values, &env));
                            cells.test(runner, other, scopes, &env, &unknown, |row, pass| {
                                memo[row * stride + other] = Some(pass)
                            });
                        }
                    }
                    lanes.retain(|&row| memo[row * stride + other] == Some(true));
                }
                Deferred::Plain { column, memo } => {
                    let Some(Column::Plain { cells, owners, .. }) = self.columns.get(*column) else {
                        continue;
                    };
                    let Some(cell) = owners.get(rule).copied().flatten() else {
                        continue;
                    };
                    let Some((Some(program), _)) = cells.get(cell) else {
                        continue;
                    };
                    let stride = cells.len();
                    let unknown: Vec<usize> = lanes
                        .iter()
                        .copied()
                        .filter(|&row| memo[row * stride + cell].is_none())
                        .collect();
                    if !unknown.is_empty() {
                        let within = (unknown.len() < bound.rows()).then_some(unknown.as_slice());
                        Self::truths(runner, bound, program, within, |row, pass| {
                            memo[row * stride + cell] = Some(pass)
                        });
                    }
                    lanes.retain(|&row| memo[row * stride + cell] == Some(true));
                }
            }
        }
    }

    fn referenced_truths(
        runner: &mut LaneRunner,
        bound: &Bound,
        cells: &CellSet,
        other: usize,
        reference: &str,
        rows: &[usize],
        mut sink: impl FnMut(usize, bool),
    ) {
        let Some((program, _)) = cells.other(other).filter(|_| !rows.is_empty()) else {
            return;
        };
        let bind = |key: &str| Self::rebound(bound, reference, key).unwrap_or(Binding::Row);
        let mut outs = Outs::take();
        bound.export_with(runner, program, Some(rows), &bind, &mut outs, |_, _, _| {});
        let truths = outs.first_mut().map(|out| Leaf::typed(Array::from_output(out)).truths(rows.len()));
        for (i, &row) in rows.iter().enumerate() {
            sink(row, truths.as_ref().is_some_and(|t| Bits::get(t, i)));
        }
    }

    fn rebound(bound: &Bound, reference: &str, key: &str) -> Option<Binding> {
        let rest = match key.strip_prefix('$') {
            Some(rest) if rest.is_empty() || rest.starts_with('.') => rest,
            Some(_) if !key.starts_with("$nodes") => return None,
            _ => return Some((bound.bind)(key)),
        };
        match (bound.bind)(&format!("{reference}{rest}")) {
            Binding::Row => None,
            binding => Some(binding),
        }
    }

    fn referenced(bound: &Bound, cells: &CellSet, reference: &str) -> bool {
        (0..cells.others()).all(|other| {
            cells.other(other).is_some_and(|(program, _)| {
                let p = program.program();
                !p.opaque()
                    && p.site_keys
                        .iter()
                        .all(|key| key.as_deref().is_some_and(|key| Self::rebound(bound, reference, key).is_some()))
            })
        })
    }

    fn truths(
        runner: &mut LaneRunner,
        bound: &Bound,
        program: &LaneProgram,
        within: Option<&[usize]>,
        mut sink: impl FnMut(usize, bool),
    ) {
        let mut outs = Outs::take();
        bound.export(runner, program, within, &mut outs, |_, _, _| {});
        let count = within.map_or(bound.rows(), <[usize]>::len);
        let Some(truths) = outs.first_mut().map(|out| Leaf::typed(Array::from_output(out)).truths(count)) else {
            return;
        };
        match within {
            Some(rows) => rows.iter().enumerate().for_each(|(i, &row)| sink(row, Bits::get(&truths, i))),
            None => (0..count).for_each(|row| sink(row, Bits::get(&truths, row))),
        }
    }

    fn ones(words: &[u64]) -> impl Iterator<Item = usize> + '_ {
        words.iter().enumerate().flat_map(|(w, &word)| {
            let mut bits = word;
            std::iter::from_fn(move || {
                (bits != 0).then(|| {
                    let bit = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    w * 64 + bit
                })
            })
        })
    }

    fn run_rule(
        runner: &mut LaneRunner,
        program: Option<&LaneProgram>,
        bound: &Bound,
        lanes: &[usize],
        sink: &mut dyn FnMut(usize, Option<Vec<Variable>>),
    ) {
        let Some(program) = program else {
            lanes.iter().for_each(|&lane| sink(lane, None));
            return;
        };
        bound.many(runner, program, Some(lanes), |row, r| sink(row, r.ok()));
    }

    thread_local! {
        static BUILT: std::cell::RefCell<ahash::HashMap<(u64, usize, u8), Option<Rc<Built>>>> = std::cell::RefCell::new(ahash::HashMap::default());
    }

    fn builder(&self, rule: usize, collect: Option<bool>) -> Option<Rc<Built>> {
        let outputs = &self.outputs[rule];
        let variant = match collect {
            None => 0u8,
            Some(false) => 1,
            Some(true) => 2,
        };
        Self::BUILT.with_borrow_mut(|cache| {
            if cache.len() >= Self::THAWS {
                cache.clear();
            }
            cache
                .entry((self.id, rule, variant))
                .or_insert_with(|| {
                    let paths: Vec<(Vec<&str>, usize)> = outputs
                        .paths
                        .iter()
                        .enumerate()
                        .filter(|(_, (_, is_collect, _))| collect.is_none_or(|c| c == *is_collect))
                        .map(|(index, (path, _, _))| (path.split('.').collect(), index))
                        .collect();
                    Built::of(&paths).map(Rc::new)
                })
                .clone()
        })
    }

    fn built(&self, builder: Option<&Rc<Built>>, rule: usize, collect: Option<bool>, values: &[Variable]) -> Variable {
        let outputs = &self.outputs[rule];
        match builder {
            Some(built) if values.len() >= outputs.paths.len() => built.make(values),
            _ => Self::assemble(&outputs.paths, values, collect),
        }
    }

    fn assemble(paths: &[(Arc<str>, bool, usize)], values: &[Variable], collect: Option<bool>) -> Variable {
        let object = Variable::empty_object();
        for ((path, is_collect, _), value) in paths.iter().zip(values) {
            if collect.is_none_or(|c| c == *is_collect) {
                object.dot_insert(path, value.deep_clone());
            }
        }
        object
    }

    pub fn first_hit(&self) -> bool {
        self.hit == Hit::First
    }

    fn next(candidates: &[u64], from: usize) -> Option<usize> {
        let (mut word, bit) = (from / 64, from % 64);
        let mut current = candidates.get(word)? & (u64::MAX << bit);
        loop {
            if current != 0 {
                return Some(word * 64 + current.trailing_zeros() as usize);
            }
            word += 1;
            current = *candidates.get(word)?;
        }
    }

    pub fn first(&self, runner: &mut LaneRunner, bound: &Bound) -> First {
        if let (Some(dispatch), true) = (&self.dispatch, self.literal) {
            if let (Some(keys), Some(chosen)) = (self.keyed(dispatch, runner, bound), &dispatch.chosen) {
                return First {
                    rules: self.outputs.len(),
                    codes: keys.iter().map(|key| chosen.get(*key as usize).copied().unwrap_or(-1)).collect(),
                    batches: Vec::new(),
                    starts: vec![0],
                };
            }
        }
        let (candidates, deferred) = self.candidates(runner, bound, true);
        self.choose(runner, bound, &candidates, deferred)
    }

    pub fn collects(&self) -> bool {
        self.split.is_some()
    }

    fn literal_lists<'a>(
        first: &First,
        candidates: &[u64],
        words: usize,
        lists: usize,
        targets: &[Vec<Option<usize>>],
        templates: &[Vec<Option<Variable>>],
    ) -> Vec<Leaf<'a>> {
        let mut keys: std::collections::HashMap<Vec<u64>, i32> = std::collections::HashMap::new();
        let mut entries: Vec<Vec<u64>> = Vec::new();
        let codes: Rc<[i32]> = (0..first.codes.len())
            .map(|row| {
                let Some(chosen) = first.chosen(row) else {
                    return -1;
                };
                let mut key: Vec<u64> = candidates[row * words..(row + 1) * words].to_vec();
                for (word, bits) in key.iter_mut().enumerate() {
                    let base = word * 64;
                    *bits &= match chosen.checked_sub(base) {
                        Some(skip) if skip >= 64 => 0,
                        Some(skip) => u64::MAX << skip,
                        None => u64::MAX,
                    };
                }
                key[chosen / 64] |= 1 << (chosen % 64);
                *keys.entry(key).or_insert_with_key(|key| {
                    entries.push(key.clone());
                    entries.len() as i32 - 1
                })
            })
            .collect();
        (0..lists)
            .map(|list| {
                let values: Vec<Variable> = entries
                    .iter()
                    .map(|key| {
                        let items: Vec<Variable> = Self::ones(key)
                            .flat_map(|rule| targets[rule].iter().zip(&templates[rule]))
                            .filter(|(target, _)| **target == Some(list))
                            .filter_map(|(_, template)| template.clone())
                            .collect();
                        Variable::from_array(items)
                    })
                    .collect();
                Leaf::typed(Array::coded(codes.clone(), Rc::new(Dict::Any(values))))
            })
            .collect()
    }

    pub fn first_collect<'a>(&self, runner: &mut LaneRunner, bound: &Bound) -> Collected<'a> {
        let paths = self.split.clone().unwrap_or_default();
        let rows = bound.rows();
        let words = self.words;
        let (candidates, deferred) = self.candidates(runner, bound, false);
        let first = self.choose(runner, bound, &candidates, deferred);
        let lists = self.collect_columns.len();
        let mut collected: Vec<Vec<Vec<Variable>>> = vec![vec![Vec::new(); rows]; lists];
        let targets: Vec<Vec<Option<usize>>> = self
            .outputs
            .iter()
            .map(|outputs| {
                outputs
                    .paths
                    .iter()
                    .map(|(_, c, column)| match c {
                        true => self.collect_columns.iter().position(|(k, _)| k == column),
                        false => None,
                    })
                    .collect()
            })
            .collect();
        let templates: Vec<Vec<Option<Variable>>> = self
            .outputs
            .iter()
            .zip(&targets)
            .map(|(outputs, targets)| match &outputs.literals {
                Some(literals) => literals
                    .iter()
                    .zip(targets)
                    .map(|(literal, target)| target.map(|_| literal.variable()))
                    .collect(),
                None => Vec::new(),
            })
            .collect();
        let literal = first.batches.is_empty()
            && self.outputs.iter().zip(&targets).all(|(outputs, targets)| match &outputs.literals {
                Some(literals) => !literals.iter().any(Literal::deep),
                None => !targets.iter().any(Option::is_some),
            });
        let lists_leaves: Vec<Leaf<'a>> = match literal {
            true => Self::literal_lists(&first, &candidates, words, lists, &targets, &templates),
            false => {
            for (row, &code) in first.codes.iter().enumerate() {
                let Some(rule) = first.rule(code) else {
                    continue;
                };
                match first.computed(code).is_some() {
                    true => {
                        for (index, target) in targets[rule].iter().enumerate() {
                            if let Some(list) = target {
                                collected[*list][row].push(first.value(row, index));
                            }
                        }
                    }
                    false => {
                        for (target, template) in targets[rule].iter().zip(&templates[rule]) {
                            if let (Some(list), Some(template)) = (target, template) {
                                collected[*list][row].push(template.deep_clone());
                            }
                        }
                    }
                }
            }
            let mut any = vec![0u64; words];
            for row in 0..rows {
                any.iter_mut()
                    .zip(&candidates[row * words..(row + 1) * words])
                    .for_each(|(a, c)| *a |= c);
            }
            for rule in Self::ones(&any) {
                let outputs = &self.outputs[rule];
                if !targets[rule].iter().any(Option::is_some) {
                    continue;
                }
                let lanes: Vec<usize> = (0..rows)
                    .filter(|&row| candidates[row * words + rule / 64] >> (rule % 64) & 1 == 1)
                    .filter(|&row| first.chosen(row).is_some_and(|c| c < rule))
                    .collect();
                if lanes.is_empty() {
                    continue;
                }
                match outputs.literals {
                    Some(_) => {
                        for &row in &lanes {
                            for (target, template) in targets[rule].iter().zip(&templates[rule]) {
                                if let (Some(list), Some(template)) = (target, template) {
                                    collected[*list][row].push(template.deep_clone());
                                }
                            }
                        }
                    }
                    None => {
                        let order: Vec<usize> = targets[rule].iter().filter_map(|t| *t).collect();
                        Self::run_rule(runner, outputs.collect.as_ref(), bound, &lanes, &mut |row, values| {
                            for (list, value) in order.iter().zip(values.iter().flatten()) {
                                collected[*list][row].push(value.deep_clone());
                            }
                        });
                    }
                }
            }
                collected
                    .into_iter()
                    .map(|lists| {
                        Leaf::Any(
                            lists
                                .into_iter()
                                .enumerate()
                                .map(|(row, items)| match first.codes[row] < 0 {
                                    true => Variable::Null,
                                    false => Variable::from_array(items),
                                })
                                .collect::<Col>(),
                        )
                    })
                    .collect()
            }
        };
        let collect_slots: Vec<bool> = self
            .all_paths
            .iter()
            .map(|p| self.collect_columns.iter().any(|(_, c)| c == p))
            .collect();
        let (scalars, masks, unmatched) = self.columns(first);
        let mut columns = Vec::with_capacity(paths.len());
        let mut present = Vec::with_capacity(paths.len());
        for ((column, mask), skip) in scalars.into_iter().zip(masks).zip(&collect_slots) {
            if !skip {
                columns.push(column);
                present.push(mask);
            }
        }
        let matched: Rc<[u64]> = {
            let mut bits: Vec<u64> = unmatched.iter().map(|w| !w).collect();
            Bits::trim(&mut bits, rows);
            bits.into()
        };
        for leaf in lists_leaves {
            columns.push(leaf);
            present.push(Mask::Bits(matched.clone()));
        }
        Collected {
            paths,
            columns,
            present,
            unmatched,
        }
    }

    fn choose(&self, runner: &mut LaneRunner, bound: &Bound, candidates: &[u64], mut deferred: Vec<Deferred>) -> First {
        let rows = bound.rows();
        let words = self.words;
        let rules = self.outputs.len();
        if deferred.is_empty() && self.literal {
            return First {
                rules,
                codes: candidates
                    .chunks_exact(words)
                    .map(|row| Self::next(row, 0).map_or(-1, |rule| rule as i32))
                    .collect(),
                batches: Vec::new(),
                starts: vec![0],
            };
        }
        let mut cursor = vec![0usize; rows];
        let mut codes: Vec<i32> = vec![-1; rows];
        let mut batches: Vec<Batch> = Vec::new();
        let mut starts: Vec<usize> = vec![0];
        let mut open: Vec<usize> = (0..rows).collect();
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); self.rules];
        let mut touched: Vec<usize> = Vec::new();
        let mut kept = vec![false; rows];
        let mut before: Vec<usize> = Vec::new();
        let mut outs = Outs::take();
        while !open.is_empty() {
            for &row in &open {
                if let Some(rule) = Self::next(&candidates[row * words..(row + 1) * words], cursor[row]) {
                    if buckets[rule].is_empty() {
                        touched.push(rule);
                    }
                    buckets[rule].push(row);
                }
            }
            open.clear();
            for rule in std::mem::take(&mut touched) {
                let mut lanes = std::mem::take(&mut buckets[rule]);
                if !deferred.is_empty() {
                    before.clear();
                    before.extend_from_slice(&lanes);
                    self.verify(runner, bound, &mut deferred, rule, &mut lanes);
                    lanes.iter().for_each(|&row| kept[row] = true);
                    for &row in before.iter().filter(|&&r| !kept[r]) {
                        cursor[row] = rule + 1;
                        open.push(row);
                    }
                    lanes.iter().for_each(|&row| kept[row] = false);
                }
                if lanes.is_empty() {
                    continue;
                }
                let outputs = &self.outputs[rule];
                let direct = match (outputs.paths.is_empty(), &outputs.literals) {
                    (false, None) => Self::direct(outputs, bound, &lanes),
                    _ => None,
                };
                match (outputs.paths.is_empty(), &outputs.literals, &outputs.full) {
                    (true, _, _) | (false, Some(_), _) => lanes.iter().for_each(|&row| codes[row] = rule as i32),
                    (false, None, _) if direct.is_some() => {
                        let start = starts.last().copied().unwrap_or(0);
                        for (pos, &row) in lanes.iter().enumerate() {
                            codes[row] = (rules + start + pos) as i32;
                        }
                        starts.push(start + lanes.len());
                        batches.push(Batch {
                            rule,
                            outputs: direct.unwrap_or_default(),
                        });
                    }
                    (false, None, Some(program)) => {
                        let mut failed = vec![false; lanes.len()];
                        bound.export(runner, program, Some(&lanes), &mut outs, |pos, _, _| {
                            if let Some(f) = failed.get_mut(pos) {
                                *f = true;
                            }
                        });
                        let start = starts.last().copied().unwrap_or(0);
                        for (pos, &row) in lanes.iter().enumerate() {
                            match failed[pos] {
                                true => {
                                    cursor[row] = rule + 1;
                                    open.push(row);
                                }
                                false => codes[row] = (rules + start + pos) as i32,
                            }
                        }
                        starts.push(start + lanes.len());
                        batches.push(Batch {
                            rule,
                            outputs: outs.iter_mut().map(Array::from_output).collect(),
                        });
                    }
                    (false, None, None) => {
                        for &row in &lanes {
                            cursor[row] = rule + 1;
                            open.push(row);
                        }
                    }
                }
            }
        }
        First {
            rules,
            codes,
            batches,
            starts,
        }
    }

    fn spread_rules(target: &mut [u64], words: usize, truth: &[u64], rules: &[u64], rows: usize) {
        for (w, &word) in truth.iter().enumerate() {
            let mut set = word;
            while set != 0 {
                let row = w * 64 + set.trailing_zeros() as usize;
                set &= set - 1;
                if row >= rows {
                    return;
                }
                match words {
                    1 => target[row] |= rules[0],
                    _ => target[row * words..(row + 1) * words].iter_mut().zip(rules).for_each(|(a, r)| *a |= r),
                }
            }
        }
    }

    fn direct(outputs: &RuleOutputs, bound: &Bound, lanes: &[usize]) -> Option<Vec<Array>> {
        outputs
            .sources
            .as_ref()?
            .iter()
            .map(|source| match source {
                OutSource::Literal(literal) => {
                    let mut builder = ColumnBuilder::with_capacity(lanes.len());
                    lanes.iter().for_each(|_| builder.push_literal(literal));
                    Some(builder.finish())
                }
                OutSource::Path(path) => match (bound.bind)(path) {
                    Binding::Column(index) => {
                        let (_, column) = bound.columns.columns.get(index)?;
                        match column.values {
                            Values::Any(_) | Values::List { .. } | Values::Dict { .. } => None,
                            _ => Some(Array::gather(column, lanes)),
                        }
                    }
                    Binding::Absent => {
                        let mut builder = ColumnBuilder::with_capacity(lanes.len());
                        lanes.iter().for_each(|_| builder.push_null());
                        Some(builder.finish())
                    }
                    Binding::Row => None,
                },
            })
            .collect()
    }

    fn literal_at(&self, rule: usize, slot: usize, slots: usize, positions: &[Option<u32>]) -> Option<Option<&Literal>> {
        let index = positions[rule * slots + slot]? as usize;
        Some(self.outputs[rule].literals.as_ref().and_then(|literals| literals.get(index)))
    }

    fn slot(
        &self,
        first: &First,
        slot: usize,
        slots: usize,
        positions: &[Option<u32>],
        used: &[u64],
    ) -> Option<(Dict, Vec<u64>, Vec<u64>)> {
        let rules = self.outputs.len();
        let entries = first.entries();
        let arrays: Vec<Option<&Array>> = first
            .batches
            .iter()
            .map(|batch| {
                let index = positions[batch.rule * slots + slot]? as usize;
                batch.outputs.get(index)
            })
            .collect();
        let mut kind = None;
        for rule in (0..rules).filter(|&rule| Bits::get(used, rule)) {
            if let Some(Some(literal)) = self.literal_at(rule, slot, slots, positions) {
                if literal.deep() {
                    return None;
                }
                kind = Kind::join(kind, Kind::of_literal(literal));
            }
        }
        for array in arrays.iter().flatten() {
            kind = Kind::join(kind, Kind::of_array(array));
        }
        let mut valid = vec![0u64; entries.div_ceil(64)];
        let mut written = vec![0u64; entries.div_ceil(64)];
        for rule in 0..rules {
            let literal = self.literal_at(rule, slot, slots, positions);
            Bits::set(&mut written, rule, literal.is_some_and(|l| l.is_some()));
            Bits::set(
                &mut valid,
                rule,
                literal.flatten().is_some_and(|l| Kind::of_literal(l).is_some_and(|k| kind == Some(Kind::Any) || Some(k) == kind)),
            );
        }
        for (batch, array) in arrays.iter().enumerate() {
            let start = rules + first.starts[batch];
            let len = first.starts[batch + 1] - first.starts[batch];
            let Some(array) = array else {
                continue;
            };
            let same = kind == Some(Kind::Any) || Kind::of_array(array) == kind;
            Bits::fill(&mut written, start, len);
            match (same, array.store(), array.validity_bits()) {
                (false, _, _) => {}
                (true, Store::Any(values), _) => {
                    for pos in 0..len {
                        if array.valid(pos) && !matches!(values.get(pos), None | Some(Variable::Null)) {
                            Bits::set(&mut valid, start + pos, true);
                        }
                    }
                }
                (true, _, None) => Bits::fill(&mut valid, start, len),
                (true, _, Some(bits)) => Bits::splice(&mut valid, start, bits, len),
            }
        }
        let literal = |rule: usize| self.literal_at(rule, slot, slots, positions).flatten();
        let dict = match kind.unwrap_or(Kind::Num) {
            Kind::Num => {
                let mut mant = Vec::with_capacity(entries);
                let mut scale = Vec::with_capacity(entries);
                for rule in 0..rules {
                    let (m, s) = literal(rule).and_then(Literal::scaled).unwrap_or((0, 0));
                    mant.push(m);
                    scale.push(s);
                }
                for (batch, array) in arrays.iter().enumerate() {
                    let len = first.starts[batch + 1] - first.starts[batch];
                    match array.map(|a| a.store()) {
                        Some(Store::Scaled { mant: m, scale: s }) if m.len() >= len && s.len() >= len => {
                            mant.extend_from_slice(&m[..len]);
                            scale.extend_from_slice(&s[..len]);
                        }
                        _ => {
                            mant.resize(mant.len() + len, 0);
                            scale.resize(scale.len() + len, 0);
                        }
                    }
                }
                Dict::Scaled { mant, scale }
            }
            Kind::Text => {
                let mut offsets = Vec::with_capacity(entries + 1);
                let mut data = String::new();
                offsets.push(0);
                for rule in 0..rules {
                    if let Some(Literal::Str(text)) = literal(rule) {
                        data.push_str(text);
                    }
                    offsets.push(data.len() as i32);
                }
                for (batch, array) in arrays.iter().enumerate() {
                    let len = first.starts[batch + 1] - first.starts[batch];
                    match array.map(|a| a.store()) {
                        Some(Store::Text { offsets: o, data: d }) if o.len() > len => {
                            let (from, to) = (o[0] as usize, o[len] as usize);
                            let base = data.len() as i32 - o[0];
                            data.push_str(d.get(from..to).unwrap_or_default());
                            offsets.extend(o[1..=len].iter().map(|x| x + base));
                        }
                        _ => offsets.extend(std::iter::repeat_n(data.len() as i32, len)),
                    }
                }
                Dict::Text { offsets, data }
            }
            Kind::Bool => {
                let mut bits = vec![0u64; entries.div_ceil(64).max(1)];
                for rule in 0..rules {
                    if let Some(Literal::Bool(true)) = literal(rule) {
                        Bits::set(&mut bits, rule, true);
                    }
                }
                for (batch, array) in arrays.iter().enumerate() {
                    let start = rules + first.starts[batch];
                    let len = first.starts[batch + 1] - first.starts[batch];
                    if let Some(Store::Bool(source)) = array.map(|a| a.store()) {
                        Bits::splice(&mut bits, start, source, len);
                    }
                }
                Dict::Bool(bits)
            }
            Kind::Any => {
                let mut values = Vec::with_capacity(entries);
                values.extend((0..rules).map(|rule| literal(rule).map_or(Variable::Null, Literal::variable)));
                for (batch, array) in arrays.iter().enumerate() {
                    let len = first.starts[batch + 1] - first.starts[batch];
                    match array {
                        Some(array) => {
                            let column = array.column();
                            values.extend((0..len).map(|pos| column.variable(pos)));
                        }
                        None => values.extend((0..len).map(|_| Variable::Null)),
                    }
                }
                Dict::Any(values)
            }
        };
        Some((dict, valid, written))
    }

    fn materialized<'a>(&self, first: &First, slot: usize, slots: usize, positions: &[Option<u32>]) -> Leaf<'a> {
        let templates: Vec<Option<Variable>> = (0..self.outputs.len())
            .map(|rule| match self.literal_at(rule, slot, slots, positions).flatten() {
                Some(literal @ (Literal::Array(_) | Literal::Object(_))) => Some(literal.variable()),
                _ => None,
            })
            .collect();
        let mut builder = ColumnBuilder::with_capacity(first.codes.len());
        for (row, &code) in first.codes.iter().enumerate() {
            match (first.rule(code), first.computed(code)) {
                (Some(rule), Some(_)) => match positions[rule * slots + slot] {
                    Some(index) => builder.push_variable(first.value(row, index as usize)),
                    None => builder.push_null(),
                },
                (Some(rule), None) => match (&templates[rule], self.literal_at(rule, slot, slots, positions).flatten()) {
                    (Some(template), _) => builder.push_any(template.deep_clone()),
                    (None, Some(literal)) => builder.push_literal(literal),
                    (None, None) => builder.push_null(),
                },
                _ => builder.push_null(),
            }
        }
        Leaf::typed(builder.finish())
    }

    pub fn columns<'a>(&self, first: First) -> (Vec<Leaf<'a>>, Vec<Mask<'a>>, Vec<u64>) {
        let rows = first.codes.len();
        let slots = self.all_paths.len();
        let positions = &self.positions;
        let entries = first.entries();
        let mut flags = vec![0u8; entries + 1];
        let mut unmatched = vec![0u64; rows.div_ceil(64)];
        for (chunk, word) in first.codes.chunks(64).zip(unmatched.iter_mut()) {
            let mut bits = 0u64;
            for (at, &code) in chunk.iter().enumerate() {
                bits |= u64::from(code < 0) << at;
                if let Some(flag) = flags.get_mut((code.max(-1) + 1) as usize) {
                    *flag = 1;
                }
            }
            *word = bits;
        }
        let used = Bits::of(entries, |code| flags[code + 1] != 0);
        let matched: Rc<[u64]> = {
            let mut bits: Vec<u64> = unmatched.iter().map(|w| !w).collect();
            Bits::trim(&mut bits, rows);
            bits.into()
        };
        let covered = |flags: &[u64]| used.iter().zip(flags).all(|(u, f)| u & !f == 0);
        let shared: Rc<[i32]> = first.codes.as_slice().into();
        let mut columns = Vec::with_capacity(slots);
        let mut present = Vec::with_capacity(slots);
        let mut masks: Vec<Keyed<'_, u64>> = Vec::new();
        let mut filtered: Vec<Keyed<'_, i32>> = Vec::new();
        for slot in 0..slots {
            let found: Option<Found<'_>> = match (self.statics.get(slot).and_then(Option::as_ref), first.batches.is_empty()) {
                (Some((dict, valid, written)), true) => Some((self.thawed(slot, dict), Cow::Borrowed(valid), Cow::Borrowed(written))),
                _ => self
                    .slot(&first, slot, slots, positions, &used)
                    .map(|(dict, valid, written)| (Rc::new(dict), Cow::Owned(valid), Cow::Owned(written))),
            };
            let Some((dict, valid, written)) = found else {
                let leaf = self.materialized(&first, slot, slots, positions);
                let written: Rc<[u64]> = Bits::of(rows, |row| {
                    first.rule(first.codes[row]).is_some_and(|rule| positions[rule * slots + slot].is_some())
                })
                .into();
                columns.push(leaf);
                present.push(Mask::Bits(written));
                continue;
            };
            let mask = match covered(&written) {
                true => matched.clone(),
                false => match masks.iter().find(|(key, _)| *key == written) {
                    Some((_, mask)) => mask.clone(),
                    None => {
                        let mask: Rc<[u64]> = Bits::of(rows, |row| {
                            usize::try_from(first.codes[row]).is_ok_and(|code| Bits::get(&written, code))
                        })
                        .into();
                        masks.push((written, mask.clone()));
                        mask
                    }
                },
            };
            present.push(Mask::Bits(mask));
            let codes = match covered(&valid) {
                true => shared.clone(),
                false => match filtered.iter().find(|(key, _)| *key == valid) {
                    Some((_, codes)) => codes.clone(),
                    None => {
                        let codes: Rc<[i32]> = first
                            .codes
                            .iter()
                            .map(|&code| match usize::try_from(code).is_ok_and(|c| Bits::get(&valid, c)) {
                                true => code,
                                false => -1,
                            })
                            .collect();
                        filtered.push((valid, codes.clone()));
                        codes
                    }
                },
            };
            columns.push(Leaf::typed(Array::coded(codes, dict)));
        }
        (columns, present, unmatched)
    }

    pub fn collect_keyed(&self, runner: &mut LaneRunner, bound: &Bound) -> Option<(Vec<Variable>, Vec<i32>, usize)> {
        let literal = self.outputs.iter().all(|o| o.paths.is_empty() || o.literals.is_some());
        if self.hit != Hit::Collect || !literal || !self.stable {
            return None;
        }
        let rows = bound.rows();
        let words = self.words;
        let (candidates, _) = self.candidates(runner, bound, false);
        let mut seen: ahash::HashMap<&[u64], i32> = ahash::HashMap::default();
        let mut sets: Vec<&[u64]> = Vec::new();
        let codes: Vec<i32> = (0..rows)
            .map(|row| {
                let key = &candidates[row * words..(row + 1) * words];
                *seen.entry(key).or_insert_with(|| {
                    sets.push(key);
                    (sets.len() - 1) as i32
                })
            })
            .collect();
        let templates: Vec<Vec<Variable>> = sets
            .iter()
            .map(|set| {
                Self::ones(set)
                    .map(|rule| {
                        let outputs = &self.outputs[rule];
                        match &outputs.literals {
                            Some(literals) if !outputs.paths.is_empty() => {
                                let values: Vec<Variable> = literals.iter().map(Literal::variable).collect();
                                self.built(self.builder(rule, None).as_ref(), rule, None, &values)
                            }
                            _ => Variable::empty_object(),
                        }
                    })
                    .collect()
            })
            .collect();
        let values = codes
            .iter()
            .map(|&code| Variable::from_array(templates[code as usize].iter().map(Variable::deep_clone).collect()))
            .collect();
        Some((values, codes, sets.len()))
    }

    pub fn evaluate(&self, runner: &mut LaneRunner, bound: &Bound) -> Vec<TableOut> {
        let rows = bound.rows();
        let words = self.words;
        let (candidates, mut deferred) = self.candidates(runner, bound, self.hit == Hit::First);
        let is_candidate = |row: usize, rule: usize| candidates[row * words + rule / 64] >> (rule % 64) & 1 == 1;
        let union = |rows: &mut dyn Iterator<Item = usize>| {
            let mut any = vec![0u64; words];
            for row in rows {
                any.iter_mut()
                    .zip(&candidates[row * words..(row + 1) * words])
                    .for_each(|(a, c)| *a |= c);
            }
            any
        };
        match self.hit {
            Hit::First => {
                let mut result: Vec<TableOut> = (0..rows).map(|_| TableOut::Value(Variable::Null)).collect();
                let mut open: Vec<usize> = (0..rows).collect();
                let any = union(&mut (0..rows));
                for rule in Self::ones(&any) {
                    let mut lanes: Vec<usize> = open.iter().copied().filter(|&r| is_candidate(r, rule)).collect();
                    self.verify(runner, bound, &mut deferred, rule, &mut lanes);
                    if lanes.is_empty() {
                        continue;
                    }
                    let outputs = &self.outputs[rule];
                    let mut done = Vec::new();
                    let program = match outputs.paths.is_empty() {
                        true => None,
                        false => outputs.full.as_ref(),
                    };
                    match (outputs.paths.is_empty(), program) {
                        (true, _) => {
                            for &lane in &lanes {
                                result[lane] = TableOut::Leaves(outputs.slots.clone(), Vec::new());
                                done.push(lane);
                            }
                        }
                        (false, _) => Self::run_rule(runner, program, bound, &lanes, &mut |lane, values| {
                            if let Some(values) = values {
                                result[lane] = TableOut::Leaves(
                                    outputs.slots.clone(),
                                    values.iter().map(Variable::deep_clone).collect(),
                                );
                                done.push(lane);
                            }
                        }),
                    }
                    open.retain(|r| !done.contains(r));
                    if open.is_empty() {
                        break;
                    }
                }
                result
            }
            Hit::Collect => {
                let mut result: Vec<Vec<Variable>> = vec![Vec::new(); rows];
                let any = union(&mut (0..rows));
                for rule in Self::ones(&any) {
                    let lanes: Vec<usize> = (0..rows).filter(|&r| is_candidate(r, rule)).collect();
                    if lanes.is_empty() {
                        continue;
                    }
                    let outputs = &self.outputs[rule];
                    match (outputs.paths.is_empty(), &outputs.literals) {
                        (true, _) => lanes.iter().for_each(|&l| result[l].push(Variable::empty_object())),
                        (false, Some(literals)) => {
                            let values: Vec<Variable> = literals.iter().map(Literal::variable).collect();
                            let template = self.built(self.builder(rule, None).as_ref(), rule, None, &values);
                            lanes.iter().for_each(|&l| result[l].push(template.deep_clone()));
                        }
                        (false, None) => {
                            let builder = self.builder(rule, None);
                            Self::run_rule(runner, outputs.full.as_ref(), bound, &lanes, &mut |lane, values| {
                                if let Some(values) = values {
                                    result[lane].push(self.built(builder.as_ref(), rule, None, &values));
                                }
                            })
                        }
                    }
                }
                result
                    .into_iter()
                    .map(|items| TableOut::Value(Variable::from_array(items)))
                    .collect()
            }
            Hit::FirstCollect => {
                let mut scalars: Vec<Option<Variable>> = vec![None; rows];
                let mut matched = vec![false; rows];
                let mut collected: Vec<Vec<Vec<Variable>>> =
                    vec![vec![Vec::new(); self.collect_columns.len()]; rows];
                let column_slot = |column: usize| self.collect_columns.iter().position(|(c, _)| *c == column);
                let any = union(&mut (0..rows));
                for rule in Self::ones(&any) {
                    let lanes: Vec<usize> = (0..rows).filter(|&r| is_candidate(r, rule)).collect();
                    if lanes.is_empty() {
                        continue;
                    }
                    let outputs = &self.outputs[rule];
                    let (first, later): (Vec<usize>, Vec<usize>) = lanes.iter().partition(|&&l| scalars[l].is_none());
                    let collect_paths: Vec<&(Arc<str>, bool, usize)> =
                        outputs.paths.iter().filter(|(_, c, _)| *c).collect();
                    let builder = self.builder(rule, Some(false));
                    let mut accept = |lane: usize, values: Vec<Variable>, with_scalars: bool| {
                        matched[lane] = true;
                        match with_scalars {
                            true => {
                                scalars[lane] = Some(self.built(builder.as_ref(), rule, Some(false), &values));
                                for ((_, c, column), value) in outputs.paths.iter().zip(&values) {
                                    if let (true, Some(slot)) = (*c, column_slot(*column)) {
                                        collected[lane][slot].push(value.deep_clone());
                                    }
                                }
                            }
                            false => {
                                for ((_, _, column), value) in collect_paths.iter().zip(&values) {
                                    if let Some(slot) = column_slot(*column) {
                                        collected[lane][slot].push(value.deep_clone());
                                    }
                                }
                            }
                        }
                    };
                    match outputs.paths.is_empty() {
                        true => {
                            first.iter().for_each(|&l| accept(l, Vec::new(), true));
                            later.iter().for_each(|&l| accept(l, Vec::new(), false));
                        }
                        false => {
                            if !first.is_empty() {
                                let mut accepted = Vec::new();
                                Self::run_rule(runner, outputs.full.as_ref(), bound, &first, &mut |lane, values| {
                                    if let Some(values) = values {
                                        accepted.push((lane, values));
                                    }
                                });
                                accepted.into_iter().for_each(|(l, v)| accept(l, v, true));
                            }
                            if !later.is_empty() {
                                match collect_paths.is_empty() {
                                    true => later.iter().for_each(|&l| accept(l, Vec::new(), false)),
                                    false => {
                                        let mut accepted = Vec::new();
                                        Self::run_rule(runner, outputs.collect.as_ref(), bound, &later, &mut |lane, values| {
                                            if let Some(values) = values {
                                                accepted.push((lane, values));
                                            }
                                        });
                                        accepted.into_iter().for_each(|(l, v)| accept(l, v, false));
                                    }
                                }
                            }
                        }
                    }
                }
                (0..rows)
                    .map(|row| match matched[row] {
                        false => TableOut::Value(Variable::Null),
                        true => TableOut::Value({
                            let output = scalars[row].take().unwrap_or_else(Variable::empty_object);
                            for (slot, (_, path)) in self.collect_columns.iter().enumerate() {
                                let values = std::mem::take(&mut collected[row][slot]);
                                output.dot_insert(path, Variable::from_array(values));
                            }
                            output
                        }),
                    })
                    .collect()
            }
        }
    }
}
