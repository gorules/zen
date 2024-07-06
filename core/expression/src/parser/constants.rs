use nohash_hasher::BuildNoHashHasher;
use once_cell::sync::Lazy;
use std::collections::HashMap;

use crate::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use Associativity::{Left, Right};

type NoHasher = BuildNoHashHasher<Operator>;

#[derive(Debug, PartialEq)]
pub(crate) enum Associativity {
    Left,
    Right,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ParserOperator {
    pub precedence: u8,
    pub associativity: Associativity,
}

macro_rules! hashmap {
    (@single $($x:tt)*) => (());
    (@count $($rest:expr),*) => (<[()]>::len(&[$(hashmap!(@single $rest)),*]));

    ($($key:expr => $value:expr,)+) => { hashmap!($($key => $value),+) };
    ($($key:expr => $value:expr),*) => {
        {
            let _cap = hashmap!(@count $($key),*);
            let mut _map = ::std::collections::HashMap::default();
            _map.reserve(_cap);
            $(
                let _ = _map.insert($key, $value);
            )*
            _map
        }
    };
}

pub(crate) static BINARY_OPERATORS: Lazy<HashMap<Operator, ParserOperator, NoHasher>> = Lazy::new(
    || {
        hashmap! {
            Operator::Logical(LogicalOperator::Or) => ParserOperator { precedence: 10, associativity: Left },
            Operator::Logical(LogicalOperator::And) => ParserOperator { precedence: 15, associativity: Left },
            Operator::Comparison(ComparisonOperator::Equal) => ParserOperator { precedence: 20, associativity: Left },
            Operator::Comparison(ComparisonOperator::NotEqual) => ParserOperator { precedence: 20, associativity: Left },
            Operator::Comparison(ComparisonOperator::LessThan) => ParserOperator { precedence: 20, associativity: Left },
            Operator::Comparison(ComparisonOperator::GreaterThan) => ParserOperator { precedence: 20, associativity: Left },
            Operator::Comparison(ComparisonOperator::LessThanOrEqual) => ParserOperator { precedence: 20, associativity: Left },
            Operator::Comparison(ComparisonOperator::GreaterThanOrEqual) => ParserOperator { precedence: 20, associativity: Left },
            Operator::Comparison(ComparisonOperator::NotIn) => ParserOperator { precedence: 20, associativity: Left },
            Operator::Comparison(ComparisonOperator::In) => ParserOperator { precedence: 20, associativity: Left },
            Operator::Arithmetic(ArithmeticOperator::Add) => ParserOperator { precedence: 30, associativity: Left },
            Operator::Arithmetic(ArithmeticOperator::Subtract) => ParserOperator { precedence: 30, associativity: Left },
            Operator::Arithmetic(ArithmeticOperator::Multiply) => ParserOperator { precedence: 60, associativity: Left },
            Operator::Arithmetic(ArithmeticOperator::Divide) => ParserOperator { precedence: 60, associativity: Left },
            Operator::Arithmetic(ArithmeticOperator::Modulus) => ParserOperator { precedence: 60, associativity: Left },
            Operator::Arithmetic(ArithmeticOperator::Power) => ParserOperator { precedence: 70, associativity: Right },
            Operator::Logical(LogicalOperator::NullishCoalescing) => ParserOperator { precedence: 80, associativity: Left },
        }
    },
);

pub(crate) static UNARY_OPERATORS: Lazy<HashMap<Operator, ParserOperator, NoHasher>> = Lazy::new(
    || {
        hashmap! {
            Operator::Logical(LogicalOperator::Not) => ParserOperator { precedence: 50, associativity: Left },
            Operator::Arithmetic(ArithmeticOperator::Add) => ParserOperator { precedence: 200, associativity: Left },
            Operator::Arithmetic(ArithmeticOperator::Subtract) => ParserOperator { precedence: 200, associativity: Left },
        }
    },
);
