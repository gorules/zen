use std::cell::RefCell;

use ahash::HashSet;
use zen_expression::intellisense::AstMetadata;
use zen_expression::parser::Node;

use crate::policy::blocks::BlockKind;
use crate::policy::types::{Diagnostic, DiagnosticCode, DiagnosticLocation, ExpressionKind, Span};

use super::{AstOps, LintContext, LintRule};

pub(crate) struct PreferMatch;

struct ChainInfo {
    conditions: usize,
    scrutinee: Option<String>,
    span: Option<Span>,
}

impl PreferMatch {
    const MIN_CONDITIONS: usize = 2;

    fn inspect(root: &Node, metadata: &AstMetadata) -> Option<ChainInfo> {
        let node = AstOps::unwrap_parens(root);
        let Node::Conditional {
            condition,
            on_false,
            ..
        } = node
        else {
            return None;
        };

        let mut condition_paths = vec![Self::maximal_paths(condition)];
        let mut tail = AstOps::unwrap_parens(on_false);
        while let Node::Conditional {
            condition,
            on_false,
            ..
        } = tail
        {
            condition_paths.push(Self::maximal_paths(condition));
            tail = AstOps::unwrap_parens(on_false);
        }
        if condition_paths.len() < Self::MIN_CONDITIONS {
            return None;
        }

        let common = condition_paths
            .iter()
            .skip(1)
            .fold(condition_paths[0].clone(), |acc, paths| {
                acc.intersection(paths).cloned().collect()
            });
        let scrutinee = match common.len() {
            1 => common.into_iter().next(),
            _ => None,
        };

        Some(ChainInfo {
            conditions: condition_paths.len(),
            scrutinee,
            span: AstOps::span(metadata, node),
        })
    }

    fn maximal_paths(node: &Node) -> HashSet<String> {
        let all = RefCell::new(HashSet::default());
        node.walk(|n| {
            if let Some(path) = AstOps::dotted_path(n) {
                all.borrow_mut().insert(path);
            }
        });
        let all = all.into_inner();
        all.iter()
            .filter(|path| {
                !all.iter()
                    .any(|other| other.len() > path.len() && other.starts_with(&format!("{path}.")))
            })
            .cloned()
            .collect()
    }
}

impl LintRule for PreferMatch {
    fn check(&self, cx: &LintContext, out: &mut Vec<Diagnostic>) {
        for block in cx.rules() {
            let BlockKind::Expression(expression) = &block.kind else {
                continue;
            };
            if expression.value.is_empty() {
                continue;
            }
            let Some(Some(chain)) =
                cx.with_ast(&expression.value, ExpressionKind::Standard, Self::inspect)
            else {
                continue;
            };

            let subject = match &chain.scrutinee {
                Some(path) => format!(
                    "chained ternary with {} conditions on '{path}'",
                    chain.conditions
                ),
                None => format!("chained ternary with {} conditions", chain.conditions),
            };
            out.push(Diagnostic::hint(
                DiagnosticCode::PreferMatch,
                DiagnosticLocation::expression(
                    cx.target().clone(),
                    block.id.clone(),
                    block.id.clone(),
                    chain.span,
                ),
                format!("{subject} — a match block expresses this more clearly"),
            ));
        }
    }
}
