use std::cell::Cell;
use std::fmt::Debug;

use bumpalo::collections::Vec as BumpVec;
use bumpalo::Bump;
use rust_decimal::Decimal;

use crate::ast::Node;
use crate::lexer::token::{Token, TokenKind};
use crate::parser::definitions::Arity;
use crate::parser::error::{ParserError, ParserResult};

type StaticTokenValues = Option<&'static [&'static str]>;

#[derive(Debug)]
pub(crate) struct ParserIterator<'arena, 'token_ref> {
    tokens: &'token_ref [Token<'arena>],
    current: Cell<&'token_ref Token<'arena>>,
    bump: &'arena Bump,
    is_done: Cell<bool>,
    position: Cell<usize>,
    depth: Cell<u8>,
    has_interval: bool,
}

impl<'arena, 'token_ref> ParserIterator<'arena, 'token_ref> {
    pub fn try_new(
        tokens: &'token_ref [Token<'arena>],
        bump: &'arena Bump,
    ) -> Result<Self, ParserError> {
        let current = tokens.get(0).ok_or(ParserError::TokenOutOfBounds)?;
        let has_interval = tokens
            .iter()
            .any(|t| t.kind == TokenKind::Operator && t.value == "..");

        Ok(Self {
            tokens,
            bump,
            has_interval,
            current: Cell::new(current),
            depth: Cell::new(0),
            position: Cell::new(0),
            is_done: Cell::new(false),
        })
    }

    pub fn has_interval(&self) -> bool {
        self.has_interval
    }

    pub fn current(&self) -> &Token<'arena> {
        self.current.get()
    }

    pub fn position(&self) -> usize {
        self.position.get()
    }

    pub fn set_position(&self, position: usize) -> ParserResult<()> {
        let Some(token) = self.tokens.get(position) else {
            return Err(ParserError::TokenOutOfBounds);
        };

        self.position.set(position);
        self.current.set(token);
        Ok(())
    }

    pub fn depth(&self) -> u8 {
        self.depth.get()
    }

    pub fn is_done(&self) -> bool {
        self.is_done.get()
    }

    pub fn next(&self) -> ParserResult<()> {
        self.position.set(self.position.get() + 1);

        if let Some(token) = self.tokens.get(self.position.get()) {
            self.current.set(token);
            Ok(())
        } else {
            if self.is_done.get() {
                return Err(ParserError::TokenOutOfBounds);
            }

            self.is_done.set(true);
            Ok(())
        }
    }

    pub fn expect(&self, kind: TokenKind, values: StaticTokenValues) -> Result<(), ParserError> {
        self.token_cmp(kind, values)?;
        self.next()?;
        Ok(())
    }

    pub fn lookup_back(&self, dx: usize, kind: TokenKind, values: StaticTokenValues) -> bool {
        if self.position.get() < dx {
            return false;
        }

        self.token_cmp_at_bool(self.position.get() - dx, kind, values)
    }

    pub fn number(&self) -> ParserResult<Option<&'arena Node<'arena>>> {
        let Ok(decimal) = Decimal::from_str_exact(self.current().value) else {
            return Ok(None);
        };

        self.next()?;
        self.node(Node::Number(decimal)).map(Some)
    }

    pub fn string(&self) -> ParserResult<Option<&'arena Node<'arena>>> {
        let current_token = self.current();
        if current_token.kind != TokenKind::String {
            return Ok(None);
        }

        self.next()?;
        self.node(Node::String(current_token.value)).map(Some)
    }

    pub fn bool(&self) -> ParserResult<Option<&'arena Node<'arena>>> {
        let current_token = self.current();
        let maybe_bool = match (current_token.value, &current_token.kind) {
            ("true", TokenKind::Identifier) => Some(true),
            ("false", TokenKind::Identifier) => Some(false),
            _ => None,
        };
        let Some(bool_value) = maybe_bool else {
            return Ok(None);
        };

        self.next()?;
        self.node(Node::Bool(bool_value)).map(Some)
    }

    pub fn null(&self) -> ParserResult<Option<&'arena Node<'arena>>> {
        let current_token = self.current();
        if current_token.kind != TokenKind::Identifier || current_token.value != "null" {
            return Ok(None);
        }

        self.next()?;
        self.node(Node::Null).map(Some)
    }

    pub fn node(&self, node: Node<'arena>) -> ParserResult<&'arena Node<'arena>> {
        Ok(self.bump.alloc(node))
    }

    fn token_cmp_at_bool(&self, index: usize, kind: TokenKind, values: StaticTokenValues) -> bool {
        return if let Some(token) = self.tokens.get(index) {
            if token.kind != kind {
                return false;
            }

            if let Some(vals) = values {
                return vals.iter().any(|&c| c == token.value);
            }

            true
        } else {
            false
        };
    }

    fn token_cmp_at(
        &self,
        index: usize,
        kind: TokenKind,
        values: StaticTokenValues,
    ) -> Result<(), ParserError> {
        let token: &Token = self
            .tokens
            .get(index)
            .ok_or(ParserError::TokenOutOfBounds)?;

        if token.kind != kind {
            return Err(ParserError::UnexpectedToken {
                expected: format!("{kind:?} {values:?}"),
                received: format!("{token:?}"),
            });
        }

        if let Some(vals) = values {
            if !vals.iter().any(|c| *c == token.value) {
                return Err(ParserError::UnexpectedToken {
                    expected: format!("{kind:?} {values:?}"),
                    received: format!("{token:?}"),
                });
            }
        }

        Ok(())
    }

    fn token_cmp(&self, kind: TokenKind, values: StaticTokenValues) -> Result<(), ParserError> {
        self.token_cmp_at(self.position.get(), kind, values)
    }

    // Higher level constructs

    pub fn with_postfix<F>(
        &self,
        node: &'arena Node<'arena>,
        expression_parser: F,
    ) -> ParserResult<&'arena Node<'arena>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        let postfix_token = self.current();
        let postfix_kind = PostfixKind::from(postfix_token);

        let processed_token = match postfix_kind {
            PostfixKind::Other => return Ok(node),
            PostfixKind::MemberAccess => {
                self.next()?;
                let property_token = self.current();
                self.next()?;

                if !is_valid_property(property_token) {
                    return Err(ParserError::UnexpectedToken {
                        expected: "member identifier token".to_string(),
                        received: format!("{postfix_token:?}"),
                    });
                }

                let property = self.node(Node::String(property_token.value))?;
                self.node(Node::Member { node, property })
            }
            PostfixKind::PropertyAccess => {
                self.next()?;
                let mut from: Option<&'arena Node<'arena>> = None;
                let mut to: Option<&'arena Node<'arena>> = None;

                let mut c = self.current();
                if c.kind == TokenKind::Operator && c.value == ":" {
                    self.next()?;
                    c = self.current();

                    if c.kind != TokenKind::Bracket && c.value != "]" {
                        to = Some(expression_parser()?);
                    }

                    self.expect(TokenKind::Bracket, Some(&["]"]))?;
                    self.node(Node::Slice { node, to, from })
                } else {
                    from = Some(expression_parser()?);
                    c = self.current();

                    if c.kind == TokenKind::Operator && c.value == ":" {
                        self.next()?;
                        c = self.current();

                        if c.kind != TokenKind::Bracket && c.value != "]" {
                            to = Some(expression_parser()?);
                        }

                        self.expect(TokenKind::Bracket, Some(&["]"]))?;
                        self.node(Node::Slice { node, from, to })
                    } else {
                        // Slice operator [:] was not found,
                        // it should be just an index node.
                        self.expect(TokenKind::Bracket, Some(&["]"]))?;
                        self.node(Node::Member {
                            node,
                            property: from.ok_or(ParserError::MemoryFailure)?,
                        })
                    }
                }
            }
        }?;

        self.with_postfix(processed_token, expression_parser)
    }

    /// Closure
    pub fn closure<F>(&self, expression_parser: F) -> ParserResult<&'arena Node<'arena>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        self.depth.set(self.depth.get() + 1);
        let node = expression_parser()?;
        self.depth.set(self.depth.get() - 1);

        self.node(Node::Closure(node))
    }

    /// Identifier expression
    /// Either <Identifier> or <Identifier Expression>
    pub fn identifier<F>(&self, expression_parser: F) -> ParserResult<Option<&'arena Node<'arena>>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        if self.current().kind != TokenKind::Identifier {
            return Ok(None);
        }

        let identifier_token = self.current();
        self.next()?;
        let current_token = self.current();
        if current_token.kind != TokenKind::Bracket || current_token.value != "(" {
            let identifier_node = self.node(Node::Identifier(identifier_token.value))?;
            return self
                .with_postfix(identifier_node, expression_parser)
                .map(Some);
        }

        // Potentially it might be a built in expression
        let builtin = crate::parser::standard::constants::BUILT_INS
            .get(identifier_token.value)
            .ok_or_else(|| ParserError::UnknownBuiltIn {
                token: identifier_token.value.to_string(),
            })?;

        self.next()?;
        let builtin_node = match builtin.arity {
            Arity::Single => {
                let arg = expression_parser()?;
                self.expect(TokenKind::Bracket, Some(&[")"]))?;

                Node::BuiltIn {
                    name: identifier_token.value,
                    arguments: self.bump.alloc_slice_copy(&[arg]),
                }
            }
            Arity::Dual => {
                let arg1 = expression_parser()?;
                self.expect(TokenKind::Operator, Some(&[","]))?;
                let arg2 = expression_parser()?;
                self.expect(TokenKind::Bracket, Some(&[")"]))?;

                Node::BuiltIn {
                    name: identifier_token.value,
                    arguments: self.bump.alloc_slice_copy(&[arg1, arg2]),
                }
            }
            Arity::Closure => {
                let arg1 = expression_parser()?;
                self.expect(TokenKind::Operator, Some(&[","]))?;
                let arg2 = self.closure(&expression_parser)?;
                self.expect(TokenKind::Bracket, Some(&[")"]))?;

                Node::BuiltIn {
                    name: identifier_token.value,
                    arguments: self.bump.alloc_slice_copy(&[arg1, arg2]),
                }
            }
        };

        self.with_postfix(self.node(builtin_node)?, expression_parser)
            .map(Some)
    }

    /// Interval node
    pub fn interval<F>(&self, expression_parser: F) -> ParserResult<Option<&'arena Node<'arena>>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        // Performance optimisation: skip if expression does not contain an interval for faster evaluation
        if !self.has_interval() {
            return Ok(None);
        }

        if self.current().kind != TokenKind::Bracket {
            return Ok(None);
        }

        let initial_position = self.position();
        let left_bracket = self.current().value;
        if let Err(_) = self.expect(TokenKind::Bracket, None) {
            self.set_position(initial_position)?;
            return Ok(None);
        };

        let Ok(left) = expression_parser() else {
            self.set_position(initial_position)?;
            return Ok(None);
        };

        if let Err(_) = self.expect(TokenKind::Operator, Some(&[".."])) {
            self.set_position(initial_position)?;
            return Ok(None);
        };

        let Ok(right) = expression_parser() else {
            self.set_position(initial_position)?;
            return Ok(None);
        };

        let right_bracket = self.current().value;

        if let Err(_) = self.expect(TokenKind::Bracket, None) {
            self.set_position(initial_position)?;
            return Ok(None);
        };

        let interval_node = self.node(Node::Interval {
            left_bracket,
            left,
            right,
            right_bracket,
        })?;

        self.with_postfix(interval_node, expression_parser)
            .map(Some)
    }

    /// Array nodes
    pub fn array<F>(&self, expression_parser: F) -> ParserResult<Option<&'arena Node<'arena>>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        let current_token = self.current();
        if current_token.kind != TokenKind::Bracket || current_token.value != "[" {
            return Ok(None);
        }

        self.next()?;
        let mut nodes = BumpVec::new_in(self.bump);
        while !(self.current().kind == TokenKind::Bracket && self.current().value == "]") {
            if !nodes.is_empty() {
                self.expect(TokenKind::Operator, Some(&[","]))?;
                if self.current().value == "]" {
                    break;
                }
            }

            nodes.push(expression_parser()?);
        }

        self.expect(TokenKind::Bracket, Some(&["]"]))?;
        let node = Node::Array(nodes.into_bump_slice());

        self.with_postfix(self.node(node)?, expression_parser)
            .map(Some)
    }

    /// Conditional
    /// condition_node ? on_true : on_false
    pub fn conditional<F>(
        &self,
        condition: &'arena Node<'arena>,
        expression_parser: F,
    ) -> ParserResult<Option<&'arena Node<'arena>>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        let current_token = self.current();
        if current_token.kind != TokenKind::Operator || current_token.value != "?" {
            return Ok(None);
        }

        self.next()?;

        let on_true = expression_parser()?;
        self.expect(TokenKind::Operator, Some(&[":"]))?;
        let on_false = expression_parser()?;

        let conditional_node = Node::Conditional {
            condition,
            on_true,
            on_false,
        };

        self.node(conditional_node).map(Some)
    }

    /// Literal - number, string, array etc.
    pub fn literal<F>(&self, expression_parser: F) -> ParserResult<&'arena Node<'arena>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        let current_token = self.current();

        match current_token.kind {
            TokenKind::Identifier => self
                .bool()
                .transpose()
                .or_else(|| self.null().transpose())
                .or_else(|| self.identifier(&expression_parser).transpose())
                .transpose()?
                .ok_or_else(|| ParserError::FailedToParse {
                    message: format!("failed to parse identifier: {:?}", current_token),
                }),
            TokenKind::Number => self.number()?.ok_or_else(|| ParserError::FailedToParse {
                message: format!("failed to parse number: {:?}", current_token),
            }),
            TokenKind::String => self.string()?.ok_or_else(|| ParserError::FailedToParse {
                message: format!("failed to parse string: {:?}", current_token),
            }),
            TokenKind::Bracket => self
                .array(&expression_parser)
                .transpose()
                .or_else(|| self.interval(&expression_parser).transpose())
                .transpose()?
                .ok_or_else(|| ParserError::FailedToParse {
                    message: format!("unexpected bracket: {:?}", current_token),
                }),
            TokenKind::Operator => Err(ParserError::FailedToParse {
                message: format!("unexpected literal token: {:?}", current_token),
            }),
        }
    }
}

fn is_valid_property(token: &Token) -> bool {
    match token.kind {
        TokenKind::Identifier => true,
        TokenKind::Operator => matches!(token.value, "and" | "or" | "in" | "not"),
        _ => false,
    }
}

#[derive(Debug)]
enum PostfixKind {
    MemberAccess,
    PropertyAccess,
    Other,
}

impl From<&Token<'_>> for PostfixKind {
    fn from(token: &Token) -> Self {
        match (&token.kind, token.value) {
            (TokenKind::Bracket, "[") => Self::PropertyAccess,
            (TokenKind::Operator, ".") => Self::MemberAccess,
            _ => Self::Other,
        }
    }
}
