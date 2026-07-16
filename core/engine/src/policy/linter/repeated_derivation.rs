use std::cell::RefCell;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::parser::Node;

use crate::policy::blocks::BlockKind;
use crate::workspace::types::{
    Diagnostic, DiagnosticCode, DiagnosticLocation, ExpressionKind, Span,
};

use super::{AstOps, LintContext, LintRule};

pub(crate) struct RepeatedDerivation;

type SiteKey = (Arc<str>, Arc<str>, Arc<str>);

struct Occurrence {
    site: SiteKey,
    span: Span,
}

struct Fragment {
    ops: usize,
    display: String,
    policies: HashSet<Arc<str>>,
    occurrences: Vec<Occurrence>,
}

impl RepeatedDerivation {
    const SIMPLE_THRESHOLD: usize = 3;
    const COMPLEX_THRESHOLD: usize = 2;
    const COMPLEX_OPS: usize = 3;
    const MIN_FINGERPRINT_LEN: usize = 6;

    fn is_op(node: &Node) -> bool {
        match node {
            Node::Binary { .. }
            | Node::Unary { .. }
            | Node::Conditional { .. }
            | Node::FunctionCall { .. }
            | Node::MethodCall { .. }
            | Node::Interval { .. }
            | Node::Slice { .. }
            | Node::TemplateString(_) => true,
            Node::Object(entries) => !entries.is_empty(),
            Node::Array(items) => !items.is_empty(),
            _ => false,
        }
    }

    fn op_count(node: &Node) -> usize {
        let count = RefCell::new(0usize);
        node.walk(|n| {
            if Self::is_op(n) {
                *count.borrow_mut() += 1;
            }
        });
        count.into_inner()
    }

    fn threshold(ops: usize) -> usize {
        if ops >= Self::COMPLEX_OPS {
            Self::COMPLEX_THRESHOLD
        } else {
            Self::SIMPLE_THRESHOLD
        }
    }

    fn collect(cx: &LintContext) -> HashMap<String, Fragment> {
        let mut fragments: HashMap<String, Fragment> = HashMap::new();

        for (policy_path, parsed) in cx.unit_policies() {
            for block in parsed.policy.rules() {
                if matches!(block.kind, BlockKind::DecisionTable(_)) {
                    continue;
                }
                for location in block.kind.expressions(&block.id) {
                    if !matches!(location.kind, ExpressionKind::Standard) {
                        continue;
                    }
                    let site: SiteKey = (
                        policy_path.clone(),
                        location.block_id.clone(),
                        location.expression_id.clone(),
                    );
                    let found = cx
                        .with_ast(&location.source, location.kind, |root, metadata| {
                            let collected = RefCell::new(Vec::new());
                            root.walk(|node| {
                                if !Self::is_op(node) {
                                    return;
                                }
                                let Some(span) = AstOps::span(metadata, node) else {
                                    return;
                                };
                                collected.borrow_mut().push((span, Self::op_count(node)));
                            });
                            collected.into_inner()
                        })
                        .unwrap_or_default();

                    for (span, ops) in found {
                        let key = AstOps::fingerprint(&location.source, span);
                        if key.chars().count() < Self::MIN_FINGERPRINT_LEN {
                            continue;
                        }
                        let fragment = fragments.entry(key).or_insert_with(|| Fragment {
                            ops,
                            display: AstOps::display_snippet(&location.source, span),
                            policies: HashSet::default(),
                            occurrences: Vec::new(),
                        });
                        fragment.policies.insert(policy_path.clone());
                        fragment.occurrences.push(Occurrence {
                            site: site.clone(),
                            span,
                        });
                    }
                }
            }
        }

        fragments
    }
}

impl LintRule for RepeatedDerivation {
    fn check(&self, cx: &LintContext, out: &mut Vec<Diagnostic>) {
        let fragments = Self::collect(cx);

        let mut ordered: Vec<(String, Fragment)> = fragments
            .into_iter()
            .filter(|(_, f)| f.occurrences.len() >= Self::threshold(f.ops))
            .collect();
        ordered.sort_by(|a, b| {
            b.1.ops
                .cmp(&a.1.ops)
                .then_with(|| b.0.len().cmp(&a.0.len()))
                .then_with(|| a.0.cmp(&b.0))
        });

        let mut covered: HashMap<SiteKey, Vec<Span>> = HashMap::new();
        for (_, fragment) in ordered {
            let live: Vec<&Occurrence> = fragment
                .occurrences
                .iter()
                .filter(|occ| {
                    covered.get(&occ.site).is_none_or(|spans| {
                        !spans
                            .iter()
                            .any(|outer| outer.0 <= occ.span.0 && occ.span.1 <= outer.1)
                    })
                })
                .collect();
            if live.len() < Self::threshold(fragment.ops) {
                continue;
            }

            let count = live.len();
            let message = if fragment.policies.len() > 1 {
                format!(
                    "'{}' is derived {count} times across {} policies — compute it once into a shared property and reuse that",
                    fragment.display,
                    fragment.policies.len()
                )
            } else {
                format!(
                    "'{}' is derived {count} times — compute it once into a dedicated property and reuse that",
                    fragment.display
                )
            };

            for occ in &live {
                if occ.site.0 != *cx.target() {
                    continue;
                }
                out.push(Diagnostic::hint(
                    DiagnosticCode::RepeatedDerivation,
                    DiagnosticLocation::expression(
                        occ.site.0.clone(),
                        occ.site.1.clone(),
                        occ.site.2.clone(),
                        Some(occ.span),
                    ),
                    message.clone(),
                ));
            }

            for occ in live {
                covered.entry(occ.site.clone()).or_default().push(occ.span);
            }
        }
    }
}
