use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use base64::Engine as _;
use rust_decimal::prelude::ToPrimitive;
use zen_expression::variable::Variable;
use zen_expression::Isolate;
use zen_types::decision::{
    DecisionNode, DecisionNodeKind, DecisionTableContent, DecisionTableHitPolicy,
    TransformExecutionMode,
};

use crate::model::GraphContent;
use crate::nodes::decision_table::DecisionTableNodeHandler;
use crate::workspace::db::{Db, Snapshot};
use crate::workspace::graph::editor::{NodePaths, ReadBase};
use crate::workspace::graph::function_source;
use crate::workspace::types::{
    BlockExecution, BlockTrace, ConditionTrace, DecisionTableExtras, EvaluationError, Trace,
    WriteTrace,
};
use crate::DecisionGraphTrace;

pub type GraphTraceMap = HashMap<Arc<str>, DecisionGraphTrace>;

struct EnhanceState<'a> {
    db: &'a Db,
    snapshot: Arc<Snapshot>,
    executions: Vec<BlockExecution>,
    visiting: HashSet<Arc<str>>,
}

impl EnhanceState<'_> {
    fn dt_environment(
        &self,
        content: &DecisionTableContent,
        node_trace: &DecisionGraphTrace,
        trace: &GraphTraceMap,
    ) -> Option<Variable> {
        let nodes = Variable::from_object(
            trace
                .values()
                .filter(|entry| entry.order < node_trace.order)
                .map(|entry| (Rc::from(entry.name.as_ref()), entry.output.clone()))
                .collect(),
        );
        let base = node_trace.input.depth_clone(1);
        base.dot_insert("$nodes", nodes.clone());
        let Some(input_field) = &content.transform_attributes.input_field else {
            return Some(base);
        };
        let mut isolate = Isolate::with_environment(base);
        let calculated = isolate.run_standard(input_field.as_ref()).ok()?;
        match &calculated {
            Variable::Array(items) => {
                let items = items
                    .borrow()
                    .iter()
                    .map(|item| {
                        let item = item.depth_clone(1);
                        item.dot_insert("$nodes", nodes.clone());
                        item
                    })
                    .collect();
                Some(Variable::from_array(items))
            }
            _ => {
                let calculated = calculated.depth_clone(1);
                calculated.dot_insert("$nodes", nodes);
                Some(calculated)
            }
        }
    }

    fn dt_extras(
        &self,
        content: &DecisionTableContent,
        environment: Variable,
    ) -> DecisionTableExtras {
        let mut isolate = Isolate::with_environment(environment.depth_clone(1));
        let bytes_per_row = content.inputs.len().div_ceil(8);
        let mut bits = vec![0u8; bytes_per_row * content.rules.len()];
        for (row, rule) in content.rules.iter().enumerate() {
            for (col, input) in content.inputs.iter().enumerate() {
                if DecisionTableNodeHandler::cell_passes(rule, input, &mut isolate) {
                    bits[row * bytes_per_row + (col >> 3)] |= 1 << (col & 7);
                }
            }
        }
        DecisionTableExtras {
            input_pass: base64::engine::general_purpose::STANDARD.encode(&bits),
        }
    }
}

