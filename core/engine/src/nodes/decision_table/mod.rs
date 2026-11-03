use crate::nodes::definition::NodeHandler;
use crate::nodes::result::NodeResult;
use crate::nodes::{NodeContext, NodeResponse};
use ahash::HashMap;
use fixedbitset::FixedBitSet;
use index::TableIndex;
use serde::Serialize;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::variable::ToVariable;
use zen_expression::Isolate;
use zen_types::decision::{
    DecisionTableContent, DecisionTableHitPolicy, DecisionTableInputField, TransformAttributes,
};
use zen_types::variable::Variable;
pub(crate) mod index;

#[derive(Debug, Clone)]
pub struct DecisionTableNodeHandler;

pub type DecisionTableNodeData = DecisionTableContent;

type DecisionTableContext = NodeContext<DecisionTableNodeData, DecisionTableNodeTrace>;

impl NodeHandler for DecisionTableNodeHandler {
    type NodeData = DecisionTableNodeData;
    type TraceData = DecisionTableNodeTrace;

    fn transform_attributes(
        &self,
        ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Option<TransformAttributes> {
        Some(ctx.node.transform_attributes.clone())
    }

    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let has_collect_columns = ctx.node.outputs.iter().any(|output| output.write_path().1);
        match ctx.node.hit_policy {
            DecisionTableHitPolicy::First if has_collect_columns => {
                self.handle_first_hit_collect(ctx)
            }
            DecisionTableHitPolicy::First => self.handle_first_hit(ctx),
            DecisionTableHitPolicy::Collect => self.handle_collect(ctx),
        }
    }
}

