use std::sync::Arc;

use ahash::{HashMap, HashSet};
use base64::Engine as _;
use fixedbitset::FixedBitSet;
use zen_expression::variable::Variable;
use zen_types::decision::DecisionTableInputField;

use super::Exact;
use crate::policy::blocks::{DecisionTableIr, ExecutionError, TableSelection};
use crate::workspace::types::{BlockTrace, DecisionTableExtras};

impl Exact<'_> {
    pub(crate) fn select_table(&mut self, ir: &DecisionTableIr) -> Result<TableSelection, ExecutionError> {
        let mut col_refs: Vec<Option<Variable>> = vec![None; ir.inputs.len()];

        let active: Vec<_> = ir.outputs.iter().filter(|c| !c.field.is_empty()).collect();
        let has_collect = active.iter().any(|c| c.collect);
        let mut pending_scalars = active.iter().filter(|c| !c.collect).count();
        let mut taken: Vec<bool> = vec![false; active.len()];

        let mut matched_rows: Vec<u32> = Vec::new();
        let mut used_cells: Vec<(u32, Arc<str>)> = Vec::new();
        let cols = ir.inputs.len();
        let bytes_per_row = cols.div_ceil(8);
        let mut input_bits: Option<Vec<u8>> = self
            .site
            .extras
            .then(|| vec![0u8; ir.rules.len() * bytes_per_row]);
        let candidates = match input_bits {
            None => self.candidate_rows(ir, &mut col_refs)?,
            Some(_) => None,
        };

        for (row_idx, rule) in ir.rules.iter().enumerate() {
            let satisfied = !has_collect && pending_scalars == 0 && !matched_rows.is_empty();
            if satisfied && !self.site.extras {
                break;
            }

            let row_matches = match input_bits.as_mut() {
                Some(bits) => {
                    let per_col = self.evaluate_row_inputs_full(ir, rule, &mut col_refs, satisfied)?;
                    for (col_idx, &passed) in per_col.iter().enumerate() {
                        if passed {
                            let byte = row_idx * bytes_per_row + col_idx / 8;
                            bits[byte] |= 1 << (col_idx % 8);
                        }
                    }
                    per_col.iter().all(|p| *p)
                }
                None => match &candidates {
                    Some(rows) if !rows.contains(row_idx) => false,
                    Some(_) => self.evaluate_row_inputs_pruned(ir, row_idx, rule, &mut col_refs)?,
                    None => self.evaluate_row_inputs(ir, rule, &mut col_refs)?,
                },
            };
            if !row_matches || satisfied {
                continue;
            }

            matched_rows.push(row_idx as u32);
            for (col_pos, col) in active.iter().enumerate() {
                if rule.get(&col.id).filter(|c| !c.is_empty()).is_none() {
                    continue;
                }
                if col.collect {
                    used_cells.push((row_idx as u32, col.id.clone()));
                } else if !taken[col_pos] {
                    taken[col_pos] = true;
                    used_cells.push((row_idx as u32, col.id.clone()));
                    pending_scalars -= 1;
                }
            }
        }

        Ok(TableSelection {
            matched_rows,
            used_cells,
            input_bits,
        })
    }

    pub(crate) fn commit_table(
        &mut self,
        ir: &DecisionTableIr,
        selection: &TableSelection,
    ) -> Result<BlockTrace, ExecutionError> {
        let used: HashSet<(u32, &str)> = selection
            .used_cells
            .iter()
            .map(|(row, col)| (*row, col.as_ref()))
            .collect();
        let mut collected: HashMap<Arc<str>, Vec<Variable>> = HashMap::default();
        let mut scalar_written: HashSet<Arc<str>> = HashSet::default();
        for col in &ir.outputs {
            if col.collect && !col.field.is_empty() {
                collected.entry(col.field.clone()).or_default();
            }
        }

        let mut evaluations: Vec<HashMap<Arc<str>, Variable>> = Vec::new();
        for &row_idx in &selection.matched_rows {
            let Some(rule) = ir.rules.get(row_idx as usize) else {
                continue;
            };
            let mut row_outputs: HashMap<Arc<str>, Variable> = HashMap::default();

            for col in &ir.outputs {
                if !used.contains(&(row_idx, col.id.as_ref())) {
                    continue;
                }
                let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                    continue;
                };

                let value = self
                    .session
                    .run_standard(cell)
                    .map_err(|e| self.site.expression_error(cell, e))?;

                if self.site.trace {
                    row_outputs.insert(col.field.clone(), value.deep_clone());
                }

                if col.collect {
                    collected.entry(col.field.clone()).or_default().push(value);
                } else {
                    self.site.write(&col.field, value);
                    scalar_written.insert(col.field.clone());
                }
            }
            if self.site.trace {
                evaluations.push(row_outputs);
            }
        }

        for col in &ir.outputs {
            if !col.collect && !col.field.is_empty() && !scalar_written.contains(&col.field) {
                self.site.write(&col.field, Variable::Null);
                scalar_written.insert(col.field.clone());
            }
        }
        for (field, values) in collected {
            self.site.write(&field, Variable::from_array(values));
        }

        let extras = selection
            .input_bits
            .as_ref()
            .map(|bits| DecisionTableExtras {
                input_pass: base64::engine::general_purpose::STANDARD.encode(bits),
            });

        Ok(BlockTrace::DecisionTable {
            matched_rows: selection.matched_rows.clone(),
            evaluations,
            extras,
        })
    }

    pub(crate) fn execute_table(&mut self, ir: &DecisionTableIr) -> Result<BlockTrace, ExecutionError> {
        let selection = self.select_table(ir)?;
        self.commit_table(ir, &selection)
    }

    fn candidate_rows(
        &mut self,
        ir: &DecisionTableIr,
        col_refs: &mut [Option<Variable>],
    ) -> Result<Option<FixedBitSet>, ExecutionError> {
        let Some(index) = ir.table_index() else {
            return Ok(None);
        };
        let mut acc: Option<FixedBitSet> = None;
        for (col_idx, column) in index.columns.iter().enumerate() {
            let Some(column) = column else {
                continue;
            };
            let Some(field) = ir.inputs[col_idx]
                .field
                .as_ref()
                .filter(|f| !f.is_empty())
            else {
                continue;
            };
            let value = match &col_refs[col_idx] {
                Some(value) => value.shallow_clone(),
                None => {
                    let value = self
                        .session
                        .run_standard(field)
                        .map_err(|e| self.site.expression_error(field, e))?;
                    col_refs[col_idx] = Some(value.shallow_clone());
                    value
                }
            };
            if matches!(value, Variable::Dynamic(_)) {
                return Ok(None);
            }
            let mut col_rows = column.fallback.clone();
            if let Some(hit) = column.rows_for(&value) {
                col_rows.union_with(hit);
            }
            match &mut acc {
                None => acc = Some(col_rows),
                Some(set) => set.intersect_with(&col_rows),
            }
        }
        Ok(acc)
    }

    fn evaluate_row_inputs_pruned(
        &mut self,
        ir: &DecisionTableIr,
        row_idx: usize,
        rule: &HashMap<Arc<str>, Arc<str>>,
        col_refs: &mut [Option<Variable>],
    ) -> Result<bool, ExecutionError> {
        let index = ir.table_index();
        for (col_idx, col) in ir.inputs.iter().enumerate() {
            if index.is_some_and(|ix| ix.decides(col_idx, row_idx)) {
                continue;
            }
            if !self.evaluate_cell(col_idx, col, rule, col_refs)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn evaluate_row_inputs(
        &mut self,
        ir: &DecisionTableIr,
        rule: &HashMap<Arc<str>, Arc<str>>,
        col_refs: &mut [Option<Variable>],
    ) -> Result<bool, ExecutionError> {
        for (col_idx, col) in ir.inputs.iter().enumerate() {
            if !self.evaluate_cell(col_idx, col, rule, col_refs)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn evaluate_row_inputs_full(
        &mut self,
        ir: &DecisionTableIr,
        rule: &HashMap<Arc<str>, Arc<str>>,
        col_refs: &mut [Option<Variable>],
        row_unreached: bool,
    ) -> Result<Vec<bool>, ExecutionError> {
        let mut row_failed = false;
        ir.inputs
            .iter()
            .enumerate()
            .map(|(col_idx, col)| {
                let passed = match self.evaluate_cell(col_idx, col, rule, col_refs) {
                    Ok(passed) => passed,
                    Err(_) if row_unreached || row_failed => false,
                    Err(e) => return Err(e),
                };
                row_failed |= !passed;
                Ok(passed)
            })
            .collect()
    }

    fn evaluate_cell(
        &mut self,
        col_idx: usize,
        col: &DecisionTableInputField,
        rule: &HashMap<Arc<str>, Arc<str>>,
        col_refs: &mut [Option<Variable>],
    ) -> Result<bool, ExecutionError> {
        let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
            return Ok(true);
        };
        match col.field.as_ref() {
            Some(field) if !field.is_empty() => {
                let value = match &col_refs[col_idx] {
                    Some(value) => value.shallow_clone(),
                    None => {
                        let value = self
                            .session
                            .run_standard(field)
                            .map_err(|e| self.site.expression_error(field, e))?;
                        col_refs[col_idx] = Some(value.shallow_clone());
                        value
                    }
                };
                self.session
                    .set_reference_value(value)
                    .map_err(|e| self.site.expression_error(field, e))?;
                self.session
                    .run_unary(cell)
                    .map_err(|e| self.site.expression_error(cell, e))
            }
            _ => {
                let result = self
                    .session
                    .run_standard(cell)
                    .map_err(|e| self.site.expression_error(cell, e))?;
                Ok(result.as_bool().unwrap_or(false))
            }
        }
    }
}
