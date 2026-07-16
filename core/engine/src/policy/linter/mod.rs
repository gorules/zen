mod prefer_match;
mod redundant_parentheses;
mod repeated_derivation;
mod table_hygiene;

use std::sync::Arc;

use zen_expression::intellisense::AstMetadata;
use zen_expression::parser::Node;

use crate::policy::blocks::Block;
use crate::policy::ir::ParsedPolicy;
use crate::workspace::db::Db;
use crate::workspace::types::{Diagnostic, ExpressionKind, Span};

pub(crate) use prefer_match::PreferMatch;
pub(crate) use redundant_parentheses::RedundantParentheses;
pub(crate) use repeated_derivation::RepeatedDerivation;
pub(crate) use table_hygiene::{NonDiscriminatingColumn, RedundantTableRow};

pub(crate) trait LintRule {
    fn check(&self, cx: &LintContext, out: &mut Vec<Diagnostic>);
}

pub(crate) struct LintContext<'a> {
    db: &'a Db,
    target: &'a Arc<str>,
    parsed: Arc<ParsedPolicy>,
}

impl LintContext<'_> {
    pub(crate) fn target(&self) -> &Arc<str> {
        self.target
    }

    pub(crate) fn rules(&self) -> impl Iterator<Item = &Block> {
        self.parsed.policy.rules()
    }

    pub(crate) fn unit_policies(&self) -> Vec<(Arc<str>, Arc<ParsedPolicy>)> {
        let unit = self.db.unit(self.target);
        let mut members: Vec<Arc<str>> = unit.members.iter().cloned().collect();
        members.sort();
        members
            .into_iter()
            .filter_map(|path| self.db.parsed(&path).map(|parsed| (path, parsed)))
            .collect()
    }

    pub(crate) fn with_ast<T>(
        &self,
        source: &str,
        kind: ExpressionKind,
        f: impl for<'arena> FnOnce(&'arena Node<'arena>, &AstMetadata) -> T,
    ) -> Option<T> {
        let intellisense = self.db.intellisense();
        let mut intellisense = intellisense.borrow_mut();
        intellisense.with_ast(source, matches!(kind, ExpressionKind::Unary), f)
    }
}

pub(crate) struct AstOps;

impl AstOps {
    pub(crate) fn unwrap_parens<'a>(node: &'a Node<'a>) -> &'a Node<'a> {
        match node {
            Node::Parenthesized(inner) => Self::unwrap_parens(inner),
            _ => node,
        }
    }

    pub(crate) fn dotted_path(node: &Node) -> Option<String> {
        match node {
            Node::Identifier(name) => Some((*name).to_string()),
            Node::Member { node, property } => {
                let base = Self::dotted_path(Self::unwrap_parens(node))?;
                match Self::unwrap_parens(property) {
                    Node::String(p) => Some(format!("{base}.{p}")),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub(crate) fn span(metadata: &AstMetadata, node: &Node) -> Option<Span> {
        metadata
            .get(&(node as *const Node as usize))
            .map(|m| m.span)
    }

    pub(crate) fn fingerprint(source: &str, span: Span) -> String {
        Self::chars_at(source, span)
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    pub(crate) fn display_snippet(source: &str, span: Span) -> String {
        let mut out = String::new();
        let mut pending_space = false;
        for c in Self::chars_at(source, span) {
            if c.is_whitespace() {
                pending_space = !out.is_empty();
            } else {
                if pending_space {
                    out.push(' ');
                    pending_space = false;
                }
                out.push(c);
            }
        }
        if out.chars().count() > 60 {
            let truncated: String = out.chars().take(59).collect();
            return format!("{truncated}…");
        }
        out
    }

    fn chars_at(source: &str, span: Span) -> impl Iterator<Item = char> + '_ {
        source
            .chars()
            .skip(span.0 as usize)
            .take((span.1 as usize).saturating_sub(span.0 as usize))
    }
}

pub(crate) struct Linter {
    rules: Vec<Box<dyn LintRule>>,
}

impl Linter {
    pub(crate) fn standard() -> Self {
        Self {
            rules: vec![
                Box::new(RepeatedDerivation),
                Box::new(PreferMatch),
                Box::new(RedundantTableRow),
                Box::new(NonDiscriminatingColumn),
                Box::new(RedundantParentheses),
            ],
        }
    }

    pub(crate) fn run(&self, db: &Db, target: &Arc<str>) -> Vec<Diagnostic> {
        let Some(parsed) = db.parsed(target) else {
            return Vec::new();
        };
        let cx = LintContext { db, target, parsed };
        let mut out = Vec::new();
        for rule in &self.rules {
            rule.check(&cx, &mut out);
        }
        out
    }
}
