use crate::lexer::Operator;
use crate::parser::builtin::BuiltInFunction;
use rust_decimal::Decimal;
use std::cell::Cell;
use thiserror::Error;

#[derive(Debug, PartialEq, Clone)]
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
        left_bracket: &'a str,
        right_bracket: &'a str,
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
    BuiltIn {
        kind: BuiltInFunction,
        arguments: &'a [&'a Node<'a>],
    },
    Error(Box<AstNodeError>),
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
            Node::Error(_) => {}
            Node::Null => {}
            Node::Bool(_) => {}
            Node::Number(_) => {}
            Node::String(_) => {}
            Node::Pointer => {}
            Node::Identifier(_) => {}
            Node::Root => {}
            Node::TemplateString(parts) => parts.iter().for_each(|n| n.walk(func.clone())),
            Node::Array(parts) => parts.iter().for_each(|n| n.walk(func.clone())),
            Node::Object(obj) => obj.iter().for_each(|(k, v)| {
                k.walk(func.clone());
                v.walk(func.clone());
            }),
            Node::Closure(closure) => closure.walk(func.clone()),
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
            Node::BuiltIn { arguments, .. } => {
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
        let error = Cell::new(None);
        self.walk(|n| {
            if let Node::Error(err) = n {
                error.set(Some(*err.clone()))
            }
        });

        error.into_inner()
    }

    pub fn has_error(&self) -> bool {
        self.first_error().is_some()
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum AstNodeError {
    #[error("Unknown built in: {name} at ({}, {})", span.0, span.1)]
    UnknownBuiltIn { name: String, span: (u32, u32) },

    #[error("Unexpected identifier: {received} at ({}, {}); Expected {expected}.", span.0, span.1)]
    UnexpectedIdentifier {
        received: String,
        expected: String,
        span: (u32, u32),
    },

    #[error("Unexpected identifier: {received} at ({}, {}); Expected {expected}.", span.0, span.1)]
    UnexpectedToken {
        received: String,
        expected: String,
        span: (u32, u32),
    },

    #[error("Invalid number: {number} at ({}, {})", span.0, span.1)]
    InvalidNumber { number: String, span: (u32, u32) },

    #[error("Invalid boolean: {boolean} at ({}, {})", span.0, span.1)]
    InvalidBoolean { boolean: String, span: (u32, u32) },

    #[error("Invalid property: {property} at ({}, {})", span.0, span.1)]
    InvalidProperty { property: String, span: (u32, u32) },

    #[error("Missing expected token: {expected} at {position}")]
    MissingToken { expected: String, position: usize },

    #[error("{message} at ({}, {})", span.0, span.1)]
    Custom { message: String, span: (u32, u32) },

    #[error("Invalid")]
    Invalid,
}
