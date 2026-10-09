use std::sync::Arc;

use serde::Serialize;
use zen_expression::slot::{Literals, SlotRole, ValueOption};
use zen_expression::variable::VariableType;
use zen_types::decision::DecisionNodeKind;

use crate::policy::blocks::ROW_ID_KEY;
use crate::policy::raw::BlockDoc;
use crate::workspace::db::Db;
use crate::workspace::graph::GraphAnalyzer;
use crate::workspace::types::{Cursor, CursorTarget, ExpressionKind, SpanOps};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionFacts {
    pub block_id: Arc<str>,
    pub target: CursorTarget,
    pub source: Arc<str>,
    pub kind: ExpressionKind,
    pub role: SlotRole,
    pub subject_type: Option<VariableType>,
    pub expected_type: Option<VariableType>,
    #[serde(flatten)]
    pub literals: Literals,
    pub subject_options: Vec<ValueOption>,
}

struct Site {
    block_id: Arc<str>,
    target: CursorTarget,
    source: Arc<str>,
}

impl Db {
    pub fn facts(&self, policy: &str) -> Vec<ExpressionFacts> {
        let path: Arc<str> = Arc::from(policy);
        let graph = self.is_graph(policy);
        let sites = if graph {
            self.graph_sites(&path)
        } else {
            self.policy_sites(&path)
        };
        if sites.is_empty() {
            return Vec::new();
        }
        let labels = self.label_resolver(policy);
        let intellisense = if graph {
            self.graph_intellisense()
        } else {
            self.intellisense()
        };
        intellisense.borrow_mut().set_labels(labels.clone());
        let mut out = Vec::with_capacity(sites.len());
        let mut cache = super::siblings::SiblingCache::default();
        for site in sites {
            let cursor = Cursor {
                policy_path: path.clone(),
                block_id: site.block_id.clone(),
                pos: 0,
                target: site.target.clone(),
            };
            let Some(scope) = self.cursor_scope_cached(&cursor, &mut cache) else {
                continue;
            };
            let subject_type = scope.subject_type();
            let subject_options = subject_type
                .as_ref()
                .filter(|_| site.source.trim().is_empty())
                .and_then(|t| ValueOption::for_type(t, labels.as_ref()))
                .unwrap_or_default();
            let literals = intellisense
                .borrow_mut()
                .literals(
                    &site.source,
                    scope.is_unary(),
                    &scope.scope,
                    scope.literal_expected(),
                )
                .map_spans(|span| SpanOps::char_span(&site.source, span));
            out.push(ExpressionFacts {
                block_id: site.block_id,
                target: site.target,
                kind: scope.kind,
                role: scope.role,
                subject_type,
                expected_type: scope.expected,
                literals,
                subject_options,
                source: site.source,
            });
        }
        intellisense.borrow_mut().set_labels(None);
        out
    }

