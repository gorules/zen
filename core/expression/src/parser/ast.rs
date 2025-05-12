use crate::functions::{FunctionKind, MethodKind};
use crate::lexer::{Bracket, Operator};
use rust_decimal::Decimal;
use std::cell::Cell;
use strum_macros::IntoStaticStr;
use thiserror::Error;

#[derive(Debug, PartialEq, Clone, IntoStaticStr)]
pub enum Node<'a> {
    Null,
    Bool(bool),
    Number(Decimal),
    String(&'a str),
    TemplateString(&'a [&'a Node<'a>]),
    Pointer,
    Array(&'a [&'a Node<'a>]),
    Object(&'a [(&'a Node<'a>, &'a Node<'a>)]),
    Identifier(&'a str),
    Closure(&'a Node<'a>),
    Parenthesized(&'a Node<'a>),
    Root,
    Member {
        node: &'a Node<'a>,
        property: &'a Node<'a>,
    },
    Slice {
        node: &'a Node<'a>,
        from: Option<&'a Node<'a>>,
        to: Option<&'a Node<'a>>,
    },
    Interval {
        left: &'a Node<'a>,
        right: &'a Node<'a>,
        left_bracket: Bracket,
        right_bracket: Bracket,
    },
    Conditional {
        condition: &'a Node<'a>,
        on_true: &'a Node<'a>,
        on_false: &'a Node<'a>,
    },
    Unary {
        node: &'a Node<'a>,
        operator: Operator,
    },
    Binary {
        left: &'a Node<'a>,
        operator: Operator,
        right: &'a Node<'a>,
    },
    FunctionCall {
        kind: FunctionKind,
        arguments: &'a [&'a Node<'a>],
    },
    MethodCall {
        kind: MethodKind,
        this: &'a Node<'a>,
        arguments: &'a [&'a Node<'a>],
    },
    Error {
        node: Option<&'a Node<'a>>,
        error: AstNodeError<'a>,
    },
}

impl<'a> Node<'a> {
    pub fn walk<F>(&self, mut func: F)
    where
        F: FnMut(&Self) + Clone,
    {
        {
            func(self);
        };

        match self {
            Node::Null => {}
            Node::Bool(_) => {}
            Node::Number(_) => {}
            Node::String(_) => {}
            Node::Pointer => {}
            Node::Identifier(_) => {}
            Node::Root => {}
            Node::Error { node, .. } => {
                if let Some(n) = node {
                    n.walk(func.clone())
                }
            }
            Node::TemplateString(parts) => parts.iter().for_each(|n| n.walk(func.clone())),
            Node::Array(parts) => parts.iter().for_each(|n| n.walk(func.clone())),
            Node::Object(obj) => obj.iter().for_each(|(k, v)| {
                k.walk(func.clone());
                v.walk(func.clone());
            }),
            Node::Closure(closure) => closure.walk(func.clone()),
            Node::Parenthesized(c) => c.walk(func.clone()),
            Node::Member { node, property } => {
                node.walk(func.clone());
                property.walk(func.clone());
            }
            Node::Slice { node, to, from } => {
                node.walk(func.clone());
                if let Some(to) = to {
                    to.walk(func.clone());
                }

                if let Some(from) = from {
                    from.walk(func.clone());
                }
            }
            Node::Interval { left, right, .. } => {
                left.walk(func.clone());
                right.walk(func.clone());
            }
            Node::Unary { node, .. } => {
                node.walk(func);
            }
            Node::Binary { left, right, .. } => {
                left.walk(func.clone());
                right.walk(func.clone());
            }
            Node::FunctionCall { arguments, .. } => {
                arguments.iter().for_each(|n| n.walk(func.clone()));
            }
            Node::MethodCall {
                this, arguments, ..
            } => {
                this.walk(func.clone());
                arguments.iter().for_each(|n| n.walk(func.clone()));
            }
            Node::Conditional {
                on_true,
                condition,
                on_false,
            } => {
                condition.walk(func.clone());
                on_true.walk(func.clone());
                on_false.walk(func.clone());
            }
        };
    }

    pub fn first_error(&self) -> Option<AstNodeError> {
        let error_cell = Cell::new(None);
        self.walk(|n| {
            if let Node::Error { error, .. } = n {
                error_cell.set(Some(error.clone()))
            }
        });

        error_cell.into_inner()
    }

    pub fn has_error(&self) -> bool {
        self.first_error().is_some()
    }

    pub(crate) fn span(&self) -> Option<(u32, u32)> {
        match self {
            Node::Error { error, .. } => match error {
                AstNodeError::UnknownBuiltIn { span, .. } => Some(span.clone()),
                AstNodeError::UnknownMethod { span, .. } => Some(span.clone()),
                AstNodeError::UnexpectedIdentifier { span, .. } => Some(span.clone()),
                AstNodeError::UnexpectedToken { span, .. } => Some(span.clone()),
                AstNodeError::InvalidNumber { span, .. } => Some(span.clone()),
                AstNodeError::InvalidBoolean { span, .. } => Some(span.clone()),
                AstNodeError::InvalidProperty { span, .. } => Some(span.clone()),
                AstNodeError::MissingToken { position, .. } => {
                    Some((*position as u32, *position as u32))
                }
                AstNodeError::Custom { span, .. } => Some(span.clone()),
            },
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum AstNodeError<'a> {
    #[error("Unknown function `{name}` at ({}, {})", span.0, span.1)]
    UnknownBuiltIn { name: &'a str, span: (u32, u32) },

    #[error("Unknown method `{name}` at ({}, {})", span.0, span.1)]
    UnknownMethod { name: &'a str, span: (u32, u32) },

    #[error("Unexpected identifier: {received} at ({}, {}); Expected {expected}.", span.0, span.1)]
    UnexpectedIdentifier {
        received: &'a str,
        expected: &'a str,
        span: (u32, u32),
    },

    #[error("Unexpected token: {received} at ({}, {}); Expected {expected}.", span.0, span.1)]
    UnexpectedToken {
        received: &'a str,
        expected: &'a str,
        span: (u32, u32),
    },

    #[error("Invalid number: {number} at ({}, {})", span.0, span.1)]
    InvalidNumber { number: &'a str, span: (u32, u32) },

    #[error("Invalid boolean: {boolean} at ({}, {})", span.0, span.1)]
    InvalidBoolean { boolean: &'a str, span: (u32, u32) },

    #[error("Invalid property: {property} at ({}, {})", span.0, span.1)]
    InvalidProperty { property: &'a str, span: (u32, u32) },

    #[error("Missing expected token: {expected} at {position}")]
    MissingToken { expected: &'a str, position: usize },

    #[error("{message} at ({}, {})", span.0, span.1)]
    Custom { message: &'a str, span: (u32, u32) },
}
