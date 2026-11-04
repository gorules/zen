mod cells;
mod collect;
mod dispatch;
mod first;
mod fused;
mod profile;
mod strict;

use crate::compiled::bound::{Bound, Outs};
use crate::compiled::graph::tap::{Field, Tap};
use crate::compiled::leaf::{Array, Bits, Dict, Literal, Store};
use cells::{Built, OutputCells};
pub(crate) use dispatch::Dispatch;
pub(crate) use first::First;
use fused::FusedCells;
use profile::{Empty, Resolve};
pub(crate) use profile::{Profile, TableIr};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;
use strict::Strict;
pub(crate) use strict::{Matching, Picked};
use zen_expression::lane::{
    Binding, CellEnv, CellSet, Column as LaneColumn, LaneProgram, LaneRunner, SourceInfo, Values,
};
use zen_expression::typed::TableSpec;
use zen_expression::Variable;

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
            Store::Any(values) => values
                .iter()
                .any(|v| !matches!(v, Variable::Null))
                .then_some(Kind::Any),
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scalar {
    Num,
    Bool,
    Text,
}

type Keyed<'k, T> = (Cow<'k, [u64]>, Rc<[T]>);
type Found<'k> = (Rc<Dict>, Cow<'k, [u64]>, Cow<'k, [u64]>);

pub(crate) struct Table {
    origins: Vec<u32>,
    id: u64,
    profile: Profile,
    fused: Option<FusedCells>,
    positions: Vec<Option<u32>>,
    dispatch: Option<Dispatch>,
    all_paths: Arc<[Arc<str>]>,
    hit: Hit,
    rules: usize,
    words: usize,
    columns: Vec<Column>,
    fields: Vec<Option<Arc<str>>>,
    outputs: Vec<RuleOutputs>,
    collect_columns: Vec<(usize, Arc<str>)>,
    split: Option<Arc<[Arc<str>]>>,
    literal: bool,
    pub stable: bool,
    pub spec: Option<TableSpec>,
    strict: Option<Box<Strict>>,
}

enum Part {
    Value(usize),
    Child(Built),
}

impl Table {
    fn reachable(mut ir: TableIr) -> (TableIr, Vec<u32>) {
        let all: Vec<u32> = (0..ir.rules.len() as u32).collect();
        if ir.collect || ir.profile.resolve != Resolve::Row || ir.outputs.iter().any(|o| o.collect)
        {
            return (ir, all);
        }
        let ids: Vec<&Arc<str>> = ir
            .inputs
            .iter()
            .map(|i| i.id)
            .chain(ir.outputs.iter().map(|o| o.id))
            .collect();
        let mut seen: std::collections::HashSet<Vec<&str>> =
            std::collections::HashSet::with_capacity(ir.rules.len());
        let keep: Vec<bool> = ir
            .rules
            .iter()
            .map(|rule| {
                seen.insert(
                    ids.iter()
                        .map(|id| ir.cell(rule, id).unwrap_or(""))
                        .collect(),
                )
            })
            .collect();
        let origins = all
            .into_iter()
            .zip(&keep)
            .filter(|(_, kept)| **kept)
            .map(|(at, _)| at)
            .collect();
        let mut keep = keep.into_iter();
        ir.rules.retain(|_| keep.next().unwrap_or(true));
        (ir, origins)
    }

    pub fn origin(&self, rule: usize) -> u32 {
        self.origins.get(rule).copied().unwrap_or(rule as u32)
    }

