use std::sync::Arc;

use serde::Serialize;
use zen_expression::slot::{subject_enum_options, EnumTable, LiteralFact, SlotRole, ValueOption};
use zen_expression::variable::VariableType;

use super::fact_utf16;
use crate::policy::blocks::ROW_ID_KEY;
use crate::policy::raw::BlockDoc;
use crate::workspace::db::Db;
use crate::workspace::graph::GraphAnalyzer;
use crate::workspace::types::{Cursor, CursorTarget, ExpressionKind};

/// Literal facts for one expression location; spans are UTF-16 code units.
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
    pub literals: Vec<LiteralFact>,
    pub enums: Vec<EnumTable>,
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
            graph_sites(self, &path)
        } else {
            policy_sites(self, &path)
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
        for site in sites {
            let cursor = Cursor {
                policy_path: path.clone(),
                block_id: site.block_id.clone(),
                pos: 0,
                target: site.target.clone(),
            };
            let Some(scope) = self.cursor_scope(&cursor) else {
                continue;
            };
            let unary = matches!(scope.kind, ExpressionKind::Unary);
            let subject_type = scope.subject_type();
            let subject_options = subject_type
                .as_ref()
                .and_then(|t| subject_enum_options(t, labels.as_ref()))
                .unwrap_or_default();
            let (literals, enums) = intellisense.borrow_mut().literals(
                &site.source,
                unary,
                &scope.scope,
                scope.expected.as_ref(),
            );
            out.push(ExpressionFacts {
                block_id: site.block_id,
                target: site.target,
                kind: scope.kind,
                role: scope.role,
                subject_type,
                expected_type: scope.expected,
                literals: literals
                    .into_iter()
                    .map(|f| fact_utf16(&site.source, f))
                    .collect(),
                enums,
                subject_options,
                source: site.source,
            });
        }
        intellisense.borrow_mut().set_labels(None);
        out
    }
}

/// Sites carry the document text verbatim, not the trimmed IR text, so facts match the live cell.
fn policy_sites(db: &Db, path: &Arc<str>) -> Vec<Site> {
    let Some(policy) = db.raw_policy(path) else {
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
            BlockDoc::DataModel { .. } | BlockDoc::Dictionary { .. } | BlockDoc::Ignored(_) => {}
        }
    }
    sites
}

fn graph_sites(db: &Db, path: &Arc<str>) -> Vec<Site> {
    let snap = db.snapshot();
    let Some(content) = snap.graphs.get(path).and_then(|doc| doc.as_graph()) else {
        return Vec::new();
    };
    content
        .nodes
        .iter()
        .flat_map(|node| {
            GraphAnalyzer::node_sites(node)
                .into_iter()
                .map(move |site| Site {
                    block_id: node.id.clone(),
                    target: site.target,
                    source: site.source,
                })
        })
        .collect()
}
