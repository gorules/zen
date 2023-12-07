use std::cell::Cell;
use std::fmt::Debug;
use std::marker::PhantomData;

use bumpalo::collections::Vec as BumpVec;
use bumpalo::Bump;
use rust_decimal::Decimal;

use crate::ast::Node;
use crate::lexer::token::{Bracket, ComparisonOperator, Identifier, Operator, Token, TokenKind};
use crate::parser::builtin::{Arity, BuiltInFunction};
use crate::parser::error::{ParserError, ParserResult};
use crate::parser::standard::Standard;
use crate::parser::unary::Unary;

#[derive(Debug)]
pub struct BaseParser;

#[derive(Debug)]
pub struct Parser<'arena, 'token_ref, Flavor> {
    tokens: &'token_ref [Token<'arena>],
    current: Cell<&'token_ref Token<'arena>>,
    pub(crate) bump: &'arena Bump,
    is_done: Cell<bool>,
    position: Cell<usize>,
    depth: Cell<u8>,
    marker_flavor: PhantomData<Flavor>,
    has_range_operator: bool,
}

impl<'arena, 'token_ref> Parser<'arena, 'token_ref, BaseParser> {
    pub fn try_new(
        tokens: &'token_ref [Token<'arena>],
        bump: &'arena Bump,
    ) -> Result<Self, ParserError> {
        let current = tokens.get(0).ok_or(ParserError::TokenOutOfBounds)?;
        let has_range_operator = tokens
            .iter()
            .any(|t| t.kind == TokenKind::Operator(Operator::Range));

        Ok(Self {
            tokens,
            bump,
            current: Cell::new(current),
            depth: Cell::new(0),
            position: Cell::new(0),
            is_done: Cell::new(false),
            has_range_operator,
            marker_flavor: PhantomData,
        })
    }

    pub fn standard(self) -> Parser<'arena, 'token_ref, Standard> {
        Parser {
            tokens: self.tokens,
            bump: self.bump,
            current: self.current,
            depth: self.depth,
            position: self.position,
            is_done: self.is_done,
            has_range_operator: self.has_range_operator,
            marker_flavor: PhantomData,
        }
    }

    pub fn unary(self) -> Parser<'arena, 'token_ref, Unary> {
        Parser {
            tokens: self.tokens,
            bump: self.bump,
            current: self.current,
            depth: self.depth,
            position: self.position,
            is_done: self.is_done,
            has_range_operator: self.has_range_operator,
            marker_flavor: PhantomData,
        }
    }
}

