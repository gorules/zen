use crate::lexer::{Bracket, Identifier, TokenKind};
use crate::parser::ast::Node;
use crate::parser::constants::{Associativity, BINARY_OPERATORS, UNARY_OPERATORS};
use crate::parser::error::ParserError::{FailedToParse, UnexpectedToken};
use crate::parser::error::ParserResult;
use crate::parser::parser::Parser;

#[derive(Debug)]
pub struct Standard;

impl<'arena, 'token_ref> Parser<'arena, 'token_ref, Standard> {
    pub fn parse(&self) -> ParserResult<&'arena Node<'arena>> {
        let result = self.binary_expression(0)?;
        if !self.is_done() {
            let token = self.current();
            return Err(FailedToParse {
                message: format!("Unterminated token {}", token.value),
                span: token.span,
            });
        }

        return Ok(result);
    }

    fn binary_expression(&self, precedence: u8) -> ParserResult<&'arena Node<'arena>> {
        let mut node_left = self.unary_expression()?;
        let mut token = self.current();

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

            self.next()?;
            let node_right = match op.associativity {
                Associativity::Left => self.binary_expression(op.precedence + 1)?,
                _ => self.binary_expression(op.precedence)?,
            };

            node_left = self.node(Node::Binary {
                operator: *operator,
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
                    expected: "UnaryOperator".to_string(),
                    received: token.kind.to_string(),
                    span: token.span,
                });
            };

            self.next()?;
            let expr = self.binary_expression(unary_operator.precedence)?;
            let node = self.node(Node::Unary {
                operator: *operator,
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
