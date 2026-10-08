use crate::policy::blocks::DecisionTableIr;
use std::sync::Arc;
use zen_expression::lane::{CellSet, LaneProgram};

pub(crate) struct TableOutput {
    pub id: Arc<str>,
    pub field: Arc<str>,
    pub cells: Vec<Option<LaneProgram>>,
    pub constants: Vec<Option<super::Fixed>>,
    pub filled: Vec<u64>,
}

pub(crate) struct TableInput {
    pub field: LaneProgram,
    pub cells: CellSet,
    pub ordered: bool,
}

pub(crate) struct TableNative {
    pub inputs: Vec<TableInput>,
    pub outputs: Vec<TableOutput>,
    pub rules: usize,
    pub words: usize,
}

impl TableNative {
    pub fn programs(&self) -> Vec<&LaneProgram> {
        self.inputs
            .iter()
            .map(|input| &input.field)
            .chain(self.outputs.iter().flat_map(|output| output.cells.iter().flatten()))
            .collect()
    }

    pub(crate) fn compile(ir: &DecisionTableIr) -> Option<Self> {
        let rules = ir.rules.len();
        let words = rules.div_ceil(64).max(1);
        let mut inputs = Vec::with_capacity(ir.inputs.len());
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
            let ordered = cells.iter().flatten().any(|cell| cell.contains(['<', '>']) || cell.contains(".."));
            inputs.push(TableInput {
                field: LaneProgram::standard(field).ok()?,
                cells: set,
                ordered,
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
            let mut runner = zen_expression::lane::LaneRunner::new();
            let constants = cells
                .iter()
                .map(|cell| super::Fixed::of(&mut runner, cell.as_ref()?))
                .collect();
            outputs.push(TableOutput {
                id: column.id.clone(),
                field: column.field.clone(),
                cells,
                constants,
                filled,
            });
        }
        Some(Self { inputs, outputs, rules, words })
    }
}
