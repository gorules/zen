use std::cell::RefCell;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::intellisense::IntelliSense;
use zen_expression::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use zen_expression::parser::Node;

use crate::analysis::proof::{FixEdit, FixProof};
use crate::policy::linter::AstOps;
use crate::workspace::types::{Diagnostic, DiagnosticCode, Span};

pub(crate) struct NullableOperand;

#[derive(Default)]
struct Fallback {
    operands: Option<(Span, Span)>,
    wrapper: Option<Span>,
}

struct Found {
    operand: Span,
    left: bool,
    path: Option<String>,
}

struct Candidate {
    idx: usize,
    span: Span,
    kept: Span,
    dropped: Span,
    keep_left: bool,
    wrapper: Option<Span>,
}

impl Candidate {
    fn edit(&self, outer: Span) -> FixEdit {
        FixEdit::unwrap(outer, self.kept, Some((self.span, self.keep_left)))
    }
}

impl NullableOperand {
    pub(crate) fn annotate(
        diagnostics: &mut [Diagnostic],
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
    ) {
        Self::default_operands(diagnostics, is, source, unary);
        Self::fallbacks(diagnostics, is, source, unary);
    }

    fn default_operands(
        diagnostics: &mut [Diagnostic],
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
    ) {
        let requests: Vec<(usize, Span, String, bool)> = diagnostics
            .iter()
            .enumerate()
            .filter(|(_, d)| d.code == DiagnosticCode::TypeMismatch)
            .filter_map(|(idx, d)| {
                let span = d.location.span?;
                let (operator, left, right) = Self::parse_message(&d.message)?;
                let (left_nullable, right_nullable) = (left.ends_with('?'), right.ends_with('?'));
                (left_nullable != right_nullable
                    && left.trim_end_matches('?') == "number"
                    && right.trim_end_matches('?') == "number")
                    .then_some((idx, span, operator, left_nullable))
            })
            .collect();
        if requests.is_empty() {
            return;
        }
        let spans: HashSet<Span> = requests.iter().map(|(_, span, _, _)| *span).collect();
        let operands = Self::locate(is, source, unary, &spans);
        for (idx, span, operator, left_nullable) in requests {
            let Some((left, right)) = operands.get(&span) else {
                continue;
            };
            let found = if left_nullable { left } else { right };
            let diagnostic = &mut diagnostics[idx];
            if let Some(path) = found.path.as_ref().filter(|p| !p.starts_with('$')) {
                diagnostic.args.insert("nullablePath", path.clone());
            }
            let defaultable = match operator.as_str() {
                "+" | "-" | "*" | ">" | "<" | ">=" | "<=" => true,
                "/" | "%" => found.left,
                _ => false,
            };
            if !defaultable {
                continue;
            }
            let Some(operand) = AstOps::text(source, found.operand) else {
                continue;
            };
            let replacement = format!("({operand} ?? 0)");
            let Some(fixed) = AstOps::splice(source, &[(found.operand, replacement.as_str())])
            else {
                continue;
            };
            diagnostic.args.insert("fixSource", fixed);
            diagnostic.args.insert("fixOriginal", source.to_string());
            diagnostic.args.insert("fixOperand", operand.to_string());
        }
    }

    fn fallbacks(diagnostics: &mut [Diagnostic], is: &mut IntelliSense, source: &str, unary: bool) {
        let candidates = Self::candidates(diagnostics, is, source, unary);
        if candidates.is_empty() {
            return;
        }
        let preferred: Vec<FixEdit> = candidates
            .iter()
            .map(|candidate| candidate.edit(candidate.wrapper.unwrap_or(candidate.span)))
            .collect();
        let mut edits: Vec<Option<FixEdit>> = FixProof::proven(is, source, unary, &preferred)
            .into_iter()
            .zip(preferred)
            .map(|(proven, edit)| proven.then_some(edit))
            .collect();
        let retry: Vec<usize> = (0..candidates.len())
            .filter(|&i| edits[i].is_none() && candidates[i].wrapper.is_some())
            .collect();
        let alternatives: Vec<FixEdit> = retry
            .iter()
            .map(|&i| candidates[i].edit(candidates[i].span))
            .collect();
        for ((i, proven), edit) in retry
            .into_iter()
            .zip(FixProof::proven(is, source, unary, &alternatives))
            .zip(alternatives)
        {
            if proven {
                edits[i] = Some(edit);
            }
        }
        let accepted: Vec<&FixEdit> = edits.iter().flatten().collect();
        let fix_all = (accepted.len() > 1)
            .then(|| FixProof::holds(is, source, unary, &accepted))
            .flatten();
        for (candidate, edit) in candidates.iter().zip(&edits) {
            let Some(fixed) = edit.as_ref().and_then(|edit| edit.apply(source)) else {
                continue;
            };
            let (Some(kept), Some(dropped)) = (
                AstOps::text(source, candidate.kept),
                AstOps::text(source, candidate.dropped),
            ) else {
                continue;
            };
            let args = &mut diagnostics[candidate.idx].args;
            args.insert("fixOriginal", source.to_string());
            args.insert("fixSource", fixed);
            args.insert(
                "fixKeep",
                if candidate.keep_left { "left" } else { "right" }.to_string(),
            );
            args.insert(
                "fixFallback",
                if candidate.keep_left { dropped } else { kept }.to_string(),
            );
            if let Some(all) = &fix_all {
                args.insert("fixAll", all.clone());
            }
        }
    }

