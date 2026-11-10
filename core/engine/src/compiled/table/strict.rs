use super::{Column, Table, TableIr};
use crate::compiled::leaf::{Bits, Leaf, Literal};
use std::sync::Arc;
use zen_expression::lane::{Binding, CellEnv, CellSet, Columns, LaneProgram, LaneRunner};
use zen_expression::Variable;

pub(crate) struct Picked {
    pub id: Arc<str>,
    pub field: Arc<str>,
    pub collect: bool,
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
        self.outputs
            .iter()
            .flat_map(|output| output.cells.iter().flatten())
    }
}

impl Picked {
    pub fn fills(&self, rule: usize) -> bool {
        Bits::get(&self.filled, rule)
    }

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

impl Strict {
    pub(super) fn compile(ir: &TableIr, ordered: Vec<bool>) -> Result<Strict, String> {
        let words = ir.rules.len().div_ceil(64).max(1);
        let mut runner = LaneRunner::new();
        let mut outputs = Vec::new();
        let fields: Vec<&str> = ir.written().map(|column| column.path).collect();
        if fields.iter().enumerate().any(|(at, field)| {
            fields[..at].contains(field)
                && ir.written().nth(at).is_some_and(|column| !column.collect)
        }) {
            return Err("output field written by two scalar columns".into());
        }
        for column in ir.written() {
            let mut filled = vec![0u64; words];
            let mut cells = Vec::with_capacity(ir.rules.len());
            for (index, rule) in ir.rules.iter().enumerate() {
                match ir.cell(rule, column.id) {
                    Some(source) => {
                        Bits::set(&mut filled, index, true);
                        cells.push(Some(
                            LaneProgram::standard(source).map_err(|e| e.to_string())?,
                        ));
                    }
                    None => cells.push(None),
                }
            }
            let constants = cells
                .iter()
                .map(|cell| Literal::constant(&mut runner, cell.as_ref()?))
                .collect();
            outputs.push(Picked {
                id: column.id.clone(),
                field: Arc::from(column.path),
                collect: column.collect,
                cells,
                constants,
                filled,
            });
        }
        Ok(Strict { ordered, outputs })
    }
}

impl Table {
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

    pub fn matched(
        &self,
        runner: &mut LaneRunner,
        context: &zen_expression::context::EvaluationContext,
        input: usize,
        leaf: &Leaf,
        matching: &mut Matching,
    ) {
        let Matching {
            all,
            host,
            bits,
            count,
        } = matching;
        let count = *count;
        let (Some(Column::Field { cells, .. }), Some(strict)) =
            (self.columns.get(input), &self.strict)
        else {
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
        cells.indexed_column(&column, count, bits, context.timezone());
        let columns = Columns::new(count);
        let env = CellEnv {
            context,
            scopes: &[],
            columns: &columns,
            bind: &|_| Binding::Row,
        };
        let mut scopes = None;
        for other in 0..cells.others() {
            let Some((_, rules)) = cells.other(other) else {
                continue;
            };
            let locals: Vec<usize> = (0..count)
                .filter(|&local| {
                    all[local * words..(local + 1) * words]
                        .iter()
                        .zip(rules)
                        .any(|(a, r)| a & r != 0)
                })
                .collect();
            if locals.is_empty() {
                continue;
            }
            let scopes = scopes.get_or_insert_with(|| {
                CellSet::scopes(
                    &(0..count).map(|local| leaf.get(local)).collect::<Vec<_>>(),
                    &env,
                )
            });
            cells.test(
                runner,
                other,
                scopes,
                &env,
                &locals,
                |local, pass| match pass {
                    None => host[local] = true,
                    Some(true) => Bits::or(&mut bits[local * words..(local + 1) * words], rules),
                    Some(false) => {}
                },
            );
        }
        Bits::and(all, bits);
    }

    pub fn firsts<'t>(&'t self, matched: &'t [u64]) -> impl Iterator<Item = u32> + 't {
        self.picked()
            .iter()
            .map(move |output| match output.collect {
                true => u32::MAX,
                false => Self::next(matched, Some(&output.filled), 0)
                    .map_or(u32::MAX, |rule| rule as u32),
            })
    }

    pub fn gathers(&self) -> bool {
        self.picked().iter().any(|output| output.collect)
    }

    pub fn used(
        &self,
        chosen: &[u32],
        matched: Option<&[u64]>,
        unit: usize,
    ) -> Vec<(u32, Arc<str>)> {
        let outputs = self.picked();
        let words = self.words;
        let mut used: Vec<(u32, usize)> = Vec::new();
        for (c, output) in outputs.iter().enumerate() {
            match (
                output.collect,
                matched.and_then(|m| m.get(unit * words..(unit + 1) * words)),
            ) {
                (true, Some(rules)) => used.extend(
                    Bits::rows(rules)
                        .filter(|&rule| Bits::get(&output.filled, rule))
                        .map(|rule| (rule as u32, c)),
                ),
                (true, None) => {}
                (false, _) => used.extend(
                    chosen
                        .get(unit * outputs.len() + c)
                        .copied()
                        .filter(|&rule| rule != u32::MAX)
                        .map(|rule| (rule, c)),
                ),
            }
        }
        used.sort();
        used.into_iter()
            .map(|(rule, c)| (rule, outputs[c].id.clone()))
            .collect()
    }
}
