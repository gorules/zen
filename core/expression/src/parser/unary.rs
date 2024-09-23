use crate::lexer::{Bracket, ComparisonOperator, Identifier, LogicalOperator, Operator, TokenKind};
use crate::parser::ast::{AstNodeError, Node};
use crate::parser::builtin::BuiltInFunction;
use crate::parser::constants::{Associativity, BINARY_OPERATORS, UNARY_OPERATORS};
use crate::parser::parser::Parser;
use crate::parser::unary::UnaryNodeBehaviour::CompareWithReference;
use crate::parser::{NodeMetadata, ParserResult};

#[derive(Debug)]
pub struct Unary;

const ROOT_NODE: Node<'static> = Node::Identifier("$");

impl<'arena, 'token_ref> Parser<'arena, 'token_ref, Unary> {
    pub fn parse(&self) -> ParserResult<'arena> {
        let root = self.root_expression();

        ParserResult {
            root,
            is_complete: self.is_done(),
            metadata: self.node_metadata.clone().map(|t| t.into_inner()),
        }
    }

    fn root_expression(&self) -> &'arena Node<'arena> {
        let mut left_node = self.expression_pair();

        while !self.is_done() {
            let Some(current_token) = self.current() else {
                break;
            };

            let join_operator = match &current_token.kind {
                TokenKind::Operator(Operator::Logical(LogicalOperator::And)) => {
                    Operator::Logical(LogicalOperator::And)
                }
                TokenKind::Operator(Operator::Logical(LogicalOperator::Or))
                | TokenKind::Operator(Operator::Comma) => Operator::Logical(LogicalOperator::Or),
                _ => {
                    return self.error(AstNodeError::Custom {
                        message: self.bump.alloc_str(
                            format!("Invalid join operator `{}`", current_token.kind).as_str(),
                        ),
                        span: current_token.span,
                    })
                }
            };

            self.next();
            let right_node = self.expression_pair();
            left_node = self.node(
                Node::Binary {
                    left: left_node,
                    operator: join_operator,
                    right: right_node,
                },
                |h| NodeMetadata {
                    span: h.span(left_node, right_node).unwrap_or_default(),
                },
            );
        }

        left_node
    }

    fn expression_pair(&self) -> &'arena Node<'arena> {
        let mut left_node = &ROOT_NODE;
        let current_token = self.current();

        if let Some(TokenKind::Operator(Operator::Comparison(_))) = self.current_kind() {
            // Skips
        } else {
            left_node = self.binary_expression(0);
        }

        match self.current_kind() {
            Some(TokenKind::Operator(Operator::Comparison(comparison))) => {
                self.next();
                let right_node = self.binary_expression(0);
                left_node = self.node(
                    Node::Binary {
                        left: left_node,
                        operator: Operator::Comparison(*comparison),
                        right: right_node,
                    },
                    |h| NodeMetadata {
                        span: (
                            current_token.map(|t| t.span.0).unwrap_or_default(),
                            h.metadata(right_node).map(|n| n.span.1).unwrap_or_default(),
                        ),
                    },
                );
            }
            _ => {
                let behaviour = UnaryNodeBehaviour::from(left_node);
                match behaviour {
                    CompareWithReference(comparator) => {
                        left_node = self.node(
                            Node::Binary {
                                left: &ROOT_NODE,
                                operator: Operator::Comparison(comparator),
                                right: left_node,
                            },
                            |h| NodeMetadata {
                                span: (
                                    current_token.map(|t| t.span.0).unwrap_or_default(),
                                    h.metadata(left_node).map(|n| n.span.1).unwrap_or_default(),
                                ),
                            },
                        )
                    }
                    UnaryNodeBehaviour::AsBoolean => {
                        left_node = self.node(
                            Node::BuiltIn {
                                kind: BuiltInFunction::Bool,
                                arguments: self.bump.alloc_slice_clone(&[left_node]),
                            },
                            |h| NodeMetadata {
                                span: (
                                    current_token.map(|t| t.span.0).unwrap_or_default(),
                                    h.metadata(left_node).map(|n| n.span.1).unwrap_or_default(),
                                ),
                            },
                        )
                    }
                }
            }
        }

        left_node
    }

    fn binary_expression(&self, precedence: u8) -> &'arena Node<'arena> {
        let mut node_left = self.unary_expression();
        let Some(mut token) = self.current() else {
            return node_left;
        };

        while let TokenKind::Operator(operator) = &token.kind {
            if self.is_done() {
                break;
            }

            if matches!(
                operator,
                Operator::Comma
                    | Operator::Logical(LogicalOperator::And)
                    | Operator::Logical(LogicalOperator::Or)
            ) {
                break;
            }

            let Some(op) = BINARY_OPERATORS.get(operator) else {
                break;
            };

            if op.precedence < precedence {
                break;
            }

            self.next();
            let node_right = match op.associativity {
                Associativity::Left => self.binary_expression(op.precedence + 1),
                _ => self.binary_expression(op.precedence),
            };

            node_left = self.node(
                Node::Binary {
                    operator: *operator,
                    left: node_left,
                    right: node_right,
                },
                |h| NodeMetadata {
                    span: h.span(node_left, node_right).unwrap_or_default(),
                },
            );

            let Some(t) = self.current() else {
                break;
            };
            token = t;
        }

        if precedence == 0 {
            if let Some(conditional_node) =
                self.conditional(node_left, || self.binary_expression(0))
            {
                node_left = conditional_node;
            }
        }

        node_left
    }

    fn unary_expression(&self) -> &'arena Node<'arena> {
        let Some(token) = self.current() else {
            return self.literal(|| self.binary_expression(0));
        };

        if self.depth() > 0 && token.kind == TokenKind::Identifier(Identifier::CallbackReference) {
            self.next();

            let node = self.node(Node::Pointer, |_| NodeMetadata { span: token.span });
            return self.with_postfix(node, || self.binary_expression(0));
        }

        if let TokenKind::Operator(operator) = &token.kind {
            let Some(unary_operator) = UNARY_OPERATORS.get(operator) else {
                return self.error(AstNodeError::UnexpectedToken {
                    expected: self.bump.alloc_str("UnaryOperator"),
                    received: self.bump.alloc_str(token.kind.to_string().as_str()),
                    span: token.span,
                });
            };

            self.next();
            let expr = self.binary_expression(unary_operator.precedence);
            let node = self.node(
                Node::Unary {
                    operator: *operator,
                    node: expr,
                },
                |h| NodeMetadata {
                    span: (
                        token.span.0,
                        h.metadata(expr).map(|n| n.span.1).unwrap_or_default(),
                    ),
                },
            );

            return node;
        }

        if let Some(interval_node) = self.interval(|| self.binary_expression(0)) {
            return interval_node;
        }

        if token.kind == TokenKind::Bracket(Bracket::LeftParenthesis) {
            let p_start = self.current().map(|s| s.span.0);

            self.next();
            let binary_node = self.binary_expression(0);
            if let Some(error_node) = self.expect(TokenKind::Bracket(Bracket::RightParenthesis)) {
                return error_node;
            };

            let expr = self.node(Node::Parenthesized(binary_node), |_| NodeMetadata {
                span: (p_start.unwrap_or_default(), self.prev_token_end()),
            });

            return self.with_postfix(expr, || self.binary_expression(0));
        }

        self.literal(|| self.binary_expression(0))
    }
}

