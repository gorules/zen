use std::cell::RefCell;
use std::rc::Rc;

use crate::lexer::cursor::{Cursor, CursorItem};
use crate::lexer::error::LexerError;
use crate::lexer::error::LexerError::{UnexpectedEof, UnmatchedSymbol};
use crate::lexer::token::{Token, TokenKind};
use crate::{is_token_type, token_type};

type TokenSlice<'a> = Rc<RefCell<Vec<Token<'a>>>>;

type VoidResult = Result<(), LexerError>;

#[derive(Debug)]
pub struct Lexer<'a> {
    tokens: TokenSlice<'a>,
}

impl<'a> Default for Lexer<'a> {
    fn default() -> Self {
        Lexer::new()
    }
}

impl<'a> Lexer<'a> {
    pub fn new() -> Self {
        Self {
            tokens: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn tokenize(&self, source: &'a str) -> Result<TokenSlice<'a>, LexerError> {
        self.tokens.borrow_mut().clear();
        Scanner::new(source, self.tokens.clone()).scan()?;
        Ok(self.tokens.clone())
    }
}

struct Scanner<'a> {
    cursor: Cursor<'a>,
    tokens: TokenSlice<'a>,
    source: &'a str,
}

impl<'a> Scanner<'a> {
    pub fn new(source: &'a str, tokens: TokenSlice<'a>) -> Self {
        Self {
            cursor: Cursor::from(source),
            source,
            tokens,
        }
    }

    pub fn scan(&self) -> VoidResult {
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

    fn push(&self, token: Token<'a>) {
        self.tokens.borrow_mut().push(token);
    }

    fn string(&self) -> VoidResult {
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

    fn number(&self) -> VoidResult {
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

    fn bracket(&self) -> VoidResult {
        let (start, _) = self.next()?;

        self.push(Token {
            kind: TokenKind::Bracket,
            span: (start, start + 1),
            value: &self.source[start..=start],
        });

        Ok(())
    }

    fn dot(&self) -> VoidResult {
        let (start, _) = self.next()?;
        let mut end = start;

        if self.cursor.next_if(|c| c == '.').is_some() {
            end += 1;
        }

        self.push(Token {
            kind: TokenKind::Operator,
            span: (start, end + 1),
            value: &self.source[start..=end],
        });

        Ok(())
    }

    fn cmp_operator(&self) -> VoidResult {
        let (start, _) = self.next()?;
        let mut end = start;

        if self.cursor.next_if(|c| c == '=').is_some() {
            end += 1;
        }

        self.push(Token {
            kind: TokenKind::Operator,
            span: (start, end + 1),
            value: &self.source[start..=end],
        });

        Ok(())
    }

    fn operator(&self) -> VoidResult {
        let (start, _) = self.next()?;

        self.push(Token {
            kind: TokenKind::Operator,
            span: (start, start + 1),
            value: &self.source[start..=start],
        });

        Ok(())
    }

    fn not(&self, start: usize) -> VoidResult {
        if self.cursor.next_if_is(" in ") {
            let end = self.cursor.position();

            self.push(Token {
                kind: TokenKind::Operator,
                span: (start, end - 1),
                value: "not in",
            })
        } else {
            let end = self.cursor.position();

            self.push(Token {
                kind: TokenKind::Operator,
                span: (start, end),
                value: "not",
            })
        }

        Ok(())
    }

    fn identifier(&self) -> VoidResult {
        let (start, _) = self.next()?;
        let mut end = start;

        while let Some((e, _)) = self.cursor.next_if(|c| is_token_type!(c, "alphanumeric")) {
            end = e;
        }

        let value = &self.source[start..=end];
        match value {
            "and" | "or" | "in" => self.push(Token {
                kind: TokenKind::Operator,
                span: (start, end + 1),
                value,
            }),
            "not" => self.not(start)?,
            _ => self.push(Token {
                kind: TokenKind::Identifier,
                span: (start, end + 1),
                value,
            }),
        }

        Ok(())
    }
}
