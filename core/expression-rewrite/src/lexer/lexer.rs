use crate::lexer::codes::{is_token_type, token_type};
use crate::lexer::cursor::{Cursor, CursorItem};
use crate::lexer::error::LexerError;
use crate::lexer::error::LexerError::{UnexpectedEof, UnmatchedSymbol};
use crate::lexer::token::{
    Bracket, ComparisonOperator, Identifier, LogicalOperator, Operator, Token, TokenKind,
};

type VoidResult = Result<(), LexerError>;

#[derive(Debug, Default)]
pub struct Lexer<'arena> {
    tokens: Vec<Token<'arena>>,
}

impl<'arena> Lexer<'arena> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tokenize(&mut self, source: &'arena str) -> Result<&[Token<'arena>], LexerError> {
        self.tokens.clear();

        Scanner::new(source, &mut self.tokens).scan()?;
        Ok(&self.tokens)
    }
}

struct Scanner<'arena, 'self_ref> {
    cursor: Cursor<'arena>,
    tokens: &'self_ref mut Vec<Token<'arena>>,
    source: &'arena str,
}

impl<'arena, 'self_ref> Scanner<'arena, 'self_ref> {
    pub fn new(source: &'arena str, tokens: &'self_ref mut Vec<Token<'arena>>) -> Self {
        Self {
            cursor: Cursor::from(source),
            source,
            tokens,
        }
    }

    pub fn scan(&mut self) -> VoidResult {
        while let Some((i, s)) = self.cursor.peek() {
            match s {
                token_type!("space") => {
                    self.cursor.next();
                    Ok(())
                }
                token_type!("quote") => self.string(),
                token_type!("digit") => self.number(),
                token_type!("bracket") => self.bracket(),
                token_type!("cmp_operator") => self.cmp_operator(),
                token_type!("operator") => self.operator(),
                '.' => self.dot(),
                token_type!("alpha") => self.identifier(),

                _ => Err(UnmatchedSymbol {
                    symbol: s,
                    position: i,
                }),
            }?;
        }

        Ok(())
    }

    fn next(&self) -> Result<CursorItem, LexerError> {
        self.cursor.next().ok_or_else(|| {
            let (a, b) = self.cursor.peek_back().unwrap_or((0, ' '));

            UnexpectedEof {
                symbol: b,
                position: a,
            }
        })
    }

    fn push(&mut self, token: Token<'arena>) {
        self.tokens.push(token);
    }

    fn string(&mut self) -> VoidResult {
        let (start, opener) = self.next()?;
        let end: usize;

        loop {
            let (e, c) = self.next()?;
            if c == opener {
                end = e;
                break;
            }
        }

        self.push(Token {
            kind: TokenKind::String,
            span: (start, end),
            value: &self.source[start + 1..end],
        });

        Ok(())
    }

    fn number(&mut self) -> VoidResult {
        let (start, _) = self.next()?;
        let mut end = start;
        let mut fractal = false;

        while let Some((e, c)) = self
            .cursor
            .next_if(|c| is_token_type!(c, "digit") || c == '_' || c == '.')
        {
            if fractal && c == '.' {
                self.cursor.back();
                break;
            }

            if c == '.' {
                if let Some((_, p)) = self.cursor.peek() {
                    if p == '.' {
                        self.cursor.back();
                        break;
                    }

                    fractal = true
                }
            }

            end = e;
        }

        self.push(Token {
            kind: TokenKind::Number,
            span: (start, end + 1),
            value: &self.source[start..=end],
        });

        Ok(())
    }

    fn bracket(&mut self) -> VoidResult {
        let (start, _) = self.next()?;

        let value = &self.source[start..=start];
        self.push(Token {
            kind: TokenKind::Bracket(Bracket::try_from(value)?),
            span: (start, start + 1),
            value,
        });

        Ok(())
    }

    fn dot(&mut self) -> VoidResult {
        let (start, _) = self.next()?;
        let mut end = start;

        if self.cursor.next_if(|c| c == '.').is_some() {
            end += 1;
        }

        let value = &self.source[start..=end];
        self.push(Token {
            kind: TokenKind::Operator(Operator::try_from(value)?),
            span: (start, end + 1),
            value,
        });

        Ok(())
    }

    fn cmp_operator(&mut self) -> VoidResult {
        let (start, _) = self.next()?;
        let mut end = start;

        if self.cursor.next_if(|c| c == '=').is_some() {
            end += 1;
        }

        let value = &self.source[start..=end];
        self.push(Token {
            kind: TokenKind::Operator(Operator::try_from(value)?),
            span: (start, end + 1),
            value,
        });

        Ok(())
    }

    fn operator(&mut self) -> VoidResult {
        let (start, _) = self.next()?;

        let value = &self.source[start..=start];
        self.push(Token {
            kind: TokenKind::Operator(Operator::try_from(value)?),
            span: (start, start + 1),
            value,
        });

        Ok(())
    }

    fn not(&mut self, start: usize) -> VoidResult {
        if self.cursor.next_if_is(" in ") {
            let end = self.cursor.position();

            self.push(Token {
                kind: TokenKind::Operator(Operator::Comparison(ComparisonOperator::NotIn)),
                span: (start, end - 1),
                value: "not in",
            })
        } else {
            let end = self.cursor.position();

            self.push(Token {
                kind: TokenKind::Operator(Operator::Logical(LogicalOperator::Not)),
                span: (start, end),
                value: "not",
            })
        }

        Ok(())
    }

    fn identifier(&mut self) -> VoidResult {
        let (start, _) = self.next()?;
        let mut end = start;

        while let Some((e, _)) = self.cursor.next_if(|c| is_token_type!(c, "alphanumeric")) {
            end = e;
        }

        let value = &self.source[start..=end];
        match value {
            "and" => self.push(Token {
                kind: TokenKind::Operator(Operator::Logical(LogicalOperator::And)),
                span: (start, end + 1),
                value,
            }),
            "or" => self.push(Token {
                kind: TokenKind::Operator(Operator::Logical(LogicalOperator::Or)),
                span: (start, end + 1),
                value,
            }),
            "in" => self.push(Token {
                kind: TokenKind::Operator(Operator::Comparison(ComparisonOperator::In)),
                span: (start, end + 1),
                value,
            }),
            "true" => self.push(Token {
                kind: TokenKind::Boolean(true),
                span: (start, end + 1),
                value,
            }),
            "false" => self.push(Token {
                kind: TokenKind::Boolean(false),
                span: (start, end + 1),
                value,
            }),
            "not" => self.not(start)?,
            _ => self.push(Token {
                kind: TokenKind::Identifier(Identifier::from(value)),
                span: (start, end + 1),
                value,
            }),
        }

        Ok(())
    }
}
