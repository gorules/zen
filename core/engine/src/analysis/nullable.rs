use std::cell::RefCell;

use zen_expression::intellisense::IntelliSense;
use zen_expression::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use zen_expression::parser::Node;

use crate::policy::linter::{AstOps, RedundantParentheses};
use crate::workspace::types::{Diagnostic, DiagnosticCode, Span};

pub(crate) struct NullableOperand;

struct FallbackEdit {
    span: Span,
    range: Span,
    kept: String,
    dropped: String,
    keep_left: bool,
}

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

impl NullableOperand {
    pub(crate) fn annotate(
        diagnostic: &mut Diagnostic,
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
    ) {
        if diagnostic.code == DiagnosticCode::RedundantNullish {
            Self::fallback_fix(diagnostic, is, source, unary);
            return;
        }
        if diagnostic.code != DiagnosticCode::TypeMismatch {
            return;
        }
        let Some(span) = diagnostic.location.span else {
            return;
        };
        let Some((operator, left, right)) = Self::parse_message(&diagnostic.message) else {
            return;
        };
        let (left_nullable, right_nullable) = (left.ends_with('?'), right.ends_with('?'));
        if left_nullable == right_nullable
            || left.trim_end_matches('?') != "number"
            || right.trim_end_matches('?') != "number"
        {
            return;
        }
        let Some(found) = Self::locate(is, source, unary, span, left_nullable) else {
            return;
        };
        if let Some(path) = found.path.filter(|p| !p.starts_with('$')) {
            diagnostic.args.insert("nullablePath", path);
        }
        let defaultable = match operator.as_str() {
            "+" | "-" | "*" | ">" | "<" | ">=" | "<=" => true,
            "/" | "%" => found.left,
            _ => false,
        };
        if !defaultable {
            return;
        }
        let operand: String = source
            .chars()
            .skip(found.operand.0 as usize)
            .take((found.operand.1 - found.operand.0) as usize)
            .collect();
        let replacement = format!("({operand} ?? 0)");
        let prefix: String = source.chars().take(found.operand.0 as usize).collect();
        let suffix: String = source.chars().skip(found.operand.1 as usize).collect();
        diagnostic
            .args
            .insert("fixSource", format!("{prefix}{replacement}{suffix}"));
        diagnostic.args.insert("fixOriginal", source.to_string());
        diagnostic.args.insert("fixOperand", operand);
    }

    fn fallback_fix(diagnostic: &mut Diagnostic, is: &mut IntelliSense, source: &str, unary: bool) {
        let Some(edit) = Self::fallback_edit(diagnostic, is, source, unary) else {
            return;
        };
        diagnostic.args.insert("fixOriginal", source.to_string());
        diagnostic.args.insert(
            "fixSource",
            Self::splice(source, &[(edit.range, edit.kept.clone())]),
        );
        diagnostic.args.insert(
            "fixKeep",
            if edit.keep_left { "left" } else { "right" }.to_string(),
        );
        diagnostic.args.insert(
            "fixFallback",
            if edit.keep_left {
                edit.dropped
            } else {
                edit.kept
            },
        );
    }

