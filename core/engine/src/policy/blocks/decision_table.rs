use std::sync::{Arc, OnceLock};

use ahash::{HashMap, HashSet};
use fixedbitset::FixedBitSet;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use zen_expression::intellisense::{ArmTest, IntelliSense, NumberCover};
use zen_expression::variable::{Variable, VariableType};
use zen_expression::Isolate;
use zen_types::decision::{
    DecisionTableHitPolicy, DecisionTableInputField, DecisionTableOutputField,
};

use base64::Engine as _;

use crate::policy::queries::scope::VariableTypeScope;
use crate::policy::types::{
    BlockTrace, Cursor, CursorTarget, DecisionTableExtras, Diagnostic, DiagnosticCode,
    ExpressionKind, NlExpression,
};

use crate::policy::ArcStrTrim;

use super::context::{AnalysisContext, ExecutionContext, ExecutionError};
use super::{
    Block, BlockKind, BlockReadPlan, CellReads, ConditionalReads, ExpressionLocation, ParseContext,
    ReadFlattenFn, WriteSite, WriteTarget,
};

pub(crate) struct TableSelection {
    pub(crate) matched_rows: Vec<u32>,
    pub(crate) used_cells: Vec<(u32, Arc<str>)>,
    input_bits: Option<Vec<u8>>,
}

const ROW_ID_KEY: &str = "_id";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableDoc {
    #[serde(default)]
    pub hit_policy: DecisionTableHitPolicy,
    #[serde(default)]
    pub inputs: Vec<DecisionTableInputField>,
    #[serde(default)]
    pub outputs: Vec<DecisionTableOutputField>,
    #[serde(default)]
    pub rules: Vec<HashMap<Arc<str>, Arc<str>>>,
}

impl DecisionTableDoc {
    pub(crate) fn decode_wire(mut value: serde_json::Value) -> Result<Self, String> {
        if let Some(obj) = value.as_object_mut() {
            let already_split = obj.contains_key("inputs") || obj.contains_key("outputs");
            if already_split && obj.contains_key("columns") {
                return Err(
                    "decision-table wire payload has both `columns` and `inputs`/`outputs`; expected exactly one form".into(),
                );
            }
            if !already_split {
                if let Some(serde_json::Value::Array(columns)) = obj.remove("columns") {
                    let mut inputs = Vec::new();
                    let mut outputs = Vec::new();
                    for col in columns {
                        let serde_json::Value::Object(mut col_obj) = col else {
                            return Err("decision-table column must be an object".into());
                        };
                        let kind = col_obj
                            .remove("kind")
                            .and_then(|k| k.as_str().map(str::to_owned));
                        match kind.as_deref() {
                            Some("input") => inputs.push(serde_json::Value::Object(col_obj)),
                            Some("output") => outputs.push(serde_json::Value::Object(col_obj)),
                            Some(other) => {
                                return Err(format!(
                                    "decision-table column has unknown kind '{other}' (expected 'input' or 'output')"
                                ));
                            }
                            None => {
                                return Err(
                                    "decision-table column is missing a 'kind' field".into()
                                );
                            }
                        }
                    }
                    obj.insert("inputs".to_string(), serde_json::Value::Array(inputs));
                    obj.insert("outputs".to_string(), serde_json::Value::Array(outputs));
                }
            }
        }

        serde_json::from_value(value).map_err(|e| format!("invalid decision table: {e}"))
    }
}

#[derive(Debug, Clone)]
pub struct DecisionTableIr {
    pub inputs: Vec<DecisionTableInputField>,
    pub outputs: Vec<OutputColumn>,
    pub rules: Vec<HashMap<Arc<str>, Arc<str>>>,
    index: OnceLock<Option<TableIndex>>,
}

const MIN_INDEX_ROWS: usize = 8;

#[derive(Debug, Clone)]
struct TableIndex {
    columns: Vec<Option<ColumnIndex>>,
}

