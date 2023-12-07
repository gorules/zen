use bumpalo::Bump;

use crate::ast::Node;
use crate::lexer::token::{Token, TokenKind};
use crate::parser::definitions::Associativity;
use crate::parser::error::ParserError::{FailedToParse, UnexpectedToken};
use crate::parser::error::ParserResult;
use crate::parser::iter::ParserIterator;
use crate::parser::standard::constants::{BINARY_OPERATORS, UNARY_OPERATORS};

pub(crate) mod constants;

#[derive(Debug)]
pub struct StandardParser<'arena, 'token_ref> {
    iterator: ParserIterator<'arena, 'token_ref>,
    bump: &'arena Bump,
}

impl<'arena, 'token_ref> StandardParser<'arena, 'token_ref> {
    pub fn try_new(tokens: &'token_ref [Token<'arena>], bump: &'arena Bump) -> ParserResult<Self> {
        Ok(Self {
            iterator: ParserIterator::try_new(tokens, bump)?,
            bump,
        })
    }

    pub fn parse(&self) -> ParserResult<&'arena Node<'arena>> {
        let result = self.binary_expression(0)?;
        if !self.iterator.is_done() {
            let token = self.iterator.current();
            return Err(FailedToParse {
                message: format!("Unterminated token {} on {:?}", token.value, token.span),
            });
        }

        return Ok(result);
    }

    fn binary_expression(&self, precedence: u8) -> ParserResult<&'arena Node<'arena>> {
        let mut node_left = self.unary_expression()?;
        let mut token = self.iterator.current();

        while !self.iterator.is_done() {
            if token.kind == TokenKind::Operator {
                if let Some(op) = BINARY_OPERATORS.get(token.value) {
                    if op.precedence >= precedence {
                        self.iterator.next()?;
                        let node_right = match op.associativity {
                            Associativity::Left => self.binary_expression(op.precedence + 1)?,
                            _ => self.binary_expression(op.precedence)?,
                        };

                        node_left = self.iterator.node(Node::Binary {
                            operator: token.value,
                            left: node_left,
                            right: node_right,
                        })?;
                        token = self.iterator.current();
                        continue;
                    }
                }
            }

            break;
        }

        if precedence == 0 {
            if let Some(conditional_node) = self
                .iterator
                .conditional(node_left, || self.binary_expression(0))?
            {
                node_left = conditional_node;
            }
        }

        Ok(node_left)
    }

    fn unary_expression(&self) -> ParserResult<&'arena Node<'arena>> {
        let token = self.iterator.current();
        if token.kind == TokenKind::Operator {
            if let Some(op) = UNARY_OPERATORS.get(token.value) {
                self.iterator.next()?;
                let expr = self.binary_expression(op.precedence)?;
                let node = self.iterator.node(Node::Unary {
                    operator: token.value,
                    node: expr,
                })?;

                return Ok(node);
            }
        }

        if let Some(interval_node) = self.iterator.interval(|| self.binary_expression(0))? {
            return Ok(interval_node);
        }

        if token.kind == TokenKind::Bracket && token.value == "(" {
            self.iterator.next()?;
            let expr = self.binary_expression(0)?;
            self.iterator.expect(TokenKind::Bracket, Some(&[")"]))?;
            return self
                .iterator
                .with_postfix(expr, || self.binary_expression(0));
        }

        if self.iterator.depth() > 0 {
            if token.kind == TokenKind::Operator && (token.value == "#" || token.value == ".") {
                if token.value == "#" {
                    self.iterator.next()?;
                }

                let node = self.iterator.node(Node::Pointer)?;
                return self
                    .iterator
                    .with_postfix(node, || self.binary_expression(0));
            }
        } else if token.kind == TokenKind::Operator && (token.value == "#" || token.value == ".") {
            return Err(UnexpectedToken {
                expected: "anything but Operator(#, .)".to_string(),
                received: format!("{token:?}"),
            });
        }

        self.iterator.literal(|| self.binary_expression(0))
    }
}
