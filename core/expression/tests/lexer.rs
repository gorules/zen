use zen_expression::lexer::token::{Token, TokenKind};
use zen_expression::lexer::Lexer;

struct LexerTest {
    test: &'static str,
    result: Vec<Token<'static>>,
}

#[test]
fn lexer_test() {
    let tests: Vec<LexerTest> = Vec::from([
        LexerTest {
            test: "'hello'",
            result: Vec::from([Token {
                kind: TokenKind::String,
                span: (0, 6),
                value: "hello",
            }]),
        },
        LexerTest {
            test: "'double' 'single' 'abc'",
            result: Vec::from([
                Token {
                    kind: TokenKind::String,
                    span: (0, 7),
                    value: "double",
                },
                Token {
                    kind: TokenKind::String,
                    span: (9, 16),
                    value: "single",
                },
                Token {
                    kind: TokenKind::String,
                    span: (18, 22),
                    value: "abc",
                },
            ]),
        },
        LexerTest {
            test: "'double' 'single' 'abc'",
            result: Vec::from([
                Token {
                    kind: TokenKind::String,
                    span: (0, 7),
                    value: "double",
                },
                Token {
                    kind: TokenKind::String,
                    span: (9, 16),
                    value: "single",
                },
                Token {
                    kind: TokenKind::String,
                    span: (18, 22),
                    value: "abc",
                },
            ]),
        },
        LexerTest {
            test: "'double' 'single' 'abc'",
            result: Vec::from([
                Token {
                    kind: TokenKind::String,
                    span: (0, 7),
                    value: "double",
                },
                Token {
                    kind: TokenKind::String,
                    span: (9, 16),
                    value: "single",
                },
                Token {
                    kind: TokenKind::String,
                    span: (18, 22),
                    value: "abc",
                },
            ]),
        },
        LexerTest {
            test: "0.5 0.025 1 02 1_000_000 _42 -0.5",
            result: Vec::from([
                Token {
                    kind: TokenKind::Number,
                    span: (0, 3),
                    value: "0.5",
                },
                Token {
                    kind: TokenKind::Number,
                    span: (4, 9),
                    value: "0.025",
                },
                Token {
                    kind: TokenKind::Number,
                    span: (10, 11),
                    value: "1",
                },
                Token {
                    kind: TokenKind::Number,
                    span: (12, 14),
                    value: "02",
                },
                Token {
                    kind: TokenKind::Number,
                    span: (15, 24),
                    value: "1_000_000",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (25, 28),
                    value: "_42",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (29, 30),
                    value: "-",
                },
                Token {
                    kind: TokenKind::Number,
                    span: (30, 33),
                    value: "0.5",
                },
            ]),
        },
        LexerTest {
            test: "a and orb().val",
            result: Vec::from([
                Token {
                    kind: TokenKind::Identifier,
                    span: (0, 1),
                    value: "a",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (2, 5),
                    value: "and",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (6, 9),
                    value: "orb",
                },
                Token {
                    kind: TokenKind::Bracket,
                    span: (9, 10),
                    value: "(",
                },
                Token {
                    kind: TokenKind::Bracket,
                    span: (10, 11),
                    value: ")",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (11, 12),
                    value: ".",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (12, 15),
                    value: "val",
                },
            ]),
        },
        LexerTest {
            test: "foo.bar",
            result: Vec::from([
                Token {
                    kind: TokenKind::Identifier,
                    span: (0, 3),
                    value: "foo",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (3, 4),
                    value: ".",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (4, 7),
                    value: "bar",
                },
            ]),
        },
        LexerTest {
            test: "foo .bar == .baz",
            result: Vec::from([
                Token {
                    kind: TokenKind::Identifier,
                    span: (0, 3),
                    value: "foo",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (4, 5),
                    value: ".",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (5, 8),
                    value: "bar",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (9, 11),
                    value: "==",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (12, 13),
                    value: ".",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (13, 16),
                    value: "baz",
                },
            ]),
        },
        LexerTest {
            test: "func()",
            result: Vec::from([
                Token {
                    kind: TokenKind::Identifier,
                    span: (0, 4),
                    value: "func",
                },
                Token {
                    kind: TokenKind::Bracket,
                    span: (4, 5),
                    value: "(",
                },
                Token {
                    kind: TokenKind::Bracket,
                    span: (5, 6),
                    value: ")",
                },
            ]),
        },
        LexerTest {
            test: "not abc not in i not(false) not  ",
            result: Vec::from([
                Token {
                    kind: TokenKind::Operator,
                    span: (0, 3),
                    value: "not",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (4, 7),
                    value: "abc",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (8, 14),
                    value: "not in",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (15, 16),
                    value: "i",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (17, 20),
                    value: "not",
                },
                Token {
                    kind: TokenKind::Bracket,
                    span: (20, 21),
                    value: "(",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (21, 26),
                    value: "false",
                },
                Token {
                    kind: TokenKind::Bracket,
                    span: (26, 27),
                    value: ")",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (28, 31),
                    value: "not",
                },
            ]),
        },
        LexerTest {
            test: "not in_var",
            result: Vec::from([
                Token {
                    kind: TokenKind::Operator,
                    span: (0, 3),
                    value: "not",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (4, 10),
                    value: "in_var",
                },
            ]),
        },
        LexerTest {
            test: "[1..5)",
            result: Vec::from([
                Token {
                    kind: TokenKind::Bracket,
                    span: (0, 1),
                    value: "[",
                },
                Token {
                    kind: TokenKind::Number,
                    span: (1, 2),
                    value: "1",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (2, 4),
                    value: "..",
                },
                Token {
                    kind: TokenKind::Number,
                    span: (4, 5),
                    value: "5",
                },
                Token {
                    kind: TokenKind::Bracket,
                    span: (5, 6),
                    value: ")",
                },
            ]),
        },
        LexerTest {
            test: "product.price > 500 ? 'hello'  :   'world'",
            result: Vec::from([
                Token {
                    kind: TokenKind::Identifier,
                    span: (0, 7),
                    value: "product",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (7, 8),
                    value: ".",
                },
                Token {
                    kind: TokenKind::Identifier,
                    span: (8, 13),
                    value: "price",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (14, 15),
                    value: ">",
                },
                Token {
                    kind: TokenKind::Number,
                    span: (16, 19),
                    value: "500",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (20, 21),
                    value: "?",
                },
                Token {
                    kind: TokenKind::String,
                    span: (22, 28),
                    value: "hello",
                },
                Token {
                    kind: TokenKind::Operator,
                    span: (31, 32),
                    value: ":",
                },
                Token {
                    kind: TokenKind::String,
                    span: (35, 41),
                    value: "world",
                },
            ]),
        },
    ]);

    let lexer = Lexer::new();

    for LexerTest { test, result } in tests {
        let tokens = lexer.tokenize(test);

        assert!(tokens.is_ok());
        assert_eq!(tokens.unwrap().borrow().as_slice(), result);
    }
}