#[derive(Debug, Clone)]
struct ColumnIndex {
    strings: HashMap<Arc<str>, FixedBitSet>,
    numbers: HashMap<Decimal, FixedBitSet>,
    bools: HashMap<bool, FixedBitSet>,
    captured: FixedBitSet,
    fallback: FixedBitSet,
}

impl TableIndex {
    fn build(table: &DecisionTableIr) -> Option<TableIndex> {
        let rows = table.rules.len();
        if rows < MIN_INDEX_ROWS {
            return None;
        }
        let mut intellisense = IntelliSense::new();
        let columns: Vec<Option<ColumnIndex>> = table
            .inputs
            .iter()
            .map(|col| ColumnIndex::build(table, col, rows, &mut intellisense))
            .collect();
        columns
            .iter()
            .any(Option::is_some)
            .then_some(TableIndex { columns })
    }

    fn decides(&self, col_idx: usize, row_idx: usize) -> bool {
        self.columns
            .get(col_idx)
            .and_then(Option::as_ref)
            .is_some_and(|c| c.captured.contains(row_idx))
    }
}

impl ColumnIndex {
    fn build(
        table: &DecisionTableIr,
        col: &DecisionTableInputField,
        rows: usize,
        intellisense: &mut IntelliSense,
    ) -> Option<ColumnIndex> {
        if col.field.as_deref().is_none_or(|f| f.is_empty()) {
            return None;
        }
        let mut strings: HashMap<Arc<str>, FixedBitSet> = HashMap::default();
        let mut numbers: HashMap<Decimal, FixedBitSet> = HashMap::default();
        let mut bools: HashMap<bool, FixedBitSet> = HashMap::default();
        let mut captured = FixedBitSet::with_capacity(rows);
        let mut fallback = FixedBitSet::with_capacity(rows);

        for (row_idx, rule) in table.rules.iter().enumerate() {
            let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                fallback.insert(row_idx);
                continue;
            };
            match intellisense.cell_test(cell) {
                ArmTest::Enum { values, .. } => {
                    for value in values {
                        strings
                            .entry(Arc::from(value.as_ref()))
                            .or_insert_with(|| FixedBitSet::with_capacity(rows))
                            .insert(row_idx);
                    }
                    captured.insert(row_idx);
                }
                ArmTest::Bool { values, .. } => {
                    for value in values {
                        bools
                            .entry(value)
                            .or_insert_with(|| FixedBitSet::with_capacity(rows))
                            .insert(row_idx);
                    }
                    captured.insert(row_idx);
                }
                ArmTest::Number { cover, .. } => match cover.points() {
                    Some(points) => {
                        for point in points {
                            numbers
                                .entry(point.normalize())
                                .or_insert_with(|| FixedBitSet::with_capacity(rows))
                                .insert(row_idx);
                        }
                        captured.insert(row_idx);
                    }
                    None => {
                        fallback.insert(row_idx);
                    }
                },
                ArmTest::Default | ArmTest::Unrecognized => {
                    fallback.insert(row_idx);
                }
            }
        }