    pub fn compile(ir: TableIr) -> Result<Self, String> {
        let (ir, origins) = Self::reachable(ir);
        let rules = ir.rules.len();
        let words = rules.div_ceil(64).max(1);
        let strict = ir.profile.strict();
        let bit = |bits: &mut Vec<u64>, rule: usize| bits[rule / 64] |= 1 << (rule % 64);
        let mut columns = Vec::with_capacity(ir.inputs.len());
        let mut fields: Vec<Option<Arc<str>>> = Vec::with_capacity(ir.inputs.len());
        let mut ordered = Vec::new();
        let mut plain_sources: Vec<(usize, usize, Arc<str>)> = Vec::new();
        for input in ir.inputs.iter() {
            let cells = ir.cells(input.id);
            if strict && cells.iter().all(Option::is_none) {
                continue;
            }
            let empty = Self::empty(&cells, words);
            let field = match strict {
                true => Some(
                    input
                        .field
                        .filter(|f| !f.trim().is_empty())
                        .ok_or("table cell without a field")?,
                ),
                false => input.field,
            };
            match field {
                Some(field) => {
                    if strict
                        && cells
                            .iter()
                            .flatten()
                            .any(|cell| LaneProgram::unary(cell).is_err())
                    {
                        return Err("table cell does not parse".into());
                    }
                    let program = LaneProgram::standard(field).map_err(|e| e.to_string())?;
                    let set = CellSet::compile(&cells).map_err(|e| e.to_string())?;
                    if strict {
                        let dollar = (0..set.others()).all(|other| {
                            set.other(other).is_some_and(|(program, _)| {
                                program
                                    .program()
                                    .site_keys
                                    .iter()
                                    .flatten()
                                    .all(|key| key.starts_with('$'))
                            })
                        });
                        if !dollar {
                            return Err("table cell reads beyond `$`".into());
                        }
                        ordered.push(
                            cells
                                .iter()
                                .flatten()
                                .any(|cell| cell.contains(['<', '>']) || cell.contains("..")),
                        );
                    }
                    columns.push(Column::Field {
                        field: Box::new(program),
                        cells: Box::new(set),
                        empty,
                    });
                    fields.push(Some(Arc::from(field)));
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
                                distinct.push((
                                    Arc::from(*cell),
                                    LaneProgram::standard(cell).ok(),
                                    bits,
                                ));
                            }
                        }
                    }
                    let mut owners = vec![None; rules];
                    let mut optimistic = vec![0u64; words];
                    for (index, (_, program, bits)) in distinct.iter().enumerate() {
                        if program.is_none() {
                            continue;
                        }
                        Bits::or(&mut optimistic, bits);
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
                    fields.push(None);
                }
            }
        }
        let mut table = Self {
            origins,
            id: Self::next_id(),
            profile: ir.profile,
            fused: None,
            positions: Vec::new(),
            dispatch: None,
            all_paths: Arc::from([]),
            hit: Hit::First,
            rules,
            words,
            columns,
            fields,
            outputs: Vec::new(),
            collect_columns: Vec::new(),
            split: None,
            literal: false,
            stable: false,
            spec: None,
            strict: None,
        };
        match ir.profile.resolve {
            Resolve::Column => table.strict = Some(Box::new(Strict::compile(&ir, ordered)?)),
            Resolve::Row => table.rows(&ir, &plain_sources)?,
        }
        Ok(table)
    }

    fn rows(
        &mut self,
        ir: &TableIr,
        plain_sources: &[(usize, usize, Arc<str>)],
    ) -> Result<(), String> {
        let rules = self.rules;
        let mut all_paths: Vec<Arc<str>> = Vec::new();
        for output in ir.written() {
            if !all_paths.iter().any(|p| p.as_ref() == output.path) {
                all_paths.push(Arc::from(output.path));
            }
        }
        let mut outputs = Vec::with_capacity(rules);
        let mut memo = OutputCells::default();
        for rule in ir.rules.iter() {
            let mut paths = Vec::new();
            let mut full: Vec<(String, &str)> = Vec::new();
            let mut collect: Vec<(String, &str)> = Vec::new();
            for (column, output) in ir.outputs.iter().enumerate() {
                if output.path.is_empty() {
                    continue;
                }
                let Some(cell) = ir.cell(rule, output.id) else {
                    continue;
                };
                if SourceInfo::reads_dollar(cell) {
                    return Err("table output cell reads `$`".into());
                }
                let key = format!("o{}", full.len());
                full.push((key, cell));
                if output.collect {
                    collect.push((format!("o{}", collect.len()), cell));
                }
                paths.push((Arc::from(output.path), output.collect, column));
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

        let has_collect = ir.outputs.iter().any(|o| o.collect);
        let hit = match (ir.collect, has_collect) {
            (false, true) => Hit::FirstCollect,
            (false, false) => Hit::First,
            (true, _) => Hit::Collect,
        };
        let collect_columns = ir
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| o.collect && !o.path.is_empty())
            .map(|(i, o)| (i, Arc::from(o.path)))
            .collect::<Vec<(usize, Arc<str>)>>();
        let collected: Vec<&Arc<str>> = collect_columns.iter().map(|(_, p)| p).collect();
        let distinct = collected
            .iter()
            .enumerate()
            .all(|(i, p)| !collected[..i].contains(p));
        let scalars: Vec<Arc<str>> = all_paths
            .iter()
            .filter(|p| !collected.contains(p))
            .cloned()
            .collect();
        let disjoint = ir.outputs.iter().all(|o| {
            o.collect || o.path.is_empty() || !collected.iter().any(|c| c.as_ref() == o.path)
        });
        let split = (hit == Hit::FirstCollect && distinct && disjoint).then(|| {
            scalars
                .into_iter()
                .chain(collected.into_iter().cloned())
                .collect()
        });

        let literal = outputs
            .iter()
            .all(|o| o.paths.is_empty() || o.literals.is_some());
        let slots = all_paths.len();
        let mut positions: Vec<Option<u32>> = vec![None; outputs.len() * slots];
        for (rule, rule_outputs) in outputs.iter().enumerate() {
            for (index, &slot) in rule_outputs.slots.iter().enumerate() {
                if slot < slots {
                    positions[rule * slots + slot] = Some(index as u32);
                }
            }
        }
        let clocked = |text: &str| {
            text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|token| {
                    matches!(
                        token,
                        "rand" | "date" | "time" | "d" | "now" | "dateString" | "isToday"
                    )
                })
        };
        self.fused = Self::fused(&self.columns, plain_sources, hit == Hit::First);
        self.stable = ir
            .rules
            .iter()
            .all(|rule| rule.values().all(|cell| !clocked(cell.as_ref())));
        self.spec = ir.spec();
        self.positions = positions;
        self.dispatch = Dispatch::build(&self.columns, rules, self.words);
        self.literal = literal;
        self.split = split;
        self.all_paths = all_paths.into();
        self.hit = hit;
        self.outputs = outputs;
        self.collect_columns = collect_columns;
        Ok(())
    }

    fn empty(cells: &[Option<&str>], words: usize) -> Vec<u64> {
        let mut empty = vec![0u64; words];
        for (rule, cell) in cells.iter().enumerate() {
            if cell.is_none() {
                empty[rule / 64] |= 1 << (rule % 64);
            }
        }
        empty
    }

    fn tail(rules: usize) -> u64 {
        match (rules, rules % 64) {
            (0, _) => 0,
            (_, 0) => u64::MAX,
            (_, t) => (1u64 << t) - 1,
        }
    }

    fn opened(rules: usize, words: usize, rows: usize) -> Vec<u64> {
        let mut all = vec![u64::MAX; rows * words];
        all.chunks_exact_mut(words)
            .for_each(|row| row[words - 1] &= Self::tail(rules));
        all
    }

    pub fn open(&self, rows: usize) -> Vec<u64> {
        Self::opened(self.rules, self.words, rows)
    }

    fn sets<'c>(&self, candidates: &'c [u64], rows: usize) -> (Vec<i32>, Vec<&'c [u64]>) {
        let words = self.words;
        let mut seen: ahash::HashMap<&[u64], i32> = ahash::HashMap::default();
        let mut sets: Vec<&[u64]> = Vec::new();
        let codes = (0..rows)
            .map(|row| {
                let key = &candidates[row * words..(row + 1) * words];
                *seen.entry(key).or_insert_with(|| {
                    sets.push(key);
                    (sets.len() - 1) as i32
                })
            })
            .collect();
        (codes, sets)
    }

    pub fn programs(&self) -> Vec<&LaneProgram> {
        if let Some(strict) = &self.strict {
            return self.fields().chain(strict.cells()).collect();
        }
        let mut programs: Vec<&LaneProgram> = Vec::new();
        for column in &self.columns {
            match column {
                Column::Field { field, cells, .. } => {
                    programs.push(field);
                    programs.extend(cells.programs());
                }
                Column::Plain { cells, .. } => {
                    programs.extend(cells.iter().filter_map(|(p, _)| p.as_ref()))
                }
            }
        }
        for outputs in &self.outputs {
            programs.extend(outputs.full.iter());
            programs.extend(outputs.collect.iter());
        }
        programs
    }

    pub fn fills(&self) -> bool {
        self.profile.empty == Empty::Fill
    }

    pub fn literal(&self) -> bool {
        self.literal
    }

    pub fn rules(&self) -> usize {
        self.outputs.len()
    }

    pub fn writes(&self, rule: usize, slot: usize) -> bool {
        self.positions
            .get(rule * self.all_paths.len() + slot)
            .is_some_and(Option::is_some)
    }

    pub fn paths(&self) -> &Arc<[Arc<str>]> {
        &self.all_paths
    }

    pub fn slot_kind(&self, path: &str) -> Option<Scalar> {
        let slots = self.all_paths.len();
        let slot = self.all_paths.iter().position(|p| p.as_ref() == path)?;
        let rules = self.outputs.len();
        if (0..rules).any(|rule| {
            self.positions[rule * slots + slot].is_some() && self.outputs[rule].literals.is_none()
        }) {
            return None;
        }
        let empty = First {
            rules,
            codes: Vec::new(),
            batches: Vec::new(),
            starts: vec![0],
        };
        match self
            .slot(&empty, slot, slots, &self.positions, &Bits::ones(rules))?
            .0
        {
            Dict::Scaled { .. } => Some(Scalar::Num),
            Dict::Bool(_) => Some(Scalar::Bool),
            Dict::Text { .. } => Some(Scalar::Text),
            Dict::Any(_) => None,
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

    fn field_column<'b>(
        runner: &mut LaneRunner,
        bound: &'b Bound,
        field: &LaneProgram,
        failed: &mut [bool],
    ) -> (Option<LaneColumn<'b>>, Option<Array>) {
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

    fn candidates(
        &self,
        runner: &mut LaneRunner,
        bound: &Bound,
        lazy: bool,
    ) -> (Vec<u64>, Vec<Deferred>) {
        if let Some(dispatch) = &self.dispatch {
            return (self.dispatched(dispatch, runner, bound), Vec::new());
        }
        let (rows, words) = (bound.rows(), self.words);
        let scopes = bound.scopes();
        let mut all = self.open(rows);
        let mut bits = Vec::new();
        let mut deferred = Vec::new();
        let fused = self.fused_truths(runner, bound);
        let mut tapped: Option<Vec<Field>> = Tap::wants_fields().then(Vec::new);
        for (index, column) in self.columns.iter().enumerate() {
            match column {
                Column::Field {
                    field,
                    cells,
                    empty,
                } => {
                    let mut failed = vec![false; rows];
                    let (direct, typed) = Self::field_column(runner, bound, field, &mut failed);
                    let column = direct.or_else(|| typed.as_ref().map(Array::column));
                    if let (Some(tapped), Some(Some(name))) =
                        (tapped.as_mut(), self.fields.get(index))
                    {
                        tapped.push(Field {
                            name: name.clone(),
                            values: (0..rows)
                                .map(|row| column.map_or(Variable::Null, |c| c.variable(row)))
                                .collect(),
                            failed: failed.clone(),
                        });
                    }
                    if let (Some(truths), Some(view), true) = (&fused, direct, self.fuses(index)) {
                        cells.indexed_column(&view, rows, &mut bits);
                        for other in 0..cells.others() {
                            let (Some((_, rules)), Some(truth)) = (
                                cells.other(other),
                                self.fused_at(index, other).and_then(|at| truths.get(at)),
                            ) else {
                                continue;
                            };
                            Self::spread_rules(&mut bits, words, truth, rules, rows);
                        }
                        Bits::and(&mut all, &bits);
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
                        && column.is_some_and(|column| {
                            Self::peeled(runner, cells, &column, rows, &mut bits)
                        });
                    match (
                        lazy && !peeled,
                        cells.others() > 0 && !peeled,
                        column,
                        referenced,
                    ) {
                        _ if peeled => {}
                        (_, false, Some(column), _) => {
                            cells.indexed_column(&column, rows, &mut bits)
                        }
                        (true, true, Some(column), _) => {
                            cells.evaluate_indexed_column(&column, rows, &mut bits)
                        }
                        (false, true, Some(column), Some(reference)) => {
                            cells.indexed_column(&column, rows, &mut bits);
                            for other in 0..cells.others() {
                                let Some((_, rules)) = cells.other(other) else {
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
                                Self::referenced_truths(
                                    runner,
                                    bound,
                                    cells,
                                    other,
                                    reference,
                                    &subset,
                                    |row, pass| {
                                        if pass {
                                            Bits::or(
                                                &mut bits[row * words..(row + 1) * words],
                                                rules,
                                            );
                                        }
                                    },
                                );
                            }
                        }
                        _ => {
                            let env = CellEnv {
                                scopes,
                                columns: &bound.columns,
                                bind: &*bound.bind,
                            };
                            cells.evaluate_within(
                                runner,
                                &values(&typed),
                                &env,
                                Some(&all),
                                &mut bits,
                            )
                        }
                    }
                    Bits::and(&mut all, &bits);
                    for row in (0..rows).filter(|&row| failed[row]) {
                        Bits::and(&mut all[row * words..(row + 1) * words], empty);
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
                            passing
                                .chunks_exact_mut(words)
                                .for_each(|row| row.copy_from_slice(empty));
                            passing
                        }
                    };
                    for (cell, (_, rules)) in cells.iter().enumerate() {
                        let found = self
                            .fused_at(index, cell)
                            .and_then(|at| truths.and_then(|t| t.get(at)));
                        let Some(truth) = found else {
                            continue;
                        };
                        Self::spread_rules(&mut passing, words, truth, rules, rows);
                    }
                    Bits::and(&mut all, &passing);
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
                                Bits::or(&mut passing[row * words..(row + 1) * words], rules);
                            }
                        });
                    }
                    Bits::and(&mut all, &passing);
                }
            }
        }
        if let Some(tapped) = tapped {
            Tap::fields(tapped);
        }
        (all, deferred)
    }

    fn literal_outputs(&self) -> bool {
        self.outputs
            .iter()
            .all(|o| o.paths.is_empty() || o.literals.is_some())
    }

    pub(crate) fn tap_hits(&self, candidates: &[u64], first: Option<&First>, exact: bool) {
        if !Tap::armed() {
            return;
        }
        if !exact || !self.literal_outputs() {
            Tap::hits(None);
            return;
        }
        let words = self.words;
        let hits = candidates
            .chunks_exact(words)
            .enumerate()
            .map(|(row, bits)| {
                let from = match first {
                    None => 0,
                    Some(first) => match first.chosen(row) {
                        None => return SmallVec::new(),
                        Some(chosen) => chosen,
                    },
                };
                let mut out: SmallVec<[u32; 4]> = SmallVec::new();
                if first.is_some() {
                    out.push(self.origin(from));
                }
                out.extend(
                    Bits::rows(bits)
                        .filter(|&rule| match first {
                            None => true,
                            Some(_) => rule > from,
                        })
                        .map(|rule| self.origin(rule)),
                );
                out
            })
            .collect();
        Tap::hits(Some(hits));
    }

    const FUSED: usize = 64;
    const THAWS: usize = 4096;

    fn next_id() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn peeled(
        runner: &mut LaneRunner,
        cells: &CellSet,
        column: &LaneColumn,
        rows: usize,
        out: &mut Vec<u64>,
    ) -> bool {
        let mut index: Vec<u32> = Vec::with_capacity(rows);
        let mut values: Vec<Variable> = Vec::new();
        let limit = rows / 2;
        let text = matches!(
            column.values,
            Values::Text { .. } | Values::Utf8 { .. } | Values::Strs(_)
        );
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
                                values.push(
                                    std::str::from_utf8(bytes)
                                        .map_or(Variable::Null, |t| Variable::String(t.into())),
                                );
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
                    if !matches!(
                        value,
                        Variable::Null
                            | Variable::Bool(_)
                            | Variable::Number(_)
                            | Variable::String(_)
                    ) {
                        return false;
                    }
                    let same = |a: &Variable, b: &Variable| match (a, b) {
                        (Variable::Number(x), Variable::Number(y)) => {
                            x.serialize() == y.serialize()
                        }
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
                        (false, Some(reference)) => Self::referenced_truths(
                            runner,
                            bound,
                            cells,
                            other,
                            reference,
                            &unknown,
                            |row, pass| memo[row * stride + other] = Some(pass),
                        ),
                        (false, None) => {
                            let env = CellEnv {
                                scopes: bound.scopes(),
                                columns: &bound.columns,
                                bind: &*bound.bind,
                            };
                            let scopes =
                                scopes.get_or_insert_with(|| CellSet::scopes(values, &env));
                            cells.test(runner, other, scopes, &env, &unknown, |row, pass| {
                                memo[row * stride + other] = Some(pass == Some(true))
                            });
                        }
                    }
                    lanes.retain(|&row| memo[row * stride + other] == Some(true));
                }
                Deferred::Plain { column, memo } => {
                    let Some(Column::Plain { cells, owners, .. }) = self.columns.get(*column)
                    else {
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

    fn chosen_values(&self, first: &First, row: usize, rule: usize) -> Vec<Variable> {
        let outputs = &self.outputs[rule];
        match (
            first.codes.get(row).and_then(|&code| first.computed(code)),
            &outputs.literals,
        ) {
            (Some(_), _) => (0..outputs.paths.len())
                .map(|index| first.value(row, index))
                .collect(),
            (None, Some(literals)) => literals.iter().map(Literal::variable).collect(),
            (None, None) => Vec::new(),
        }
    }

    pub fn evaluate(&self, runner: &mut LaneRunner, bound: &Bound) -> Vec<TableOut> {
        let rows = bound.rows();
        match self.hit {
            Hit::First => {
                let first = self.first(runner, bound);
                (0..rows)
                    .map(|row| match first.chosen(row) {
                        None => TableOut::Value(Variable::Null),
                        Some(rule) => TableOut::Leaves(
                            self.outputs[rule].slots.clone(),
                            self.chosen_values(&first, row, rule),
                        ),
                    })
                    .collect()
            }
            Hit::Collect => {
                let words = self.words;
                let (candidates, _) = self.candidates(runner, bound, false);
                self.tap_hits(&candidates, None, true);
                let mut result: Vec<Vec<Variable>> = vec![Vec::new(); rows];
                let mut any = vec![0u64; words];
                candidates
                    .chunks_exact(words)
                    .for_each(|row| Bits::or(&mut any, row));
                for rule in Bits::rows(&any) {
                    let lanes: Vec<usize> = (0..rows)
                        .filter(|&r| Bits::get(&candidates[r * words..(r + 1) * words], rule))
                        .collect();
                    if lanes.is_empty() {
                        continue;
                    }
                    let outputs = &self.outputs[rule];
                    match (outputs.paths.is_empty(), &outputs.literals) {
                        (true, _) => lanes
                            .iter()
                            .for_each(|&l| result[l].push(Variable::empty_object())),
                        (false, Some(literals)) => {
                            let values: Vec<Variable> =
                                literals.iter().map(Literal::variable).collect();
                            let template =
                                self.built(self.builder(rule, None).as_ref(), rule, None, &values);
                            lanes
                                .iter()
                                .for_each(|&l| result[l].push(template.deep_clone()));
                        }
                        (false, None) => {
                            let builder = self.builder(rule, None);
                            Self::run_rule(
                                runner,
                                outputs.full.as_ref(),
                                bound,
                                &lanes,
                                &mut |lane, values| {
                                    if let Some(values) = values {
                                        result[lane].push(self.built(
                                            builder.as_ref(),
                                            rule,
                                            None,
                                            &values,
                                        ));
                                    }
                                },
                            )
                        }
                    }
                }
                result
                    .into_iter()
                    .map(|items| TableOut::Value(Variable::from_array(items)))
                    .collect()
            }
            Hit::FirstCollect => {
                let (candidates, deferred) = self.candidates(runner, bound, false);
                let exact = deferred.is_empty();
                let first = self.choose(runner, bound, &candidates, deferred);
                self.tap_hits(&candidates, Some(&first), exact);
                let mut collected =
                    self.gather(runner, bound, &first, &candidates, &self.targets());
                (0..rows)
                    .map(|row| {
                        let output = match first.chosen(row) {
                            Some(rule) => self.built(
                                self.builder(rule, Some(false)).as_ref(),
                                rule,
                                Some(false),
                                &self.chosen_values(&first, row, rule),
                            ),
                            None => Variable::empty_object(),
                        };
                        for (at, (lists, (_, path))) in
                            collected.iter_mut().zip(&self.collect_columns).enumerate()
                        {
                            if !self.collect_columns[..at].iter().any(|(_, p)| p == path) {
                                output.dot_insert(
                                    path,
                                    Variable::from_array(std::mem::take(&mut lists[row])),
                                );
                            }
                        }
                        TableOut::Value(output)
                    })
                    .collect()
            }
        }
    }
}
