use bumpalo::Bump;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use zen_expression::functions::{DeprecatedFunction, FunctionKind};
use zen_expression::lexer::{Bracket, ComparisonOperator, Lexer, LogicalOperator, Operator};
use zen_expression::parser::{Node, Parser};

struct UnaryTest {
    src: &'static str,
    result: &'static Node<'static>,
}

const D1: Decimal = dec!(1);
const D2: Decimal = dec!(2);
const D3: Decimal = dec!(3);
const D10: Decimal = dec!(10);
const D20P5: Decimal = dec!(20.5);
const D50: Decimal = dec!(50);

#[test]
fn unary_test() {
    let tests: Vec<UnaryTest> = Vec::from([
        UnaryTest {
            src: "'str'",
            result: &Node::Binary {
                left: &Node::Identifier("$"),
                operator: Operator::Comparison(ComparisonOperator::Equal),
                right: &Node::String("str"),
            },
        },
        UnaryTest {
            src: "20.5",
            result: &Node::Binary {
                left: &Node::Identifier("$"),
                operator: Operator::Comparison(ComparisonOperator::Equal),
                right: &Node::Number(D20P5),
            },
        },
        UnaryTest {
            src: "'a', 'b', 'c'",
            result: &Node::Binary {
                operator: Operator::Logical(LogicalOperator::Or),
                left: &Node::Binary {
                    operator: Operator::Logical(LogicalOperator::Or),
                    left: &Node::Binary {
                        left: &Node::Identifier("$"),
                        operator: Operator::Comparison(ComparisonOperator::Equal),
                        right: &Node::String("a"),
                    },
                    right: &Node::Binary {
                        left: &Node::Identifier("$"),
                        operator: Operator::Comparison(ComparisonOperator::Equal),
                        right: &Node::String("b"),
                    },
                },
                right: &Node::Binary {
                    left: &Node::Identifier("$"),
                    operator: Operator::Comparison(ComparisonOperator::Equal),
                    right: &Node::String("c"),
                },
            },
        },
        UnaryTest {
            src: "[1..10]",
            result: &Node::Binary {
                operator: Operator::Comparison(ComparisonOperator::In),
                left: &Node::Identifier("$"),
                right: &Node::Interval {
                    left_bracket: Bracket::LeftSquareBracket,
                    right_bracket: Bracket::RightSquareBracket,
                    left: &Node::Number(D1),
                    right: &Node::Number(D10),
                },
            },
        },
        UnaryTest {
            src: "in [1..10]",
            result: &Node::Binary {
                operator: Operator::Comparison(ComparisonOperator::In),
                left: &Node::Identifier("$"),
                right: &Node::Interval {
                    left_bracket: Bracket::LeftSquareBracket,
                    right_bracket: Bracket::RightSquareBracket,
                    left: &Node::Number(D1),
                    right: &Node::Number(D10),
                },
            },
        },
        UnaryTest {
            src: "not in [1..10]",
            result: &Node::Binary {
                operator: Operator::Comparison(ComparisonOperator::NotIn),
                left: &Node::Identifier("$"),
                right: &Node::Interval {
                    left_bracket: Bracket::LeftSquareBracket,
                    right_bracket: Bracket::RightSquareBracket,
                    left: &Node::Number(D1),
                    right: &Node::Number(D10),
                },
            },
        },
        UnaryTest {
            src: "[1, 2, 3]",
            result: &Node::Binary {
                operator: Operator::Comparison(ComparisonOperator::In),
                left: &Node::Identifier("$"),
                right: &Node::Array(&[&Node::Number(D1), &Node::Number(D2), &Node::Number(D3)]),
            },
        },
        UnaryTest {
            src: "date('2022-01-01')",
            result: &Node::Binary {
                operator: Operator::Comparison(ComparisonOperator::Equal),
                left: &Node::Identifier("$"),
                right: &Node::FunctionCall {
                    kind: FunctionKind::Deprecated(DeprecatedFunction::Date),
                    arguments: &[&Node::String("2022-01-01")],
                },
            },
        },
        UnaryTest {
            src: "time('14:00:00')",
            result: &Node::Binary {
                operator: Operator::Comparison(ComparisonOperator::Equal),
                left: &Node::Identifier("$"),
                right: &Node::FunctionCall {
                    kind: FunctionKind::Deprecated(DeprecatedFunction::Time),
                    arguments: &[&Node::String("14:00:00")],
                },
            },
        },
        UnaryTest {
            src: "< 50",
            result: &Node::Binary {
                operator: Operator::Comparison(ComparisonOperator::LessThan),
                left: &Node::Identifier("$"),
                right: &Node::Number(D50),
            },
        },
    ]);

    let mut lexer = Lexer::new();
    let mut bump = Bump::new();

    for UnaryTest { src, result } in tests {
        let tokens = lexer.tokenize(src).unwrap();
        let parser = Parser::try_new(tokens, &bump).unwrap().unary();
        let parser_result = parser.parse();

        assert!(parser_result.error().is_ok(), "Parser failed");
        assert_eq!(parser_result.root, result);

        bump.reset();
    }
}

#[test]
fn failure_tests() {
    let tests: Vec<&str> = Vec::from(["a + b ++", "null.a", "false.b"]);

    let mut lexer = Lexer::new();
    let mut bump = Bump::new();

    for test in tests {
        let tokens = lexer.tokenize(test).unwrap();
        let unary_parser = Parser::try_new(tokens, &bump).unwrap().standard();
        let parser_result = unary_parser.parse();

        assert!(
            parser_result.error().is_err(),
            "Parsing expected to fail for: {test}"
        );

        bump.reset();
    }
}
