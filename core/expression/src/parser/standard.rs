use crate::lexer::{Bracket, Identifier, TokenKind};
use crate::parser::ast::{AstNodeError, Node};
use crate::parser::constants::{Associativity, BINARY_OPERATORS, UNARY_OPERATORS};
use crate::parser::parser::Parser;
use crate::parser::result::ParserResult;
use crate::parser::NodeMetadata;

#[derive(Debug)]
pub struct Standard;

impl<'arena, 'token_ref> Parser<'arena, 'token_ref, Standard> {
    pub fn parse(&self) -> ParserResult<'arena> {
        let root = self.binary_expression(0);

        ParserResult {
            root,
            is_complete: self.is_done(),
            metadata: self.node_metadata.clone().map(|t| t.into_inner()),
        }
    }

    #[cfg_attr(feature = "stack-protection", recursive::recursive)]
    fn binary_expression(&self, precedence: u8) -> &'arena Node<'arena> {
        let mut node_left = self.unary_expression();
        let Some(mut token) = self.current() else {
            return node_left;
        };

        while let TokenKind::Operator(operator) = &token.kind {
            if self.is_done() {
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
                self.conditional(node_left, |_| self.binary_expression(0))
            {
                node_left = conditional_node;
            }
        }

        node_left
    }

    fn unary_expression(&self) -> &'arena Node<'arena> {
        let Some(token) = self.current() else {
            return self.error(AstNodeError::Custom {
                message: self.bump.alloc_str("Unexpected end of unary expression"),
                span: (self.prev_token_end(), self.prev_token_end()),
            });
        };

        if self.depth() > 0 && token.kind == TokenKind::Identifier(Identifier::CallbackReference) {
            self.next();

            let node = self.node(Node::Pointer, |_| NodeMetadata { span: token.span });
            return self.with_postfix(node, |_| self.binary_expression(0));
        }

        if let TokenKind::Operator(operator) = &token.kind {
            let Some(unary_operator) = UNARY_OPERATORS.get(operator) else {
                return self.error(AstNodeError::UnexpectedToken {
                    expected: "UnaryOperator",
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

        if let Some(interval_node) = self.interval(|_| self.binary_expression(0)) {
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

            return self.with_postfix(expr, |_| self.binary_expression(0));
        }

        self.literal(|_| self.binary_expression(0))
    }
}