    fn candidates(
        diagnostics: &[Diagnostic],
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
    ) -> Vec<Candidate> {
        let targets: Vec<(usize, Span, bool)> = diagnostics
            .iter()
            .enumerate()
            .filter(|(_, d)| d.code == DiagnosticCode::RedundantNullish)
            .filter_map(|(idx, d)| {
                let keep_left = if d.message.contains("is never null") {
                    true
                } else if d.message.contains("is always null") {
                    false
                } else {
                    return None;
                };
                Some((idx, d.location.span?, keep_left))
            })
            .collect();
        if targets.is_empty() {
            return Vec::new();
        }
        let spans: HashSet<Span> = targets.iter().map(|(_, span, _)| *span).collect();
        let located = is
            .with_ast(source, unary, |root, metadata| {
                let found: RefCell<HashMap<Span, Fallback>> = RefCell::new(HashMap::new());
                root.walk(|node| match node {
                    Node::Binary {
                        left,
                        operator: Operator::Logical(LogicalOperator::NullishCoalescing),
                        right,
                    } => {
                        let Some(span) =
                            AstOps::span(metadata, node).filter(|span| spans.contains(span))
                        else {
                            return;
                        };
                        if let (Some(left), Some(right)) =
                            (AstOps::span(metadata, left), AstOps::span(metadata, right))
                        {
                            found.borrow_mut().entry(span).or_default().operands =
                                Some((left, right));
                        }
                    }
                    Node::Parenthesized(inner) => {
                        if let Some(span) =
                            AstOps::span(metadata, inner).filter(|span| spans.contains(span))
                        {
                            found.borrow_mut().entry(span).or_default().wrapper =
                                AstOps::span(metadata, node);
                        }
                    }
                    _ => {}
                });
                found.into_inner()
            })
            .unwrap_or_default();
        targets
            .into_iter()
            .filter_map(|(idx, span, keep_left)| {
                let fallback = located.get(&span)?;
                let (left, right) = fallback.operands?;
                let (kept, dropped) = if keep_left {
                    (left, right)
                } else {
                    (right, left)
                };
                Some(Candidate {
                    idx,
                    span,
                    kept,
                    dropped,
                    keep_left,
                    wrapper: fallback.wrapper,
                })
            })
            .collect()
    }

    fn parse_message(message: &str) -> Option<(String, String, String)> {
        let rest = message.strip_prefix("Operator `")?;
        let parts: Vec<&str> = rest.split('`').collect();
        match parts.as_slice() {
            [operator, " cannot be applied to types ", left, " and ", right, "."] => {
                Some((operator.to_string(), left.to_string(), right.to_string()))
            }
            _ => None,
        }
    }

    fn locate(
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
        spans: &HashSet<Span>,
    ) -> HashMap<Span, (Found, Found)> {
        is.with_ast(source, unary, |root, metadata| {
            let found: RefCell<HashMap<Span, (Found, Found)>> = RefCell::new(HashMap::new());
            root.walk(|node| {
                let Node::Binary {
                    left,
                    operator,
                    right,
                } = node
                else {
                    return;
                };
                let numeric = matches!(
                    operator,
                    Operator::Arithmetic(
                        ArithmeticOperator::Add
                            | ArithmeticOperator::Subtract
                            | ArithmeticOperator::Multiply
                            | ArithmeticOperator::Divide
                            | ArithmeticOperator::Modulus
                    ) | Operator::Comparison(
                        ComparisonOperator::LessThan
                            | ComparisonOperator::LessThanOrEqual
                            | ComparisonOperator::GreaterThan
                            | ComparisonOperator::GreaterThanOrEqual
                    )
                );
                if !numeric {
                    return;
                }
                let Some(span) = AstOps::span(metadata, node).filter(|span| spans.contains(span))
                else {
                    return;
                };
                let (Some(left_span), Some(right_span)) =
                    (AstOps::span(metadata, left), AstOps::span(metadata, right))
                else {
                    return;
                };
                found.borrow_mut().insert(
                    span,
                    (
                        Found {
                            operand: left_span,
                            left: true,
                            path: Self::path(left),
                        },
                        Found {
                            operand: right_span,
                            left: false,
                            path: Self::path(right),
                        },
                    ),
                );
            });
            found.into_inner()
        })
        .unwrap_or_default()
    }

    fn path(node: &Node) -> Option<String> {
        match node {
            Node::Parenthesized(inner) => Self::path(inner),
            Node::Identifier(name) => Some(name.to_string()),
            Node::Member {
                node,
                property: Node::String(key),
            } => Some(format!("{}.{key}", Self::path(node)?)),
            _ => None,
        }
    }
}
