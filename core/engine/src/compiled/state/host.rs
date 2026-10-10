use crate::decision_graph::graph::DecisionGraph;
use crate::loader::DynamicLoader;
use crate::model::{DecisionNodeKind, GraphContent};
use ahash::HashMap;
use crate::nodes::custom::CustomNodeHandler;
use crate::nodes::function::FunctionNodeHandler;
use crate::nodes::input::dates::DeclaredDates;
use crate::nodes::{NodeError, NodeResult};
use crate::nodes::validator_cache::ValidatorCache;
use crate::nodes::variable_json::{Guards, VariableJson, VariableNode};
use jsonschema::Validator;
use crate::nodes::input::InputNodeHandler;
use crate::nodes::output::OutputNodeHandler;
use crate::nodes::{NodeContextBase, NodeContextConfig, NodeHandlerExtensions};
use std::sync::Arc;
use crate::compiled::table::TableOut;
use crate::compiled::bound::Bound;
use crate::compiled::data::{Col, Data, Layer, Mask};
use crate::compiled::leaf::{Array, Leaf};
use zen_expression::lane::LaneRunner;
use zen_expression::Variable;
use crate::compiled::row::Row;
use zen_types::decision::{
    DecisionNode, InputNodeContent, OutputNodeContent,
};
use crate::compiled::{Body, CompiledGraph, Step, call, leaf, schema};

impl CompiledGraph {
    pub(in crate::compiled) async fn validate_input(
        &self,
        step: &Step,
        data: &Data<'_>,
        nodes: &[Option<Variable>],
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
    ) -> Vec<(usize, String)> {
        if let Some(message) = Self::failure(content, &step.node) {
            return (0..data.len()).map(|i| (i, message.to_string())).collect();
        }
        let mut failures = Vec::new();
        let Some((validator, schema)) = Self::validator(step, content, extensions) else {
            return failures;
        };
        let verdicts = schema::ObjectSchema::cached(&schema).and_then(|columns| columns.verdicts(data));
        let doubtful: Vec<(usize, Option<Variable>, Option<u64>)> = match &verdicts {
            Some(verdicts) => (0..data.len())
                .filter(|&i| !verdicts.sure[i])
                .map(|i| match verdicts.missing.get(i).copied().flatten() {
                    Some(key) => (i, None, Some(key)),
                    None => (i, Some(data.materialize_row(i)), None),
                })
                .collect(),
            None => data.materialized().iter().cloned().enumerate().map(|(i, value)| (i, Some(value), None)).collect(),
        };
        let mut memo: Vec<(u64, Option<String>)> = Vec::new();
        for (i, value, key) in doubtful {
            if let Some((_, known)) = key.and_then(|key| memo.iter().find(|(k, _)| *k == key)) {
                if let Some(e) = known {
                    failures.push((i, e.clone()));
                }
                continue;
            }
            let value = match value {
                Some(value) if validator.is_valid(VariableNode::new(&value, &Guards::default())) => continue,
                Some(value) => value,
                None => data.materialize_row(i),
            };
            let nodes = nodes.get(i).cloned().flatten();
            let failure = self.boundary(step, value, nodes, content, extensions).await.err();
            if let Some(key) = key.filter(|_| memo.len() < 64) {
                memo.push((key, failure.clone()));
            }
            if let Some(e) = failure {
                failures.push((i, e));
            }
        }
        failures
    }