/// Dictates the behaviour of nodes in unary mode.
/// If `CompareWithReference` is set, node will attempt to make the comparison with the reference,
/// essentially making it (in case of Equal operator) `$ == nodeValue`, or (in case of In operator)
/// `$ in nodeValue`.
///
/// Using `AsBoolean` will cast the nodeValue to boolean and skip comparison with reference ($).
/// You may still use references in such case directly, e.g. `contains($, 'hello')`.
///
/// Rationale behind this is to avoid scenarios where e.g. $ = false and expression is
/// `contains($, 'needle')`. If we didn't ignore the reference, unary expression will be
/// reduced to `$ == contains($, 'needle')` which will be truthy when $ does not
/// contain needle.
#[derive(Debug, PartialEq)]
enum UnaryNodeBehaviour {
    CompareWithReference(ComparisonOperator),
    AsBoolean,
}

impl From<&Node<'_>> for UnaryNodeBehaviour {
    fn from(value: &Node) -> Self {
        use ComparisonOperator::*;
        use UnaryNodeBehaviour::*;

        match value {
            Node::Null => CompareWithReference(Equal),
            Node::Root => CompareWithReference(Equal),
            Node::Bool(_) => CompareWithReference(Equal),
            Node::Number(_) => CompareWithReference(Equal),
            Node::String(_) => CompareWithReference(Equal),
            Node::TemplateString(_) => CompareWithReference(Equal),
            Node::Object(_) => CompareWithReference(Equal),
            Node::Pointer => AsBoolean,
            Node::Array(_) => CompareWithReference(In),
            Node::Identifier(_) => CompareWithReference(Equal),
            Node::Closure(_) => AsBoolean,
            Node::Member { .. } => CompareWithReference(Equal),
            Node::Slice { .. } => CompareWithReference(In),
            Node::Interval { .. } => CompareWithReference(In),
            Node::Conditional {
                on_true, on_false, ..
            } => {
                let a = UnaryNodeBehaviour::from(*on_true);
                let b = UnaryNodeBehaviour::from(*on_false);

                if a == b {
                    a
                } else {
                    CompareWithReference(Equal)
                }
            }
            Node::Unary { node, .. } => UnaryNodeBehaviour::from(*node),
            Node::Parenthesized(n) => UnaryNodeBehaviour::from(*n),
            Node::Binary {
                left,
                operator,
                right,
            } => match operator {
                Operator::Arithmetic(_) => {
                    let a = UnaryNodeBehaviour::from(*left);
                    let b = UnaryNodeBehaviour::from(*right);

                    if a == b {
                        a
                    } else {
                        CompareWithReference(Equal)
                    }
                }
                Operator::Logical(_) => AsBoolean,
                Operator::Comparison(_) => AsBoolean,
                Operator::Range => CompareWithReference(In),
                Operator::Slice => CompareWithReference(In),
                Operator::Comma => AsBoolean,
                Operator::Dot => AsBoolean,
                Operator::QuestionMark => AsBoolean,
            },
            Node::BuiltIn { kind, .. } => match kind {
                BuiltInFunction::Len => CompareWithReference(Equal),
                BuiltInFunction::Upper => CompareWithReference(Equal),
                BuiltInFunction::Lower => CompareWithReference(Equal),
                BuiltInFunction::Abs => CompareWithReference(Equal),
                BuiltInFunction::Sum => CompareWithReference(Equal),
                BuiltInFunction::Avg => CompareWithReference(Equal),
                BuiltInFunction::Min => CompareWithReference(Equal),
                BuiltInFunction::Max => CompareWithReference(Equal),
                BuiltInFunction::Rand => CompareWithReference(Equal),
                BuiltInFunction::Median => CompareWithReference(Equal),
                BuiltInFunction::Mode => CompareWithReference(Equal),
                BuiltInFunction::Floor => CompareWithReference(Equal),
                BuiltInFunction::Ceil => CompareWithReference(Equal),
                BuiltInFunction::Round => CompareWithReference(Equal),
                BuiltInFunction::String => CompareWithReference(Equal),
                BuiltInFunction::Number => CompareWithReference(Equal),
                BuiltInFunction::Bool => CompareWithReference(Equal),
                BuiltInFunction::Date => CompareWithReference(Equal),
                BuiltInFunction::Time => CompareWithReference(Equal),
                BuiltInFunction::Duration => CompareWithReference(Equal),
                BuiltInFunction::Year => CompareWithReference(Equal),
                BuiltInFunction::DayOfWeek => CompareWithReference(Equal),
                BuiltInFunction::DayOfMonth => CompareWithReference(Equal),
                BuiltInFunction::DayOfYear => CompareWithReference(Equal),
                BuiltInFunction::WeekOfYear => CompareWithReference(Equal),
                BuiltInFunction::MonthOfYear => CompareWithReference(Equal),
                BuiltInFunction::MonthString => CompareWithReference(Equal),
                BuiltInFunction::DateString => CompareWithReference(Equal),
                BuiltInFunction::WeekdayString => CompareWithReference(Equal),
                BuiltInFunction::StartOf => CompareWithReference(Equal),
                BuiltInFunction::Count => CompareWithReference(Equal),
                BuiltInFunction::EndOf => CompareWithReference(Equal),
                BuiltInFunction::Flatten => CompareWithReference(In),
                BuiltInFunction::Extract => CompareWithReference(In),
                BuiltInFunction::Filter => CompareWithReference(In),
                BuiltInFunction::Map => CompareWithReference(In),
                BuiltInFunction::FlatMap => CompareWithReference(In),
                BuiltInFunction::Contains => AsBoolean,
                BuiltInFunction::StartsWith => AsBoolean,
                BuiltInFunction::EndsWith => AsBoolean,
                BuiltInFunction::Matches => AsBoolean,
                BuiltInFunction::FuzzyMatch => CompareWithReference(Equal),
                BuiltInFunction::Split => CompareWithReference(In),
                BuiltInFunction::IsNumeric => AsBoolean,
                BuiltInFunction::Keys => CompareWithReference(In),
                BuiltInFunction::Values => CompareWithReference(In),
                BuiltInFunction::All => AsBoolean,
                BuiltInFunction::Some => AsBoolean,
                BuiltInFunction::None => AsBoolean,
                BuiltInFunction::One => AsBoolean,
                BuiltInFunction::Type => CompareWithReference(Equal),
            },
            Node::Error { .. } => AsBoolean,
        }
    }
}
