use crate::functions::{
    ClosureFunction, DateMethod, DeprecatedFunction, FunctionKind, InternalFunction, MethodKind,
};
use crate::lexer::{Bracket, ComparisonOperator, Identifier, LogicalOperator, Operator, TokenKind};
use crate::parser::ast::{AstNodeError, Node};
use crate::parser::constants::{Associativity, BINARY_OPERATORS, UNARY_OPERATORS};
use crate::parser::parser::{Parser, ParserContext};
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
            left_node = self.binary_expression(0, ParserContext::Global);
        }

        match self.current_kind() {
            Some(TokenKind::Operator(Operator::Comparison(comparison))) => {
                self.next();
                let right_node = self.binary_expression(0, ParserContext::Global);
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
                            Node::FunctionCall {
                                kind: FunctionKind::Internal(InternalFunction::Bool),
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

    #[cfg_attr(feature = "stack-protection", recursive::recursive)]
    fn binary_expression(&self, precedence: u8, ctx: ParserContext) -> &'arena Node<'arena> {
        let mut node_left = self.unary_expression();
        let Some(mut token) = self.current() else {
            return node_left;
        };

        while let TokenKind::Operator(operator) = &token.kind {
            if self.is_done() {
                break;
            }

            if ctx == ParserContext::Global
                && matches!(
                    operator,
                    Operator::Comma
                        | Operator::Logical(LogicalOperator::And)
                        | Operator::Logical(LogicalOperator::Or)
                )
            {
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
                Associativity::Left => {
                    self.binary_expression(op.precedence + 1, ParserContext::Global)
                }
                _ => self.binary_expression(op.precedence, ParserContext::Global),
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
                self.conditional(node_left, |c| self.binary_expression(0, c))
            {
                node_left = conditional_node;
            }
        }

        node_left
    }

    fn unary_expression(&self) -> &'arena Node<'arena> {
        let Some(token) = self.current() else {
            return self.literal(|c| self.binary_expression(0, c));
        };

        if self.depth() > 0 && token.kind == TokenKind::Identifier(Identifier::CallbackReference) {
            self.next();

            let node = self.node(Node::Pointer, |_| NodeMetadata { span: token.span });
            return self.with_postfix(node, |c| self.binary_expression(0, c));
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
            let expr = self.binary_expression(unary_operator.precedence, ParserContext::Global);
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

        if let Some(interval_node) = self.interval(|c| self.binary_expression(0, c)) {
            return interval_node;
        }

        if token.kind == TokenKind::Bracket(Bracket::LeftParenthesis) {
            let p_start = self.current().map(|s| s.span.0);

            self.next();
            let binary_node = self.binary_expression(0, ParserContext::Global);
            if let Some(error_node) = self.expect(TokenKind::Bracket(Bracket::RightParenthesis)) {
                return error_node;
            };

            let expr = self.node(Node::Parenthesized(binary_node), |_| NodeMetadata {
                span: (p_start.unwrap_or_default(), self.prev_token_end()),
            });

            return self.with_postfix(expr, |c| self.binary_expression(0, c));
        }

        self.literal(|c| self.binary_expression(0, c))
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
            Node::FunctionCall { kind, .. } => match kind {
                FunctionKind::Internal(i) => match i {
                    InternalFunction::Len => CompareWithReference(Equal),
                    InternalFunction::Upper => CompareWithReference(Equal),
                    InternalFunction::Lower => CompareWithReference(Equal),
                    InternalFunction::Trim => CompareWithReference(Equal),
                    InternalFunction::Abs => CompareWithReference(Equal),
                    InternalFunction::Sum => CompareWithReference(Equal),
                    InternalFunction::Avg => CompareWithReference(Equal),
                    InternalFunction::Min => CompareWithReference(Equal),
                    InternalFunction::Max => CompareWithReference(Equal),
                    InternalFunction::Rand => CompareWithReference(Equal),
                    InternalFunction::Median => CompareWithReference(Equal),
                    InternalFunction::Mode => CompareWithReference(Equal),
                    InternalFunction::Floor => CompareWithReference(Equal),
                    InternalFunction::Ceil => CompareWithReference(Equal),
                    InternalFunction::Round => CompareWithReference(Equal),
                    InternalFunction::Trunc => CompareWithReference(Equal),
                    InternalFunction::String => CompareWithReference(Equal),
                    InternalFunction::Number => CompareWithReference(Equal),
                    InternalFunction::Bool => CompareWithReference(Equal),
                    InternalFunction::Flatten => CompareWithReference(In),
                    InternalFunction::Extract => CompareWithReference(In),
                    InternalFunction::Contains => AsBoolean,
                    InternalFunction::StartsWith => AsBoolean,
                    InternalFunction::EndsWith => AsBoolean,
                    InternalFunction::Matches => AsBoolean,
                    InternalFunction::FuzzyMatch => CompareWithReference(Equal),
                    InternalFunction::Split => CompareWithReference(In),
                    InternalFunction::IsNumeric => AsBoolean,
                    InternalFunction::Keys => CompareWithReference(In),
                    InternalFunction::Values => CompareWithReference(In),
                    InternalFunction::Type => CompareWithReference(Equal),
                    InternalFunction::Date => CompareWithReference(Equal),
                },
                FunctionKind::Deprecated(d) => match d {
                    DeprecatedFunction::Date => CompareWithReference(Equal),
                    DeprecatedFunction::Time => CompareWithReference(Equal),
                    DeprecatedFunction::Duration => CompareWithReference(Equal),
                    DeprecatedFunction::Year => CompareWithReference(Equal),
                    DeprecatedFunction::DayOfWeek => CompareWithReference(Equal),
                    DeprecatedFunction::DayOfMonth => CompareWithReference(Equal),
                    DeprecatedFunction::DayOfYear => CompareWithReference(Equal),
                    DeprecatedFunction::WeekOfYear => CompareWithReference(Equal),
                    DeprecatedFunction::MonthOfYear => CompareWithReference(Equal),
                    DeprecatedFunction::MonthString => CompareWithReference(Equal),
                    DeprecatedFunction::DateString => CompareWithReference(Equal),
                    DeprecatedFunction::WeekdayString => CompareWithReference(Equal),
                    DeprecatedFunction::StartOf => CompareWithReference(Equal),
                    DeprecatedFunction::EndOf => CompareWithReference(Equal),
                },
                FunctionKind::Closure(c) => match c {
                    ClosureFunction::All => AsBoolean,
                    ClosureFunction::Some => AsBoolean,
                    ClosureFunction::None => AsBoolean,
                    ClosureFunction::One => AsBoolean,
                    ClosureFunction::Filter => CompareWithReference(In),
                    ClosureFunction::Map => CompareWithReference(In),
                    ClosureFunction::FlatMap => CompareWithReference(In),
                    ClosureFunction::Count => CompareWithReference(Equal),
                },
            },
            Node::MethodCall { kind, .. } => match kind {
                MethodKind::DateMethod(dm) => match dm {
                    DateMethod::Add => CompareWithReference(Equal),
                    DateMethod::Sub => CompareWithReference(Equal),
                    DateMethod::Format => CompareWithReference(Equal),
                    DateMethod::Month => CompareWithReference(Equal),
                    DateMethod::Year => CompareWithReference(Equal),
                    DateMethod::Set => CompareWithReference(Equal),
                    DateMethod::StartOf => CompareWithReference(Equal),
                    DateMethod::EndOf => CompareWithReference(Equal),
                    DateMethod::Diff => CompareWithReference(Equal),
                    DateMethod::Tz => CompareWithReference(Equal),
                    DateMethod::Second => CompareWithReference(Equal),
                    DateMethod::Minute => CompareWithReference(Equal),
                    DateMethod::Hour => CompareWithReference(Equal),
                    DateMethod::Day => CompareWithReference(Equal),
                    DateMethod::DayOfYear => CompareWithReference(Equal),
                    DateMethod::Week => CompareWithReference(Equal),
                    DateMethod::Weekday => CompareWithReference(Equal),
                    DateMethod::Quarter => CompareWithReference(Equal),
                    DateMethod::Timestamp => CompareWithReference(Equal),
                    DateMethod::OffsetName => CompareWithReference(Equal),
                    DateMethod::IsSame => AsBoolean,
                    DateMethod::IsBefore => AsBoolean,
                    DateMethod::IsAfter => AsBoolean,
                    DateMethod::IsSameOrBefore => AsBoolean,
                    DateMethod::IsSameOrAfter => AsBoolean,
                    DateMethod::IsValid => AsBoolean,
                    DateMethod::IsYesterday => AsBoolean,
                    DateMethod::IsToday => AsBoolean,
                    DateMethod::IsTomorrow => AsBoolean,
                    DateMethod::IsLeapYear => AsBoolean,
                },
            },
            Node::Error { .. } => AsBoolean,
        }
    }
}
