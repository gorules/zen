use crate::lexer::Operator;
use crate::parser::builtin::BuiltInFunction;
use rust_decimal::Decimal;
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
    Error(AstNodeError),
}

impl<'a> Node<'a> {
    pub fn walk<F>(&self, mut func: F)
    where
        F: FnMut(&Self),
    {
        func(self);

        match self {
            Node::Error(_) => {}
            Node::Null => {}
            Node::Bool(_) => {}
            Node::Number(_) => {}
            Node::String(_) => {}
            Node::Pointer => {}
            Node::Identifier(_) => {}
            Node::Root => {}
            Node::TemplateString(parts) => parts.iter().for_each(|n| func(n)),
            Node::Array(parts) => parts.iter().for_each(|n| func(n)),
            Node::Object(obj) => obj.iter().for_each(|(k, v)| {
                func(k);
                func(v);
            }),
            Node::Closure(closure) => func(closure),
            Node::Member { node, property } => {
                func(node);
                func(property);
            }
            Node::Slice { node, to, from } => {
                func(node);
                if let Some(to) = to {
                    func(to);
                }

                if let Some(from) = from {
                    func(from);
                }
            }
            Node::Interval { right, left, .. } => {
                func(right);
                func(left);
            }
            Node::Conditional {
                on_true,
                condition,
                on_false,
            } => {
                func(condition);
                func(on_true);
                func(on_false);
            }
            Node::Unary { node, .. } => {
                func(node);
            }
            Node::Binary { left, right, .. } => {
                func(left);
                func(right);
            }
            Node::BuiltIn { arguments, .. } => {
                arguments.iter().for_each(|n| func(n));
            }
        }
    }

    pub fn is_error(&self) -> bool {
        match self {
            Node::Error(_) => true,
            _ => false,
        }
    }
    
    pub fn has_error(&self) -> bool {
        let mut has_error = false;
        self.walk(|n| {
            if n.is_error() {
                has_error = true
            }
        });
        
        has_error
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
