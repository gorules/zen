use zen_expression::intellisense::AstMetadata;
use zen_expression::lexer::Operator;
use zen_expression::parser::{Associativity, Node, ParserOperator};

use crate::workspace::types::{
    Diagnostic, DiagnosticArgs, DiagnosticCode, DiagnosticLocation, ExpressionKind, Span,
};

use super::{AstOps, LintContext, LintRule};

pub(crate) struct RedundantParentheses;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy)]
enum ParenSite {
    Delimited,
    Operand {
        operator: Operator,
        info: &'static ParserOperator,
        side: Side,
    },
    PostfixBase,
    Guarded,
}

struct ParenScan<'m> {
    metadata: &'m AstMetadata,
    findings: Vec<(Option<Span>, Option<Span>)>,
}

impl ParenScan<'_> {
    fn visit(&mut self, node: &Node, site: ParenSite) {
        match node {
            Node::Parenthesized(inner) => {
                if Self::is_redundant(inner, site) {
                    self.findings.push((
                        AstOps::span(self.metadata, node),
                        AstOps::span(self.metadata, inner),
                    ));
                }
                match inner {
                    Node::Parenthesized(_) => self.visit(inner, site),
                    _ => self.visit(inner, ParenSite::Delimited),
                }
            }
            Node::Binary {
                left,
                operator,
                right,
            } => {
                self.visit(left, Self::operand_site(operator, Side::Left));
                self.visit(right, Self::operand_site(operator, Side::Right));
            }
            Node::Unary { node, .. } => self.visit(node, ParenSite::Guarded),
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                self.visit(condition, ParenSite::Guarded);
                self.visit(on_true, ParenSite::Guarded);
                self.visit(on_false, ParenSite::Guarded);
            }
            Node::Member { node, property } => {
                self.visit(node, ParenSite::PostfixBase);
                self.visit(property, ParenSite::Delimited);
            }
            Node::Slice { node, from, to } => {
                self.visit(node, ParenSite::PostfixBase);
                from.iter()
                    .for_each(|n| self.visit(n, ParenSite::Delimited));
                to.iter().for_each(|n| self.visit(n, ParenSite::Delimited));
            }
            Node::Interval { left, right, .. } => {
                self.visit(left, ParenSite::Delimited);
                self.visit(right, ParenSite::Delimited);
            }
            Node::Array(items) => items
                .iter()
                .for_each(|n| self.visit(n, ParenSite::Delimited)),
            Node::TemplateString(parts) => parts
                .iter()
                .for_each(|n| self.visit(n, ParenSite::Delimited)),
            Node::Object(entries) => entries.iter().for_each(|(k, v)| {
                self.visit(k, ParenSite::Delimited);
                self.visit(v, ParenSite::Delimited);
            }),
            Node::Assignments { list, output } => {
                list.iter().for_each(|(k, v)| {
                    self.visit(k, ParenSite::Guarded);
                    self.visit(v, ParenSite::Delimited);
                });
                output
                    .iter()
                    .for_each(|n| self.visit(n, ParenSite::Delimited));
            }
            Node::FunctionCall { arguments, .. } => arguments
                .iter()
                .for_each(|n| self.visit(n, ParenSite::Delimited)),
            Node::MethodCall {
                this, arguments, ..
            } => {
                self.visit(this, ParenSite::PostfixBase);
                arguments
                    .iter()
                    .for_each(|n| self.visit(n, ParenSite::Delimited));
            }
            Node::Closure { body, .. } => self.visit(body, ParenSite::Delimited),
            Node::Error { node, .. } => node.iter().for_each(|n| self.visit(n, ParenSite::Guarded)),
            _ => {}
        }
    }

    fn operand_site(operator: &Operator, side: Side) -> ParenSite {
        ParserOperator::binary(operator).map_or(ParenSite::Guarded, |info| ParenSite::Operand {
            operator: *operator,
            info,
            side,
        })
    }

    fn is_redundant(inner: &Node, site: ParenSite) -> bool {
        match site {
            ParenSite::Delimited => Self::is_atom(inner) || Self::is_compound(inner),
            ParenSite::Guarded => Self::is_atom(inner),
            ParenSite::PostfixBase => matches!(
                inner,
                Node::Identifier(_)
                    | Node::Root
                    | Node::Pointer
                    | Node::Member { .. }
                    | Node::Slice { .. }
                    | Node::FunctionCall { .. }
                    | Node::MethodCall { .. }
                    | Node::Parenthesized(_)
            ),
            ParenSite::Operand {
                operator: parent_operator,
                info: parent,
                side,
            } => match inner {
                Node::Binary { operator, .. } => {
                    Self::same_family(*operator, parent_operator)
                        && ParserOperator::binary(operator).is_some_and(|info| {
                            info.precedence > parent.precedence
                                || (info.precedence == parent.precedence
                                    && matches!(
                                        (side, parent.associativity),
                                        (Side::Left, Associativity::Left)
                                            | (Side::Right, Associativity::Right)
                                    ))
                        })
                }
                Node::Unary { operator, .. } => {
                    Self::same_family(*operator, parent_operator)
                        && ParserOperator::unary(operator)
                            .is_some_and(|info| info.precedence > parent.precedence)
                }
                _ => Self::is_atom(inner),
            },
        }
    }

    fn is_atom(node: &Node) -> bool {
        !matches!(
            node,
            Node::Binary { .. }
                | Node::Unary { .. }
                | Node::Conditional { .. }
                | Node::Closure { .. }
                | Node::Assignments { .. }
                | Node::Error { .. }
        )
    }

    fn is_compound(node: &Node) -> bool {
        matches!(
            node,
            Node::Binary { .. } | Node::Unary { .. } | Node::Conditional { .. }
        )
    }

    fn same_family(a: Operator, b: Operator) -> bool {
        a == b || matches!((a, b), (Operator::Arithmetic(_), Operator::Arithmetic(_)))
    }
}