impl DecisionTableNodeHandler {
    fn handle_first_hit(&self, ctx: DecisionTableContext) -> NodeResult {
        let mut isolate = ctx.isolate();

        if !ctx.config.trace {
            let index = Self::table_index(&ctx);
            let candidates =
                index.and_then(|ix| Self::candidate_rows(ix, &ctx.node.inputs, &mut isolate));
            let pruner = candidates.as_ref().and(index);
            for (row_idx, rule) in ctx.node.rules.iter().enumerate() {
                if candidates.as_ref().is_some_and(|c| !c.contains(row_idx)) {
                    continue;
                }
                let pruned = pruner.map(|ix| (ix, row_idx));
                if let Some(RowResult::Output(output)) =
                    self.evaluate_row(&ctx, rule, &mut isolate, pruned)
                {
                    return ctx.success(output);
                }
            }
            return Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            });
        }

        let hit = ctx.node.rules.iter().enumerate().find_map(|(index, rule)| {
            match self.evaluate_row(&ctx, rule, &mut isolate, None)? {
                RowResult::WithTrace {
                    output,
                    reference_map,
                    rule,
                } => Some((index, output, reference_map, rule)),
                RowResult::Output(output) => {
                    Some((index, output, Default::default(), Default::default()))
                }
            }
        });

        match hit {
            Some((index, output, reference_map, rule)) => {
                ctx.trace(|t| {
                    *t = DecisionTableNodeTrace::FirstHit(DecisionTableRowTrace {
                        reference_map,
                        index,
                        rule,
                    })
                });
                ctx.success(output)
            }
            None => Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            }),
        }
    }

    fn handle_collect(&self, ctx: DecisionTableContext) -> NodeResult {
        let mut outputs = Vec::new();
        let mut traces = Vec::new();
        let mut isolate = ctx.isolate();

        let table_index = (!ctx.config.trace)
            .then(|| Self::table_index(&ctx))
            .flatten();
        let candidates =
            table_index.and_then(|ix| Self::candidate_rows(ix, &ctx.node.inputs, &mut isolate));
        let pruner = candidates.as_ref().and(table_index);

        for (index, rule) in ctx.node.rules.iter().enumerate() {
            if candidates.as_ref().is_some_and(|c| !c.contains(index)) {
                continue;
            }
            let pruned = pruner.map(|ix| (ix, index));
            if let Some(result) = self.evaluate_row(&ctx, rule, &mut isolate, pruned) {
                match result {
                    RowResult::Output(output) => {
                        outputs.push(output);
                    }
                    RowResult::WithTrace {
                        output,
                        reference_map,
                        rule,
                    } => {
                        outputs.push(output);
                        traces.push(DecisionTableRowTrace {
                            index,
                            rule,
                            reference_map,
                        });
                    }
                }
            }
        }

        ctx.trace(|t| {
            *t = DecisionTableNodeTrace::Collect(traces);
        });

        ctx.success(Variable::from_array(outputs))
    }

    pub(crate) fn cell_passes(
        rule: &HashMap<Arc<str>, Arc<str>>,
        input: &zen_types::decision::DecisionTableInputField,
        isolate: &mut Isolate,
    ) -> bool {
        let Some(rule_value) = rule.get(&input.id) else {
            return true;
        };
        if rule_value.is_empty() {
            return true;
        }
        match &input.field {
            None => isolate
                .run_standard(rule_value)
                .ok()
                .and_then(|result| result.as_bool())
                .unwrap_or(false),
            Some(field) => {
                if isolate.set_reference(field).is_err() {
                    return false;
                }
                isolate.run_unary(rule_value).unwrap_or(false)
            }
        }
    }

    fn table_index(ctx: &DecisionTableContext) -> Option<&TableIndex> {
        ctx.extensions.dt_indexes.as_ref()?.get(&ctx.id)
    }

    fn candidate_rows(
        index: &TableIndex,
        inputs: &[DecisionTableInputField],
        isolate: &mut Isolate,
    ) -> Option<FixedBitSet> {
        let mut acc: Option<FixedBitSet> = None;
        for (col_idx, column) in index.columns.iter().enumerate() {
            let Some(column) = column else {
                continue;
            };
            let Some(field) = inputs[col_idx].field.as_ref().filter(|f| !f.is_empty()) else {
                continue;
            };
            isolate.set_reference(field).ok()?;
            let value = isolate.get_reference(field)?;
            if matches!(value, Variable::Dynamic(_)) {
                return None;
            }
            let hit = column.rows_for(&value);
            match &mut acc {
                None => {
                    let mut first = column.fallback.clone();
                    if let Some(hit) = hit {
                        first.union_with(hit);
                    }
                    acc = Some(first);
                }
                Some(acc) => {
                    let fallback = column.fallback.as_slice();
                    let hit = hit.map(FixedBitSet::as_slice).unwrap_or_default();
                    for (i, word) in acc.as_mut_slice().iter_mut().enumerate() {
                        let f = fallback.get(i).copied().unwrap_or(0);
                        let h = hit.get(i).copied().unwrap_or(0);
                        *word &= f | h;
                    }
                }
            }
        }
        acc
    }

    fn handle_first_hit_collect(&self, ctx: DecisionTableContext) -> NodeResult {
        let mut isolate = ctx.isolate();

        let table_index = (!ctx.config.trace)
            .then(|| Self::table_index(&ctx))
            .flatten();
        let candidates =
            table_index.and_then(|ix| Self::candidate_rows(ix, &ctx.node.inputs, &mut isolate));
        let pruner = candidates.as_ref().and(table_index);

        let mut scalars: Option<Variable> = None;
        let mut collected: Vec<Vec<Variable>> = vec![Vec::new(); ctx.node.outputs.len()];
        let mut matched = false;
        let mut traces = Vec::new();

        for (row_idx, rule) in ctx.node.rules.iter().enumerate() {
            if candidates.as_ref().is_some_and(|c| !c.contains(row_idx)) {
                continue;
            }
            let pruned = pruner.map(|ix| (ix, row_idx));
            if !Self::row_matches(&ctx, rule, &mut isolate, pruned) {
                continue;
            }

            let Some((row_scalars, row_collects)) =
                Self::evaluate_row_cells(&ctx, rule, &mut isolate, scalars.is_none())
            else {
                continue;
            };
            matched = true;
            if let Some(row_scalars) = row_scalars {
                scalars = Some(row_scalars);
            }
            for (column_idx, value) in row_collects {
                collected[column_idx].push(value);
            }

            if ctx.config.trace {
                let (reference_map, rule_trace) = Self::row_trace_parts(&ctx, rule, &mut isolate);
                traces.push(DecisionTableRowTrace {
                    index: row_idx,
                    reference_map,
                    rule: rule_trace,
                });
            }
        }

        if !matched {
            return Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            });
        }

        let output = scalars.unwrap_or_else(Variable::empty_object);
        for (column_idx, column) in ctx.node.outputs.iter().enumerate() {
            let (path, collect) = column.write_path();
            if !collect || path.is_empty() {
                continue;
            }
            let values = std::mem::take(&mut collected[column_idx]);
            output.dot_insert(path, Variable::from_array(values));
        }

        ctx.trace(|t| {
            *t = DecisionTableNodeTrace::Collect(traces);
        });
        ctx.success(output)
    }

    fn row_matches(
        ctx: &DecisionTableContext,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        pruned: Option<(&TableIndex, usize)>,
    ) -> bool {
        for (col_idx, input) in ctx.node.inputs.iter().enumerate() {
            if pruned.is_some_and(|(ix, row_idx)| ix.decides(col_idx, row_idx)) {
                continue;
            }
            let Some(rule_value) = rule.get(&input.id) else {
                continue;
            };
            if rule_value.is_empty() {
                continue;
            }

            let passed = match &input.field {
                None => isolate
                    .run_standard(rule_value)
                    .ok()
                    .and_then(|result| result.as_bool())
                    .unwrap_or(false),
                Some(field) => {
                    isolate.set_reference(field).is_ok()
                        && isolate.run_unary(rule_value).unwrap_or(false)
                }
            };
            if !passed {
                return false;
            }
        }
        true
    }

    fn evaluate_row_cells(
        ctx: &DecisionTableContext,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        include_scalars: bool,
    ) -> Option<(Option<Variable>, Vec<(usize, Variable)>)> {
        let scalars = include_scalars.then(Variable::empty_object);
        let mut collects = Vec::new();
        for (column_idx, output) in ctx.node.outputs.iter().enumerate() {
            let (path, collect) = output.write_path();
            if path.is_empty() || (!collect && !include_scalars) {
                continue;
            }
            let Some(rule_value) = rule.get(&output.id) else {
                continue;
            };
            if rule_value.is_empty() {
                continue;
            }

            let value = isolate.run_standard(rule_value).ok()?.deep_clone();
            if collect {
                collects.push((column_idx, value));
            } else if let Some(scalars) = &scalars {
                scalars.dot_insert(path, value);
            }
        }
        Some((scalars, collects))
    }

    fn row_trace_parts(
        ctx: &DecisionTableContext,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
    ) -> (HashMap<Rc<str>, Variable>, HashMap<Rc<str>, Rc<str>>) {
        let id_str = Rc::<str>::from("_id");
        let description_str = Rc::<str>::from("_description");

        let rule_id = match rule.get(id_str.as_ref()) {
            Some(rid) => Rc::<str>::from(rid.deref()),
            None => Rc::from(""),
        };

        let mut expressions: HashMap<Rc<str>, Rc<str>> = Default::default();
        let mut reference_map: HashMap<Rc<str>, Variable> = Default::default();

        expressions.insert(id_str.clone(), rule_id.clone());
        if let Some(description) = rule.get(description_str.as_ref()) {
            expressions.insert(description_str.clone(), Rc::from(description.deref()));
        }

        for input in ctx.node.inputs.iter() {
            let Some(rule_value) = rule.get(input.id.deref()) else {
                continue;
            };
            let Some(input_field) = &input.field else {
                continue;
            };

            if let Some(reference) = isolate.get_reference(input_field.deref()) {
                reference_map.insert(Rc::from(input_field.deref()), reference);
            } else if let Some(reference) = isolate.run_standard(input_field.deref()).ok() {
                reference_map.insert(Rc::from(input_field.deref()), reference);
            }

            let input_identifier = format!("{input_field}[{}]", &input.id);
            expressions.insert(
                Rc::from(input_identifier.as_str()),
                Rc::from(rule_value.deref()),
            );
        }

        (reference_map, expressions)
    }

    fn evaluate_row<'a>(
        &self,
        ctx: &'a DecisionTableContext,
        rule: &'a HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        pruned: Option<(&TableIndex, usize)>,
    ) -> Option<RowResult> {
        if !Self::row_matches(ctx, rule, isolate, pruned) {
            return None;
        }

        let outputs = Variable::empty_object();
        for output in ctx.node.outputs.iter() {
            let (path, _) = output.write_path();
            if path.is_empty() {
                continue;
            }
            let Some(rule_value) = rule.get(&output.id) else {
                continue;
            };
            if rule_value.is_empty() {
                continue;
            }

            let res = isolate.run_standard(rule_value).ok()?;
            outputs.dot_insert(path, res.deep_clone());
        }

        if !ctx.config.trace {
            return Some(RowResult::Output(outputs));
        }

        let (reference_map, expressions) = Self::row_trace_parts(ctx, rule, isolate);
        Some(RowResult::WithTrace {
            output: outputs.to_variable(),
            reference_map,
            rule: expressions,
        })
    }
}

enum RowResult {
    Output(Variable),
    WithTrace {
        output: Variable,
        reference_map: HashMap<Rc<str>, Variable>,
        rule: HashMap<Rc<str>, Rc<str>>,
    },
}

#[derive(Debug, Clone, Serialize, ToVariable)]
pub struct DecisionTableRowTrace {
    index: usize,
    reference_map: HashMap<Rc<str>, Variable>,
    rule: HashMap<Rc<str>, Rc<str>>,
}

#[derive(Debug, Clone, Serialize, ToVariable)]
#[serde(untagged)]
pub enum DecisionTableNodeTrace {
    FirstHit(DecisionTableRowTrace),
    Collect(Vec<DecisionTableRowTrace>),
}

impl Default for DecisionTableNodeTrace {
    fn default() -> Self {
        DecisionTableNodeTrace::Collect(Default::default())
    }
}
