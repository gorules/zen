use std::cell::Cell;

use bumpalo::Bump;

use crate::ast::Node;
use crate::lexer::token::{Token, TokenKind};
use crate::parser::definitions::{Arity, Associativity};
use crate::parser::error::ParserError::{
    FailedToParse, MemoryFailure, UnexpectedToken, UnknownBuiltIn,
};
use crate::parser::error::ParserResult;
use crate::parser::iter::ParserIterator;
use crate::parser::standard::constants::{BINARY_OPERATORS, BUILT_INS, UNARY_OPERATORS};

mod constants;

#[derive(Debug)]
pub struct StandardParser<'a, 'b>
where
    'b: 'a,
{
    iterator: ParserIterator<'a, 'b>,
    bump: &'b Bump,
    depth: Cell<u8>,
}

impl<'a, 'b> StandardParser<'a, 'b>
where
    'b: 'a,
{
    pub fn try_new(tokens: &'a Vec<Token>, bump: &'b Bump) -> ParserResult<Self> {
        Ok(Self {
            iterator: ParserIterator::try_new(tokens, bump)?,
            bump,
            depth: Cell::new(0),
        })
    }

    pub fn parse(&self) -> ParserResult<&'b Node<'b>> {
        let result = self.expression(0)?;
        if !self.iterator.is_done() {
            let token = self.iterator.current();
            return Err(FailedToParse {
                message: format!("Unterminated token {} on {:?}", token.value, token.span),
            });
        }

        return Ok(result);
    }

    fn expression(&self, precedence: u8) -> ParserResult<&'b Node<'b>> {
        let mut node_left = self.primary_expression()?;
        let mut token = self.iterator.current();

        while !self.iterator.is_done() {
            if token.kind == TokenKind::Operator {
                if let Some(op) = BINARY_OPERATORS.get(token.value) {
                    if op.precedence >= precedence {
                        self.iterator.next()?;
                        let node_right = match op.associativity {
                            Associativity::Left => self.expression(op.precedence + 1)?,
                            _ => self.expression(op.precedence)?,
                        };

                        node_left = self.iterator.node(Node::Binary {
                            operator: self.iterator.str_value(token.value),
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
            node_left = self.conditional(node_left)?;
        }

        Ok(node_left)
    }

    fn primary_expression(&self) -> ParserResult<&'b Node<'b>> {
        let token = self.iterator.current();
        if token.kind == TokenKind::Operator {
            if let Some(op) = UNARY_OPERATORS.get(token.value) {
                self.iterator.next()?;
                let expr = self.expression(op.precedence)?;
                let node = self.iterator.node(Node::Unary {
                    operator: self.iterator.str_value(token.value),
                    node: expr,
                })?;

                return self.postfix(node);
            }
        }

        if let Some(interval_node) = self.interval()? {
            return self.postfix(interval_node);
        }

        if token.kind == TokenKind::Bracket && token.value == "(" {
            self.iterator.next()?;
            let expr = self.expression(0)?;
            self.iterator.expect(TokenKind::Bracket, Some(&[")"]))?;
            return self.postfix(expr);
        }

        if self.depth.get() > 0 {
            if token.kind == TokenKind::Operator && (token.value == "#" || token.value == ".") {
                if token.value == "#" {
                    self.iterator.next()?;
                }
                let node = self.iterator.node(Node::Pointer)?;
                return self.postfix(node);
            }
        } else if token.kind == TokenKind::Operator && (token.value == "#" || token.value == ".") {
            return Err(UnexpectedToken {
                expected: "anything but Operator(#, .)".to_string(),
                received: format!("{token:?}"),
            });
        }

        self.literal()
    }

    fn conditional(&self, node: &'b Node<'b>) -> ParserResult<&'b Node<'b>> {
        let mut nd = self.iterator.node(node.clone())?;
        let mut expr1: &'b Node;
        let mut expr2: &'b Node;

        while self.iterator.current().kind == TokenKind::Operator
            && self.iterator.current().value == "?"
        {
            self.iterator.next()?;

            let token = self.iterator.current();
            if token.kind != TokenKind::Operator && token.value != ":" {
                expr1 = self.expression(0)?;
                self.iterator.expect(TokenKind::Operator, Some(&[":"]))?;
                expr2 = self.expression(0)?;
            } else {
                self.iterator.next()?;
                expr1 = node;
                expr2 = self.expression(0)?;
            }

            nd = self.iterator.node(Node::Conditional {
                condition: nd,
                on_true: expr1,
                on_false: expr2,
            })?;
        }

        Ok(nd)
    }

    fn literal(&self) -> ParserResult<&'b Node<'b>> {
        let node: &'b Node;
        let token = self.iterator.current();

        match token.kind {
            TokenKind::Identifier => {
                self.iterator.next()?;
                match token.value {
                    "true" | "false" => return self.iterator.bool(token),
                    "null" => return self.iterator.null(token),
                    _ => node = self.identifier_expression(token)?,
                }
            }
            TokenKind::Number => return self.iterator.number(token),
            TokenKind::String => return self.iterator.string(token),
            _ => {
                if token.kind == TokenKind::Bracket && token.value == "[" {
                    node = self.array(token)?;
                } else {
                    return Err(UnexpectedToken {
                        expected: "identifier, string, number or opening bracket".to_string(),
                        received: format!("{token:?}"),
                    });
                }
            }
        }

        self.postfix(node)
    }

    fn interval(&self) -> ParserResult<Option<&'b Node<'b>>> {
        // Performance optimisation: skip if expression does not contain an interval for faster evaluation
        if !self.iterator.has_interval() {
            return Ok(None);
        }

        if self.iterator.current().kind != TokenKind::Bracket {
            return Ok(None);
        }

        let initial_position = self.iterator.position();
        let left_bracket = self.iterator.current().value;
        if let Err(_) = self.iterator.expect(TokenKind::Bracket, None) {
            self.iterator.set_position(initial_position)?;
            return Ok(None);
        };

        let Ok(left) = self.primary_expression() else {
            self.iterator.set_position(initial_position)?;
            return Ok(None);
        };

        if let Err(_) = self.iterator.expect(TokenKind::Operator, Some(&[".."])) {
            self.iterator.set_position(initial_position)?;
            return Ok(None);
        };

        let Ok(right) = self.primary_expression() else {
            self.iterator.set_position(initial_position)?;
            return Ok(None);
        };

        let right_bracket = self.iterator.current().value;

        if let Err(_) = self.iterator.expect(TokenKind::Bracket, None) {
            self.iterator.set_position(initial_position)?;
            return Ok(None);
        };

        let interval_node = self.iterator.node(Node::Interval {
            left_bracket: self.iterator.str_value(left_bracket),
            left,
            right,
            right_bracket: self.iterator.str_value(right_bracket),
        })?;

        Ok(Some(interval_node))
    }

    fn identifier_expression(&self, token: &Token) -> ParserResult<&'b Node<'b>> {
        if self.iterator.current().kind != TokenKind::Bracket
            || self.iterator.current().value != "("
        {
            return self
                .iterator
                .node(Node::Identifier(self.iterator.str_value(token.value)));
        }

        let builtin = BUILT_INS.get(token.value).ok_or_else(|| UnknownBuiltIn {
            token: token.value.to_string(),
        })?;

        self.iterator.expect(TokenKind::Bracket, Some(&["("]))?;

        return match builtin.arity {
            Arity::Single => {
                let arg = self.expression(0)?;
                self.iterator.expect(TokenKind::Bracket, Some(&[")"]))?;

                self.iterator.node(Node::BuiltIn {
                    name: self.iterator.str_value(token.value),
                    arguments: self.bump.alloc_slice_copy(&[arg]),
                })
            }
            Arity::Dual => {
                let arg1 = self.expression(0)?;
                self.iterator.expect(TokenKind::Operator, Some(&[","]))?;
                let arg2 = self.expression(0)?;
                self.iterator.expect(TokenKind::Bracket, Some(&[")"]))?;

                self.iterator.node(Node::BuiltIn {
                    name: self.iterator.str_value(token.value),
                    arguments: self.bump.alloc_slice_copy(&[arg1, arg2]),
                })
            }
            Arity::Closure => {
                let arg1 = self.expression(0)?;
                self.iterator.expect(TokenKind::Operator, Some(&[","]))?;
                let arg2 = self.closure()?;
                self.iterator.expect(TokenKind::Bracket, Some(&[")"]))?;

                self.iterator.node(Node::BuiltIn {
                    name: self.iterator.str_value(token.value),
                    arguments: self.bump.alloc_slice_copy(&[arg1, arg2]),
                })
            }
        };
    }

    fn array(&self, _token: &Token) -> ParserResult<&'b Node<'b>> {
        let mut nodes = Vec::new();
        self.iterator.expect(TokenKind::Bracket, Some(&["["]))?;

        while !(self.iterator.current().kind == TokenKind::Bracket
            && self.iterator.current().value == "]")
        {
            if !nodes.is_empty() {
                self.iterator.expect(TokenKind::Operator, Some(&[","]))?;
                if self.iterator.current().value == "]" {
                    break;
                }
            }

            nodes.push(self.expression(0)?);
        }

        self.iterator.expect(TokenKind::Bracket, Some(&["]"]))?;
        let node = Node::Array(self.bump.alloc_slice_copy(nodes.as_slice()));

        self.iterator.node(node)
    }

    fn closure(&self) -> ParserResult<&'b Node<'b>> {
        self.depth.set(self.depth.get() + 1);
        let node = self.expression(0)?;
        self.depth.set(self.depth.get() - 1);

        return self.iterator.node(Node::Closure(node));
    }

    fn postfix(&self, node: &'b Node<'b>) -> ParserResult<&'b Node<'b>> {
        let mut postfix_token = self.iterator.current();
        let mut nd = self.iterator.node(node.clone())?;

        while postfix_token.kind == TokenKind::Bracket || postfix_token.kind == TokenKind::Operator
        {
            if postfix_token.value == "." {
                self.iterator.next()?;
                let property_token = self.iterator.current();
                self.iterator.next()?;

                if property_token.kind != TokenKind::Identifier
                    && (property_token.kind != TokenKind::Operator
                        || !is_valid_identifier(property_token.value))
                {
                    return Err(UnexpectedToken {
                        expected: "member identifier token".to_string(),
                        received: format!("{postfix_token:?}"),
                    });
                }

                let property = self
                    .iterator
                    .node(Node::String(self.iterator.str_value(property_token.value)))?;
                nd = self.iterator.node(Node::Member { node: nd, property })?;
            } else if postfix_token.value == "[" {
                self.iterator.next()?;
                let mut from: Option<&'b Node<'b>> = None;
                let mut to: Option<&'b Node<'b>> = None;

                let mut c = self.iterator.current();
                if c.kind == TokenKind::Operator && c.value == ":" {
                    self.iterator.next()?;
                    c = self.iterator.current();

                    if c.kind != TokenKind::Bracket && c.value != "]" {
                        to = Some(self.expression(0)?);
                    }

                    nd = self.iterator.node(Node::Slice { node: nd, to, from })?;
                    self.iterator.expect(TokenKind::Bracket, Some(&["]"]))?;
                } else {
                    from = Some(self.expression(0)?);
                    c = self.iterator.current();

                    if c.kind == TokenKind::Operator && c.value == ":" {
                        self.iterator.next()?;
                        c = self.iterator.current();

                        if c.kind != TokenKind::Bracket && c.value != "]" {
                            to = Some(self.expression(0)?);
                        }

                        nd = self.iterator.node(Node::Slice { node: nd, from, to })?;
                        self.iterator.expect(TokenKind::Bracket, Some(&["]"]))?;
                    } else {
                        // Slice operator [:] was not found,
                        // it should be just an index node.
                        nd = self.iterator.node(Node::Member {
                            node: nd,
                            property: from.ok_or(MemoryFailure)?,
                        })?;
                        self.iterator.expect(TokenKind::Bracket, Some(&["]"]))?;
                    }
                }
            } else {
                break;
            }

            postfix_token = self.iterator.current();
        }

        Ok(nd)
    }
}

fn is_valid_identifier(str: &str) -> bool {
    matches!(str, "and" | "or" | "in" | "not")
}