impl Db {
    pub fn enhance_graph_trace(
        &self,
        document: &Arc<str>,
        trace: &GraphTraceMap,
    ) -> Result<Trace, EvaluationError> {
        let snapshot = self.snapshot();
        let Some(content) = snapshot
            .graphs
            .get(document)
            .and_then(|content| content.as_graph())
            .cloned()
        else {
            return Err(EvaluationError::PolicyNotFound(document.clone()));
        };

        let mut state = EnhanceState {
            db: self,
            snapshot: snapshot.clone(),
            executions: Vec::new(),
            visiting: HashSet::new(),
        };
        state.visiting.insert(document.clone());
        walk_graph(&mut state, &content, trace, "", None);

        let mut properties: HashMap<Arc<str>, Variable> = HashMap::new();
        for execution in &state.executions {
            for write in &execution.writes {
                properties.insert(write.path.clone(), write.value.clone());
            }
            match &execution.trace {
                BlockTrace::Expression { property, value } if !property.is_empty() => {
                    properties.insert(property.clone(), value.clone());
                }
                BlockTrace::DecisionTable { evaluations, .. } => {
                    for evaluation in evaluations {
                        for (path, value) in evaluation {
                            properties.insert(path.clone(), value.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(Trace {
            engine_version: Arc::from(crate::ENGINE_VERSION),
            properties,
            executions: state.executions,
        })
    }
}

fn walk_graph(
    state: &mut EnhanceState,
    content: &GraphContent,
    trace: &GraphTraceMap,
    id_prefix: &str,
    inherited_instance: Option<&Arc<str>>,
) {
    let mut executed: Vec<(&DecisionNode, &DecisionGraphTrace)> = content
        .nodes
        .iter()
        .filter_map(|node| trace.get(node.id.as_ref()).map(|t| (node.as_ref(), t)))
        .collect();
    executed.sort_by_key(|(_, node_trace)| node_trace.order);

    for (node, node_trace) in executed {
        if matches!(
            node.kind,
            DecisionNodeKind::InputNode { .. } | DecisionNodeKind::OutputNode { .. }
        ) {
            continue;
        }
        let block_id = prefixed(id_prefix, &node.id);
        let paths = NodePaths::new(node);

        match &node.kind {
            DecisionNodeKind::ExpressionNode { content } => {
                let loop_mode = matches!(
                    content.transform_attributes.execution_mode,
                    TransformExecutionMode::Loop
                );
                let iterations = trace_entries(node_trace.trace_data.as_ref(), loop_mode);
                for row in content.expressions.iter() {
                    if row.key.is_empty() || row.value.is_empty() {
                        continue;
                    }
                    let row_block_id: Arc<str> = Arc::from(format!("{block_id}:{}", row.id));
                    let reads = state.db.node_global_reads(
                        node,
                        &paths,
                        Some(std::slice::from_ref(&row.id)),
                    );
                    let local_reads = state
                        .db
                        .node_local_reads(node, Some(std::slice::from_ref(&row.id)));
                    let property = output_prefixed(&paths, &row.key);
                    for (index, entry) in iterations.iter().enumerate() {
                        let value = entry
                            .dot(row.key.as_ref())
                            .and_then(|slot| slot.dot("result"))
                            .unwrap_or(Variable::Null);
                        let element = iteration_element(node_trace, &paths, loop_mode, index);
                        let dollar = expression_dollar_scope(&content.expressions, entry);
                        state.executions.push(BlockExecution {
                            block_id: row_block_id.clone(),
                            policy_path: None,
                            instance_path: instance_for(
                                node,
                                &paths,
                                loop_mode,
                                iterations.len(),
                                index,
                                inherited_instance,
                            ),
                            trace: BlockTrace::Expression {
                                property: property.clone(),
                                value,
                            },
                            operand_values: operand_values(
                                &local_reads,
                                &paths,
                                &node_trace.input,
                                element.as_ref(),
                                Some(&dollar),
                            ),
                            writes: Vec::new(),
                            reads: reads.clone(),
                        });
                    }
                }
            }
            DecisionNodeKind::DecisionTableNode { content } => {
                let loop_mode = matches!(
                    content.transform_attributes.execution_mode,
                    TransformExecutionMode::Loop
                );
                let collect = matches!(content.hit_policy, DecisionTableHitPolicy::Collect);
                let iterations = trace_entries(node_trace.trace_data.as_ref(), loop_mode);
                let reads = state.db.node_global_reads(node, &paths, None);
                let environment_root = state.dt_environment(content, node_trace, trace);
                let outputs_root = match &paths.output_path {
                    Some(path) => node_trace.output.dot(path).unwrap_or(Variable::Null),
                    None => node_trace.output.clone(),
                };
                for (index, entry) in iterations.iter().enumerate() {
                    let row_traces: Vec<Variable> = if collect {
                        entry
                            .as_array()
                            .map(|rows| rows.borrow().iter().cloned().collect())
                            .unwrap_or_default()
                    } else {
                        vec![entry.clone()]
                    };
                    let matched_rows: Vec<u32> = row_traces
                        .iter()
                        .filter_map(|row| row.dot("index"))
                        .filter_map(|value| match value {
                            Variable::Number(number) => number.to_u32(),
                            _ => None,
                        })
                        .collect();

                    let iter_result = if loop_mode {
                        element_at(&outputs_root, index).unwrap_or(Variable::Null)
                    } else {
                        outputs_root.clone()
                    };
                    let mut evaluations: Vec<HashMap<Arc<str>, Variable>> = Vec::new();
                    if collect {
                        for (row_index, _) in row_traces.iter().enumerate() {
                            let Some(row) = element_at(&iter_result, row_index) else {
                                continue;
                            };
                            let mut evaluation: HashMap<Arc<str>, Variable> = HashMap::new();
                            for column in content.outputs.iter() {
                                if let Some(value) = row.dot(column.field.as_ref()) {
                                    evaluation.insert(
                                        output_prefixed(&paths, &column.field),
                                        value.deep_clone(),
                                    );
                                }
                            }
                            if !evaluation.is_empty() {
                                evaluations.push(evaluation);
                            }
                        }
                    } else {
                        let mut evaluation: HashMap<Arc<str>, Variable> = HashMap::new();
                        for column in content.outputs.iter() {
                            if let Some(value) = iter_result.dot(column.field.as_ref()) {
                                evaluation.insert(
                                    output_prefixed(&paths, &column.field),
                                    value.deep_clone(),
                                );
                            }
                        }
                        evaluations.push(evaluation);
                    }

                    let element = iteration_element(node_trace, &paths, loop_mode, index);
                    let mut operands: HashMap<Arc<str>, Variable> = HashMap::new();
                    for row in &row_traces {
                        let Some(reference_map) =
                            row.dot("reference_map").and_then(|value| value.as_object())
                        else {
                            continue;
                        };
                        for (field, value) in reference_map.borrow().iter() {
                            operands.insert(Arc::from(field.as_ref()), value.deep_clone());
                        }
                    }
                    if operands.is_empty() {
                        let local_reads = state.db.node_local_reads(node, None);
                        operands = operand_values(
                            &local_reads,
                            &paths,
                            &node_trace.input,
                            element.as_ref(),
                            None,
                        );
                    }

                    let environment = if loop_mode {
                        environment_root
                            .as_ref()
                            .and_then(|env| element_at(env, index))
                    } else {
                        environment_root.clone()
                    };
                    let extras = environment.map(|env| state.dt_extras(content, env));
                    state.executions.push(BlockExecution {
                        block_id: block_id.clone(),
                        policy_path: None,
                        instance_path: instance_for(
                            node,
                            &paths,
                            loop_mode,
                            iterations.len(),
                            index,
                            inherited_instance,
                        ),
                        trace: BlockTrace::DecisionTable {
                            matched_rows,
                            evaluations,
                            extras,
                        },
                        operand_values: operands,
                        writes: Vec::new(),
                        reads: reads.clone(),
                    });
                }
            }
            DecisionNodeKind::SwitchNode { content } => {
                let taken: HashSet<Arc<str>> = node_trace
                    .trace_data
                    .as_ref()
                    .and_then(|data| data.dot("statements"))
                    .and_then(|statements| statements.as_array())
                    .map(|statements| {
                        statements
                            .borrow()
                            .iter()
                            .filter_map(|statement| statement.dot("id"))
                            .filter_map(|id| id.as_str().map(Arc::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let arms: Vec<ConditionTrace> = content
                    .statements
                    .iter()
                    .map(|statement| ConditionTrace {
                        id: statement.id.clone(),
                        result: taken.contains(&statement.id),
                    })
                    .collect();
                let matched_arm = content
                    .statements
                    .iter()
                    .find(|statement| taken.contains(&statement.id))
                    .map(|statement| statement.id.clone());
                let reads = state.db.node_global_reads(node, &paths, None);
                let local_reads = state.db.node_local_reads(node, None);
                state.executions.push(BlockExecution {
                    block_id: block_id.clone(),
                    policy_path: None,
                    instance_path: inherited_instance.cloned(),
                    trace: BlockTrace::Match {
                        matched_arm,
                        value: Variable::Null,
                        arms,
                    },
                    operand_values: operand_values(
                        &local_reads,
                        &paths,
                        &node_trace.input,
                        None,
                        None,
                    ),
                    writes: Vec::new(),
                    reads,
                });
            }
            DecisionNodeKind::FunctionNode { content } => {
                let source = function_source(content);
                let local_reads: Vec<Arc<str>> = Db::function_input_reads(&source)
                    .into_iter()
                    .map(Arc::from)
                    .collect();
                let reads: Vec<Arc<str>> = local_reads
                    .iter()
                    .filter_map(|read| map_read(&paths, read))
                    .collect();
                state.executions.push(BlockExecution {
                    block_id: block_id.clone(),
                    policy_path: None,
                    instance_path: inherited_instance.cloned(),
                    trace: BlockTrace::Expression {
                        property: Arc::from(""),
                        value: node_trace.output.clone(),
                    },
                    operand_values: operand_values(
                        &local_reads,
                        &paths,
                        &node_trace.input,
                        None,
                        None,
                    ),
                    writes: shallow_writes(&node_trace.input, &node_trace.output),
                    reads,
                });
            }
            DecisionNodeKind::DecisionNode { content } => {
                let key = content.key.clone();
                let sub_content = (!state.visiting.contains(&key))
                    .then(|| {
                        state
                            .snapshot
                            .graphs
                            .get(&key)
                            .and_then(|content| content.as_graph())
                            .cloned()
                    })
                    .flatten();
                let sub_traces = sub_content
                    .as_ref()
                    .map(|_| sub_trace_maps(node_trace.trace_data.as_ref()))
                    .unwrap_or_default();

                if let (Some(sub_content), false) = (sub_content, sub_traces.is_empty()) {
                    let group_index = state.executions.len();
                    state.executions.push(BlockExecution {
                        block_id: block_id.clone(),
                        policy_path: None,
                        instance_path: inherited_instance.cloned(),
                        trace: BlockTrace::Expression {
                            property: Arc::from(""),
                            value: node_trace.output.clone(),
                        },
                        operand_values: HashMap::new(),
                        writes: shallow_writes(&node_trace.input, &node_trace.output),
                        reads: Vec::new(),
                    });
                    let child_start = state.executions.len();
                    state.visiting.insert(key.clone());
                    let looped = sub_traces.len() > 1;
                    for (index, sub_trace) in sub_traces.iter().enumerate() {
                        let prefix = if looped {
                            format!("{block_id}[{index}]/")
                        } else {
                            format!("{block_id}/")
                        };
                        let instance: Option<Arc<str>> = if looped {
                            Some(Arc::from(format!("{}.{index}", loop_label(node, &paths))))
                        } else {
                            inherited_instance.cloned()
                        };
                        walk_graph(state, &sub_content, sub_trace, &prefix, instance.as_ref());
                    }
                    state.visiting.remove(&key);
                    let free_reads = subtree_free_reads(&state.executions[child_start..], &paths);
                    state.executions[group_index].reads = free_reads;
                } else {
                    push_opaque(state, &block_id, node_trace, inherited_instance);
                }
            }
            _ => {
                push_opaque(state, &block_id, node_trace, inherited_instance);
            }
        }
    }
}

fn push_opaque(
    state: &mut EnhanceState,
    block_id: &Arc<str>,
    node_trace: &DecisionGraphTrace,
    inherited_instance: Option<&Arc<str>>,
) {
    state.executions.push(BlockExecution {
        block_id: block_id.clone(),
        policy_path: None,
        instance_path: inherited_instance.cloned(),
        trace: BlockTrace::Expression {
            property: Arc::from(""),
            value: node_trace.output.clone(),
        },
        operand_values: HashMap::new(),
        writes: shallow_writes(&node_trace.input, &node_trace.output),
        reads: Vec::new(),
    });
}

fn prefixed(prefix: &str, id: &str) -> Arc<str> {
    if prefix.is_empty() {
        Arc::from(id)
    } else {
        Arc::from(format!("{prefix}{id}"))
    }
}

fn output_prefixed(paths: &NodePaths, key: &str) -> Arc<str> {
    if paths.output_prefix.is_empty() {
        Arc::from(key)
    } else {
        Arc::from(format!("{}.{key}", paths.output_prefix.join(".")))
    }
}

fn map_read(paths: &NodePaths, path: &str) -> Option<Arc<str>> {
    match &paths.read_base {
        ReadBase::NodeInput => Some(Arc::from(path)),
        ReadBase::Opaque => None,
        ReadBase::Prefixed(prefix) => Some(Arc::from(format!("{}.{path}", prefix.join(".")))),
    }
}

fn loop_label(node: &DecisionNode, paths: &NodePaths) -> String {
    match &paths.read_base {
        ReadBase::Prefixed(segments) => segments.join("."),
        _ => node.name.to_string(),
    }
}

fn instance_for(
    node: &DecisionNode,
    paths: &NodePaths,
    loop_mode: bool,
    total: usize,
    index: usize,
    inherited: Option<&Arc<str>>,
) -> Option<Arc<str>> {
    if loop_mode && total > 1 {
        Some(Arc::from(format!("{}.{index}", loop_label(node, paths))))
    } else {
        inherited.cloned()
    }
}

fn trace_entries(trace_data: Option<&Variable>, loop_mode: bool) -> Vec<Variable> {
    match trace_data {
        Some(Variable::Array(items)) if loop_mode => items.borrow().iter().cloned().collect(),
        Some(data) => vec![data.clone()],
        None => vec![Variable::Null],
    }
}

fn element_at(value: &Variable, index: usize) -> Option<Variable> {
    value
        .as_array()
        .and_then(|items| items.borrow().get(index).cloned())
}

fn iteration_element(
    node_trace: &DecisionGraphTrace,
    paths: &NodePaths,
    loop_mode: bool,
    index: usize,
) -> Option<Variable> {
    if !loop_mode {
        return None;
    }
    let ReadBase::Prefixed(prefix) = &paths.read_base else {
        return None;
    };
    node_trace
        .input
        .dot(&prefix.join("."))
        .and_then(|collection| element_at(&collection, index))
}

fn operand_values(
    local_reads: &[Arc<str>],
    paths: &NodePaths,
    node_input: &Variable,
    element: Option<&Variable>,
    dollar: Option<&Variable>,
) -> HashMap<Arc<str>, Variable> {
    let mut out: HashMap<Arc<str>, Variable> = HashMap::new();
    for read in local_reads {
        let value = if let Some(rest) = read.strip_prefix("$.") {
            dollar.and_then(|scope| scope.dot(rest))
        } else if let Some(element) = element {
            element.dot(read)
        } else {
            match &paths.read_base {
                ReadBase::NodeInput => node_input.dot(read),
                ReadBase::Prefixed(prefix) => {
                    node_input.dot(&format!("{}.{read}", prefix.join(".")))
                }
                ReadBase::Opaque => None,
            }
        };
        if let Some(value) = value {
            out.insert(read.clone(), value.deep_clone());
        }
    }
    out
}

fn expression_dollar_scope(rows: &[zen_types::decision::Expression], entry: &Variable) -> Variable {
    let scope = Variable::empty_object();
    for row in rows {
        if row.key.is_empty() {
            continue;
        }
        let Some(value) = entry
            .dot(row.key.as_ref())
            .and_then(|slot| slot.dot("result"))
        else {
            continue;
        };
        scope.dot_insert(row.key.as_ref(), value);
    }
    scope
}

fn shallow_writes(input: &Variable, output: &Variable) -> Vec<WriteTrace> {
    let Some(entries) = output.as_object() else {
        return Vec::new();
    };
    let mut writes: Vec<WriteTrace> = Vec::new();
    for (key, value) in entries.borrow().iter() {
        if key.starts_with('$') {
            continue;
        }
        if input.dot(key).is_some_and(|previous| previous == *value) {
            continue;
        }
        writes.push(WriteTrace {
            path: Arc::from(key.as_ref()),
            value: value.deep_clone(),
        });
    }
    writes.sort_by(|a, b| a.path.cmp(&b.path));
    writes
}

fn sub_trace_maps(trace_data: Option<&Variable>) -> Vec<GraphTraceMap> {
    match trace_data {
        Some(Variable::Array(items)) => items.borrow().iter().filter_map(as_trace_map).collect(),
        Some(data) => as_trace_map(data).map(|map| vec![map]).unwrap_or_default(),
        None => Vec::new(),
    }
}

fn as_trace_map(value: &Variable) -> Option<GraphTraceMap> {
    if !matches!(value, Variable::Object(_)) {
        return None;
    }
    let json = serde_json::to_value(value).ok()?;
    let map: GraphTraceMap = serde_json::from_value(json).ok()?;
    (!map.is_empty()).then_some(map)
}

fn subtree_free_reads(children: &[BlockExecution], paths: &NodePaths) -> Vec<Arc<str>> {
    let mut written: Vec<Arc<str>> = Vec::new();
    for child in children {
        for write in &child.writes {
            written.push(write.path.clone());
        }
        match &child.trace {
            BlockTrace::Expression { property, .. } if !property.is_empty() => {
                written.push(property.clone());
            }
            BlockTrace::DecisionTable { evaluations, .. } => {
                for evaluation in evaluations {
                    written.extend(evaluation.keys().cloned());
                }
            }
            _ => {}
        }
    }
    let covered = |read: &str| {
        written
            .iter()
            .any(|path| read == path.as_ref() || read.starts_with(&format!("{path}.")))
    };
    let mut out: Vec<Arc<str>> = Vec::new();
    for child in children {
        for read in &child.reads {
            if covered(read) {
                continue;
            }
            if let Some(mapped) = map_read(paths, read) {
                out.push(mapped);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
