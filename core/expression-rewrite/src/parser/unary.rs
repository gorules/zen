use crate::ast::Node;
use crate::lexer::token::{
    Bracket, ComparisonOperator, Identifier, LogicalOperator, Operator, TokenKind,
};
use crate::parser::builtin::BuiltInFunction;
use crate::parser::constants::{Associativity, BINARY_OPERATORS, UNARY_OPERATORS};
use crate::parser::error::ParserError::{FailedToParse, UnexpectedToken};
use crate::parser::error::{ParserError, ParserResult};
use crate::parser::parser::Parser;
use crate::parser::unary::UnaryNodeBehaviour::CompareWithReference;

/// Unary evaluation (or Unary-test evaluation) is used for evaluating expressions to a boolean
/// value. It's mostly used DecisionTable cells, as it allows for easy to read expressions which
/// excel at performance and readability.
///
/// Unary expressions rely on the context reference ($) - and context reference serves as a value
/// which we are testing.
///
/// Some examples of valid expressions (supposing e.g. $ = 50):
/// ```js
/// // For $ = 50
/// >= 50 or < 100         // evaluates to true
/// [50..150)              // evaluates to true
/// (50..150)              // evaluates to true
///
/// // For $ = 'hello'
/// 'hi', 'hello'          // evaluates to true
/// startsWith($, 'he')    // evaluates to true
/// endsWith($, 'he')      // evaluates to false
/// ```
#[derive(Debug)]
pub struct Unary;

const ROOT_NODE: Node<'static> = Node::Identifier("$");

impl<'arena, 'token_ref> Parser<'arena, 'token_ref, Unary> {
    pub fn parse(&self) -> ParserResult<&'arena Node<'arena>> {
        let result = self.root_expression()?;
        if !self.is_done() {
            let token = self.current();
            return Err(FailedToParse {
                message: format!("Unterminated token {} on {:?}", token.value, token.span),
            });
        }

        return Ok(result);
    }

    fn root_expression(&self) -> ParserResult<&'arena Node<'arena>> {
        let mut left_node = self.expression_pair()?;
        while !self.is_done() {
            let current_token = self.current();
            let join_operator = match &current_token.kind {
                TokenKind::Operator(Operator::Logical(LogicalOperator::And)) => {
                    Operator::Logical(LogicalOperator::And)
                }
                TokenKind::Operator(Operator::Logical(LogicalOperator::Or))
                | TokenKind::Operator(Operator::Comma) => Operator::Logical(LogicalOperator::Or),
                _ => {
                    return Err(ParserError::MemoryFailure);
                }
            };

            self.next()?;
            let right_node = self.expression_pair()?;
            left_node = self.node(Node::Binary {
                left: left_node,
                operator: join_operator,
                right: right_node,
            });
        }

        Ok(left_node)
    }

    fn expression_pair(&self) -> ParserResult<&'arena Node<'arena>> {
        let mut left_node = &ROOT_NODE;
        let initial_token = self.current();
        if let TokenKind::Operator(Operator::Comparison(_)) = &initial_token.kind {
            // Skips
        } else {
            left_node = self.binary_expression(0)?;
        }

        let current_token = self.current();
        match &current_token.kind {
            TokenKind::Operator(Operator::Comparison(comparison)) => {
                self.next()?;
                let right_node = self.binary_expression(0)?;
                left_node = self.node(Node::Binary {
                    left: left_node,
                    operator: Operator::Comparison(comparison.clone()),
                    right: right_node,
                });
            }
            _ => {
                let behaviour = UnaryNodeBehaviour::from(left_node);
                match behaviour {
                    CompareWithReference(comparator) => {
                        left_node = self.node(Node::Binary {
                            left: &ROOT_NODE,
                            operator: Operator::Comparison(comparator),
                            right: left_node,
                        })
                    }
                    UnaryNodeBehaviour::AsBoolean => {
                        left_node = self.node(Node::BuiltIn {
                            kind: BuiltInFunction::Bool,
                            arguments: self.bump.alloc_slice_clone(&[left_node]),
                        })
                    }
                }
            }
        }

        Ok(left_node)
    }

    fn binary_expression(&self, precedence: u8) -> ParserResult<&'arena Node<'arena>> {
        let mut node_left = self.unary_expression()?;
        let mut token = self.current();

        while let TokenKind::Operator(operator) = &token.kind {
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

            self.next()?;
            let node_right = match op.associativity {
                Associativity::Left => self.binary_expression(op.precedence + 1)?,
                _ => self.binary_expression(op.precedence)?,
            };

            node_left = self.node(Node::Binary {
                operator: operator.clone(),
                left: node_left,
                right: node_right,
            });
            token = self.current();
        }

        if precedence == 0 {
            if let Some(conditional_node) =
                self.conditional(node_left, || self.binary_expression(0))?
            {
                node_left = conditional_node;
            }
        }

        Ok(node_left)
    }

    fn unary_expression(&self) -> ParserResult<&'arena Node<'arena>> {
        let token = self.current();

        if self.depth() > 0 && token.kind == TokenKind::Identifier(Identifier::CallbackReference) {
            self.next()?;

            let node = self.node(Node::Pointer);
            return self.with_postfix(node, || self.binary_expression(0));
        }

        if let TokenKind::Operator(operator) = &token.kind {
            let Some(unary_operator) = UNARY_OPERATORS.get(operator) else {
                return Err(UnexpectedToken {
                    expected: "Unary token".to_string(),
                    received: format!("{token:?}"),
                });
            };

            self.next()?;
            let expr = self.binary_expression(unary_operator.precedence)?;
            let node = self.node(Node::Unary {
                operator: operator.clone(),
                node: expr,
            });

            return Ok(node);
        }

        if let Some(interval_node) = self.interval(|| self.binary_expression(0))? {
            return Ok(interval_node);
        }

        if token.kind == TokenKind::Bracket(Bracket::LeftParenthesis) {
            self.next()?;
            let expr = self.binary_expression(0)?;
            self.expect(TokenKind::Bracket(Bracket::RightParenthesis))?;
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
            Node::Bool(_) => CompareWithReference(Equal),
            Node::Number(_) => CompareWithReference(Equal),
            Node::String(_) => CompareWithReference(Equal),
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

                return if a == b {
                    a
                } else {
                    CompareWithReference(Equal)
                };
            }
            Node::Unary { node, .. } => UnaryNodeBehaviour::from(*node),
            Node::Binary {
                left,
                operator,
                right,
            } => match operator {
                Operator::Arithmetic(_) => {
                    let a = UnaryNodeBehaviour::from(*left);
                    let b = UnaryNodeBehaviour::from(*right);

                    return if a == b {
                        a
                    } else {
                        CompareWithReference(Equal)
                    };
                }
                Operator::Logical(_) => AsBoolean,
                Operator::Comparison(_) => AsBoolean,
                Operator::Range => CompareWithReference(In),
                Operator::Slice => CompareWithReference(In),
                Operator::Comma => AsBoolean,
                Operator::Dot => AsBoolean,
                Operator::QuestionMark => AsBoolean,
            },
            Node::BuiltIn { .. } => AsBoolean,
        }
    }
}
