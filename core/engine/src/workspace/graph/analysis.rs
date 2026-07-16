use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::variable::VariableType;
use zen_types::decision::{
    DecisionNode, DecisionNodeContent, DecisionNodeKind, DecisionTableContent,
    DecisionTableHitPolicy, DecisionTableOutputField, ExpressionNodeContent, FunctionNodeContent,
    SwitchNodeContent, SwitchStatementHitPolicy, TransformAttributes, TransformExecutionMode,
};

use zen_expression::intellisense::ArmTest;

use crate::model::GraphContent;
use crate::policy::blocks::{DecisionTableIr, DeclaredType, IntelliSenseSource, ReadFlattener};
use crate::policy::linter::{AstOps, RedundantParentheses};
use crate::policy::queries::scope::VariableTypeScope;
use crate::workspace::db::Db;
use crate::workspace::graph::function::FunctionTypeOutcome;
use crate::workspace::types::{
    CursorTarget, Diagnostic, DiagnosticCode, DiagnosticLocation, ExpressionKind,
};

const NODES_KEY: &str = "$nodes";

#[derive(Debug, Clone)]
pub struct GraphSignature {
    pub input: VariableType,
    pub output: VariableType,
}

#[derive(Debug, Clone)]
pub struct GraphNodeAnalysis {
    pub input: VariableType,
    pub handler_input: VariableType,
    pub output: VariableType,
    pub dollar: Option<VariableType>,
    pub nodes_scope: VariableType,
    pub branch_outputs: HashMap<Arc<str>, VariableType>,
    pub opaque: bool,
    pub unchecked: bool,
    pub open: bool,
}

#[derive(Debug)]
pub struct GraphAnalysis {
    pub diagnostics: Vec<Diagnostic>,
    pub signature: GraphSignature,
    pub nodes: HashMap<Arc<str>, GraphNodeAnalysis>,
    pub inferred_inputs: Vec<Arc<str>>,
}

pub(crate) enum SignatureResolution {
    Found(GraphSignature),
    Recursive,
    Missing,
}

pub(crate) struct GraphExpressionSite {
    pub(crate) target: CursorTarget,
    pub(crate) expression_id: Option<Arc<str>>,
    pub(crate) source: Arc<str>,
    pub(crate) kind: ExpressionKind,
}

pub(crate) struct GraphAnalyzer<'a> {
    db: &'a Db,
    path: Arc<str>,
    content: &'a GraphContent,
    diagnostics: Vec<Diagnostic>,
    validate: bool,
    nodes_scope: VariableType,
    dictionary_types: HashMap<Arc<str>, VariableType>,
}

type IncomingEdges = Vec<Vec<(usize, Option<Arc<str>>)>>;

struct GraphTopology {
    node_index: HashMap<Arc<str>, usize>,
    incoming: IncomingEdges,
    outgoing: Vec<Vec<usize>>,
    order: Option<Vec<usize>>,
}

impl<'a> GraphAnalyzer<'a> {
    pub(crate) fn new(db: &'a Db, path: Arc<str>, content: &'a GraphContent) -> Self {
        let dictionary_types = db.graph_dictionary_types(&content.imports);
        Self {
            db,
            path,
            content,
            diagnostics: Vec::new(),
            validate: false,
            nodes_scope: VariableType::Any,
            dictionary_types,
        }
    }

    pub(crate) fn analyze(mut self) -> GraphAnalysis {
        self.check_imports();
        let topology = self.build_topology();
        let graph_input = self.graph_input_type();
        let mut nodes: HashMap<Arc<str>, GraphNodeAnalysis> = HashMap::new();

        if let Some(order) = &topology.order {
            let descendants = Self::descendant_sets(&topology);
            let mut ancestors: HashMap<usize, HashSet<usize>> = HashMap::new();
            for &idx in order {
                let mut ancestor_set: HashSet<usize> = HashSet::default();
                for (pred, _) in &topology.incoming[idx] {
                    ancestor_set.insert(*pred);
                    if let Some(pred_ancestors) = ancestors.get(pred) {
                        ancestor_set.extend(pred_ancestors.iter().copied());
                    }
                }
                let node = &self.content.nodes[idx];
                let (input, unchecked, open) =
                    Self::merged_input(self.content, &topology, &nodes, idx);
                self.nodes_scope = Self::nodes_scope_of(
                    self.content,
                    idx,
                    &ancestor_set,
                    &descendants[idx],
                    &nodes,
                );
                let analysis = self.analyze_node(node, input, unchecked, open, &graph_input);
                nodes.insert(node.id.clone(), analysis);
                ancestors.insert(idx, ancestor_set);
            }
        }

        let output = Self::terminal_output(self.content, &topology, &nodes);
        let inferred_inputs = self.inferred_inputs(&topology, &nodes, &graph_input);
        self.lint_unreachable(&topology);
        self.lint_expressions();
        self.sort_diagnostics(&topology);

        GraphAnalysis {
            diagnostics: self.diagnostics,
            signature: GraphSignature {
                input: graph_input,
                output,
            },
            nodes,
            inferred_inputs,
        }
    }

    fn check_imports(&mut self) {
        let snap = self.db.snapshot();
        let mut seen: HashSet<&str> = HashSet::default();
        for import in &self.content.imports {
            if !seen.insert(import.as_ref()) {
                continue;
            }
            let message = if snap.all_parsed.contains_key(import) {
                continue;
            } else if snap.graphs.contains_key(import) {
                format!("imported document '{import}' is a graph; only policies can be imported")
            } else {
                format!("imported policy '{import}' not found in workspace")
            };
            self.diagnostics.push(Diagnostic::error(
                DiagnosticCode::ImportNotFound,
                DiagnosticLocation::policy(self.path.clone()),
                message,
            ));
        }
    }

