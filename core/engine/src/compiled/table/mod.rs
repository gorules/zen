mod cells;
mod collect;
mod dispatch;
mod first;
mod frozen;
mod fused;

pub(crate) use dispatch::Dispatch;
pub(crate) use first::First;
use cells::{Built, OutputCells};
use frozen::{Frozen, Static};
use fused::FusedCells;
use crate::compiled::bound::{Bound, Outs};
use crate::compiled::leaf::{Array, Bits, Dict, Literal, Store};
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Binding, CellEnv, CellSet, Column as LaneColumn, LaneProgram, LaneRunner, SourceInfo, Values};
use zen_expression::typed::{TableRule, TableSpec};
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

#[derive(Clone)]
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


type Keyed<'k, T> = (Cow<'k, [u64]>, Rc<[T]>);
type Found<'k> = (Rc<Dict>, Cow<'k, [u64]>, Cow<'k, [u64]>);

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
    pub spec: Option<TableSpec>,
}

enum Part {
    Value(usize),
    Child(Built),
}


impl Table {
    fn reachable(content: &DecisionTableContent) -> Option<DecisionTableContent> {
        if !matches!(content.hit_policy, DecisionTableHitPolicy::First) || content.outputs.iter().any(|o| o.write_path().1) {
            return None;
        }
        let ids: Vec<&Arc<str>> = content.inputs.iter().map(|i| &i.id).chain(content.outputs.iter().map(|o| &o.id)).collect();
        let mut seen: std::collections::HashSet<Vec<&str>> = std::collections::HashSet::with_capacity(content.rules.len());
        let keep: Vec<bool> = content
            .rules
            .iter()
            .map(|rule| seen.insert(ids.iter().map(|id| rule.get(*id).map_or("", |c| c.as_ref())).collect()))
            .collect();
        if keep.iter().all(|k| *k) {
            return None;
        }
        let rules = content.rules.iter().zip(&keep).filter(|(_, keep)| **keep).map(|(rule, _)| rule.clone()).collect();
        Some(DecisionTableContent {
            rules: Arc::new(rules),
            ..content.clone()
        })
    }

    pub fn compile(content: &DecisionTableContent) -> Result<Self, String> {
        if let Some(reduced) = Self::reachable(content) {
            return Self::compile(&reduced);
        }
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
        let mut memo = OutputCells::default();
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
            let literals = full
                .iter()
                .map(|(_, cell)| memo.cell(cell).0.clone())
                .collect::<Option<Vec<_>>>();
            let sources = match literals {
                Some(_) => None,
                None => full
                    .iter()
                    .map(|(_, cell)| memo.cell(cell).1.clone())
                    .collect::<Option<Vec<_>>>(),
            };
            outputs.push(RuleOutputs {
                literals,
                sources,
                full: memo.many(&full),
                collect: memo.many(&collect),
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
            spec: Self::spec(content),
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

    fn spec(content: &DecisionTableContent) -> Option<TableSpec> {
        if !matches!(content.hit_policy, DecisionTableHitPolicy::First) {
            return None;
        }
        let mut paths: Vec<&str> = Vec::new();
        for output in content.outputs.iter() {
            let (path, collect) = output.write_path();
            if collect {
                return None;
            }
            if !path.is_empty() && !paths.contains(&path) {
                paths.push(path);
            }
        }
        let nested = paths.iter().any(|a| paths.iter().any(|b| b.strip_prefix(a).is_some_and(|rest| rest.starts_with('.'))));
        let plain = paths.iter().all(|path| path.split('.').all(|segment| !segment.is_empty() && !segment.contains(['$', '[', ']'])));
        let blank = content.inputs.iter().any(|input| input.field.as_deref().is_some_and(|field| field.trim().is_empty()))
            || content.rules.iter().any(|rule| rule.values().any(|cell| !cell.is_empty() && cell.trim().is_empty()));
        if nested || !plain || blank {
            return None;
        }
        let rules = content
            .rules
            .iter()
            .map(|rule| {
                let values = paths
                    .iter()
                    .map(|path| {
                        content
                            .outputs
                            .iter()
                            .filter(|output| output.write_path().0 == *path)
                            .filter_map(|output| rule.get(&output.id).filter(|cell| !cell.is_empty()))
                            .next_back()
                            .map(|cell| cell.to_string())
                    })
                    .collect();
                TableRule {
                    cells: content.inputs.iter().map(|input| rule.get(&input.id).filter(|cell| !cell.is_empty()).map(|cell| cell.to_string())).collect(),
                    values,
                }
            })
            .collect();
        Some(TableSpec {
            inputs: content.inputs.iter().map(|input| input.field.as_deref().map(str::to_string)).collect(),
            outputs: paths.iter().map(|path| path.to_string()).collect(),
            rules,
            hit: zen_expression::typed::Hit::Lenient,
            selector: true,
        })
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

    pub fn literal(&self) -> bool {
        self.literal
    }

    pub fn rules(&self) -> usize {
        self.outputs.len()
    }

    pub fn writes(&self, rule: usize, slot: usize) -> bool {
        self.positions.get(rule * self.all_paths.len() + slot).is_some_and(Option::is_some)
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
