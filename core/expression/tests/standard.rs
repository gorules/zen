use bumpalo::Bump;
use rust_decimal::Decimal;

use rust_decimal_macros::dec;

use zen_expression::lexer::{
    ArithmeticOperator, ComparisonOperator, Lexer, LogicalOperator, Operator,
};
use zen_expression::parser::{Node, Parser};

struct StandardTest {
    src: &'static str,
    result: &'static Node<'static>,
}

const D0: Decimal = dec!(0);
const D1: Decimal = dec!(1);
const D2: Decimal = dec!(2);
const D2P5: Decimal = dec!(2.5);
const D3: Decimal = dec!(3);
const D4: Decimal = dec!(4);
const D9: Decimal = dec!(9);
const D10: Decimal = dec!(10);
const D25: Decimal = dec!(25);
const D10_000_000: Decimal = dec!(10_000_000);

#[test]
fn standard_test() {
    let tests: Vec<StandardTest> = Vec::from([
        StandardTest {
            src: ")10..25(",
            result: &Node::Interval {
                left_bracket: ")",
                left: &Node::Number(D10),
                right: &Node::Number(D25),
                right_bracket: "(",
            },
        },
        StandardTest {
            src: "a",
            result: &Node::Identifier("a"),
        },
        StandardTest {
            src: "'str'",
            result: &Node::String("str"),
        },
        StandardTest {
            src: "3",
            result: &Node::Number(D3),
        },
        StandardTest {
            src: "10_000_000",
            result: &Node::Number(D10_000_000),
        },
        StandardTest {
            src: "2.5",
            result: &Node::Number(D2P5),
        },
        StandardTest {
            src: "true",
            result: &Node::Bool(true),
        },
        StandardTest {
            src: "false",
            result: &Node::Bool(false),
        },
        StandardTest {
            src: "null",
            result: &Node::Null,
        },
        StandardTest {
            src: "-3",
            result: &Node::Unary {
                operator: Operator::Arithmetic(ArithmeticOperator::Subtract),
                node: &Node::Number(D3),
            },
        },
        StandardTest {
            src: "1 - 2",
            result: &Node::Binary {
                left: &Node::Number(D1),
                operator: Operator::Arithmetic(ArithmeticOperator::Subtract),
                right: &Node::Number(D2),
            },
        },
        StandardTest {
            src: "(1 - 2) * 3",
            result: &Node::Binary {
                left: &Node::Parenthesized(&Node::Binary {
                    left: &Node::Number(D1),
                    operator: Operator::Arithmetic(ArithmeticOperator::Subtract),
                    right: &Node::Number(D2),
                }),
                operator: Operator::Arithmetic(ArithmeticOperator::Multiply),
                right: &Node::Number(D3),
            },
        },
        StandardTest {
            src: "a or b or c",
            result: &Node::Binary {
                operator: Operator::Logical(LogicalOperator::Or),
                left: &Node::Binary {
                    left: &Node::Identifier("a"),
                    right: &Node::Identifier("b"),
                    operator: Operator::Logical(LogicalOperator::Or),
                },
                right: &Node::Identifier("c"),
            },
        },
        StandardTest {
            src: "a or b and c",
            result: &Node::Binary {
                operator: Operator::Logical(LogicalOperator::Or),
                left: &Node::Identifier("a"),
                right: &Node::Binary {
                    left: &Node::Identifier("b"),
                    right: &Node::Identifier("c"),
                    operator: Operator::Logical(LogicalOperator::And),
                },
            },
        },
        StandardTest {
            src: "(a or b) and c",
            result: &Node::Binary {
                operator: Operator::Logical(LogicalOperator::And),
                left: &Node::Parenthesized(&Node::Binary {
                    left: &Node::Identifier("a"),
                    right: &Node::Identifier("b"),
                    operator: Operator::Logical(LogicalOperator::Or),
                }),
                right: &Node::Identifier("c"),
            },
        },
        StandardTest {
            src: "2^4 - 1",
            result: &Node::Binary {
                left: &Node::Binary {
                    operator: Operator::Arithmetic(ArithmeticOperator::Power),
                    left: &Node::Number(D2),
                    right: &Node::Number(D4),
                },
                operator: Operator::Arithmetic(ArithmeticOperator::Subtract),
                right: &Node::Number(D1),
            },
        },
        StandardTest {
            src: "foo.and",
            result: &Node::Member {
                node: &Node::Identifier("foo"),
                property: &Node::String("and"),
            },
        },
        StandardTest {
            src: "foo.all",
            result: &Node::Member {
                node: &Node::Identifier("foo"),
                property: &Node::String("all"),
            },
        },
        StandardTest {
            src: "foo[3]",
            result: &Node::Member {
                node: &Node::Identifier("foo"),
                property: &Node::Number(D3),
            },
        },
        StandardTest {
            src: "true ? true : false",
            result: &Node::Conditional {
                condition: &Node::Bool(true),
                on_true: &Node::Bool(true),
                on_false: &Node::Bool(false),
            },
        },
        StandardTest {
            src: "a ? [b] : c",
            result: &Node::Conditional {
                condition: &Node::Identifier("a"),
                on_true: &Node::Array(&[&Node::Identifier("b")]),
                on_false: &Node::Identifier("c"),
            },
        },
        StandardTest {
            src: "'a' == 'b'",
            result: &Node::Binary {
                left: &Node::String("a"),
                right: &Node::String("b"),
                operator: Operator::Comparison(ComparisonOperator::Equal),
            },
        },
        StandardTest {
            src: "+0 != -1",
            result: &Node::Binary {
                left: &Node::Unary {
                    operator: Operator::Arithmetic(ArithmeticOperator::Add),
                    node: &Node::Number(D0),
                },
                right: &Node::Unary {
                    operator: Operator::Arithmetic(ArithmeticOperator::Subtract),
                    node: &Node::Number(D1),
                },
                operator: Operator::Comparison(ComparisonOperator::NotEqual),
            },
        },
        StandardTest {
            src: "[a, b, c]",
            result: &Node::Array(&[
                &Node::Identifier("a"),
                &Node::Identifier("b"),
                &Node::Identifier("c"),
            ]),
        },
        StandardTest {
            src: "[9].foo",
            result: &Node::Member {
                node: &Node::Array(&[&Node::Number(D9)]),
                property: &Node::String("foo"),
            },
        },
        StandardTest {
            src: "x not in (1..9]",
            result: &Node::Binary {
                left: &Node::Identifier("x"),
                operator: Operator::Comparison(ComparisonOperator::NotIn),
                right: &Node::Interval {
                    left_bracket: "(",
                    left: &Node::Number(D1),
                    right: &Node::Number(D9),
                    right_bracket: "]",
                },
            },
        },
        StandardTest {
            src: "not in_var",
            result: &Node::Unary {
                operator: Operator::Logical(LogicalOperator::Not),
                node: &Node::Identifier("in_var"),
            },
        },
        StandardTest {
            src: "array[1:2]",
            result: &Node::Slice {
                node: &Node::Identifier("array"),
                from: Some(&Node::Number(D1)),
                to: Some(&Node::Number(D2)),
            },
        },
        StandardTest {
            src: "array[:2]",
            result: &Node::Slice {
                node: &Node::Identifier("array"),
                from: None,
                to: Some(&Node::Number(D2)),
            },
        },
        StandardTest {
            src: "array[1:]",
            result: &Node::Slice {
                node: &Node::Identifier("array"),
                from: Some(&Node::Number(D1)),
                to: None,
            },
        },
        StandardTest {
            src: "array[:]",
            result: &Node::Slice {
                node: &Node::Identifier("array"),
                from: None,
                to: None,
            },
        },
        StandardTest {
            src: "[]",
            result: &Node::Array(&[]),
        },
        StandardTest {
            src: "0 in []",
            result: &Node::Binary {
                left: &Node::Number(D0),
                operator: Operator::Comparison(ComparisonOperator::In),
                right: &Node::Array(&[]),
            },
        },
        StandardTest {
            src: "25 + 2.5",
            result: &Node::Binary {
                left: &Node::Number(D25),
                right: &Node::Number(D2P5),
                operator: Operator::Arithmetic(ArithmeticOperator::Add),
            },
        },
    ]);

    let mut lexer = Lexer::new();
    let mut bump = Bump::new();

    for StandardTest { src, result } in tests {
        let tokens = lexer.tokenize(src).unwrap();
        let unary_parser = Parser::try_new(tokens, &bump).unwrap().standard();
        let parser_result = unary_parser.parse();
        // let Ok(ast) = parser_result else {
        //     assert!(
        //         false,
        //         "Failed on expression: {}. Error: {:?}.",
        //         src,
        //         parser_result.unwrap_err()
        //     );
        //     return;
        // };

        assert!(parser_result.error().is_ok(), "Expression failed: {src}");
        assert_eq!(parser_result.root, result, "Failed on expression: {}", src);

        bump.reset();
    }
}

#[test]
fn failure_tests() {
    let tests: Vec<&str> = Vec::from(["a + b ++", "null.nested.property", "false.nested.property"]);

    let mut lexer = Lexer::new();
    let mut bump = Bump::new();

    for test in tests {
        let tokens = lexer.tokenize(test).unwrap();
        let parser = Parser::try_new(tokens, &bump).unwrap().standard();
        let parser_result = parser.parse();

        assert!(parser_result.error().is_err(), "{parser_result:?}");

        bump.reset();
    }
}
