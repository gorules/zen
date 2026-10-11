use crate::compiled::leaf::{Bits, Leaf, Literal};
use crate::policy::blocks::DecisionTableIr;
use std::sync::Arc;
use zen_expression::lane::{Binding, CellSet, Columns, LaneProgram, LaneRunner};
use zen_expression::{Scope, Variable};
use super::{Column, Hit, Table};

pub(crate) struct Picked {
    pub id: Arc<str>,
    pub field: Arc<str>,
    pub cells: Vec<Option<LaneProgram>>,
    pub constants: Vec<Option<Literal>>,
    filled: Vec<u64>,
}

pub(crate) struct Matching {
    pub all: Vec<u64>,
    pub host: Vec<bool>,
    bits: Vec<u64>,
    count: usize,
}

pub(super) struct Strict {
    ordered: Vec<bool>,
    outputs: Vec<Picked>,
}

impl Strict {
    pub(super) fn cells(&self) -> impl Iterator<Item = &LaneProgram> {
        self.outputs.iter().flat_map(|output| output.cells.iter().flatten())
    }
}

impl Picked {
    pub fn constants(&self) -> Option<(Vec<Variable>, Vec<i32>)> {
        let mut values = Vec::new();
        let mut remap = vec![-1i32; self.constants.len()];
        for (rule, constant) in self.constants.iter().enumerate() {
            if !Bits::get(&self.filled, rule) {
                continue;
            }
            remap[rule] = values.len() as i32;
            values.push(constant.as_ref()?.variable());
        }
        Some((values, remap))
    }
}

impl Table {
    pub(crate) fn strict(ir: &DecisionTableIr) -> Option<Self> {
        let rules = ir.rules.len();
        let words = rules.div_ceil(64).max(1);
        let mut columns = Vec::with_capacity(ir.inputs.len());
        let mut ordered = Vec::with_capacity(ir.inputs.len());
        for column in &ir.inputs {
            let cells: Vec<Option<&str>> = ir.rules.iter().map(|rule| rule.get(&column.id).map(|c| c.as_ref()).filter(|c| !c.is_empty())).collect();
            if cells.iter().all(Option::is_none) {
                continue;
            }
            let field = column.field.as_deref().filter(|f| !f.trim().is_empty())?;
            if cells.iter().flatten().any(|cell| LaneProgram::unary(cell).is_err()) {
                return None;
            }
            let set = CellSet::compile(&cells).ok()?;
            let dollar = (0..set.others()).all(|other| {
                set.other(other).is_some_and(|(program, _)| program.program().site_keys.iter().flatten().all(|key| key.starts_with('$')))
            });
            if !dollar {
                return None;
            }
            ordered.push(cells.iter().flatten().any(|cell| cell.contains(['<', '>']) || cell.contains("..")));
            columns.push(Column::Field {
                field: Box::new(LaneProgram::standard(field).ok()?),
                cells: Box::new(set),
                empty: Self::empty(&cells, words),
            });
        }
        let mut outputs = Vec::new();
        for column in ir.outputs.iter().filter(|c| !c.field.is_empty()) {
            if column.collect {
                return None;
            }
            let mut filled = vec![0u64; words];
            let mut cells = Vec::with_capacity(rules);
            for (index, rule) in ir.rules.iter().enumerate() {
                match rule.get(&column.id).map(|c| c.as_ref()).filter(|c| !c.is_empty()) {
                    Some(source) => {
                        filled[index / 64] |= 1 << (index % 64);
                        cells.push(Some(LaneProgram::standard(source).ok()?));
                    }
                    None => cells.push(None),
                }
            }
            let mut runner = LaneRunner::new();
            let constants = cells.iter().map(|cell| Literal::constant(&mut runner, cell.as_ref()?)).collect();
            outputs.push(Picked {
                id: column.id.clone(),
                field: column.field.clone(),
                cells,
                constants,
                filled,
            });
        }
        Some(Self {
            id: Self::next_id(),
            fused: None,
            positions: Vec::new(),
            dispatch: None,
            all_paths: Arc::from([]),
            hit: Hit::First,
            rules,
            words,
            columns,
            outputs: Vec::new(),
            collect_columns: Vec::new(),
            split: None,
            literal: false,
            stable: false,
            spec: None,
            strict: Some(Box::new(Strict { ordered, outputs })),
        })
    }

    pub fn words(&self) -> usize {
        self.words
    }

    pub fn fields(&self) -> impl Iterator<Item = &LaneProgram> {
        self.columns.iter().filter_map(|column| match column {
            Column::Field { field, .. } => Some(field.as_ref()),
            Column::Plain { .. } => None,
        })
    }

    pub fn picked(&self) -> &[Picked] {
        self.strict.as_ref().map_or(&[], |strict| &strict.outputs)
    }

    pub fn matching(&self, count: usize) -> Matching {
        Matching {
            all: self.open(count),
            host: vec![false; count],
            bits: Vec::new(),
            count,
        }
    }

    pub fn matched(&self, runner: &mut LaneRunner, input: usize, leaf: &Leaf, matching: &mut Matching) {
        let Matching { all, host, bits, count } = matching;
        let count = *count;
        let (Some(Column::Field { cells, .. }), Some(strict)) = (self.columns.get(input), &self.strict) else {
            return;
        };
        let words = self.words;
        let column = leaf.column();
        if strict.ordered.get(input).copied().unwrap_or(false) {
            for (local, hosted) in host.iter_mut().enumerate() {
                if !column.valid(local) || column.number(local).is_none() {
                    *hosted = true;
                }
            }
        }
        cells.indexed_column(&column, count, bits);
        for other in 0..cells.others() {
            let Some((program, rules)) = cells.other(other) else {
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
            runner.evaluate_bound(program, &scopes, &Columns::new(locals.len()), &|_| Binding::Row, None, |at, result| {
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
        all.iter_mut().zip(bits.iter()).for_each(|(a, b)| *a &= *b);
    }

    pub fn firsts<'t>(&'t self, matched: &'t [u64]) -> impl Iterator<Item = u32> + 't {
        self.picked().iter().map(move |output| {
            matched
                .iter()
                .zip(&output.filled)
                .enumerate()
                .find_map(|(w, (m, f))| (m & f != 0).then(|| (w * 64) as u32 + (m & f).trailing_zeros()))
                .unwrap_or(u32::MAX)
        })
    }

    pub fn used(&self, chosen: &[u32], unit: usize) -> Vec<(u32, Arc<str>)> {
        let outputs = self.picked();
        let mut used: Vec<(u32, usize)> = (0..outputs.len())
            .filter_map(|c| {
                let rule = chosen.get(unit * outputs.len() + c).copied()?;
                (rule != u32::MAX).then_some((rule, c))
            })
            .collect();
        used.sort();
        used.into_iter().map(|(rule, c)| (rule, outputs[c].id.clone())).collect()
    }
}