impl RedundantParentheses {
    pub(crate) fn scan(root: &Node, metadata: &AstMetadata) -> Vec<(Option<Span>, Option<Span>)> {
        let mut scan = ParenScan {
            metadata,
            findings: Vec::new(),
        };
        scan.visit(root, ParenSite::Delimited);
        scan.findings
    }
}

impl RedundantParentheses {
    pub(crate) fn fix_args(
        source: &str,
        findings: &[(Option<Span>, Option<Span>)],
        mut shape: impl FnMut(&str) -> Option<String>,
    ) -> Vec<DiagnosticArgs> {
        let pairs: Vec<Option<(Span, Span)>> = findings
            .iter()
            .map(|(outer, inner)| Some(((*outer)?, (*inner)?)))
            .collect();
        let Some(expected) = shape(source) else {
            return vec![DiagnosticArgs::new(); findings.len()];
        };
        let mut verified =
            |fixed: String| (shape(&fixed).as_deref() == Some(expected.as_str())).then_some(fixed);
        let all: Vec<(Span, Span)> = pairs.iter().flatten().copied().collect();
        let fix_all = (all.len() > 1)
            .then(|| Self::strip(source, &all))
            .and_then(&mut verified);
        pairs
            .iter()
            .map(|pair| {
                let mut args = DiagnosticArgs::new();
                if let Some(fixed) = pair.and_then(|pair| verified(Self::strip(source, &[pair]))) {
                    args.insert("fixOriginal", source.to_string());
                    args.insert("fixSource", fixed);
                    if let Some(all) = &fix_all {
                        args.insert("fixAll", all.clone());
                    }
                }
                args
            })
            .collect()
    }

    pub(crate) fn tree_shape(debug: &str) -> String {
        const WRAPPER: &str = "Parenthesized(";
        let chars: Vec<char> = debug.chars().collect();
        let wrapper: Vec<char> = WRAPPER.chars().collect();
        let mut drop_close: Vec<usize> = Vec::new();
        let mut depth = 0usize;
        let mut quoted = false;
        let mut out = String::with_capacity(debug.len());
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if quoted {
                out.push(c);
                if c == '\\' && i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                quoted = c != '"';
                i += 1;
                continue;
            }
            if c == '"' {
                quoted = true;
                out.push(c);
            } else if chars[i..].starts_with(&wrapper) {
                depth += 1;
                drop_close.push(depth);
                i += wrapper.len();
                continue;
            } else if c == '(' {
                depth += 1;
                out.push(c);
            } else if c == ')' {
                if drop_close.last() == Some(&depth) {
                    drop_close.pop();
                } else {
                    out.push(c);
                }
                depth = depth.saturating_sub(1);
            } else {
                out.push(c);
            }
            i += 1;
        }
        out
    }

    fn strip(source: &str, pairs: &[(Span, Span)]) -> String {
        let chars: Vec<char> = source.chars().collect();
        let mut removed = vec![false; chars.len()];
        for (outer, inner) in pairs {
            for idx in
                (outer.0 as usize..inner.0 as usize).chain(inner.1 as usize..outer.1 as usize)
            {
                if let Some(slot) = removed.get_mut(idx) {
                    *slot = true;
                }
            }
        }
        let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '#');
        let mut out = String::with_capacity(source.len());
        let mut gap = false;
        for (idx, c) in chars.iter().enumerate() {
            if removed[idx] {
                gap = true;
                continue;
            }
            if gap && out.chars().last().is_some_and(word) && word(*c) {
                out.push(' ');
            }
            gap = false;
            out.push(*c);
        }
        out
    }
}

impl LintRule for RedundantParentheses {
    fn check(&self, cx: &LintContext, out: &mut Vec<Diagnostic>) {
        for block in cx.rules() {
            for expression in block.kind.expressions(&block.id) {
                if !matches!(expression.kind, ExpressionKind::Standard) {
                    continue;
                }
                let findings = cx
                    .with_ast(&expression.source, expression.kind, |root, metadata| {
                        RedundantParentheses::scan(root, metadata)
                    })
                    .unwrap_or_default();
                let fixes = Self::fix_args(&expression.source, &findings, |source| {
                    cx.with_ast(source, expression.kind, |root, _| {
                        Self::tree_shape(&format!("{root:?}"))
                    })
                });
                for ((span, inner_span), args) in findings.into_iter().zip(fixes) {
                    let message = match inner_span {
                        Some(inner) => format!(
                            "unnecessary parentheses around '{}'",
                            AstOps::display_snippet(&expression.source, inner)
                        ),
                        None => "unnecessary parentheses".to_string(),
                    };
                    let mut diagnostic = Diagnostic::hint(
                        DiagnosticCode::RedundantParentheses,
                        DiagnosticLocation::expression(
                            cx.target().clone(),
                            block.id.clone(),
                            expression.expression_id.clone(),
                            span,
                        ),
                        message,
                    );
                    diagnostic.args = args;
                    out.push(diagnostic);
                }
            }
        }
    }
}