    pub(in crate::compiled) fn placed<'a>(
        runner: &mut LaneRunner,
        body: &Body,
        path: &Arc<str>,
        pass_through: bool,
        input: &Data<'a>,
        bound: &Bound,
    ) -> (Data<'a>, Vec<(usize, String)>) {
        let count = input.rows.len();
        let mut failures = Vec::new();
        let values: Vec<Variable> = match body {
            Body::Expression { program, entries, keys, .. } => {
                let mut values = vec![Variable::Null; count];
                bound.many(runner, program, None, |row, result| match result {
                    Ok(outputs) => {
                        let object = Variable::empty_object();
                        for (key, value) in keys.iter().zip(outputs) {
                            object.dot_insert(key, value);
                        }
                        values[row] = object;
                    }
                    Err((stage, _)) => failures.push((
                        row,
                        format!(
                            r#"Failed to evaluate expression: "{}""#,
                            entries.get(stage).map(|(_, v)| v.as_ref()).unwrap_or_default()
                        ),
                    )),
                });
                values
            }
            Body::Table(table) => table
                .evaluate(runner, bound)
                .into_iter()
                .map(|out| table.value(out))
                .collect(),
        };
        let layer = Layer::new(Arc::from([path.clone()]), vec![Leaf::Any(values.into())], None);
        let data = match pass_through {
            true => input.layered(layer),
            false => Data::record(&input.rows, layer, None),
        };
        (data, failures)
    }

    pub(in crate::compiled) fn layered<'a>(
        runner: &mut LaneRunner,
        body: &Body,
        pass_through: bool,
        input: &Data<'a>,
        bound: &Bound,
        prefix: Option<&Arc<str>>,
    ) -> (Data<'a>, Vec<(usize, String)>) {
        let rows = input.rows.clone();
        let count = rows.len();
        let mut failures = Vec::new();
        match body {
            Body::Expression { program, entries, keys, .. } => {
                let mut outs = Vec::new();
                bound.export(runner, program, None, &mut outs, |row, stage, _| {
                    failures.push((
                        row,
                        format!(
                            r#"Failed to evaluate expression: "{}""#,
                            entries.get(stage).map(|(_, v)| v.as_ref()).unwrap_or_default()
                        ),
                    ))
                });
                let keys = match prefix {
                    None => keys.clone(),
                    Some(prefix) => keys.iter().map(|key| Arc::from(format!("{prefix}.{key}"))).collect(),
                };
                let layer = Layer::new(
                    keys,
                    outs.iter_mut().map(|out| Leaf::typed(Array::from_output(out))).collect(),
                    None,
                );
                let data = match pass_through {
                    true => input.layered(layer),
                    false => Data::record(&rows, layer, None),
                };
                (data, failures)
            }
            Body::Table(table) if table.first_hit() => {
                let (columns, present, unmatched) = table.columns(table.first(runner, bound));
                let layer = Layer::new(table.paths().clone(), columns, Some(present));
                let data = match pass_through {
                    true => input.layered(layer),
                    false => Data::record(&rows, layer, Some(unmatched.into())),
                };
                (data, failures)
            }
            Body::Table(table) if table.collects() => {
                let collected = table.first_collect(runner, bound);
                let layer = Layer::new(collected.paths, collected.columns, Some(collected.present));
                let data = match pass_through {
                    true => input.layered(layer),
                    false => Data::record(&rows, layer, Some(collected.unmatched.into())),
                };
                (data, failures)
            }
            Body::Table(table) => {
                let outs = table.evaluate(runner, bound);
                let first = outs.iter().all(|o| matches!(o, TableOut::Leaves(..) | TableOut::Value(Variable::Null)));
                match (pass_through, first) {
                    (true, true) => {
                        let paths = table.paths();
                        let mut columns: Vec<Vec<Variable>> = (0..paths.len()).map(|_| vec![Variable::Null; count]).collect();
                        let mut present: Vec<Vec<bool>> = (0..paths.len()).map(|_| vec![false; count]).collect();
                        for (row, out) in outs.into_iter().enumerate() {
                            if let TableOut::Leaves(slots, values) = out {
                                for (&slot, value) in slots.iter().zip(values) {
                                    columns[slot][row] = value;
                                    present[slot][row] = true;
                                }
                            }
                        }
                        let layer = Layer::new(
                            paths.clone(),
                            columns.into_iter().map(|c| Leaf::Any(Col::from(c))).collect(),
                            Some(
                                present
                                    .into_iter()
                                    .map(|p| Mask::Bits(leaf::Bits::of(count, |row| p[row]).into()))
                                    .collect(),
                            ),
                        );
                        (input.layered(layer), failures)
                    }
                    (true, false) => {
                        let materialized = input.materialized();
                        let values: Col = outs
                            .into_iter()
                            .enumerate()
                            .map(|(row, out)| match table.value(out) {
                                output @ Variable::Object(_) => materialized[row].clone().merge_clone(&output),
                                output @ Variable::Array(_) => output,
                                _ => materialized[row].clone(),
                            })
                            .collect();
                        (Data::plain(rows, values), failures)
                    }
                    (false, _) => (
                        Data::plain(rows, outs.into_iter().map(|o| table.value(o)).collect()),
                        failures,
                    ),
                }
            }
        }
    }

    pub(in crate::compiled) async fn host(
        step: &Step,
        input: Variable,
        nodes: Option<Variable>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        iteration: u8,
        batched: bool,
    ) -> Result<Variable, NodeError> {
        let node = &step.node;
        let extensions = match &node.kind {
            DecisionNodeKind::DecisionNode { .. } if batched => NodeHandlerExtensions {
                function_runtime: Default::default(),
                ..extensions.clone()
            },
            _ => extensions.clone(),
        };
        let base = NodeContextBase {
            id: node.id.clone(),
            name: node.name.clone(),
            input,
            nodes,
            extensions,
            iteration,
            trace: None,
            config: NodeContextConfig {
                max_depth,
                trace: false,
                ..Default::default()
            },
        };
        let response = match &node.kind {
            DecisionNodeKind::FunctionNode { content } => {
                DecisionGraph::handle(base, content.clone(), FunctionNodeHandler).await
            }
            DecisionNodeKind::DecisionNode { content } => {
                let call = call::Call {
                    key: &content.key,
                    extensions: &base.extensions,
                    max_depth,
                    iteration,
                };
                let row = Row {
                    id: &node.id,
                    input: base.input.clone(),
                    nodes: base.nodes.clone(),
                    programs: None,
                    trace: false,
                };
                call.node(row, &content.transform_attributes).await
            }
            DecisionNodeKind::CustomNode { content } => {
                DecisionGraph::handle(base, content.clone(), CustomNodeHandler).await
            }
            DecisionNodeKind::ExpressionNode { .. } | DecisionNodeKind::DecisionTableNode { .. } => {
                let index = base.extensions.dt_indexes.as_ref().and_then(|indexes| indexes.get(&node.id));
                let row = Row {
                    id: &node.id,
                    input: base.input.clone(),
                    nodes: base.nodes.clone(),
                    programs: step.shared_programs(),
                    trace: false,
                };
                row.node(node, index, true).await
            }
            _ => return Ok(base.input),
        };
        response.map(|r| r.output)
    }

    pub(in crate::compiled) fn validator(
        step: &Step,
        content: &GraphContent,
        extensions: &NodeHandlerExtensions,
    ) -> Option<(Arc<Validator<VariableJson>>, Arc<serde_json::Value>)> {
        let node = &step.node;
        if Self::failure(content, node).is_some() {
            return None;
        }
        let (schema, salt) = match content.resolved_schemas.as_ref().and_then(|r| r.get(&node.id)) {
            Some((schema, salt)) => (schema.clone(), *salt),
            None => match &node.kind {
                DecisionNodeKind::InputNode { content } => (content.schema.clone()?, 0),
                DecisionNodeKind::OutputNode { content } => (content.schema.clone()?, 0),
                _ => return None,
            },
        };
        let key = ValidatorCache::key(&node.id, &node.name, salt);
        let validator = extensions.validator_cache().get_or_insert(key, &schema).ok()?;
        Some((validator, schema))
    }

    fn failure<'c>(content: &'c GraphContent, node: &DecisionNode) -> Option<&'c Arc<str>> {
        content.schema_failures.as_ref()?.get(&node.id)
    }

    pub(crate) async fn schemas(&self, content: &Arc<GraphContent>, loader: &DynamicLoader) -> Arc<GraphContent> {
        if !self.dictionaries || content.resolved_schemas.is_some() {
            return content.clone();
        }
        let mut resolved = HashMap::default();
        let mut failures = HashMap::default();
        for node in &content.nodes {
            let schema = match &node.kind {
                DecisionNodeKind::InputNode { content } => content.schema.as_deref(),
                DecisionNodeKind::OutputNode { content } => content.schema.as_deref(),
                _ => continue,
            };
            match content.validation_schema(loader, &node.id, schema).await {
                Ok(Some(schema)) => {
                    resolved.insert(node.id.clone(), schema);
                }
                Ok(None) => {}
                Err(message) => {
                    failures.insert(node.id.clone(), Arc::from(message));
                }
            }
        }
        let mut owned = (**content).clone();
        owned.resolved_schemas = Some(Arc::new(resolved));
        owned.schema_failures = Some(Arc::new(failures));
        Arc::new(owned)
    }

    pub(in crate::compiled) fn columnar_input(step: &Step, content: &GraphContent, extensions: &NodeHandlerExtensions) -> bool {
        match Self::validator(step, content, extensions) {
            None => !step.schema && Self::unresolved(content, &step.node),
            Some((_, schema)) => !DeclaredDates::mentions(&schema),
        }
    }

    pub(in crate::compiled) fn unresolved(content: &GraphContent, node: &DecisionNode) -> bool {
        content
            .resolved_schemas
            .as_ref()
            .is_none_or(|resolved| !resolved.contains_key(&node.id))
    }

    pub(in crate::compiled) async fn boundary(
        &self,
        step: &Step,
        input: Variable,
        nodes: Option<Variable>,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
    ) -> Result<Variable, String> {
        let node = &step.node;
        let base = NodeContextBase {
            id: node.id.clone(),
            name: node.name.clone(),
            input,
            nodes,
            extensions: extensions.clone(),
            iteration: 0,
            trace: None,
            config: NodeContextConfig::default(),
        };
        self.edge(step, base, content)
            .await
            .map(|r| r.output)
            .map_err(|e| e.source.to_string())
    }

    pub(in crate::compiled) async fn edge(&self, step: &Step, mut base: NodeContextBase, content: &Arc<GraphContent>) -> NodeResult {
        let node = &step.node;
        if let Some(message) = Self::failure(content, node) {
            return base.error(message.to_string());
        }
        let resolved = content
            .resolved_schemas
            .as_ref()
            .and_then(|r| r.get(&node.id).cloned());
        if let Some((_, salt)) = &resolved {
            base.config.validation_salt = *salt;
        }
        match &node.kind {
            DecisionNodeKind::InputNode { content } => {
                let content = match resolved {
                    Some((schema, _)) => InputNodeContent { schema: Some(schema) },
                    None => content.clone(),
                };
                DecisionGraph::handle(base, content, InputNodeHandler).await
            }
            DecisionNodeKind::OutputNode { content } => {
                let content = match resolved {
                    Some((schema, _)) => OutputNodeContent { schema: Some(schema) },
                    None => content.clone(),
                };
                DecisionGraph::handle(base, content, OutputNodeHandler).await
            }
            _ => base.success(base.input.clone()),
        }
    }
}
