use rust_decimal::Decimal;

#[derive(Debug, PartialEq, Clone)]
pub enum Node<'a> {
    Null,
    Bool(bool),
    Number(Decimal),
    String(&'a str),
    Pointer,
    Array(&'a [&'a Node<'a>]),
    Identifier(&'a str),
    Closure(&'a Node<'a>),
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
        operator: &'a str,
    },
    Binary {
        left: &'a Node<'a>,
        operator: &'a str,
        right: &'a Node<'a>,
    },
    BuiltIn {
        name: &'a str,
        arguments: &'a [&'a Node<'a>],
    },
}