    pub(crate) fn fallback_all(
        diagnostics: &mut [Diagnostic],
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
    ) {
        let mut edits: Vec<(usize, FallbackEdit)> = diagnostics
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.code == DiagnosticCode::RedundantNullish && d.args.contains_key("fixSource")
            })
            .filter_map(|(idx, d)| Some((idx, Self::fallback_edit(d, is, source, unary)?)))
            .collect();
        if edits.len() < 2 {
            return;
        }
        edits.sort_by_key(|(_, edit)| edit.range.0);
        if edits
            .windows(2)
            .any(|pair| pair[0].1.range.1 > pair[1].1.range.0)
        {
            return;
        }
        let replacements: Vec<(Span, String)> = edits
            .iter()
            .map(|(_, edit)| (edit.range, edit.kept.clone()))
            .collect();
        let combined = Self::splice(source, &replacements);
        let targets: Vec<(Span, bool)> = edits
            .iter()
            .map(|(_, edit)| (edit.span, edit.keep_left))
            .collect();
        let expected = is.with_ast(source, unary, |root, metadata| {
            let swaps: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
            root.walk(|node| {
                let Node::Binary {
                    left,
                    operator: Operator::Logical(LogicalOperator::NullishCoalescing),
                    right,
                } = node
                else {
                    return;
                };
                let Some(span) = AstOps::span(metadata, node) else {
                    return;
                };
                if let Some((_, keep_left)) = targets.iter().find(|(target, _)| *target == span) {
                    let kept = if *keep_left { *left } else { *right };
                    swaps
                        .borrow_mut()
                        .push((format!("{node:?}"), format!("{kept:?}")));
                }
            });
            let swaps = swaps.into_inner();
            if swaps.len() != targets.len() {
                return None;
            }
            let mut debug = format!("{root:?}");
            for (from, to) in swaps {
                debug = debug.replace(&from, &to);
            }
            Some(RedundantParentheses::tree_shape(&debug))
        });
        let actual = is.with_ast(&combined, unary, |root, _| {
            RedundantParentheses::tree_shape(&format!("{root:?}"))
        });
        match (expected.flatten(), actual) {
            (Some(expected), Some(actual)) if expected == actual => {}
            _ => return,
        }
        for (idx, _) in edits {
            diagnostics[idx].args.insert("fixAll", combined.clone());
        }
    }

    fn splice(source: &str, replacements: &[(Span, String)]) -> String {
        let chars: Vec<char> = source.chars().collect();
        let mut out = String::with_capacity(source.len());
        let mut cursor = 0usize;
        let mut sorted: Vec<&(Span, String)> = replacements.iter().collect();
        sorted.sort_by_key(|(range, _)| range.0);
        for (range, with) in sorted {
            let (start, end) = (range.0 as usize, range.1 as usize);
            out.extend(&chars[cursor.min(chars.len())..start.min(chars.len())]);
            out.push_str(with);
            cursor = end;
        }
        out.extend(&chars[cursor.min(chars.len())..]);
        out
    }

    fn fallback_edit(
        diagnostic: &Diagnostic,
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
    ) -> Option<FallbackEdit> {
        let span = diagnostic.location.span?;
        let keep_left = if diagnostic.message.contains("is never null") {
            true
        } else if diagnostic.message.contains("is always null") {
            false
        } else {
            return None;
        };
        let located = is.with_ast(source, unary, |root, metadata| {
            let found: RefCell<Fallback> = RefCell::new(Fallback::default());
            root.walk(|node| match node {
                Node::Binary {
                    left,
                    operator: Operator::Logical(LogicalOperator::NullishCoalescing),
                    right,
                } if AstOps::span(metadata, node) == Some(span) => {
                    let (kept, dropped) = if keep_left {
                        (*left, *right)
                    } else {
                        (*right, *left)
                    };
                    if let (Some(kept), Some(dropped)) = (
                        AstOps::span(metadata, kept),
                        AstOps::span(metadata, dropped),
                    ) {
                        found.borrow_mut().operands = Some((kept, dropped));
                    }
                }
                Node::Parenthesized(inner) if AstOps::span(metadata, inner) == Some(span) => {
                    found.borrow_mut().wrapper = AstOps::span(metadata, node);
                }
                _ => {}
            });
            found.into_inner()
        })?;
        let Fallback {
            operands: Some((kept, dropped)),
            wrapper,
        } = located
        else {
            return None;
        };
        let text = |range: Span| -> String {
            source
                .chars()
                .skip(range.0 as usize)
                .take((range.1 - range.0) as usize)
                .collect()
        };
        let kept_text = text(kept);
        let mut shape = |candidate: &str| {
            is.with_ast(candidate, unary, |root, _| {
                RedundantParentheses::tree_shape(&format!("{root:?}"))
            })
        };
        let plain_shape = shape(&Self::splice(source, &[(span, kept_text.clone())]))?;
        let range = wrapper
            .filter(|wrapper| {
                shape(&Self::splice(source, &[(*wrapper, kept_text.clone())])).as_deref()
                    == Some(plain_shape.as_str())
            })
            .unwrap_or(span);
        Some(FallbackEdit {
            span,
            range,
            kept: kept_text,
            dropped: text(dropped),
            keep_left,
        })
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
        span: Span,
        left_nullable: bool,
    ) -> Option<Found> {
        is.with_ast(source, unary, |root, metadata| {
            let found: RefCell<Option<Found>> = RefCell::new(None);
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
                if !numeric || AstOps::span(metadata, node) != Some(span) {
                    return;
                }
                let operand = if left_nullable { *left } else { *right };
                let Some(operand_span) = AstOps::span(metadata, operand) else {
                    return;
                };
                found.replace(Some(Found {
                    operand: operand_span,
                    left: left_nullable,
                    path: Self::path(operand),
                }));
            });
            found.into_inner()
        })
        .flatten()
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