        (captured.count_ones(..) > 0).then_some(ColumnIndex {
            strings,
            numbers,
            bools,
            captured,
            fallback,
        })
    }

    fn rows_for(&self, value: &Variable) -> Option<&FixedBitSet> {
        match value {
            Variable::String(s) => self.strings.get(s.as_ref()),
            Variable::Number(n) => self.numbers.get(&n.normalize()),
            Variable::Bool(b) => self.bools.get(b),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputColumn {
    pub id: Arc<str>,
    pub field: Arc<str>,
    pub raw_field: Arc<str>,
    pub collect: bool,
}

impl DecisionTableIr {
    pub(crate) fn parse(
        id: &Arc<str>,
        doc: &DecisionTableDoc,
        policy_path: &Arc<str>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Block {
        let mut cx = ParseContext {
            policy_path,
            block_id: id,
            diagnostics,
        };
        if doc.rules.is_empty() {
            cx.block_warning(DiagnosticCode::EmptyBlock, "decision table has no rules");
        }

        let mut inputs = doc.inputs.clone();
        for col in inputs.iter_mut() {
            if let Some(f) = col.field.as_ref() {
                col.field = Some(f.trimmed());
            }
            col.name = col.name.trimmed();
        }

        let collect_all = doc.hit_policy == DecisionTableHitPolicy::Collect;
        let outputs = doc
            .outputs
            .iter()
            .map(|col| Self::parse_output_column(col, collect_all, &mut cx))
            .collect();

        Block {
            id: id.clone(),
            kind: BlockKind::DecisionTable(Arc::new(DecisionTableIr {
                inputs,
                outputs,
                rules: doc.rules.clone(),
                index: OnceLock::new(),
            })),
        }
    }

    fn parse_output_column(
        col: &DecisionTableOutputField,
        collect_all: bool,
        cx: &mut ParseContext,
    ) -> OutputColumn {
        let raw_field = col.field.trimmed();
        let (path, collect) = match raw_field.strip_suffix("[]") {
            Some(stripped) => (stripped.trim_end(), true),
            None => (raw_field.as_ref(), collect_all),
        };

        let field: Arc<str> = if raw_field.is_empty() {
            cx.expression_warning(
                &col.id,
                DiagnosticCode::InvalidWritePath,
                "output column is missing a field path",
            );
            Arc::from("")
        } else if path.is_empty() {
            cx.expression_error(
                &col.id,
                DiagnosticCode::InvalidWritePath,
                "output field '[]' is missing a path before the collect marker",
            );
            Arc::from("")
        } else if path.contains("[]") {
            cx.expression_error(
                &col.id,
                DiagnosticCode::InvalidWritePath,
                format!("invalid write path '{raw_field}': `[]` may only appear at the end of an output field"),
            );
            Arc::from("")
        } else if let Err(reason) = WriteTarget::validate_path(path) {
            cx.expression_error(
                &col.id,
                DiagnosticCode::InvalidWritePath,
                format!("invalid write path '{path}': {reason}"),
            );
            Arc::from("")
        } else {
            Arc::from(path)
        };

        OutputColumn {
            id: col.id.clone(),
            field,
            raw_field,
            collect,
        }
    }

    pub(super) fn expressions(&self, block_id: &Arc<str>) -> Vec<ExpressionLocation> {
        let mut out = Vec::new();
        for rule in &self.rules {
            for col in &self.inputs {
                let Some(cell) = rule.get(&col.id) else {
                    continue;
                };
                let kind = match col.field.as_deref() {
                    Some(f) if !f.is_empty() => ExpressionKind::Unary,
                    _ => ExpressionKind::Standard,
                };
                if let Some(loc) = ExpressionLocation::try_new(
                    block_id.clone(),
                    col.id.clone(),
                    kind,
                    cell.clone(),
                ) {
                    out.push(loc);
                }
            }
            for col in &self.outputs {
                let Some(cell) = rule.get(&col.id) else {
                    continue;
                };
                if let Some(loc) = ExpressionLocation::try_new(
                    block_id.clone(),
                    col.id.clone(),
                    ExpressionKind::Standard,
                    cell.clone(),
                ) {
                    out.push(loc);
                }
            }
        }
        out
    }

    pub(super) fn write_sites(&self) -> Vec<WriteSite> {
        let shared_inputs: HashSet<Arc<str>> = self.inputs.iter().map(|c| c.id.clone()).collect();
        self.outputs
            .iter()
            .filter(|c| !c.field.is_empty())
            .map(|c| {
                let mut contributing = shared_inputs.clone();
                contributing.insert(c.id.clone());
                WriteSite {
                    path: c.field.clone(),
                    expression_id: Some(c.id.clone()),
                    resolved_type: VariableType::Any,
                    contributing_expr_ids: contributing,
                }
            })
            .collect()
    }

    pub(super) fn analyze(&self, cx: &mut AnalysisContext) {
        let mut input_cell_scopes: HashMap<Arc<str>, VariableType> = HashMap::default();
        let mut input_field_types: HashMap<Arc<str>, VariableType> = HashMap::default();
        for col in &self.inputs {
            let Some(field) = col.field.as_ref().filter(|f| !f.is_empty()) else {
                continue;
            };
            let field_analysis = cx.analyze_standard(field, Some(col.id.clone()));
            input_field_types.insert(col.id.clone(), field_analysis.return_type.clone());
            input_cell_scopes.insert(
                col.id.clone(),
                cx.scope().with_dollar(&field_analysis.return_type),
            );
        }

        for rule in &self.rules {
            for col in &self.inputs {
                let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                    continue;
                };

                if let Some(cell_scope) = input_cell_scopes.get(&col.id) {
                    cx.analyze_unary_in_scope(cell, cell_scope, Some(col.id.clone()));
                    continue;
                }

                let analysis = cx.analyze_standard(cell, Some(col.id.clone()));
                if !matches!(analysis.return_type, VariableType::Bool | VariableType::Any) {
                    cx.error(
                        DiagnosticCode::TypeMismatch,
                        Some(col.id.clone()),
                        None,
                        format!(
                            "input condition must return a boolean, got {:?}",
                            analysis.return_type
                        ),
                    );
                }
            }
        }

        for col in &self.outputs {
            if col.field.is_empty() {
                continue;
            }

            let mut cell_types: Vec<VariableType> = Vec::new();
            for rule in &self.rules {
                let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                    continue;
                };
                let analysis = cx.analyze_standard(cell, Some(col.id.clone()));
                cell_types.push(analysis.return_type.clone());
            }

            if !col.collect && cx.is_enriched() && !self.column_covered(col, cx, &input_field_types)
            {
                cell_types.push(VariableType::Null);
            }

            let target = Some(CursorTarget::DecisionTableHead {
                col: col.id.clone(),
            });
            let mut resolved = cx.merge_types(
                &cell_types,
                &col.field,
                Some(col.id.clone()),
                target.clone(),
            );

            if col.collect {
                resolved = resolved.array();
            }

            cx.record_write(col.field.clone(), resolved, Some(col.id.clone()), target);
        }
    }

    fn column_covered(
        &self,
        col: &OutputColumn,
        cx: &mut AnalysisContext,
        input_field_types: &HashMap<Arc<str>, VariableType>,
    ) -> bool {
        let value_rows: Vec<&HashMap<Arc<str>, Arc<str>>> = self
            .rules
            .iter()
            .filter(|rule| rule.get(&col.id).is_some_and(|c| !c.is_empty()))
            .collect();
        if value_rows.is_empty() {
            return false;
        }

        let row_is_catch_all = |rule: &HashMap<Arc<str>, Arc<str>>| {
            self.inputs
                .iter()
                .all(|ic| rule.get(&ic.id).is_none_or(|c| c.is_empty()))
        };
        if value_rows.iter().any(|rule| row_is_catch_all(rule)) {
            return true;
        }

        let mut groups: HashMap<Arc<str>, Vec<ArmTest>> = HashMap::default();
        for rule in &value_rows {
            let mut constrained = self
                .inputs
                .iter()
                .filter(|ic| rule.get(&ic.id).is_some_and(|c| !c.is_empty()));
            let (Some(ic), None) = (constrained.next(), constrained.next()) else {
                continue;
            };
            if ic.field.as_deref().is_none_or(str::is_empty) {
                continue;
            }
            let Some(cell) = rule.get(&ic.id) else {
                continue;
            };
            groups
                .entry(ic.id.clone())
                .or_default()
                .push(cx.cell_test(cell));
        }

        groups.iter().any(|(col_id, tests)| {
            input_field_types
                .get(col_id)
                .is_some_and(|t| Self::cells_cover(tests, t))
        })
    }

    fn cells_cover(tests: &[ArmTest], resolved_type: &VariableType) -> bool {
        let (resolved, nullable) = resolved_type.unwrap_nullable();
        if nullable {
            return false;
        }
        match resolved {
            VariableType::Enum(_, declared) => {
                let mut collected: HashSet<&str> = HashSet::default();
                for test in tests {
                    let ArmTest::Enum { values, .. } = test else {
                        return false;
                    };
                    collected.extend(values.iter().map(|v| v.as_ref()));
                }
                declared.iter().all(|d| collected.contains(d.as_ref()))
            }
            VariableType::Bool => {
                let (mut seen_true, mut seen_false) = (false, false);
                for test in tests {
                    let ArmTest::Bool { values, .. } = test else {
                        return false;
                    };
                    for value in values {
                        seen_true |= *value;
                        seen_false |= !*value;
                    }
                }
                seen_true && seen_false
            }
            VariableType::Number => {
                let mut cover: Option<NumberCover> = None;
                for test in tests {
                    let ArmTest::Number { cover: segment, .. } = test else {
                        return false;
                    };
                    match &mut cover {
                        Some(acc) => acc.merged_with(segment),
                        None => cover = Some(segment.clone()),
                    }
                }
                cover.is_some_and(|c| c.is_total())
            }
            _ => false,
        }
    }

    pub(super) fn write_target(&self, path: &str) -> Option<CursorTarget> {
        self.outputs
            .iter()
            .find(|c| c.field.as_ref() == path)
            .map(|c| CursorTarget::DecisionTableHead { col: c.id.clone() })
    }

    pub(super) fn read_plan(&self, flatten: &mut ReadFlattenFn) -> BlockReadPlan {
        let mut unconditional = Vec::new();
        for col in &self.inputs {
            if let Some(field) = col.field.as_ref().filter(|f| !f.is_empty()) {
                unconditional.extend(flatten(field, ExpressionKind::Standard));
            }
        }

        let mut cells = Vec::new();
        for (row_idx, rule) in self.rules.iter().enumerate() {
            for col in &self.inputs {
                let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                    continue;
                };
                let kind = match col.field.as_deref() {
                    Some(f) if !f.is_empty() => ExpressionKind::Unary,
                    _ => ExpressionKind::Standard,
                };
                unconditional.extend(flatten(cell, kind));
            }
            for col in &self.outputs {
                if col.field.is_empty() {
                    continue;
                }
                let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                    continue;
                };
                cells.push(CellReads {
                    row_idx: row_idx as u32,
                    col_id: col.id.clone(),
                    cell_reads: BlockReadPlan::dedup(flatten(cell, ExpressionKind::Standard)),
                });
            }
        }

        BlockReadPlan {
            unconditional: BlockReadPlan::dedup(unconditional),
            conditional: ConditionalReads::DecisionTable(Arc::from(cells)),
        }
    }

    pub(crate) fn select(&self, cx: &ExecutionContext) -> Result<TableSelection, ExecutionError> {
        let mut isolate = cx.isolate.borrow_mut();
        let mut col_refs: Vec<Option<Variable>> = vec![None; self.inputs.len()];

        let active: Vec<&OutputColumn> = self
            .outputs
            .iter()
            .filter(|c| !c.field.is_empty())
            .collect();
        let has_collect = active.iter().any(|c| c.collect);
        let mut pending_scalars = active.iter().filter(|c| !c.collect).count();
        let mut taken: Vec<bool> = vec![false; active.len()];

        let mut matched_rows: Vec<u32> = Vec::new();
        let mut used_cells: Vec<(u32, Arc<str>)> = Vec::new();
        let cols = self.inputs.len();
        let bytes_per_row = cols.div_ceil(8);
        let mut input_bits: Option<Vec<u8>> = cx
            .extras
            .then(|| vec![0u8; self.rules.len() * bytes_per_row]);
        let candidates = match input_bits {
            None => self.candidate_rows(&mut isolate, &mut col_refs, cx)?,
            Some(_) => None,
        };

        for (row_idx, rule) in self.rules.iter().enumerate() {
            let satisfied = !has_collect && pending_scalars == 0 && !matched_rows.is_empty();
            if satisfied && !cx.extras {
                break;
            }

            let row_matches = match input_bits.as_mut() {
                Some(bits) => {
                    let per_col = self.evaluate_row_inputs_full(
                        rule,
                        &mut isolate,
                        &mut col_refs,
                        cx,
                        satisfied,
                    )?;
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
                    Some(_) => self.evaluate_row_inputs_pruned(
                        row_idx,
                        rule,
                        &mut isolate,
                        &mut col_refs,
                        cx,
                    )?,
                    None => self.evaluate_row_inputs(rule, &mut isolate, &mut col_refs, cx)?,
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

    pub(crate) fn commit(
        &self,
        cx: &ExecutionContext,
        selection: &TableSelection,
    ) -> Result<BlockTrace, ExecutionError> {
        let mut isolate = cx.isolate.borrow_mut();

        let used: HashSet<(u32, &str)> = selection
            .used_cells
            .iter()
            .map(|(row, col)| (*row, col.as_ref()))
            .collect();
        let mut collected: HashMap<Arc<str>, Vec<Variable>> = HashMap::default();
        let mut scalar_written: HashSet<Arc<str>> = HashSet::default();
        for col in &self.outputs {
            if col.collect && !col.field.is_empty() {
                collected.entry(col.field.clone()).or_default();
            }
        }

        let mut evaluations: Vec<HashMap<Arc<str>, Variable>> = Vec::new();
        for &row_idx in &selection.matched_rows {
            let Some(rule) = self.rules.get(row_idx as usize) else {
                continue;
            };
            let mut row_outputs: HashMap<Arc<str>, Variable> = HashMap::default();

            for col in &self.outputs {
                if !used.contains(&(row_idx, col.id.as_ref())) {
                    continue;
                }
                let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                    continue;
                };

                let value = isolate
                    .run_standard(cell)
                    .map_err(|e| cx.expression_error(cell, e))?;

                if cx.trace {
                    row_outputs.insert(col.field.clone(), value.deep_clone());
                }

                if col.collect {
                    collected.entry(col.field.clone()).or_default().push(value);
                } else {
                    cx.write(&col.field, value);
                    scalar_written.insert(col.field.clone());
                }
            }
            if cx.trace {
                evaluations.push(row_outputs);
            }
        }

        for col in &self.outputs {
            if !col.collect && !col.field.is_empty() && !scalar_written.contains(&col.field) {
                cx.write(&col.field, Variable::Null);
                scalar_written.insert(col.field.clone());
            }
        }
        for (field, values) in collected {
            cx.write(&field, Variable::from_array(values));
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

    pub(super) fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        let selection = self.select(cx)?;
        self.commit(cx, &selection)
    }

    pub(super) fn nl(
        &self,
        policy_path: &Arc<str>,
        block_id: &Arc<str>,
        scope: &VariableType,
        is: &mut IntelliSense,
    ) -> Vec<NlExpression> {
        let mut out = Vec::new();
        let mut input_scopes: HashMap<Arc<str>, (ExpressionKind, VariableType)> = HashMap::default();

        for col in &self.inputs {
            match col.field.as_ref().filter(|f| !f.is_empty()) {
                Some(field) => {
                    out.push(NlExpression::project(
                        is,
                        policy_path,
                        block_id,
                        CursorTarget::DecisionTableHead {
                            col: col.id.clone(),
                        },
                        ExpressionKind::Standard,
                        field.as_ref(),
                        scope,
                    ));
                    let field_type = is.analyze(field.as_ref(), scope).return_type.clone();
                    input_scopes.insert(
                        col.id.clone(),
                        (ExpressionKind::Unary, scope.with_dollar(&field_type)),
                    );
                }
                None => {
                    input_scopes
                        .insert(col.id.clone(), (ExpressionKind::Standard, scope.shallow_clone()));
                }
            }
        }

        for rule in &self.rules {
            let Some(row) = rule.get(ROW_ID_KEY) else {
                continue;
            };
            for col in &self.inputs {
                let cell: &str = rule.get(&col.id).map(|c| c.as_ref()).unwrap_or("");
                let Some((kind, cell_scope)) = input_scopes.get(&col.id) else {
                    continue;
                };
                out.push(NlExpression::project(
                    is,
                    policy_path,
                    block_id,
                    CursorTarget::DecisionTableCell {
                        row: row.clone(),
                        col: col.id.clone(),
                    },
                    *kind,
                    cell,
                    cell_scope,
                ));
            }
            for col in &self.outputs {
                let cell: &str = rule.get(&col.id).map(|c| c.as_ref()).unwrap_or("");
                out.push(NlExpression::project(
                    is,
                    policy_path,
                    block_id,
                    CursorTarget::DecisionTableCell {
                        row: row.clone(),
                        col: col.id.clone(),
                    },
                    ExpressionKind::Standard,
                    cell,
                    scope,
                ));
            }
        }

        out
    }

    pub(super) fn nl_scope(
        &self,
        cursor: &Cursor,
        scope: VariableType,
        is: &mut IntelliSense,
    ) -> (ExpressionKind, VariableType) {
        let CursorTarget::DecisionTableCell { col, .. } = &cursor.target else {
            return (ExpressionKind::Standard, scope);
        };
        let Some(ColumnRef::Input(column)) = self.column_by_id(col) else {
            return (ExpressionKind::Standard, scope);
        };
        match column.field.as_ref().filter(|f| !f.is_empty()) {
            Some(field) => {
                let field_type = is.analyze(field.as_ref(), &scope).return_type.clone();
                (ExpressionKind::Unary, scope.with_dollar(&field_type))
            }
            None => (ExpressionKind::Standard, scope),
        }
    }

    pub(super) fn resolve_cursor(
        &self,
        cursor: &Cursor,
        scope: VariableType,
    ) -> Option<(Arc<str>, ExpressionKind, VariableType)> {
        let (col_id, row_id) = match &cursor.target {
            CursorTarget::DecisionTableHead { col } => {
                let column = self.column_by_id(col)?;
                let head = match column {
                    ColumnRef::Input(c) => c.field.clone().unwrap_or_else(|| Arc::from("")),
                    ColumnRef::Output(c) => c.field.clone(),
                };
                return Some((head, ExpressionKind::Standard, scope));
            }
            CursorTarget::DecisionTableCell { row, col } => (col.clone(), row.clone()),
            _ => return None,
        };

        let column = self.column_by_id(&col_id)?;
        let rule = self
            .rules
            .iter()
            .find(|r| r.get(ROW_ID_KEY).map(|s| s.as_ref()) == Some(row_id.as_ref()))?;
        let source = rule.get(&col_id).filter(|c| !c.is_empty()).cloned()?;

        let (kind, narrowed_scope) = match column {
            ColumnRef::Input(c) => match c.field.as_deref() {
                Some(f) if !f.is_empty() => (ExpressionKind::Unary, scope.resolve_at(f)),
                _ => (ExpressionKind::Standard, scope),
            },
            ColumnRef::Output(_) => (ExpressionKind::Standard, scope),
        };
        Some((source, kind, narrowed_scope))
    }

    pub(super) fn write_keys(&self) -> Vec<(Option<Arc<str>>, Arc<str>)> {
        let mut keys: Vec<_> = self
            .inputs
            .iter()
            .filter_map(|col| {
                col.field
                    .as_ref()
                    .filter(|f| !f.is_empty())
                    .map(|f| (Some(col.id.clone()), f.clone()))
            })
            .collect();
        keys.extend(
            self.outputs
                .iter()
                .filter(|c| !c.field.is_empty())
                .map(|c| (Some(c.id.clone()), c.raw_field.clone())),
        );
        keys
    }
}

enum ColumnRef<'a> {
    Input(&'a DecisionTableInputField),
    Output(&'a OutputColumn),
}

impl DecisionTableIr {
    fn column_by_id(&self, id: &Arc<str>) -> Option<ColumnRef<'_>> {
        if let Some(c) = self.inputs.iter().find(|c| c.id == *id) {
            return Some(ColumnRef::Input(c));
        }
        self.outputs
            .iter()
            .find(|c| c.id == *id)
            .map(ColumnRef::Output)
    }

    fn table_index(&self) -> Option<&TableIndex> {
        self.index.get_or_init(|| TableIndex::build(self)).as_ref()
    }

    fn candidate_rows(
        &self,
        isolate: &mut Isolate,
        col_refs: &mut [Option<Variable>],
        cx: &ExecutionContext,
    ) -> Result<Option<FixedBitSet>, ExecutionError> {
        let Some(index) = self.table_index() else {
            return Ok(None);
        };
        let mut acc: Option<FixedBitSet> = None;
        for (col_idx, column) in index.columns.iter().enumerate() {
            let Some(column) = column else {
                continue;
            };
            let Some(field) = self.inputs[col_idx]
                .field
                .as_ref()
                .filter(|f| !f.is_empty())
            else {
                continue;
            };
            let value = match &col_refs[col_idx] {
                Some(value) => value.shallow_clone(),
                None => {
                    let value = isolate
                        .run_standard(field)
                        .map_err(|e| cx.expression_error(field, e))?;
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
        &self,
        row_idx: usize,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        col_refs: &mut [Option<Variable>],
        cx: &ExecutionContext,
    ) -> Result<bool, ExecutionError> {
        let index = self.table_index();
        for (col_idx, col) in self.inputs.iter().enumerate() {
            if index.is_some_and(|ix| ix.decides(col_idx, row_idx)) {
                continue;
            }
            if !self.evaluate_cell(col_idx, col, rule, isolate, col_refs, cx)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn evaluate_row_inputs(
        &self,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        col_refs: &mut [Option<Variable>],
        cx: &ExecutionContext,
    ) -> Result<bool, ExecutionError> {
        for (col_idx, col) in self.inputs.iter().enumerate() {
            if !self.evaluate_cell(col_idx, col, rule, isolate, col_refs, cx)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn evaluate_row_inputs_full(
        &self,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        col_refs: &mut [Option<Variable>],
        cx: &ExecutionContext,
        row_unreached: bool,
    ) -> Result<Vec<bool>, ExecutionError> {
        let mut row_failed = false;
        self.inputs
            .iter()
            .enumerate()
            .map(|(col_idx, col)| {
                let passed = match self.evaluate_cell(col_idx, col, rule, isolate, col_refs, cx) {
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
        &self,
        col_idx: usize,
        col: &DecisionTableInputField,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        col_refs: &mut [Option<Variable>],
        cx: &ExecutionContext,
    ) -> Result<bool, ExecutionError> {
        let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
            return Ok(true);
        };
        match col.field.as_ref() {
            Some(field) if !field.is_empty() => {
                let value = match &col_refs[col_idx] {
                    Some(value) => value.shallow_clone(),
                    None => {
                        let value = isolate
                            .run_standard(field)
                            .map_err(|e| cx.expression_error(field, e))?;
                        col_refs[col_idx] = Some(value.shallow_clone());
                        value
                    }
                };
                isolate
                    .set_reference_value(value)
                    .map_err(|e| cx.expression_error(field, e))?;
                isolate
                    .run_unary(cell)
                    .map_err(|e| cx.expression_error(cell, e))
            }
            _ => {
                let result = isolate
                    .run_standard(cell)
                    .map_err(|e| cx.expression_error(cell, e))?;
                Ok(result.as_bool().unwrap_or(false))
            }
        }
    }
}
