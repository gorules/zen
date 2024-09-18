use std::cell::Cell;
use std::fmt::Debug;
use std::marker::PhantomData;

use bumpalo::collections::Vec as BumpVec;
use bumpalo::Bump;
use rust_decimal::Decimal;

use crate::lexer::{
    Bracket, ComparisonOperator, Identifier, Operator, QuotationMark, TemplateString, Token,
    TokenKind,
};
use crate::parser::ast::{AstNodeError, Node};
use crate::parser::builtin::{Arity, BuiltInFunction};
use crate::parser::error::ParserError;
use crate::parser::standard::Standard;
use crate::parser::unary::Unary;

#[derive(Debug)]
pub struct BaseParser;

#[derive(Debug)]
pub struct Parser<'arena, 'token_ref, Flavor> {
    tokens: &'token_ref [Token<'arena>],
    current: Cell<Option<&'token_ref Token<'arena>>>,
    pub(crate) bump: &'arena Bump,
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
        let current = tokens.get(0);
        let has_range_operator = tokens
            .iter()
            .any(|t| t.kind == TokenKind::Operator(Operator::Range));

        Ok(Self {
            tokens,
            bump,
            current: Cell::new(current),
            depth: Cell::new(0),
            position: Cell::new(0),
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
            has_range_operator: self.has_range_operator,
            marker_flavor: PhantomData,
        }
    }
}

