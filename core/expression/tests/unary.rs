use bumpalo::Bump;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use zen_expression::ast::Node;
use zen_expression::lexer::Lexer;
use zen_expression::parser::UnaryParser;

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
                operator: "==",
                right: &Node::String("str"),
            },
        },
        UnaryTest {
            src: "20.5",
            result: &Node::Binary {
                left: &Node::Identifier("$"),
                operator: "==",
                right: &Node::Number(D20P5),
            },
        },
        UnaryTest {
            src: "'a', 'b', 'c'",
            result: &Node::Binary {
                operator: "or",
                left: &Node::Binary {
                    left: &Node::Identifier("$"),
                    operator: "==",
                    right: &Node::String("a"),
                },
                right: &Node::Binary {
                    operator: "or",
                    left: &Node::Binary {
                        left: &Node::Identifier("$"),
                        operator: "==",
                        right: &Node::String("b"),
                    },
                    right: &Node::Binary {
                        left: &Node::Identifier("$"),
                        operator: "==",
                        right: &Node::String("c"),
                    },
                },
            },
        },
        UnaryTest {
            src: "[1..10]",
            result: &Node::Binary {
                operator: "in",
                left: &Node::Identifier("$"),
                right: &Node::Interval {
                    left_bracket: "[",
                    right_bracket: "]",
                    left: &Node::Number(D1),
                    right: &Node::Number(D10),
                },
            },
        },
        UnaryTest {
            src: "in [1..10]",
            result: &Node::Binary {
                operator: "in",
                left: &Node::Identifier("$"),
                right: &Node::Interval {
                    left_bracket: "[",
                    right_bracket: "]",
                    left: &Node::Number(D1),
                    right: &Node::Number(D10),
                },
            },
        },
        UnaryTest {
            src: "not in [1..10]",
            result: &Node::Binary {
                operator: "not in",
                left: &Node::Identifier("$"),
                right: &Node::Interval {
                    left_bracket: "[",
                    right_bracket: "]",
                    left: &Node::Number(D1),
                    right: &Node::Number(D10),
                },
            },
        },
        UnaryTest {
            src: "[1, 2, 3]",
            result: &Node::Binary {
                operator: "in",
                left: &Node::Identifier("$"),
                right: &Node::Array(&[&Node::Number(D1), &Node::Number(D2), &Node::Number(D3)]),
            },
        },
        UnaryTest {
            src: "date('2022-01-01')",
            result: &Node::Binary {
                operator: "==",
                left: &Node::Identifier("$"),
                right: &Node::BuiltIn {
                    name: "date",
                    arguments: &[&Node::String("2022-01-01")],
                },
            },
        },
        UnaryTest {
            src: "time('14:00:00')",
            result: &Node::Binary {
                operator: "==",
                left: &Node::Identifier("$"),
                right: &Node::BuiltIn {
                    name: "time",
                    arguments: &[&Node::String("14:00:00")],
                },
            },
        },
        UnaryTest {
            src: "< 50",
            result: &Node::Binary {
                operator: "<",
                left: &Node::Identifier("$"),
                right: &Node::Number(D50),
            },
        },
    ]);

    let lexer = Lexer::new();
    let mut bump = Bump::new();

    for UnaryTest { src, result } in tests {
        let t_res = lexer.tokenize(src).unwrap();
        let tokens = t_res.borrow();
        let unary_parser = UnaryParser::try_new(tokens.as_ref(), &bump).unwrap();
        let ast = unary_parser.parse().unwrap();
        assert_eq!(ast, result);

        bump.reset();
    }
}

#[test]
fn failure_tests() {
    let tests: Vec<&str> = Vec::from(["a + b ++", "a +++ b +--= fa", "null.a", "false.b"]);

    let lexer = Lexer::new();
    let mut bump = Bump::new();

    for test in tests {
        let t_res = lexer.tokenize(test).unwrap();
        let tokens = t_res.borrow();
        let unary_parser = UnaryParser::try_new(tokens.as_ref(), &bump).unwrap();
        let ast = unary_parser.parse();
        assert!(ast.is_err());

        bump.reset();
    }
}