    fn build_topology(&mut self) -> GraphTopology {
        let content = self.content;
        let mut node_index: HashMap<Arc<str>, usize> = HashMap::with_capacity(content.nodes.len());
        for (idx, node) in content.nodes.iter().enumerate() {
            if node_index.insert(node.id.clone(), idx).is_some() {
                self.diagnostics.push(Diagnostic::error(
                    DiagnosticCode::InvalidGraphStructure,
                    DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                    format!("duplicate node id '{}'", node.id),
                ));
            }
        }

        let mut incoming: IncomingEdges = vec![Vec::new(); content.nodes.len()];
        let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); content.nodes.len()];
        for edge in &content.edges {
            let (Some(&source), Some(&target)) = (
                node_index.get(&edge.source_id),
                node_index.get(&edge.target_id),
            ) else {
                let missing = if node_index.contains_key(&edge.source_id) {
                    &edge.target_id
                } else {
                    &edge.source_id
                };
                self.diagnostics.push(Diagnostic::error(
                    DiagnosticCode::InvalidGraphStructure,
                    DiagnosticLocation::policy(self.path.clone()),
                    format!("edge '{}' references unknown node '{}'", edge.id, missing),
                ));
                continue;
            };
            outgoing[source].push(target);
            incoming[target].push((source, edge.source_handle.clone()));
        }

        let input_count = content
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, DecisionNodeKind::InputNode { .. }))
            .count();
        if input_count != 1 {
            self.diagnostics.push(Diagnostic::error(
                DiagnosticCode::InvalidGraphStructure,
                DiagnosticLocation::policy(self.path.clone()),
                format!("graph must have exactly one input node, found {input_count}"),
            ));
        }

        let order = Self::topological_order(&incoming, &outgoing);
        if order.is_none() {
            self.diagnostics.push(Diagnostic::error(
                DiagnosticCode::CyclicDependency,
                DiagnosticLocation::policy(self.path.clone()),
                "graph contains a cycle",
            ));
        }

        GraphTopology {
            node_index,
            incoming,
            outgoing,
            order,
        }
    }

    fn topological_order(incoming: &IncomingEdges, outgoing: &[Vec<usize>]) -> Option<Vec<usize>> {
        let mut indegree: Vec<usize> = incoming.iter().map(Vec::len).collect();
        let mut queue: VecDeque<usize> = indegree
            .iter()
            .enumerate()
            .filter(|(_, &d)| d == 0)
            .map(|(i, _)| i)
            .collect();
        let mut order = Vec::with_capacity(incoming.len());
        while let Some(idx) = queue.pop_front() {
            order.push(idx);
            for &next in &outgoing[idx] {
                indegree[next] -= 1;
                if indegree[next] == 0 {
                    queue.push_back(next);
                }
            }
        }
        (order.len() == incoming.len()).then_some(order)
    }

    fn descendant_sets(topology: &GraphTopology) -> Vec<HashSet<usize>> {
        let count = topology.outgoing.len();
        let mut descendants: Vec<HashSet<usize>> = vec![HashSet::default(); count];
        for (start, reachable) in descendants.iter_mut().enumerate() {
            let mut stack: Vec<usize> = topology.outgoing[start].clone();
            while let Some(next) = stack.pop() {
                if reachable.insert(next) {
                    stack.extend(topology.outgoing[next].iter().copied());
                }
            }
        }
        descendants
    }

    fn nodes_scope_of(
        content: &GraphContent,
        current: usize,
        ancestor_set: &HashSet<usize>,
        descendant_set: &HashSet<usize>,
        nodes: &HashMap<Arc<str>, GraphNodeAnalysis>,
    ) -> VariableType {
        let scope = VariableType::empty_object();
        let VariableType::Object(fields) = &scope else {
            return scope;
        };
        let mut map = fields.borrow_mut();
        for (idx, node) in content.nodes.iter().enumerate() {
            if idx == current || descendant_set.contains(&idx) {
                continue;
            }
            let resolved = if ancestor_set.contains(&idx) {
                match nodes.get(&node.id) {
                    Some(analysis) => analysis.output.shallow_clone(),
                    None => VariableType::Any,
                }
            } else {
                VariableType::Any
            };
            let merged = match map.get(node.name.as_ref()) {
                Some(existing) => existing.merge(&resolved),
                None => resolved,
            };
            map.insert(Rc::from(node.name.as_ref()), merged);
        }
        drop(map);
        scope
    }

    fn merged_input(
        content: &GraphContent,
        topology: &GraphTopology,
        nodes: &HashMap<Arc<str>, GraphNodeAnalysis>,
        idx: usize,
    ) -> (VariableType, bool, bool) {
        let mut unchecked = false;
        let mut open = false;
        let mut merged: Option<VariableType> = None;
        for (pred, handle) in &topology.incoming[idx] {
            let Some(analysis) = nodes.get(&content.nodes[*pred].id) else {
                continue;
            };
            unchecked |= analysis.opaque || analysis.unchecked;
            open |= analysis.open || matches!(analysis.output, VariableType::Any);
            let branch = handle
                .as_ref()
                .and_then(|h| analysis.branch_outputs.get(h.as_ref()))
                .unwrap_or(&analysis.output);
            merged = Some(match merged {
                None => branch.shallow_clone(),
                Some(acc) => acc.merge(branch),
            });
        }
        (
            merged.unwrap_or_else(VariableType::empty_object),
            unchecked,
            open,
        )
    }

    fn reachable_from_inputs(
        content: &GraphContent,
        topology: &GraphTopology,
    ) -> Option<Vec<bool>> {
        let input_indices: Vec<usize> = content
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| matches!(node.kind, DecisionNodeKind::InputNode { .. }))
            .map(|(idx, _)| idx)
            .collect();
        if input_indices.is_empty() {
            return None;
        }
        let mut reachable = vec![false; content.nodes.len()];
        let mut stack = input_indices;
        while let Some(idx) = stack.pop() {
            if std::mem::replace(&mut reachable[idx], true) {
                continue;
            }
            stack.extend(topology.outgoing[idx].iter().copied());
        }
        Some(reachable)
    }

    fn terminal_output(
        content: &GraphContent,
        topology: &GraphTopology,
        nodes: &HashMap<Arc<str>, GraphNodeAnalysis>,
    ) -> VariableType {
        let reachable = Self::reachable_from_inputs(content, topology);
        let mut terminals: Vec<&GraphNodeAnalysis> = content
            .nodes
            .iter()
            .enumerate()
            .filter(|(idx, _)| topology.outgoing.get(*idx).is_some_and(Vec::is_empty))
            .filter(|(idx, _)| reachable.as_ref().is_none_or(|r| r[*idx]))
            .filter_map(|(_, node)| nodes.get(&node.id))
            .collect();
        let Some(first) = terminals.pop() else {
            return VariableType::empty_object();
        };
        terminals
            .into_iter()
            .fold(first.output.shallow_clone(), |acc, t| acc.merge(&t.output))
    }

    fn graph_input_type(&self) -> VariableType {
        self.content
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                DecisionNodeKind::InputNode { content } => content.schema.as_ref(),
                _ => None,
            })
            .map(|schema| super::SchemaType::variable_type_with(schema, &self.dictionary_types))
            .unwrap_or(VariableType::Any)
    }

    fn check_schema_dictionaries(&mut self, node: &DecisionNode, schema: &serde_json::Value) {
        let mut names: Vec<Arc<str>> = Vec::new();
        super::SchemaType::dictionary_names(schema, &mut names);
        names.sort();
        names.dedup();
        for name in names {
            if self.dictionary_types.contains_key(&name) {
                continue;
            }
            self.diagnostics.push(Diagnostic::error(
                DiagnosticCode::TypeMismatch,
                DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                format!(
                    "unknown dictionary '{name}' in schema: no dictionary with that name is in scope — import the policy that defines it"
                ),
            ));
        }
    }

    fn analyze_node(
        &mut self,
        node: &'a DecisionNode,
        input: VariableType,
        unchecked: bool,
        open: bool,
        graph_input: &VariableType,
    ) -> GraphNodeAnalysis {
        let scope_input = if unchecked || matches!(input, VariableType::Any) {
            VariableType::empty_object()
        } else {
            input.shallow_clone()
        };
        self.validate = !unchecked && !open && !matches!(input, VariableType::Any);

        let mut analysis = GraphNodeAnalysis {
            input: scope_input.shallow_clone(),
            handler_input: scope_input.shallow_clone(),
            output: VariableType::Any,
            dollar: None,
            nodes_scope: self.nodes_scope.shallow_clone(),
            branch_outputs: HashMap::default(),
            opaque: false,
            unchecked,
            open,
        };

        match &node.kind {
            DecisionNodeKind::InputNode { content } => {
                if let Some(schema) = content.schema.as_ref() {
                    self.check_schema_dictionaries(node, schema);
                }
                analysis.output = graph_input.shallow_clone();
                if matches!(graph_input, VariableType::Any) {
                    analysis.opaque = true;
                    analysis.open = true;
                    self.diagnostics.push(Diagnostic::warning(
                        DiagnosticCode::MissingInputSchema,
                        DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                        "input node has no schema; input properties are unknown and downstream expressions cannot be strictly checked — define the request schema",
                    ));
                } else {
                    let mut any_paths = Vec::new();
                    Self::collect_any_paths(graph_input, String::new(), &mut any_paths);
                    for path in any_paths.iter().take(8) {
                        self.diagnostics.push(Diagnostic::error(
                            DiagnosticCode::ImplicitAny,
                            DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                            format!(
                                "schema leaves `{path}` untyped (`any`) — everything computed from it degrades to `any`; declare its type in the request schema"
                            ),
                        ));
                    }
                }
            }
            DecisionNodeKind::OutputNode { content } => {
                if let Some(schema) = content.schema.as_ref() {
                    self.check_schema_dictionaries(node, schema);
                }
                if let Some(schema) = content.schema.as_ref().filter(|_| self.validate) {
                    let expected =
                        super::SchemaType::variable_type_with(schema, &self.dictionary_types);
                    self.check_output_schema(node, &scope_input, &expected);
                }
                analysis.output = scope_input;
            }
            DecisionNodeKind::SwitchNode { content } => {
                analysis.branch_outputs = self.check_switch(node, content, &scope_input);
                analysis.output = scope_input;
            }
            DecisionNodeKind::CustomNode { content } => {
                self.diagnostics.push(Diagnostic::warning(
                    DiagnosticCode::UncheckedNode,
                    DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                    format!(
                        "unknown node kind '{}' — this node is not type-checked and downstream nodes are unchecked",
                        content.kind
                    ),
                ));
                analysis.opaque = true;
                analysis.open = true;
            }
            DecisionNodeKind::FunctionNode { content } => {
                self.check_function(node, content, &scope_input, &mut analysis);
            }
            DecisionNodeKind::ExpressionNode { content } => {
                let (handler_input, output) = self.transformed(
                    node,
                    &content.transform_attributes,
                    &scope_input,
                    |analyzer, scope| {
                        let (output, dollar) = analyzer.check_expression_rows(node, content, scope);
                        analysis.dollar = Some(dollar);
                        output
                    },
                );
                analysis.handler_input = handler_input;
                analysis.output = output;
                analysis.open = open && content.transform_attributes.pass_through;
            }
            DecisionNodeKind::DecisionTableNode { content } => {
                let (handler_input, output) = self.transformed(
                    node,
                    &content.transform_attributes,
                    &scope_input,
                    |analyzer, scope| analyzer.check_decision_table(node, content, scope),
                );
                analysis.handler_input = handler_input;
                analysis.output = output;
                analysis.open = open && content.transform_attributes.pass_through;
            }
            DecisionNodeKind::DecisionNode { content } => {
                let signature = self.resolve_decision_signature(node, content);
                let resolved = signature
                    .as_ref()
                    .map(|s| s.output.shallow_clone())
                    .unwrap_or(VariableType::Any);
                let (handler_input, output) = self.transformed(
                    node,
                    &content.transform_attributes,
                    &scope_input,
                    |analyzer, scope| {
                        if let Some(signature) = &signature {
                            analyzer.check_decision_input(node, content, signature, scope);
                        }
                        resolved
                    },
                );
                analysis.handler_input = handler_input;
                analysis.output = output;
                analysis.open = open && content.transform_attributes.pass_through;
            }
        }

        analysis
    }

    fn check_function(
        &mut self,
        node: &DecisionNode,
        content: &FunctionNodeContent,
        scope_input: &VariableType,
        analysis: &mut GraphNodeAnalysis,
    ) {
        let source = super::function_source(content);
        match self.db.function_output_type(&source, scope_input) {
            FunctionTypeOutcome::Typed(resolved) => {
                if matches!(resolved, VariableType::Any) {
                    self.diagnostics.push(Diagnostic::error(
                        DiagnosticCode::ImplicitAny,
                        DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                        "function handler type resolved to `any` — add explicit types to the handler",
                    ));
                    analysis.opaque = true;
                    analysis.open = true;
                } else {
                    let mut any_paths = Vec::new();
                    Self::collect_any_paths(&resolved, String::new(), &mut any_paths);
                    for path in any_paths.iter().take(8) {
                        self.diagnostics.push(Diagnostic::error(
                            DiagnosticCode::ImplicitAny,
                            DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                            format!("function output `{path}` is `any` — type it explicitly"),
                        ));
                    }
                    analysis.output = resolved;
                }
            }
            FunctionTypeOutcome::Unresolved => {
                self.diagnostics.push(Diagnostic::warning(
                    DiagnosticCode::UnresolvedFunctionType,
                    DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                    "the type resolver could not determine the handler type; downstream nodes are unchecked",
                ));
                analysis.opaque = true;
                analysis.open = true;
            }
            FunctionTypeOutcome::Unknown => {
                self.diagnostics.push(Diagnostic::warning(
                    DiagnosticCode::UnresolvedFunctionType,
                    DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                    "function node types are unknown; register a function type resolver",
                ));
                analysis.opaque = true;
                analysis.open = true;
            }
        }
    }

    fn collect_any_paths(variable_type: &VariableType, path: String, out: &mut Vec<String>) {
        match variable_type {
            VariableType::Any => {
                if !path.is_empty() {
                    out.push(path);
                }
            }
            VariableType::Array(items) => {
                Self::collect_any_paths(items, format!("{path}[]"), out);
            }
            VariableType::Nullable(inner) => {
                Self::collect_any_paths(inner, path, out);
            }
            VariableType::Object(fields) => {
                let map = fields.borrow();
                let mut keys: Vec<_> = map.keys().cloned().collect();
                keys.sort();
                for key in keys {
                    let Some(field) = map.get(key.as_ref()) else {
                        continue;
                    };
                    let child = if path.is_empty() {
                        key.to_string()
                    } else {
                        format!("{path}.{key}")
                    };
                    Self::collect_any_paths(field, child, out);
                }
            }
            _ => {}
        }
    }

    fn transformed(
        &mut self,
        node: &DecisionNode,
        attributes: &TransformAttributes,
        scope_input: &VariableType,
        handler: impl FnOnce(&mut Self, &VariableType) -> VariableType,
    ) -> (VariableType, VariableType) {
        let base = match &attributes.input_field {
            Some(field) => {
                let field_scope =
                    Self::scope_with_nodes(scope_input, &self.nodes_scope.shallow_clone());
                self.check_expression(
                    &node.id,
                    None,
                    Some(CursorTarget::TransformInput),
                    field,
                    ExpressionKind::Standard,
                    &field_scope,
                )
            }
            None => scope_input.shallow_clone(),
        };
        if attributes.input_field.is_some() && matches!(base, VariableType::Any) {
            self.validate = false;
        }

        let (handler_scope, mut output) = match attributes.execution_mode {
            TransformExecutionMode::Single => {
                let output = handler(self, &base);
                (base, output)
            }
            TransformExecutionMode::Loop => {
                let element = match base.iterator() {
                    Some(inner) => inner.as_ref().shallow_clone(),
                    None => {
                        if !matches!(base, VariableType::Any) {
                            self.diagnostics.push(Diagnostic::error(
                                DiagnosticCode::TypeMismatch,
                                DiagnosticLocation::block(self.path.clone(), node.id.clone())
                                    .maybe_target(
                                        attributes
                                            .input_field
                                            .as_ref()
                                            .map(|_| CursorTarget::TransformInput),
                                    ),
                                format!("loop execution expects an array input, got `{base}`"),
                            ));
                        }
                        self.validate = false;
                        VariableType::Any
                    }
                };
                if matches!(element, VariableType::Any) {
                    self.validate = false;
                }
                let mut output = handler(self, &element);
                if attributes.pass_through {
                    output = element.merge(&output);
                }
                (element, output.array())
            }
        };

        if let Some(output_path) = &attributes.output_path {
            let wrapped = VariableType::empty_object();
            wrapped.insert_at_path(output_path, &output, true);
            output = wrapped;
        }
        if attributes.pass_through {
            output = match &output {
                VariableType::Array(_) => output,
                VariableType::Object(_) => scope_input.merge(&output),
                VariableType::Nullable(inner)
                    if matches!(inner.as_ref(), VariableType::Object(_)) =>
                {
                    scope_input.merge(inner)
                }
                VariableType::Any => VariableType::Any,
                _ => scope_input.shallow_clone(),
            };
        }

        (handler_scope, output)
    }

    fn check_expression_rows(
        &mut self,
        node: &DecisionNode,
        content: &ExpressionNodeContent,
        scope: &VariableType,
    ) -> (VariableType, VariableType) {
        let output = VariableType::empty_object();
        let dollar = VariableType::empty_object();
        for row in content.expressions.iter() {
            if row.key.is_empty() || row.value.is_empty() {
                continue;
            }
            let row_scope = Self::scope_with(
                scope,
                &[
                    ("$", dollar.shallow_clone()),
                    (NODES_KEY, self.nodes_scope.shallow_clone()),
                ],
            );
            let resolved = self.check_expression(
                &node.id,
                Some(row.id.clone()),
                None,
                &row.value,
                ExpressionKind::Standard,
                &row_scope,
            );
            output.insert_at_path(&row.key, &resolved, true);
            dollar.insert_at_path(&row.key, &resolved, true);
        }
        (output, dollar)
    }

    fn check_decision_table(
        &mut self,
        node: &DecisionNode,
        content: &DecisionTableContent,
        scope: &VariableType,
    ) -> VariableType {
        let base_scope = Self::scope_with_nodes(scope, &self.nodes_scope.shallow_clone());

        let mut cell_scopes: HashMap<Arc<str>, VariableType> = HashMap::new();
        let mut input_field_types: HashMap<Arc<str>, VariableType> = HashMap::new();
        for col in content.inputs.iter() {
            let Some(field) = &col.field else {
                continue;
            };
            let field_type = self.check_expression(
                &node.id,
                Some(col.id.clone()),
                Some(CursorTarget::DecisionTableHead {
                    col: col.id.clone(),
                }),
                field,
                ExpressionKind::Standard,
                &base_scope,
            );
            cell_scopes.insert(col.id.clone(), base_scope.with_dollar(&field_type));
            input_field_types.insert(col.id.clone(), field_type);
        }

        for (row_idx, rule) in content.rules.iter().enumerate() {
            let row_key = Self::row_key(rule, row_idx);
            for col in content.inputs.iter() {
                let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                    continue;
                };
                let target = CursorTarget::DecisionTableCell {
                    row: row_key.clone(),
                    col: col.id.clone(),
                };
                match cell_scopes.get(&col.id) {
                    Some(cell_scope) => {
                        self.check_expression(
                            &node.id,
                            Some(col.id.clone()),
                            Some(target),
                            cell,
                            ExpressionKind::Unary,
                            &cell_scope.shallow_clone(),
                        );
                    }
                    None => {
                        let resolved = self.check_expression(
                            &node.id,
                            Some(col.id.clone()),
                            Some(target.clone()),
                            cell,
                            ExpressionKind::Standard,
                            &base_scope,
                        );
                        if !matches!(resolved, VariableType::Bool | VariableType::Any) {
                            self.diagnostics.push(Diagnostic::error(
                                DiagnosticCode::TypeMismatch,
                                DiagnosticLocation::expression(
                                    self.path.clone(),
                                    node.id.clone(),
                                    col.id.clone(),
                                    None,
                                )
                                .with_target(target),
                                format!("input condition must return a boolean, got `{resolved}`"),
                            ));
                        }
                    }
                }
            }
        }

        let output = VariableType::empty_object();
        for col in content.outputs.iter() {
            if col.field.is_empty() {
                continue;
            }
            let declared = self.declared_output_type(node, col);
            let mut cell_types: Vec<VariableType> = Vec::new();
            let mut has_null_cell = false;
            for (row_idx, rule) in content.rules.iter().enumerate() {
                let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                    continue;
                };
                let target = CursorTarget::DecisionTableCell {
                    row: Self::row_key(rule, row_idx),
                    col: col.id.clone(),
                };
                let resolved = self.check_expression(
                    &node.id,
                    Some(col.id.clone()),
                    Some(target.clone()),
                    cell,
                    ExpressionKind::Standard,
                    &base_scope,
                );
                has_null_cell |= resolved.is_null();
                match &declared {
                    Some(expected) => {
                        if !resolved.is_null() && !resolved.satisfies(expected) {
                            self.diagnostics.push(Diagnostic::error(
                                DiagnosticCode::TypeMismatch,
                                DiagnosticLocation::expression(
                                    self.path.clone(),
                                    node.id.clone(),
                                    col.id.clone(),
                                    None,
                                )
                                .with_target(target),
                                format!("output cell must be `{expected}`, got `{resolved}`"),
                            ));
                        }
                    }
                    None => cell_types.push(resolved),
                }
            }
            let has_empty_cell = content
                .rules
                .iter()
                .any(|rule| rule.get(&col.id).is_none_or(|c| c.is_empty()));
            let mut merged = match &declared {
                Some(expected) => expected.shallow_clone(),
                None => {
                    let Some(merged) = cell_types
                        .iter()
                        .map(VariableType::shallow_clone)
                        .reduce(|acc, t| acc.merge(&t))
                    else {
                        continue;
                    };
                    merged
                }
            };
            if has_empty_cell || (has_null_cell && declared.is_some()) {
                merged = super::wrap_optional(merged);
            }
            if declared.is_none()
                && matches!(merged, VariableType::Any)
                && cell_types.len() > 1
                && !cell_types.iter().any(|t| matches!(t, VariableType::Any))
            {
                self.diagnostics.push(Diagnostic::error(
                    DiagnosticCode::TypeMismatch,
                    DiagnosticLocation::expression(
                        self.path.clone(),
                        node.id.clone(),
                        col.id.clone(),
                        None,
                    )
                    .with_target(CursorTarget::DecisionTableHead {
                        col: col.id.clone(),
                    }),
                    format!(
                        "'{}' has incompatible types: {}",
                        col.field,
                        cell_types
                            .iter()
                            .map(|t| format!("`{t}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
            output.insert_at_path(&col.field, &merged, true);
        }

        match content.hit_policy {
            DecisionTableHitPolicy::First => {
                if self.table_covered(content, &input_field_types) {
                    output
                } else if content.transform_attributes.pass_through {
                    if let VariableType::Object(fields) = &output {
                        let mut map = fields.borrow_mut();
                        let keys: Vec<Rc<str>> = map.keys().cloned().collect();
                        for key in keys {
                            if let Some(current) = map.get(&key).map(VariableType::shallow_clone) {
                                map.insert(key, super::wrap_optional(current));
                            }
                        }
                    }
                    output
                } else {
                    VariableType::Nullable(Rc::new(output))
                }
            }
            DecisionTableHitPolicy::Collect => output.array(),
        }
    }

    fn declared_output_type(
        &mut self,
        node: &DecisionNode,
        col: &DecisionTableOutputField,
    ) -> Option<VariableType> {
        let head = CursorTarget::DecisionTableHead {
            col: col.id.clone(),
        };
        let declared = match Self::parse_declared_column(col.column_type.as_deref()) {
            Ok(declared) => declared?,
            Err(message) => {
                self.diagnostics.push(Diagnostic::error(
                    DiagnosticCode::TypeMismatch,
                    DiagnosticLocation::expression(
                        self.path.clone(),
                        node.id.clone(),
                        col.id.clone(),
                        None,
                    )
                    .with_target(head),
                    message,
                ));
                return None;
            }
        };
        let resolved = declared.resolve(&self.dictionary_types);
        if resolved.is_none() {
            self.diagnostics.push(Diagnostic::error(
                DiagnosticCode::TypeMismatch,
                DiagnosticLocation::expression(
                    self.path.clone(),
                    node.id.clone(),
                    col.id.clone(),
                    None,
                )
                .with_target(head),
                format!(
                    "unknown output type '{declared}': no dictionary with that name is in scope"
                ),
            ));
        }
        resolved
    }

    fn parse_declared_column(column_type: Option<&str>) -> Result<Option<DeclaredType>, String> {
        DeclaredType::parse(column_type.unwrap_or(""))
    }

    pub(crate) fn output_expected(
        content: &DecisionTableContent,
        col_id: &str,
        dictionaries: &HashMap<Arc<str>, VariableType>,
    ) -> Option<VariableType> {
        let column = content.outputs.iter().find(|c| c.id.as_ref() == col_id)?;
        let declared = Self::parse_declared_column(column.column_type.as_deref()).ok()??;
        declared.resolve(dictionaries)
    }

    fn table_covered(
        &self,
        content: &DecisionTableContent,
        input_field_types: &HashMap<Arc<str>, VariableType>,
    ) -> bool {
        if content.rules.is_empty() {
            return false;
        }
        let row_is_catch_all = |rule: &ahash::HashMap<Arc<str>, Arc<str>>| {
            content
                .inputs
                .iter()
                .all(|ic| rule.get(&ic.id).is_none_or(|c| c.is_empty()))
        };
        if content.rules.iter().any(row_is_catch_all) {
            return true;
        }

        let intellisense = self.db.graph_intellisense();
        let mut groups: HashMap<Arc<str>, Vec<ArmTest>> = HashMap::new();
        for rule in content.rules.iter() {
            let mut constrained = content
                .inputs
                .iter()
                .filter(|ic| rule.get(&ic.id).is_some_and(|c| !c.is_empty()));
            let (Some(column), None) = (constrained.next(), constrained.next()) else {
                continue;
            };
            if column.field.is_none() {
                continue;
            }
            let Some(cell) = rule.get(&column.id) else {
                continue;
            };
            groups
                .entry(column.id.clone())
                .or_default()
                .push(IntelliSenseSource::cell_test(
                    &mut intellisense.borrow_mut(),
                    cell,
                ));
        }
        groups.iter().any(|(col_id, tests)| {
            input_field_types
                .get(col_id)
                .is_some_and(|t| DecisionTableIr::cells_cover(tests, t))
        })
    }

    fn check_output_schema(
        &mut self,
        node: &DecisionNode,
        actual: &VariableType,
        expected: &VariableType,
    ) {
        let VariableType::Object(expected_fields) = expected else {
            return;
        };
        let (actual_base, _) = actual.unwrap_nullable();
        let VariableType::Object(actual_fields) = actual_base else {
            return;
        };
        let mut keys: Vec<Rc<str>> = expected_fields.borrow().keys().cloned().collect();
        keys.sort();
        for key in keys {
            let Some(expected_type) = expected_fields.borrow().get(&key).cloned() else {
                continue;
            };
            let actual_type = actual_fields.borrow().get(&key).cloned();
            match actual_type {
                None => {
                    let (inner, optional) = expected_type.unwrap_nullable();
                    if !optional && !matches!(inner, VariableType::Any | VariableType::Null) {
                        self.diagnostics.push(Diagnostic::error(
                            DiagnosticCode::TypeMismatch,
                            DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                            format!(
                                "output schema requires property '{key}' of type `{inner}`, but it is never produced"
                            ),
                        ));
                    }
                }
                Some(actual_type) => {
                    if !actual_type.satisfies(&expected_type) {
                        self.diagnostics.push(Diagnostic::error(
                            DiagnosticCode::TypeMismatch,
                            DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                            format!(
                                "output property '{key}' has type `{actual_type}`, but the output schema expects `{expected_type}`"
                            ),
                        ));
                    }
                }
            }
        }
    }

    fn lint_unreachable(&mut self, topology: &GraphTopology) {
        let input_indices: Vec<usize> = self
            .content
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| matches!(node.kind, DecisionNodeKind::InputNode { .. }))
            .map(|(idx, _)| idx)
            .collect();
        if input_indices.is_empty() {
            return;
        }
        let mut reachable = vec![false; self.content.nodes.len()];
        let mut stack = input_indices;
        while let Some(idx) = stack.pop() {
            if std::mem::replace(&mut reachable[idx], true) {
                continue;
            }
            stack.extend(topology.outgoing[idx].iter().copied());
        }
        for (idx, node) in self.content.nodes.iter().enumerate() {
            if !reachable[idx] {
                self.diagnostics.push(Diagnostic::hint(
                    DiagnosticCode::UnreachableNode,
                    DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                    format!("node '{}' is not reachable from the input node", node.name),
                ));
            }
        }
    }

    fn lint_expressions(&mut self) {
        let intellisense = self.db.graph_intellisense();
        for node in &self.content.nodes {
            for site in Self::node_sites(node) {
                if !matches!(site.kind, ExpressionKind::Standard) {
                    continue;
                }
                let findings = intellisense
                    .borrow_mut()
                    .with_ast(&site.source, false, |root, metadata| {
                        RedundantParentheses::scan(root, metadata)
                    })
                    .unwrap_or_default();
                for (span, inner_span) in findings {
                    let message = match inner_span {
                        Some(inner) => format!(
                            "unnecessary parentheses around '{}'",
                            AstOps::display_snippet(&site.source, inner)
                        ),
                        None => "unnecessary parentheses".to_string(),
                    };
                    let location = DiagnosticLocation {
                        policy_path: self.path.clone(),
                        block_id: Some(node.id.clone()),
                        expression_id: site.expression_id.clone(),
                        span,
                        target: Some(site.target.clone()),
                    };
                    self.diagnostics.push(Diagnostic::hint(
                        DiagnosticCode::RedundantParentheses,
                        location,
                        message,
                    ));
                }
            }
        }
    }

    fn check_switch(
        &mut self,
        node: &DecisionNode,
        content: &SwitchNodeContent,
        scope: &VariableType,
    ) -> HashMap<Arc<str>, VariableType> {
        let condition_scope = Self::scope_with_nodes(scope, &self.nodes_scope.shallow_clone());
        let first_hit = matches!(content.hit_policy, SwitchStatementHitPolicy::First);
        let mut branches: HashMap<Arc<str>, VariableType> = HashMap::new();
        let mut prior_tests: Vec<ArmTest> = Vec::new();

        for statement in content.statements.iter() {
            let test = if statement.condition.is_empty() {
                ArmTest::Default
            } else {
                let resolved = self.check_expression(
                    &node.id,
                    Some(statement.id.clone()),
                    None,
                    &statement.condition,
                    ExpressionKind::Standard,
                    &condition_scope,
                );
                if !matches!(resolved, VariableType::Bool | VariableType::Any) {
                    self.diagnostics.push(Diagnostic::error(
                        DiagnosticCode::TypeMismatch,
                        DiagnosticLocation::expression(
                            self.path.clone(),
                            node.id.clone(),
                            statement.id.clone(),
                            None,
                        ),
                        format!("switch condition must return a boolean, got `{resolved}`"),
                    ));
                }
                let intellisense = self.db.graph_intellisense();
                let mut is = intellisense.borrow_mut();
                IntelliSenseSource::arm_test(&mut is, &statement.condition)
            };

            let mut narrowed = scope.shallow_clone();
            if first_hit {
                for prior in &prior_tests {
                    narrowed = Self::narrow_negative(&narrowed, prior);
                }
            }
            narrowed = Self::narrow_positive(&narrowed, &test);
            branches.insert(statement.id.clone(), narrowed);
            if first_hit {
                prior_tests.push(test);
            }
        }
        branches
    }

    fn narrow_positive(scope: &VariableType, test: &ArmTest) -> VariableType {
        match test {
            ArmTest::Enum { path, values } => Self::narrow_path(scope, path, |current| {
                let (base, _) = current.unwrap_nullable();
                match base {
                    VariableType::Enum(_, declared) => {
                        let retained: Vec<Rc<str>> = declared
                            .iter()
                            .filter(|d| values.iter().any(|v| v.as_ref() == d.as_ref()))
                            .cloned()
                            .collect();
                        match retained.len() {
                            0 => base.shallow_clone(),
                            1 => VariableType::Const(retained[0].clone()),
                            _ => VariableType::Enum(None, retained),
                        }
                    }
                    VariableType::String => match values.len() {
                        1 => VariableType::Const(Rc::from(values[0].as_ref())),
                        _ => VariableType::Enum(
                            None,
                            values.iter().map(|v| Rc::from(v.as_ref())).collect(),
                        ),
                    },
                    other => other.shallow_clone(),
                }
            }),
            ArmTest::Bool { path, .. } => Self::narrow_path(scope, path, |current| {
                current.unwrap_nullable().0.shallow_clone()
            }),
            ArmTest::Number { path, .. } => Self::narrow_path(scope, path, |current| {
                current.unwrap_nullable().0.shallow_clone()
            }),
            ArmTest::Default | ArmTest::Unrecognized => scope.shallow_clone(),
        }
    }

    fn narrow_negative(scope: &VariableType, test: &ArmTest) -> VariableType {
        let ArmTest::Enum { path, values } = test else {
            return scope.shallow_clone();
        };
        Self::narrow_path(scope, path, |current| {
            let (base, nullable) = current.unwrap_nullable();
            let VariableType::Enum(_, declared) = base else {
                return current.shallow_clone();
            };
            let retained: Vec<Rc<str>> = declared
                .iter()
                .filter(|d| !values.iter().any(|v| v.as_ref() == d.as_ref()))
                .cloned()
                .collect();
            let narrowed = match retained.len() {
                0 => return current.shallow_clone(),
                1 => VariableType::Const(retained[0].clone()),
                _ => VariableType::Enum(None, retained),
            };
            if nullable {
                VariableType::Nullable(Rc::new(narrowed))
            } else {
                narrowed
            }
        })
    }

    fn narrow_path(
        scope: &VariableType,
        path: &[Rc<str>],
        narrow: impl FnOnce(&VariableType) -> VariableType,
    ) -> VariableType {
        let Some(head) = path.first() else {
            return scope.shallow_clone();
        };
        let VariableType::Object(fields) = scope else {
            return scope.shallow_clone();
        };
        let map = fields.borrow();
        let Some(current) = map.get(head.as_ref()) else {
            return scope.shallow_clone();
        };
        let replaced = if path.len() == 1 {
            narrow(current)
        } else {
            Self::narrow_path(current, &path[1..], narrow)
        };
        let mut cloned = map.clone();
        drop(map);
        cloned.insert(head.clone(), replaced);
        VariableType::Object(Rc::new(std::cell::RefCell::new(cloned)))
    }

    fn resolve_decision_signature(
        &mut self,
        node: &DecisionNode,
        content: &DecisionNodeContent,
    ) -> Option<GraphSignature> {
        match self.db.decision_signature(&content.key) {
            SignatureResolution::Found(signature) => Some(signature),
            SignatureResolution::Recursive => None,
            SignatureResolution::Missing => {
                self.diagnostics.push(Diagnostic::error(
                    DiagnosticCode::ImportNotFound,
                    DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                    format!(
                        "referenced decision '{}' was not found in the workspace",
                        content.key
                    ),
                ));
                None
            }
        }
    }

    fn check_decision_input(
        &mut self,
        node: &DecisionNode,
        content: &DecisionNodeContent,
        signature: &GraphSignature,
        scope: &VariableType,
    ) {
        if !self.validate {
            return;
        }
        let VariableType::Object(expected) = &signature.input else {
            return;
        };
        let (scope_base, _) = scope.unwrap_nullable();
        let VariableType::Object(actual) = scope_base else {
            return;
        };
        let mut missing: Vec<(String, VariableType)> = Vec::new();
        let mut mismatched: Vec<(String, VariableType, VariableType)> = Vec::new();
        Self::diff_required(
            String::new(),
            &expected.borrow(),
            &actual.borrow(),
            &mut missing,
            &mut mismatched,
        );
        for (path, expected_type) in missing {
            self.diagnostics.push(Diagnostic::error(
                DiagnosticCode::TypeMismatch,
                DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                format!(
                    "decision '{}' requires input '{path}' of type `{expected_type}`, but it is not provided",
                    content.key
                ),
            ));
        }
        for (path, actual_type, expected_type) in mismatched {
            self.diagnostics.push(Diagnostic::error(
                DiagnosticCode::TypeMismatch,
                DiagnosticLocation::block(self.path.clone(), node.id.clone()),
                format!(
                    "input '{path}' for decision '{}' has type `{actual_type}`, but `{expected_type}` is expected",
                    content.key
                ),
            ));
        }
    }

    fn diff_required(
        prefix: String,
        expected: &HashMap<Rc<str>, VariableType>,
        actual: &HashMap<Rc<str>, VariableType>,
        missing: &mut Vec<(String, VariableType)>,
        mismatched: &mut Vec<(String, VariableType, VariableType)>,
    ) {
        let mut keys: Vec<&Rc<str>> = expected.keys().collect();
        keys.sort();
        for key in keys {
            let expected_type = &expected[key];
            let path = if prefix.is_empty() {
                key.to_string()
            } else {
                format!("{prefix}.{key}")
            };
            let (expected_inner, optional) = expected_type.unwrap_nullable();
            match actual.get(key) {
                None => {
                    if !optional
                        && !matches!(expected_inner, VariableType::Any | VariableType::Null)
                    {
                        missing.push((path, expected_inner.shallow_clone()));
                    }
                }
                Some(actual_type) => {
                    let (actual_inner, _) = actual_type.unwrap_nullable();
                    if matches!(actual_inner, VariableType::Any) {
                        continue;
                    }
                    if let (VariableType::Object(e), VariableType::Object(a)) =
                        (expected_inner, actual_inner)
                    {
                        Self::diff_required(path, &e.borrow(), &a.borrow(), missing, mismatched);
                        continue;
                    }
                    if !actual_type.satisfies(expected_type) {
                        mismatched.push((
                            path,
                            actual_type.shallow_clone(),
                            expected_type.shallow_clone(),
                        ));
                    }
                }
            }
        }
    }

    fn check_expression(
        &mut self,
        node_id: &Arc<str>,
        expression_id: Option<Arc<str>>,
        target: Option<CursorTarget>,
        source: &Arc<str>,
        kind: ExpressionKind,
        scope: &VariableType,
    ) -> VariableType {
        let intellisense = self.db.graph_intellisense();
        let analysis =
            IntelliSenseSource::analyze(&mut intellisense.borrow_mut(), source, kind, scope);
        for diagnostic in &analysis.diagnostics {
            let location = DiagnosticLocation {
                policy_path: self.path.clone(),
                block_id: Some(node_id.clone()),
                expression_id: expression_id.clone(),
                span: Some(diagnostic.span),
                target: target.clone(),
            };
            self.diagnostics
                .push(Diagnostic::from_expression(diagnostic, location));
        }
        if self.validate {
            self.validate_read_paths(node_id, &expression_id, &target, &analysis.reads, scope);
        }
        analysis.return_type.shallow_clone()
    }

    fn validate_read_paths(
        &mut self,
        node_id: &Arc<str>,
        expression_id: &Option<Arc<str>>,
        target: &Option<CursorTarget>,
        reads: &[zen_expression::intellisense::ReadDependency],
        scope: &VariableType,
    ) {
        let mut flattened = Vec::new();
        ReadFlattener::extend_from_deps(reads, expression_id, &mut flattened);
        for read in flattened {
            if read.unresolved || read.via_alias {
                continue;
            }
            let root = read.path.split('.').next().unwrap_or_default();
            if root.is_empty() || root.starts_with('$') {
                continue;
            }
            let Some(unknown) = Self::unknown_segment(scope, root) else {
                continue;
            };
            let location = DiagnosticLocation {
                policy_path: self.path.clone(),
                block_id: Some(node_id.clone()),
                expression_id: read.expression_id.clone(),
                span: read.span,
                target: target.clone(),
            };
            self.diagnostics.push(Diagnostic::error(
                DiagnosticCode::UndefinedVariable,
                location,
                format!("Unknown property '{unknown}'"),
            ));
        }
    }

    fn unknown_segment(scope: &VariableType, path: &str) -> Option<String> {
        let mut current = scope.shallow_clone();
        let mut walked: Vec<&str> = Vec::new();
        for segment in path.split('.') {
            while let VariableType::Nullable(inner) = current {
                current = inner.as_ref().shallow_clone();
            }
            let VariableType::Object(fields) = &current else {
                return None;
            };
            walked.push(segment);
            let next = fields.borrow().get(segment).cloned();
            match next {
                Some(t) => current = t,
                None => return Some(walked.join(".")),
            }
        }
        None
    }

    fn inferred_inputs(
        &self,
        topology: &GraphTopology,
        nodes: &HashMap<Arc<str>, GraphNodeAnalysis>,
        graph_input: &VariableType,
    ) -> Vec<Arc<str>> {
        if !matches!(graph_input, VariableType::Any) {
            return Vec::new();
        }
        let Some(order) = &topology.order else {
            return Vec::new();
        };

        let input_successors: HashSet<usize> = order
            .iter()
            .filter(|&&idx| {
                matches!(
                    self.content.nodes[idx].kind,
                    DecisionNodeKind::InputNode { .. }
                )
            })
            .flat_map(|&idx| topology.outgoing[idx].iter().copied())
            .collect();

        let mut paths: Vec<Arc<str>> = Vec::new();
        for &idx in &input_successors {
            let node = &self.content.nodes[idx];
            let provided: HashSet<Rc<str>> = topology.incoming[idx]
                .iter()
                .filter_map(|(pred, _)| {
                    let pred_node = &self.content.nodes[*pred];
                    if matches!(pred_node.kind, DecisionNodeKind::InputNode { .. }) {
                        return None;
                    }
                    nodes.get(&pred_node.id)
                })
                .filter_map(|analysis| match &analysis.output {
                    VariableType::Object(fields) => {
                        Some(fields.borrow().keys().cloned().collect::<Vec<Rc<str>>>())
                    }
                    _ => None,
                })
                .flatten()
                .collect();
            paths.extend(self.node_read_paths(node, &provided));
        }
        paths.sort();
        paths.dedup();
        paths
    }

    fn node_read_paths(&self, node: &DecisionNode, provided: &HashSet<Rc<str>>) -> Vec<Arc<str>> {
        let intellisense = self.db.graph_intellisense();
        let mut is = intellisense.borrow_mut();
        let mut reads = Vec::new();
        for site in Self::node_sites(node) {
            let deps = match site.kind {
                ExpressionKind::Standard => is.reads(&site.source),
                ExpressionKind::Unary => is.reads_unary(&site.source),
            };
            ReadFlattener::extend_from_deps(&deps, &None, &mut reads);
        }
        reads
            .into_iter()
            .filter(|read| !read.unresolved && !read.via_alias)
            .filter_map(|read| {
                let root = read
                    .path
                    .split_once('.')
                    .map_or(read.path.as_ref(), |(root, _)| root);
                let external = !root.starts_with('$') && !provided.contains(root);
                external.then_some(read.path)
            })
            .collect()
    }

    pub(crate) fn node_sites(node: &DecisionNode) -> Vec<GraphExpressionSite> {
        let mut sites: Vec<GraphExpressionSite> = Vec::new();
        let mut push_input_field = |attributes: &TransformAttributes| {
            if let Some(field) = &attributes.input_field {
                sites.push(GraphExpressionSite {
                    target: CursorTarget::TransformInput,
                    expression_id: None,
                    source: field.clone(),
                    kind: ExpressionKind::Standard,
                });
            }
        };
        match &node.kind {
            DecisionNodeKind::ExpressionNode { content } => {
                push_input_field(&content.transform_attributes);
                for row in content.expressions.iter() {
                    if !row.key.is_empty() && !row.value.is_empty() {
                        sites.push(GraphExpressionSite {
                            target: CursorTarget::Expression { id: row.id.clone() },
                            expression_id: Some(row.id.clone()),
                            source: row.value.clone(),
                            kind: ExpressionKind::Standard,
                        });
                    }
                }
            }
            DecisionNodeKind::DecisionTableNode { content } => {
                push_input_field(&content.transform_attributes);
                for col in content.inputs.iter() {
                    if let Some(field) = &col.field {
                        sites.push(GraphExpressionSite {
                            target: CursorTarget::DecisionTableHead {
                                col: col.id.clone(),
                            },
                            expression_id: Some(col.id.clone()),
                            source: field.clone(),
                            kind: ExpressionKind::Standard,
                        });
                    }
                }
                for (row_idx, rule) in content.rules.iter().enumerate() {
                    let row_key = Self::row_key(rule, row_idx);
                    for col in content.inputs.iter() {
                        let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                            continue;
                        };
                        let kind = if col.field.is_some() {
                            ExpressionKind::Unary
                        } else {
                            ExpressionKind::Standard
                        };
                        sites.push(GraphExpressionSite {
                            target: CursorTarget::DecisionTableCell {
                                row: row_key.clone(),
                                col: col.id.clone(),
                            },
                            expression_id: Some(col.id.clone()),
                            source: cell.clone(),
                            kind,
                        });
                    }
                    for col in content.outputs.iter() {
                        if let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) {
                            sites.push(GraphExpressionSite {
                                target: CursorTarget::DecisionTableCell {
                                    row: row_key.clone(),
                                    col: col.id.clone(),
                                },
                                expression_id: Some(col.id.clone()),
                                source: cell.clone(),
                                kind: ExpressionKind::Standard,
                            });
                        }
                    }
                }
            }
            DecisionNodeKind::SwitchNode { content } => {
                for statement in content.statements.iter() {
                    if !statement.condition.is_empty() {
                        sites.push(GraphExpressionSite {
                            target: CursorTarget::Expression {
                                id: statement.id.clone(),
                            },
                            expression_id: Some(statement.id.clone()),
                            source: statement.condition.clone(),
                            kind: ExpressionKind::Standard,
                        });
                    }
                }
            }
            DecisionNodeKind::DecisionNode { content } => {
                push_input_field(&content.transform_attributes);
            }
            _ => {}
        }
        sites
    }

    pub(crate) fn row_key(rule: &ahash::HashMap<Arc<str>, Arc<str>>, row_idx: usize) -> Arc<str> {
        rule.get("_id")
            .cloned()
            .unwrap_or_else(|| Arc::from(row_idx.to_string()))
    }

    pub(crate) fn scope_with(base: &VariableType, extras: &[(&str, VariableType)]) -> VariableType {
        let mut opened = base.shallow_clone();
        while let VariableType::Nullable(inner) = opened {
            opened = inner.as_ref().shallow_clone();
        }
        if matches!(opened, VariableType::Any) {
            opened = VariableType::empty_object();
        }
        let VariableType::Object(fields) = &opened else {
            return opened;
        };
        let mut extended = fields.borrow().clone();
        for (key, value) in extras {
            extended.insert(Rc::from(*key), value.shallow_clone());
        }
        VariableType::Object(Rc::new(std::cell::RefCell::new(extended)))
    }

    pub(crate) fn scope_with_nodes(base: &VariableType, nodes: &VariableType) -> VariableType {
        Self::scope_with(base, &[(NODES_KEY, nodes.shallow_clone())])
    }

    fn sort_diagnostics(&mut self, topology: &GraphTopology) {
        self.diagnostics.sort_by_key(|d| {
            d.location
                .block_id
                .as_ref()
                .and_then(|id| topology.node_index.get(id).copied())
                .map_or((0, 0), |idx| (1, idx))
        });
    }
}
