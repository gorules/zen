use std::cell::Cell;
use std::fmt::Debug;

use bumpalo::Bump;
use rust_decimal::Decimal;

use crate::ast::Node;
use crate::lexer::token::{Token, TokenKind};
use crate::parser::error::{ParserError, ParserResult};

type StaticTokenValues = Option<&'static [&'static str]>;

#[derive(Debug)]
pub(crate) struct ParserIterator<'arena, 'token_ref> {
    tokens: &'token_ref [Token<'arena>],
    current: Cell<&'token_ref Token<'arena>>,
    position: Cell<usize>,
    bump: &'arena Bump,
    is_done: Cell<bool>,
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

    #[allow(dead_code)]
    pub fn lookup(&self, dx: usize, kind: TokenKind, values: StaticTokenValues) -> bool {
        self.token_cmp_at_bool(self.position.get() + dx, kind, values)
    }

    pub fn lookup_back(&self, dx: usize, kind: TokenKind, values: StaticTokenValues) -> bool {
        if self.position.get() < dx {
            return false;
        }

        self.token_cmp_at_bool(self.position.get() - dx, kind, values)
    }

    pub fn number(&self, token: &Token) -> Result<&'arena Node<'arena>, ParserError> {
        self.next()?;

        let decimal =
            Decimal::from_str_exact(token.value).map_err(|_| ParserError::FailedToParse {
                message: format!("unknown float value: {:?}", token.value),
            })?;

        self.node(Node::Number(decimal))
    }

    pub fn string(&self, token: &Token<'arena>) -> Result<&'arena Node<'arena>, ParserError> {
        self.next()?;
        self.node(Node::String(token.value))
    }

    pub fn bool(&self, token: &Token) -> Result<&'arena Node<'arena>, ParserError> {
        match token.value {
            "true" => self.node(Node::Bool(true)),
            "false" => self.node(Node::Bool(false)),
            _ => Err(ParserError::FailedToParse {
                message: format!("unknown bool value: {:?}", token.value),
            }),
        }
    }

    pub fn null(&self, _token: &Token) -> Result<&'arena Node<'arena>, ParserError> {
        self.node(Node::Null)
    }

    pub fn node(&self, node: Node<'arena>) -> Result<&'arena Node<'arena>, ParserError> {
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
            if !vals.iter().any(|&c| c == token.value) {
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
}
