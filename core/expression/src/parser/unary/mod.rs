use bumpalo::Bump;

use crate::ast::Node;
use crate::lexer::token::{Token, TokenKind};
use crate::parser::definitions::{Arity, Associativity};
use crate::parser::error::{ParserError, ParserResult};
use crate::parser::iter::ParserIterator;
use crate::parser::unary::constants::{BUILT_INS, STANDARD_OPERATORS};

mod constants;

pub struct UnaryParser<'a, 'b>
where
    'b: 'a,
{
    iterator: ParserIterator<'a, 'b>,
    bump: &'b Bump,
}

const MAIN_NODE: Node<'static> = Node::Identifier("$");

impl<'a, 'b> UnaryParser<'a, 'b>
where
    'b: 'a,
{
    pub fn try_new(tokens: &'a Vec<Token>, bump: &'b Bump) -> ParserResult<Self> {
        Ok(Self {
            iterator: ParserIterator::try_new(tokens, bump)?,
            bump,
        })
    }

    pub fn parse(&self) -> ParserResult<&'b Node<'b>> {
        self.expression(0, true)
    }

    fn expression(&self, precedence: u8, root: bool) -> ParserResult<&'b Node<'b>> {
        let mut node_left: &'b Node<'b> = self.iterator.node(MAIN_NODE)?;
        let mut token = self.iterator.current();

        while !self.iterator.is_done() {
            match token.kind {
                TokenKind::Operator => {
                    if token.value == "," {
                        self.iterator.next()?;
                        let node_right: &'b Node<'b> = self.expression(0, true)?;

                        token = self.iterator.current();
                        node_left = self.iterator.node(Node::Binary {
                            left: node_left,
                            operator: "or",
                            right: node_right,
                        })?;
                    } else if let Some(op) = STANDARD_OPERATORS.get(token.value) {
                        if op.precedence >= precedence {
                            self.iterator.next()?;
                            let node_right = match op.associativity {
                                Associativity::Left => self.expression(op.precedence + 1, false)?,
                                Associativity::Right => self.expression(op.precedence, false)?,
                            };

                            node_left = self.iterator.node(Node::Binary {
                                left: node_left,
                                operator: self.iterator.str_value(token.value),
                                right: node_right,
                            })?;
                            token = self.iterator.current();
                        }
                    } else {
                        return Err(ParserError::FailedToParse {
                            message: format!(
                                "Unexpected operator {} on {:?}",
                                token.value, token.span
                            ),
                        });
                    }
                }
                TokenKind::Identifier | TokenKind::String | TokenKind::Number => {
                    let node_right = self.literal()?;
                    if !root {
                        return Ok(node_right);
                    }

                    token = self.iterator.current();
                    node_left = self.iterator.node(Node::Binary {
                        left: node_left,
                        operator: "==",
                        right: node_right,
                    })?;
                }
                TokenKind::Bracket => {
                    let node_right: &Node;

                    if let Some(interval) = self.interval(node_left)? {
                        node_left = interval;
                    } else if token.value == "[" {
                        let should_wrap = !self.iterator.lookup_back(
                            1,
                            TokenKind::Operator,
                            Some(&["not in", "in"]),
                        );
                        node_right = self.array(token)?;

                        if should_wrap {
                            node_left = self.iterator.node(Node::Binary {
                                left: node_left,
                                right: node_right,
                                operator: "in",
                            })?;
                        } else {
                            node_left = node_right;
                        }
                    } else {
                        return Err(ParserError::FailedToParse {
                            message: format!(
                                "Unexpected bracket {} on {:?}",
                                token.value, token.span
                            ),
                        });
                    }
                }
            }
        }

        Ok(node_left)
    }

    fn interval(&self, node: &'b Node<'b>) -> ParserResult<Option<&'b Node<'b>>> {
        // Performance optimisation: skip if expression does not contain an interval for faster evaluation
        if !self.iterator.has_interval() {
            return Ok(None);
        }

        let current_token = self.iterator.current();
        if current_token.kind != TokenKind::Bracket {
            return Ok(None);
        }

        let initial_position = self.iterator.position();
        let should_wrap =
            !self
                .iterator
                .lookup_back(1, TokenKind::Operator, Some(&["not in", "in"]));

        let left_bracket = self.iterator.current().value;
        if let Err(_) = self.iterator.expect(TokenKind::Bracket, None) {
            self.iterator.set_position(initial_position)?;
            return Ok(None);
        }

        let Ok(left) = self.literal() else {
            self.iterator.set_position(initial_position)?;
            return Ok(None);
        };

        if let Err(_) = self.iterator.expect(TokenKind::Operator, Some(&[".."])) {
            self.iterator.set_position(initial_position)?;
            return Ok(None);
        }

        let Ok(right) = self.literal() else {
            self.iterator.set_position(initial_position)?;
            return Ok(None);
        };

        let right_bracket = self.iterator.current().value;

        if let Err(_) = self.iterator.expect(TokenKind::Bracket, None) {
            self.iterator.set_position(initial_position)?;
            return Ok(None);
        }

        let interval_node = self.iterator.node(Node::Interval {
            left,
            left_bracket: self.iterator.str_value(left_bracket),
            right,
            right_bracket: self.iterator.str_value(right_bracket),
        })?;

        if should_wrap {
            return Ok(Some(self.iterator.node(Node::Binary {
                left: node,
                right: interval_node,
                operator: "in",
            })?));
        }

        Ok(Some(interval_node))
    }

    fn array(&self, _token: &Token) -> ParserResult<&'b Node<'b>> {
        let mut nodes = Vec::new();

        self.iterator.expect(TokenKind::Bracket, Some(&["["]))?;
        while self.iterator.current().kind != TokenKind::Bracket
            && self.iterator.current().value != "]"
        {
            if !nodes.is_empty() {
                self.iterator.expect(TokenKind::Operator, Some(&[","]))?;
                if self.iterator.current().value == "]" {
                    break;
                }
            }

            nodes.push(self.literal()?);
        }

        self.iterator.expect(TokenKind::Bracket, Some(&["]"]))?;
        let node = Node::Array(self.bump.alloc_slice_copy(nodes.as_slice()));
        self.iterator.node(node)
    }

    fn literal(&self) -> ParserResult<&'b Node<'b>> {
        let token = self.iterator.current();

        match token.kind {
            TokenKind::Identifier => {
                self.iterator.next()?;
                match token.value {
                    "true" | "false" => self.iterator.bool(token),
                    "null" => self.iterator.null(token),
                    _ => self.builtin(token),
                }
            }
            TokenKind::Number => self.iterator.number(token),
            TokenKind::String => self.iterator.string(token),
            _ => Err(ParserError::UnexpectedToken {
                expected: "one of [identifier, number, string]".to_string(),
                received: format!("{:?}", token.kind),
            }),
        }
    }

    fn builtin(&self, token: &Token<'a>) -> ParserResult<&'b Node<'b>> {
        let current_token = self.iterator.current();
        let valid_token = current_token.kind == TokenKind::Bracket && current_token.value == "(";

        if !valid_token {
            return Err(ParserError::UnknownBuiltIn {
                token: token.value.to_string(),
            });
        }

        let built_in = BUILT_INS
            .get(token.value)
            .ok_or_else(|| ParserError::UnknownBuiltIn {
                token: token.value.to_string(),
            })?;

        self.iterator.expect(TokenKind::Bracket, Some(&["("]))?;

        match built_in.arity {
            Arity::Single => {
                let arg = self.literal()?;
                let node = self.iterator.node(Node::BuiltIn {
                    name: self.iterator.str_value(token.value),
                    arguments: self.bump.alloc_slice_copy(&[arg]),
                })?;

                self.iterator.expect(TokenKind::Bracket, Some(&[")"]))?;

                Ok(node)
            }
            _ => Err(ParserError::UnsupportedBuiltIn {
                token: token.value.to_string(),
            }),
        }
    }
}