impl<'arena, 'token_ref, Flavor> Parser<'arena, 'token_ref, Flavor> {
    pub(crate) fn current(&self) -> &Token<'arena> {
        self.current.get()
    }

    fn position(&self) -> usize {
        self.position.get()
    }

    fn set_position(&self, position: usize) -> ParserResult<()> {
        let Some(token) = self.tokens.get(position) else {
            return Err(ParserError::TokenOutOfBounds);
        };

        self.position.set(position);
        self.current.set(token);
        Ok(())
    }

    pub(crate) fn depth(&self) -> u8 {
        self.depth.get()
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done.get()
    }

    pub(super) fn next(&self) -> ParserResult<()> {
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

    pub(crate) fn expect(&self, kind: TokenKind) -> Result<(), ParserError> {
        let token = self.current();
        if token.kind != kind {
            return Err(ParserError::UnexpectedToken {
                expected: format!("{kind:?}"),
                received: format!("{token:?}"),
            });
        }

        self.next()
    }

    pub(crate) fn number(&self) -> ParserResult<Option<&'arena Node<'arena>>> {
        let Ok(decimal) = Decimal::from_str_exact(self.current().value) else {
            return Ok(None);
        };

        self.next()?;
        Ok(Some(self.node(Node::Number(decimal))))
    }

    pub(crate) fn string(&self) -> ParserResult<Option<&'arena Node<'arena>>> {
        let current_token = self.current();
        if current_token.kind != TokenKind::String {
            return Ok(None);
        }

        self.next()?;
        Ok(Some(self.node(Node::String(current_token.value))))
    }

    pub(crate) fn bool(&self) -> ParserResult<Option<&'arena Node<'arena>>> {
        let current_token = self.current();
        let TokenKind::Boolean(boolean) = current_token.kind else {
            return Ok(None);
        };

        self.next()?;
        Ok(Some(self.node(Node::Bool(boolean))))
    }

    pub(crate) fn null(&self) -> ParserResult<Option<&'arena Node<'arena>>> {
        let current_token = self.current();
        if current_token.kind != TokenKind::Identifier(Identifier::Null) {
            return Ok(None);
        }

        self.next()?;
        Ok(Some(self.node(Node::Null)))
    }

    pub(crate) fn node(&self, node: Node<'arena>) -> &'arena Node<'arena> {
        self.bump.alloc(node)
    }

    // Higher level constructs

    pub(crate) fn with_postfix<F>(
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

                let property = self.node(Node::String(property_token.value));
                Ok(self.node(Node::Member { node, property }))
            }
            PostfixKind::PropertyAccess => {
                self.next()?;
                let mut from: Option<&'arena Node<'arena>> = None;
                let mut to: Option<&'arena Node<'arena>> = None;

                let mut c = self.current();
                if c.kind == TokenKind::Operator(Operator::Slice) {
                    self.next()?;
                    c = self.current();

                    if c.kind != TokenKind::Bracket(Bracket::RightSquareBracket) {
                        to = Some(expression_parser()?);
                    }

                    self.expect(TokenKind::Bracket(Bracket::RightSquareBracket))?;
                    Ok(self.node(Node::Slice { node, to, from }))
                } else {
                    from = Some(expression_parser()?);
                    c = self.current();

                    if c.kind == TokenKind::Operator(Operator::Slice) {
                        self.next()?;
                        c = self.current();

                        if c.kind != TokenKind::Bracket(Bracket::RightSquareBracket) {
                            to = Some(expression_parser()?);
                        }

                        self.expect(TokenKind::Bracket(Bracket::RightSquareBracket))?;
                        Ok(self.node(Node::Slice { node, from, to }))
                    } else {
                        // Slice operator [:] was not found,
                        // it should be just an index node.
                        self.expect(TokenKind::Bracket(Bracket::RightSquareBracket))?;
                        Ok(self.node(Node::Member {
                            node,
                            property: from.ok_or(ParserError::MemoryFailure)?,
                        }))
                    }
                }
            }
        }?;

        self.with_postfix(processed_token, expression_parser)
    }

    /// Closure
    pub(crate) fn closure<F>(&self, expression_parser: F) -> ParserResult<&'arena Node<'arena>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        self.depth.set(self.depth.get() + 1);
        let node = expression_parser()?;
        self.depth.set(self.depth.get() - 1);

        Ok(self.node(Node::Closure(node)))
    }

    /// Identifier expression
    /// Either <Identifier> or <Identifier Expression>
    pub(crate) fn identifier<F>(
        &self,
        expression_parser: F,
    ) -> ParserResult<Option<&'arena Node<'arena>>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        let TokenKind::Identifier(_) = &self.current().kind else {
            return Ok(None);
        };

        let identifier_token = self.current();
        self.next()?;
        let current_token = self.current();
        if current_token.kind != TokenKind::Bracket(Bracket::LeftParenthesis) {
            let identifier_node = self.node(Node::Identifier(identifier_token.value));
            return self
                .with_postfix(identifier_node, expression_parser)
                .map(Some);
        }

        // Potentially it might be a built in expression
        let builtin = BuiltInFunction::try_from(identifier_token.value).map_err(|e| {
            println!("{:?}", e);
            ParserError::UnknownBuiltIn {
                token: identifier_token.value.to_string(),
            }
        })?;

        self.next()?;
        let builtin_node = match builtin.arity() {
            Arity::Single => {
                let arg = expression_parser()?;
                self.expect(TokenKind::Bracket(Bracket::RightParenthesis))?;

                Node::BuiltIn {
                    kind: builtin,
                    arguments: self.bump.alloc_slice_copy(&[arg]),
                }
            }
            Arity::Dual => {
                let arg1 = expression_parser()?;
                self.expect(TokenKind::Operator(Operator::Comma))?;
                let arg2 = expression_parser()?;
                self.expect(TokenKind::Bracket(Bracket::RightParenthesis))?;

                Node::BuiltIn {
                    kind: builtin,
                    arguments: self.bump.alloc_slice_copy(&[arg1, arg2]),
                }
            }
            Arity::Closure => {
                let arg1 = expression_parser()?;
                self.expect(TokenKind::Operator(Operator::Comma))?;
                let arg2 = self.closure(&expression_parser)?;
                self.expect(TokenKind::Bracket(Bracket::RightParenthesis))?;

                Node::BuiltIn {
                    kind: builtin,
                    arguments: self.bump.alloc_slice_copy(&[arg1, arg2]),
                }
            }
        };

        self.with_postfix(self.node(builtin_node), expression_parser)
            .map(Some)
    }

    /// Interval node
    pub(crate) fn interval<F>(
        &self,
        expression_parser: F,
    ) -> ParserResult<Option<&'arena Node<'arena>>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        // Performance optimisation: skip if expression does not contain an interval for faster evaluation
        if !self.has_range_operator {
            return Ok(None);
        }

        let TokenKind::Bracket(_) = &self.current().kind else {
            return Ok(None);
        };

        let initial_position = self.position();
        let left_bracket = self.current().value;

        let TokenKind::Bracket(_) = &self.current().kind else {
            self.set_position(initial_position)?;
            return Ok(None);
        };
        self.next()?;

        let Ok(left) = expression_parser() else {
            self.set_position(initial_position)?;
            return Ok(None);
        };

        if let Err(_) = self.expect(TokenKind::Operator(Operator::Range)) {
            self.set_position(initial_position)?;
            return Ok(None);
        };

        let Ok(right) = expression_parser() else {
            self.set_position(initial_position)?;
            return Ok(None);
        };

        let right_bracket = self.current().value;

        let TokenKind::Bracket(_) = &self.current().kind else {
            self.set_position(initial_position)?;
            return Ok(None);
        };
        self.next()?;

        let interval_node = self.node(Node::Interval {
            left_bracket,
            left,
            right,
            right_bracket,
        });

        self.with_postfix(interval_node, expression_parser)
            .map(Some)
    }

    /// Array nodes
    pub(crate) fn array<F>(
        &self,
        expression_parser: F,
    ) -> ParserResult<Option<&'arena Node<'arena>>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        let current_token = self.current();
        if current_token.kind != TokenKind::Bracket(Bracket::LeftSquareBracket) {
            return Ok(None);
        }

        self.next()?;
        let mut nodes = BumpVec::new_in(self.bump);
        while !(self.current().kind == TokenKind::Bracket(Bracket::RightSquareBracket)) {
            if !nodes.is_empty() {
                self.expect(TokenKind::Operator(Operator::Comma))?;
                if self.current().kind == TokenKind::Bracket(Bracket::RightSquareBracket) {
                    break;
                }
            }

            nodes.push(expression_parser()?);
        }

        self.expect(TokenKind::Bracket(Bracket::RightSquareBracket))?;
        let node = Node::Array(nodes.into_bump_slice());

        self.with_postfix(self.node(node), expression_parser)
            .map(Some)
    }

    /// Conditional
    /// condition_node ? on_true : on_false
    pub(crate) fn conditional<F>(
        &self,
        condition: &'arena Node<'arena>,
        expression_parser: F,
    ) -> ParserResult<Option<&'arena Node<'arena>>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        let current_token = self.current();
        if current_token.kind != TokenKind::Operator(Operator::QuestionMark) {
            return Ok(None);
        }

        self.next()?;

        let on_true = expression_parser()?;
        self.expect(TokenKind::Operator(Operator::Slice))?;
        let on_false = expression_parser()?;

        let conditional_node = Node::Conditional {
            condition,
            on_true,
            on_false,
        };

        Ok(Some(self.node(conditional_node)))
    }

    /// Literal - number, string, array etc.
    pub(crate) fn literal<F>(&self, expression_parser: F) -> ParserResult<&'arena Node<'arena>>
    where
        F: Fn() -> ParserResult<&'arena Node<'arena>>,
    {
        let current_token = self.current();
        match &current_token.kind {
            TokenKind::Identifier(identifier) => match identifier {
                Identifier::Null => self.null()?.ok_or_else(|| ParserError::FailedToParse {
                    message: format!("failed to parse null identifier: {:?}", current_token),
                }),
                _ => {
                    self.identifier(&expression_parser)?
                        .ok_or_else(|| ParserError::FailedToParse {
                            message: format!("failed to parse identifier: {:?}", current_token),
                        })
                }
            },
            TokenKind::Boolean(_) => self.bool()?.ok_or_else(|| ParserError::FailedToParse {
                message: format!("failed to parse boolean: {:?}", current_token),
            }),
            TokenKind::Number => self.number()?.ok_or_else(|| ParserError::FailedToParse {
                message: format!("failed to parse number: {:?}", current_token),
            }),
            TokenKind::String => self.string()?.ok_or_else(|| ParserError::FailedToParse {
                message: format!("failed to parse string: {:?}", current_token),
            }),
            TokenKind::Bracket(_) => self
                .interval(&expression_parser)
                .transpose()
                .or_else(|| self.array(&expression_parser).transpose())
                .transpose()?
                .ok_or_else(|| ParserError::FailedToParse {
                    message: format!("unexpected bracket: {:?}", current_token),
                }),
            TokenKind::Operator(_) => Err(ParserError::FailedToParse {
                message: format!("unexpected literal token: {:?}", current_token),
            }),
        }
    }
}

fn is_valid_property(token: &Token) -> bool {
    match &token.kind {
        TokenKind::Identifier(_) => true,
        TokenKind::Operator(operator) => match operator {
            Operator::Logical(_) => true,
            Operator::Comparison(comparison) => matches!(comparison, ComparisonOperator::In),
            _ => false,
        },
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
        match &token.kind {
            TokenKind::Bracket(Bracket::LeftSquareBracket) => Self::PropertyAccess,
            TokenKind::Operator(Operator::Dot) => Self::MemberAccess,
            _ => Self::Other,
        }
    }
}
