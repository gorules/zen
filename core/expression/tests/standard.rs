use bumpalo::Bump;
use rust_decimal::Decimal;

use rust_decimal_macros::dec;

use zen_expression::ast::Node;
use zen_expression::lexer::Lexer;
use zen_expression::parser::StandardParser;

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
                operator: "-",
                node: &Node::Number(D3),
            },
        },
        StandardTest {
            src: "1 - 2",
            result: &Node::Binary {
                left: &Node::Number(D1),
                operator: "-",
                right: &Node::Number(D2),
            },
        },
        StandardTest {
            src: "(1 - 2) * 3",
            result: &Node::Binary {
                left: &Node::Binary {
                    left: &Node::Number(D1),
                    operator: "-",
                    right: &Node::Number(D2),
                },
                operator: "*",
                right: &Node::Number(D3),
            },
        },
        StandardTest {
            src: "a or b or c",
            result: &Node::Binary {
                operator: "or",
                left: &Node::Binary {
                    left: &Node::Identifier("a"),
                    right: &Node::Identifier("b"),
                    operator: "or",
                },
                right: &Node::Identifier("c"),
            },
        },
        StandardTest {
            src: "a or b and c",
            result: &Node::Binary {
                operator: "or",
                left: &Node::Identifier("a"),
                right: &Node::Binary {
                    left: &Node::Identifier("b"),
                    right: &Node::Identifier("c"),
                    operator: "and",
                },
            },
        },
        StandardTest {
            src: "(a or b) and c",
            result: &Node::Binary {
                operator: "and",
                left: &Node::Binary {
                    left: &Node::Identifier("a"),
                    right: &Node::Identifier("b"),
                    operator: "or",
                },
                right: &Node::Identifier("c"),
            },
        },
        StandardTest {
            src: "2^4 - 1",
            result: &Node::Binary {
                left: &Node::Binary {
                    operator: "^",
                    left: &Node::Number(D2),
                    right: &Node::Number(D4),
                },
                operator: "-",
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
                operator: "==",
            },
        },
        StandardTest {
            src: "+0 != -1",
            result: &Node::Binary {
                left: &Node::Unary {
                    operator: "+",
                    node: &Node::Number(D0),
                },
                right: &Node::Unary {
                    operator: "-",
                    node: &Node::Number(D1),
                },
                operator: "!=",
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
                operator: "not in",
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
                operator: "not",
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
                operator: "in",
                right: &Node::Array(&[]),
            },
        },
    ]);

    let lexer = Lexer::new();
    let mut bump = Bump::new();

    for StandardTest { src, result } in tests {
        let t_res = lexer.tokenize(src).unwrap();
        let tokens = t_res.borrow();
        let unary_parser = StandardParser::try_new(tokens.as_ref(), &bump).unwrap();
        let ast = unary_parser.parse();
        assert_eq!(ast.unwrap(), result);

        bump.reset();
    }
}

#[test]
fn failure_tests() {
    let tests: Vec<&str> = Vec::from(["a + b ++"]);

    let lexer = Lexer::new();
    let mut bump = Bump::new();

    for test in tests {
        let t_res = lexer.tokenize(test).unwrap();
        let tokens = t_res.borrow();
        let unary_parser = StandardParser::try_new(tokens.as_ref(), &bump).unwrap();
        let ast = unary_parser.parse();
        assert!(ast.is_err());

        bump.reset();
    }
}