impl<'arena, 'token_ref, Flavor> Parser<'arena, 'token_ref, Flavor> {
    pub(crate) fn current(&self) -> Option<&Token<'arena>> {
        self.current.get()
    }

    fn position(&self) -> usize {
        self.position.get()
    }

    fn set_position(&self, position: usize) -> bool {
        let target_token = self.tokens.get(position);

        self.position.set(position);
        self.current.set(target_token);

        target_token.is_some()
    }

    pub(crate) fn depth(&self) -> u8 {
        self.depth.get()
    }

    pub(crate) fn is_done(&self) -> bool {
        self.current.get().is_none()
    }

    pub(crate) fn node(&self, node: Node<'arena>) -> &'arena Node<'arena> {
        self.bump.alloc(node)
    }

    pub(crate) fn error(&self, error: AstNodeError) -> &'arena Node<'arena> {
        self.node(Node::Error(error))
    }

    pub(crate) fn next(&self) {
        let new_position = self.position.get() + 1;

        self.position.set(new_position);
        self.current.set(self.tokens.get(new_position));
    }

    pub(crate) fn expect(&self, kind: TokenKind) -> Option<&'arena Node<'arena>> {
        let token = self.current();
        if token.is_some_and(|t| t.kind == kind) {
            self.next();
            return None;
        }

        Some(
            self.error(AstNodeError::UnexpectedToken {
                expected: kind.to_string(),
                received: token
                    .map(|t| t.kind.to_string())
                    .unwrap_or_else(|| "None".to_string()),
                span: token.map(|t| t.span).unwrap_or((0, 0)),
            }),
        )
    }

    pub(crate) fn number(&self) -> &'arena Node<'arena> {
        let Some(token) = self.current() else {
            return self.error(AstNodeError::MissingToken {
                expected: "Number".to_string(),
                position: self.position(),
            });
        };

        let Ok(decimal) = Decimal::from_str_exact(token.value) else {
            return self.error(AstNodeError::InvalidNumber {
                number: token.value.to_string(),
                span: token.span,
            });
        };

        self.next();
        self.node(Node::Number(decimal))
    }

    pub(crate) fn bool(&self) -> &'arena Node<'arena> {
        let Some(token) = self.current() else {
            return self.error(AstNodeError::MissingToken {
                expected: "Boolean".to_string(),
                position: self.position(),
            });
        };

        let TokenKind::Boolean(boolean) = token.kind else {
            return self.error(AstNodeError::InvalidBoolean {
                boolean: token.value.to_string(),
                span: token.span,
            });
        };

        self.next();
        self.node(Node::Bool(boolean))
    }

    pub(crate) fn null(&self) -> &'arena Node<'arena> {
        let Some(token) = self.current() else {
            return self.error(AstNodeError::MissingToken {
                expected: "Null".to_string(),
                position: self.position(),
            });
        };

        if token.kind != TokenKind::Identifier(Identifier::Null) {
            return self.error(AstNodeError::UnexpectedIdentifier {
                expected: "Null".to_string(),
                received: token.value.to_string(),
                span: token.span,
            });
        }

        self.next();
        self.node(Node::Null)
    }

    pub(crate) fn simple_string(&self, quote_mark: &QuotationMark) -> &'arena Node<'arena> {
        if let Some(error_node) = self.expect(TokenKind::QuotationMark(quote_mark.clone())) {
            return error_node;
        }

        let string_value = self.current().map(|s| s.value);

        let error_literal = self.expect(TokenKind::Literal);
        let error_mark_end = self.expect(TokenKind::QuotationMark(quote_mark.clone()));

        error_literal
            .or(error_mark_end)
            .or(string_value.map(|s| self.node(Node::String(s))))
            .unwrap_or_else(|| self.error(AstNodeError::Invalid))
    }

    pub(crate) fn template_string<F>(&self, expression_parser: F) -> &'arena Node<'arena>
    where
        F: Fn() -> &'arena Node<'arena>,
    {
        if let Some(error_node) = self.expect(TokenKind::QuotationMark(QuotationMark::Backtick)) {
            return error_node;
        }

        let Some(mut current_token) = self.current() else {
            return self.error(AstNodeError::MissingToken {
                expected: "Backtick (`)".to_string(),
                position: self.position(),
            });
        };

        let mut nodes = BumpVec::new_in(self.bump);
        while TokenKind::QuotationMark(QuotationMark::Backtick) != current_token.kind {
            match current_token.kind {
                TokenKind::TemplateString(template) => match template {
                    TemplateString::ExpressionStart => {
                        self.next();
                        nodes.push(expression_parser());
                    }
                    TemplateString::ExpressionEnd => {
                        self.next();
                    }
                },
                TokenKind::Literal => {
                    nodes.push(self.node(Node::String(current_token.value)));
                    self.next();
                }
                _ => {
                    return self.error(AstNodeError::UnexpectedToken {
                        expected: "Valid TemplateString token".to_string(),
                        received: current_token.kind.to_string(),
                        span: current_token.span,
                    })
                }
            }

            if let Some(ct) = self.current() {
                current_token = ct;
            } else {
                break;
            }
        }

        if let Some(err) = self.expect(TokenKind::QuotationMark(QuotationMark::Backtick)) {
            return err;
        };

        self.node(Node::TemplateString(nodes.into_bump_slice()))
    }

    pub(crate) fn with_postfix<F>(
        &self,
        node: &'arena Node<'arena>,
        expression_parser: F,
    ) -> &'arena Node<'arena>
    where
        F: Fn() -> &'arena Node<'arena>,
    {
        let Some(postfix_token) = self.current() else {
            return node;
        };

        let postfix_kind = PostfixKind::from(postfix_token);

        let processed_token = match postfix_kind {
            PostfixKind::Other => return node,
            PostfixKind::MemberAccess => {
                self.next();
                let property_token = self.current();
                self.next();

                let property = match property_token {
                    None => self.error(AstNodeError::Invalid),
                    Some(t) => match is_valid_property(t) {
                        true => self.node(Node::String(t.value)),
                        false => self.error(AstNodeError::InvalidProperty {
                            property: t.value.to_string(),
                            span: t.span,
                        }),
                    },
                };

                self.node(Node::Member { node, property })
            }
            PostfixKind::PropertyAccess => {
                self.next();
                let mut from: Option<&'arena Node<'arena>> = None;
                let mut to: Option<&'arena Node<'arena>> = None;

                let Some(mut c) = self.current() else {
                    return self.error(AstNodeError::Invalid);
                };

                if c.kind == TokenKind::Operator(Operator::Slice) {
                    self.next();

                    let Some(cc) = self.current() else {
                        return self.error(AstNodeError::Invalid);
                    };
                    c = cc;

                    if c.kind != TokenKind::Bracket(Bracket::RightSquareBracket) {
                        to = Some(expression_parser());
                    }

                    self.expect(TokenKind::Bracket(Bracket::RightSquareBracket));
                    self.node(Node::Slice { node, to, from })
                } else {
                    from = Some(expression_parser());
                    let Some(cc) = self.current() else {
                        return self.error(AstNodeError::Invalid);
                    };
                    c = cc;

                    if c.kind == TokenKind::Operator(Operator::Slice) {
                        self.next();
                        let Some(cc) = self.current() else {
                            return self.error(AstNodeError::Invalid);
                        };
                        c = cc;

                        if c.kind != TokenKind::Bracket(Bracket::RightSquareBracket) {
                            to = Some(expression_parser());
                        }

                        self.expect(TokenKind::Bracket(Bracket::RightSquareBracket));
                        self.node(Node::Slice { node, from, to })
                    } else {
                        // Slice operator [:] was not found,
                        // it should be just an index node.
                        self.expect(TokenKind::Bracket(Bracket::RightSquareBracket));
                        self.node(Node::Member {
                            node,
                            property: from.unwrap_or_else(|| self.error(AstNodeError::Invalid)),
                        })
                    }
                }
            }
        };

        self.with_postfix(processed_token, expression_parser)
    }

    /// Closure
    pub(crate) fn closure<F>(&self, expression_parser: F) -> &'arena Node<'arena>
    where
        F: Fn() -> &'arena Node<'arena>,
    {
        self.depth.set(self.depth.get() + 1);
        let node = expression_parser();
        self.depth.set(self.depth.get() - 1);

        self.node(Node::Closure(node))
    }

    /// Identifier expression
    /// Either <Identifier> or <Identifier Expression>
    pub(crate) fn identifier<F>(&self, expression_parser: F) -> &'arena Node<'arena>
    where
        F: Fn() -> &'arena Node<'arena>,
    {
        let Some(token) = self.current() else {
            return self.error(AstNodeError::MissingToken {
                expected: "Identifier".to_string(),
                position: self.position(),
            });
        };

        match token.kind {
            TokenKind::Identifier(_) | TokenKind::Literal => {
                // ok
            }
            _ => return self.error(AstNodeError::Invalid),
        }

        let Some(identifier_token) = self.current() else {
            return self.error(AstNodeError::Invalid);
        };
        self.next();

        let current_token = self.current();
        if current_token.map(|t| t.kind) != Some(TokenKind::Bracket(Bracket::LeftParenthesis)) {
            let identifier_node = match identifier_token.kind {
                TokenKind::Identifier(Identifier::RootReference) => self.node(Node::Root),
                _ => self.node(Node::Identifier(identifier_token.value)),
            };

            return self.with_postfix(identifier_node, expression_parser);
        }

        // Potentially it might be a built-in expression
        let Ok(builtin) = BuiltInFunction::try_from(identifier_token.value) else {
            return self.error(AstNodeError::UnknownBuiltIn {
                name: identifier_token.value.to_string(),
                span: identifier_token.span,
            });
        };

        self.next();
        let builtin_node = match builtin.arity() {
            Arity::Single => {
                let arg = expression_parser();
                self.expect(TokenKind::Bracket(Bracket::RightParenthesis));

                Node::BuiltIn {
                    kind: builtin,
                    arguments: self.bump.alloc_slice_copy(&[arg]),
                }
            }
            Arity::Dual => {
                let arg1 = expression_parser();
                self.expect(TokenKind::Operator(Operator::Comma));
                let arg2 = expression_parser();
                self.expect(TokenKind::Bracket(Bracket::RightParenthesis));

                Node::BuiltIn {
                    kind: builtin,
                    arguments: self.bump.alloc_slice_copy(&[arg1, arg2]),
                }
            }
            Arity::Closure => {
                let arg1 = expression_parser();
                self.expect(TokenKind::Operator(Operator::Comma));
                let arg2 = self.closure(&expression_parser);
                self.expect(TokenKind::Bracket(Bracket::RightParenthesis));

                Node::BuiltIn {
                    kind: builtin,
                    arguments: self.bump.alloc_slice_copy(&[arg1, arg2]),
                }
            }
        };

        self.with_postfix(self.node(builtin_node), expression_parser)
    }

    /// Interval node
    pub(crate) fn interval<F>(&self, expression_parser: F) -> Option<&'arena Node<'arena>>
    where
        F: Fn() -> &'arena Node<'arena>,
    {
        // Performance optimisation: skip if expression does not contain an interval for faster evaluation
        if !self.has_range_operator {
            return None;
        }

        let TokenKind::Bracket(_) = &self.current()?.kind else {
            return None;
        };

        let initial_position = self.position();
        let left_bracket = self.current()?.value;

        let TokenKind::Bracket(_) = &self.current()?.kind else {
            self.set_position(initial_position);
            return None;
        };

        self.next();
        let left = expression_parser();
        if left.has_error() {
            self.set_position(initial_position);
            return None;
        };

        if let Some(_) = self.expect(TokenKind::Operator(Operator::Range)) {
            self.set_position(initial_position);
            return None;
        };

        let right = expression_parser();
        if right.has_error() {
            self.set_position(initial_position);
            return None;
        };

        let right_bracket = self.current()?.value;
        let TokenKind::Bracket(_) = &self.current()?.kind else {
            self.set_position(initial_position);
            return None;
        };

        self.next();

        let interval_node = self.node(Node::Interval {
            left_bracket,
            left,
            right,
            right_bracket,
        });

        Some(self.with_postfix(interval_node, expression_parser))
    }

    /// Array nodes
    pub(crate) fn array<F>(&self, expression_parser: F) -> &'arena Node<'arena>
    where
        F: Fn() -> &'arena Node<'arena>,
    {
        let Some(current_token) = self.current() else {
            return self.error(AstNodeError::MissingToken {
                expected: "Array".to_string(),
                position: self.position(),
            });
        };

        if current_token.kind != TokenKind::Bracket(Bracket::LeftSquareBracket) {
            return self.error(AstNodeError::UnexpectedToken {
                expected: TokenKind::Bracket(Bracket::LeftSquareBracket).to_string(),
                received: current_token.kind.to_string(),
                span: current_token.span,
            });
        }

        self.next();
        let mut nodes = BumpVec::new_in(self.bump);
        while !(self.current().map(|t| t.kind)
            == Some(TokenKind::Bracket(Bracket::RightSquareBracket)))
        {
            if !nodes.is_empty() {
                self.expect(TokenKind::Operator(Operator::Comma));
                if self.current().map(|t| t.kind)
                    == Some(TokenKind::Bracket(Bracket::RightSquareBracket))
                {
                    break;
                }
            }

            nodes.push(expression_parser());
        }

        self.expect(TokenKind::Bracket(Bracket::RightSquareBracket));
        let node = Node::Array(nodes.into_bump_slice());

        self.with_postfix(self.node(node), expression_parser)
    }

    pub(crate) fn object<F>(&self, expression_parser: F) -> &'arena Node<'arena>
    where
        F: Fn() -> &'arena Node<'arena>,
    {
        if let Some(err_node) = self.expect(TokenKind::Bracket(Bracket::LeftCurlyBracket)) {
            return err_node;
        };

        let mut key_value_pairs = BumpVec::new_in(self.bump);
        if let Some(TokenKind::Bracket(Bracket::RightCurlyBracket)) = self.current().map(|t| t.kind)
        {
            self.next();
            return self.node(Node::Object(key_value_pairs.into_bump_slice()));
        }

        loop {
            let key = self.object_key(&expression_parser);
            self.expect(TokenKind::Operator(Operator::Slice));
            let value = expression_parser();

            key_value_pairs.push((key, value));

            let Some(current_token) = self.current() else {
                break;
            };

            match current_token.kind {
                TokenKind::Operator(Operator::Comma) => {
                    self.expect(TokenKind::Operator(Operator::Comma));
                }
                TokenKind::Bracket(Bracket::RightCurlyBracket) => break,
                _ => return self.error(AstNodeError::Invalid),
            }
        }

        self.expect(TokenKind::Bracket(Bracket::RightCurlyBracket));
        self.node(Node::Object(key_value_pairs.into_bump_slice()))
    }

    pub(crate) fn object_key<F>(&self, expression_parser: F) -> &'arena Node<'arena>
    where
        F: Fn() -> &'arena Node<'arena>,
    {
        let Some(key_token) = self.current() else {
            return self.error(AstNodeError::Invalid);
        };

        let key = match key_token.kind {
            TokenKind::Identifier(identifier) => {
                self.next();
                self.node(Node::String(identifier.into()))
            }
            TokenKind::Boolean(boolean) => match boolean {
                true => {
                    self.next();
                    self.node(Node::String("true"))
                }
                false => {
                    self.next();
                    self.node(Node::String("false"))
                }
            },
            TokenKind::Number => {
                self.next();
                self.node(Node::String(key_token.value))
            }
            TokenKind::Literal => {
                self.next();
                self.node(Node::String(key_token.value))
            }
            TokenKind::Bracket(bracket) => match bracket {
                Bracket::LeftSquareBracket => {
                    self.expect(TokenKind::Bracket(Bracket::LeftSquareBracket));
                    let token = expression_parser();
                    self.expect(TokenKind::Bracket(Bracket::RightSquareBracket));

                    token
                }
                _ => {
                    return self.error(AstNodeError::Custom {
                        message: "Operator is not supported as object key".to_string(),
                        span: key_token.span,
                    })
                }
            },
            TokenKind::QuotationMark(qm) => match qm {
                QuotationMark::SingleQuote => self.simple_string(&QuotationMark::SingleQuote),
                QuotationMark::DoubleQuote => self.simple_string(&QuotationMark::DoubleQuote),
                QuotationMark::Backtick => {
                    return self.error(AstNodeError::Custom {
                        message: "TemplateString expression not supported as object key"
                            .to_string(),
                        span: key_token.span,
                    })
                }
            },
            TokenKind::TemplateString(_) => {
                return self.error(AstNodeError::Custom {
                    message: "TemplateString expression not supported as object key".to_string(),
                    span: key_token.span,
                })
            }
            TokenKind::Operator(_) => {
                return self.error(AstNodeError::Custom {
                    message: "Operator is not supported as object key".to_string(),
                    span: key_token.span,
                })
            }
        };

        key
    }

    /// Conditional
    /// condition_node ? on_true : on_false
    pub(crate) fn conditional<F>(
        &self,
        condition: &'arena Node<'arena>,
        expression_parser: F,
    ) -> Option<&'arena Node<'arena>>
    where
        F: Fn() -> &'arena Node<'arena>,
    {
        let Some(current_token) = self.current() else {
            return None;
        };
        if current_token.kind != TokenKind::Operator(Operator::QuestionMark) {
            return None;
        }

        self.next();

        let on_true = expression_parser();
        self.expect(TokenKind::Operator(Operator::Slice));
        let on_false = expression_parser();

        let conditional_node = Node::Conditional {
            condition,
            on_true,
            on_false,
        };

        Some(self.node(conditional_node))
    }

    /// Literal - number, string, array etc.
    pub(crate) fn literal<F>(&self, expression_parser: F) -> &'arena Node<'arena>
    where
        F: Fn() -> &'arena Node<'arena>,
    {
        let Some(current_token) = self.current() else {
            return self.error(AstNodeError::Invalid);
        };

        match &current_token.kind {
            TokenKind::Identifier(identifier) => match identifier {
                Identifier::Null => self.null(),
                _ => self.identifier(&expression_parser),
            },
            TokenKind::Literal => self.identifier(&expression_parser),
            TokenKind::Boolean(_) => self.bool(),
            TokenKind::Number => self.number(),
            TokenKind::QuotationMark(quote_mark) => match quote_mark {
                QuotationMark::SingleQuote | QuotationMark::DoubleQuote => {
                    self.simple_string(quote_mark)
                }
                QuotationMark::Backtick => self.template_string(&expression_parser),
            },
            TokenKind::Bracket(bracket) => match bracket {
                Bracket::LeftParenthesis
                | Bracket::RightParenthesis
                | Bracket::RightSquareBracket => self
                    .interval(&expression_parser)
                    .unwrap_or_else(|| self.error(AstNodeError::Invalid)),
                Bracket::LeftSquareBracket => self
                    .interval(&expression_parser)
                    .unwrap_or_else(|| self.array(&expression_parser)),
                Bracket::LeftCurlyBracket => self.object(&expression_parser),
                Bracket::RightCurlyBracket => self.error(AstNodeError::Custom {
                    message: "Unexpected RightCurlyBracket token".to_string(),
                    span: current_token.span,
                }),
            },
            TokenKind::Operator(_) => self.error(AstNodeError::Custom {
                message: "Unexpected Operator token".to_string(),
                span: current_token.span,
            }),
            TokenKind::TemplateString(_) => self.error(AstNodeError::Custom {
                message: "Unexpected TemplateString token".to_string(),
                span: current_token.span,
            }),
        }
    }
}

fn is_valid_property(token: &Token) -> bool {
    match &token.kind {
        TokenKind::Identifier(_) => true,
        TokenKind::Literal => true,
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