    fn policy_sites(&self, path: &Arc<str>) -> Vec<Site> {
        let Some(policy) = self.raw_policy(path) else {
            return Vec::new();
        };
        let mut sites = Vec::new();
        for block in &policy.blocks {
            let Some(block_id) = block.id() else {
                continue;
            };
            let block_id: Arc<str> = Arc::from(block_id);
            let mut push = |target: CursorTarget, source: &Arc<str>| {
                sites.push(Site {
                    block_id: block_id.clone(),
                    target,
                    source: source.clone(),
                });
            };
            match block {
                BlockDoc::DecisionTable { data: table, .. } => {
                    for col in &table.inputs {
                        if let Some(field) = col.field.as_ref().filter(|f| !f.is_empty()) {
                            push(
                                CursorTarget::DecisionTableHead {
                                    col: col.id.clone(),
                                },
                                field,
                            );
                        }
                    }
                    let empty: Arc<str> = Arc::from("");
                    for rule in &table.rules {
                        let Some(row) = rule.get(ROW_ID_KEY) else {
                            continue;
                        };
                        let ids = table
                            .inputs
                            .iter()
                            .map(|c| &c.id)
                            .chain(table.outputs.iter().map(|c| &c.id));
                        for col in ids {
                            push(
                                CursorTarget::DecisionTableCell {
                                    row: row.clone(),
                                    col: col.clone(),
                                },
                                rule.get(col).unwrap_or(&empty),
                            );
                        }
                    }
                }
                BlockDoc::Expression { id, data } => {
                    if !data.value.is_empty() {
                        push(CursorTarget::Expression { id: id.clone() }, &data.value);
                    }
                }
                BlockDoc::Assertion { data, .. } => {
                    for condition in &data.conditions {
                        if !condition.expression.is_empty() {
                            push(
                                CursorTarget::Expression {
                                    id: condition.id.clone(),
                                },
                                &condition.expression,
                            );
                        }
                    }
                }
                BlockDoc::Match { data, .. } => {
                    if !data.key.is_empty() {
                        push(CursorTarget::MatchTarget, &data.key);
                    }
                    for arm in &data.arms {
                        if !arm.condition.is_empty() {
                            push(
                                CursorTarget::Expression { id: arm.id.clone() },
                                &arm.condition,
                            );
                        }
                        if !arm.value.is_empty() {
                            push(CursorTarget::MatchValue { id: arm.id.clone() }, &arm.value);
                        }
                    }
                }
                // The expressions on a data model's properties (features, per
                // event values, model inputs and `when`): facts like any other
                // expression, so business mode shows their pills unfocused.
                BlockDoc::DataModel { data, .. } => {
                    for prop in &data.properties {
                        if let Some(feature) = prop.feature.as_ref().filter(|f| !f.expr.is_empty()) {
                            push(CursorTarget::FeatureExpr { id: prop.id.clone() }, &feature.expr);
                        }
                        if let Some(compute) = prop.compute.as_ref().filter(|c| !c.is_empty()) {
                            push(CursorTarget::ComputeExpr { id: prop.id.clone() }, compute);
                        }
                        let Some(model) = prop.model.as_ref() else {
                            continue;
                        };
                        if let Some(inputs) = crate::policy::raw::call_inputs(model).and_then(serde_json::Value::as_object) {
                            for (input, expr) in inputs {
                                if let Some(expr) = expr.as_str().filter(|e| !e.is_empty()) {
                                    push(
                                        CursorTarget::ModelInput {
                                            id: prop.id.clone(),
                                            input: Arc::from(input.as_str()),
                                        },
                                        &Arc::from(expr),
                                    );
                                }
                            }
                        }
                        if let Some(when) = model.get("when").and_then(serde_json::Value::as_str).filter(|w| !w.is_empty()) {
                            push(CursorTarget::ModelWhen { id: prop.id.clone() }, &Arc::from(when));
                        }
                        if let Some(request) = model.get("request").and_then(serde_json::Value::as_str).filter(|s| !s.is_empty() && crate::policy::raw::call_inputs(model).is_none()) {
                            push(CursorTarget::ModelRequest { id: prop.id.clone() }, &Arc::from(request));
                        }
                        if let Some(response) = model.get("response").and_then(serde_json::Value::as_str).filter(|a| !a.is_empty()) {
                            push(CursorTarget::ModelResponse { id: prop.id.clone() }, &Arc::from(response));
                        }
                    }
                }
                BlockDoc::Dictionary { .. } | BlockDoc::Ignored(_) => {}
            }
        }
        sites
    }

    fn graph_sites(&self, path: &Arc<str>) -> Vec<Site> {
        let snap = self.snapshot();
        let Some(content) = snap.graphs.get(path).and_then(|doc| doc.as_graph()) else {
            return Vec::new();
        };
        let empty: Arc<str> = Arc::from("");
        content
            .nodes
            .iter()
            .flat_map(|node| {
                let mut sites: Vec<Site> = GraphAnalyzer::node_sites(node)
                    .into_iter()
                    .map(|site| Site {
                        block_id: node.id.clone(),
                        target: site.target,
                        source: site.source,
                    })
                    .collect();
                if let DecisionNodeKind::DecisionTableNode { content: table } = &node.kind {
                    for (index, rule) in table.rules.iter().enumerate() {
                        let row = GraphAnalyzer::row_key(rule, index);
                        let columns = table
                            .inputs
                            .iter()
                            .map(|c| &c.id)
                            .chain(table.outputs.iter().map(|c| &c.id));
                        for col in columns {
                            if rule.get(col).is_some_and(|cell| !cell.is_empty()) {
                                continue;
                            }
                            sites.push(Site {
                                block_id: node.id.clone(),
                                target: CursorTarget::DecisionTableCell {
                                    row: row.clone(),
                                    col: col.clone(),
                                },
                                source: empty.clone(),
                            });
                        }
                    }
                }
                sites
            })
            .collect()
    }
}
