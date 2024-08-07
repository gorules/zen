use rust_decimal::Decimal;

use crate::lexer::Operator;
use crate::parser::builtin::BuiltInFunction;

#[derive(Debug, PartialEq, Clone)]
pub enum Node<'a> {
    Null,
    Bool(bool),
    Number(Decimal),
    String(&'a str),
    TemplateString(&'a [&'a Node<'a>]),
    Pointer,
    Array(&'a [&'a Node<'a>]),
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
}
