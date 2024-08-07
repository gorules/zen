use crate::lexer::codes::{is_token_type, token_type};
use crate::lexer::cursor::{Cursor, CursorItem};
use crate::lexer::error::LexerResult;
use crate::lexer::token::{
    Bracket, ComparisonOperator, Identifier, LogicalOperator, Operator, Token, TokenKind,
};
use crate::lexer::{LexerError, QuotationMark, TemplateString};
use std::str::FromStr;

#[derive(Debug, Default)]
pub struct Lexer<'arena> {
    tokens: Vec<Token<'arena>>,
}

impl<'arena> Lexer<'arena> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tokenize(&mut self, source: &'arena str) -> LexerResult<&[Token<'arena>]> {
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

    pub fn scan(&mut self) -> LexerResult<()> {
        while let Some(cursor_item) = self.cursor.peek() {
            self.scan_cursor_item(cursor_item)?;
        }

        Ok(())
    }

    pub(crate) fn scan_cursor_item(&mut self, cursor_item: CursorItem) -> LexerResult<()> {
        let (i, s) = cursor_item;

        match s {
            token_type!("space") => {
                self.cursor.next();
                Ok(())
            }
            '\'' => self.string(QuotationMark::SingleQuote),
            '"' => self.string(QuotationMark::DoubleQuote),
            token_type!("digit") => self.number(),
            token_type!("bracket") => self.bracket(),
            token_type!("cmp_operator") => self.cmp_operator(),
            token_type!("operator") => self.operator(),
            token_type!("question_mark") => self.question_mark(),
            '`' => self.template_string(),
            '.' => self.dot(),
            token_type!("alpha") => self.identifier(),
            _ => Err(LexerError::UnmatchedSymbol {
                symbol: s,
                position: i as u32,
            }),
        }
    }

    fn next(&self) -> LexerResult<CursorItem> {
        self.cursor.next().ok_or_else(|| {
            let (a, b) = self.cursor.peek_back().unwrap_or((0, ' '));

            LexerError::UnexpectedEof {
                symbol: b,
                position: a as u32,
            }
        })
    }

    fn push(&mut self, token: Token<'arena>) {
        self.tokens.push(token);
    }

    fn template_string(&mut self) -> LexerResult<()> {
        let (start, _) = self.next()?;

        self.tokens.push(Token {
            kind: TokenKind::QuotationMark(QuotationMark::Backtick),
            span: (start as u32, (start + 1) as u32),
            value: QuotationMark::Backtick.into(),
        });

        let mut in_expression = false;
        let mut str_start = start + 1;
        loop {
            let (e, c) = self.next()?;

            match (c, in_expression) {
                ('`', _) => {
                    if str_start < e {
                        self.tokens.push(Token {
                            kind: TokenKind::Literal,
                            span: (str_start as u32, e as u32),
                            value: &self.source[str_start..e],
                        });
                    }

                    self.tokens.push(Token {
                        kind: TokenKind::QuotationMark(QuotationMark::Backtick),
                        span: (e as u32, (e + 1) as u32),
                        value: QuotationMark::Backtick.into(),
                    });

                    break;
                }
                ('$', false) => {
                    in_expression = self.cursor.next_if_is("{");
                    if in_expression {
                        self.tokens.push(Token {
                            kind: TokenKind::Literal,
                            span: (str_start as u32, e as u32),
                            value: &self.source[str_start..e],
                        });

                        self.tokens.push(Token {
                            kind: TokenKind::TemplateString(TemplateString::ExpressionStart),
                            span: (e as u32, (e + 2) as u32),
                            value: TemplateString::ExpressionStart.into(),
                        });
                    }
                }
                ('}', true) => {
                    in_expression = false;
                    self.tokens.push(Token {
                        kind: TokenKind::TemplateString(TemplateString::ExpressionEnd),
                        span: (str_start as u32, e as u32),
                        value: TemplateString::ExpressionEnd.into(),
                    });

                    str_start = e + 1;
                }
                (_, false) => {
                    // Continue reading string
                }
                (_, true) => {
                    self.cursor.back();
                    self.scan_cursor_item((e, c))?;
                }
            }
        }

        Ok(())
    }

    fn string(&mut self, quote_kind: QuotationMark) -> LexerResult<()> {
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
            kind: TokenKind::QuotationMark(quote_kind),
            span: (start as u32, (start + 1) as u32),
            value: quote_kind.into(),
        });

        self.push(Token {
            kind: TokenKind::Literal,
            span: ((start + 1) as u32, end as u32),
            value: &self.source[start + 1..end],
        });

        self.push(Token {
            kind: TokenKind::QuotationMark(quote_kind),
            span: (end as u32, (end + 1) as u32),
            value: quote_kind.into(),
        });

        Ok(())
    }

    fn number(&mut self) -> LexerResult<()> {
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
            span: (start as u32, (end + 1) as u32),
            value: &self.source[start..=end],
        });

        Ok(())
    }

    fn bracket(&mut self) -> LexerResult<()> {
        let (start, _) = self.next()?;

        let value = &self.source[start..=start];
        let span = (start as u32, (start + 1) as u32);
        self.push(Token {
            kind: TokenKind::Bracket(Bracket::from_str(value).map_err(|_| {
                LexerError::UnexpectedSymbol {
                    symbol: value.to_string(),
                    span,
                }
            })?),
            span,
            value,
        });

        Ok(())
    }

    fn dot(&mut self) -> LexerResult<()> {
        let (start, _) = self.next()?;
        let mut end = start;

        if self.cursor.next_if(|c| c == '.').is_some() {
            end += 1;
        }

        let value = &self.source[start..=end];
        let span = (start as u32, (end + 1) as u32);
        self.push(Token {
            kind: TokenKind::Operator(Operator::from_str(value).map_err(|_| {
                LexerError::UnexpectedSymbol {
                    symbol: value.to_string(),
                    span,
                }
            })?),
            span,
            value,
        });

        Ok(())
    }

    fn cmp_operator(&mut self) -> LexerResult<()> {
        let (start, _) = self.next()?;
        let mut end = start;

        if self.cursor.next_if(|c| c == '=').is_some() {
            end += 1;
        }

        let value = &self.source[start..=end];
        self.push(Token {
            kind: TokenKind::Operator(Operator::from_str(value).map_err(|_| {
                LexerError::UnexpectedSymbol {
                    symbol: value.to_string(),
                    span: (start as u32, (end + 1) as u32),
                }
            })?),
            span: (start as u32, (end + 1) as u32),
            value,
        });

        Ok(())
    }

    fn question_mark(&mut self) -> LexerResult<()> {
        let (start, _) = self.next()?;
        let mut kind = TokenKind::Operator(Operator::QuestionMark);
        let mut end = start;

        if self.cursor.next_if(|c| c == '?').is_some() {
            kind = TokenKind::Operator(Operator::Logical(LogicalOperator::NullishCoalescing));
            end += 1;
        }

        let value = &self.source[start..=end];
        self.push(Token {
            kind,
            value,
            span: (start as u32, (end + 1) as u32),
        });

        Ok(())
    }

    fn operator(&mut self) -> LexerResult<()> {
        let (start, _) = self.next()?;

        let value = &self.source[start..=start];
        let span = (start as u32, (start + 1) as u32);
        self.push(Token {
            kind: TokenKind::Operator(Operator::from_str(value).map_err(|_| {
                LexerError::UnexpectedSymbol {
                    symbol: value.to_string(),
                    span,
                }
            })?),
            span,
            value,
        });

        Ok(())
    }

    fn not(&mut self, start: usize) -> LexerResult<()> {
        if self.cursor.next_if_is(" in ") {
            let end = self.cursor.position();

            self.push(Token {
                kind: TokenKind::Operator(Operator::Comparison(ComparisonOperator::NotIn)),
                span: (start as u32, (end - 1) as u32),
                value: "not in",
            })
        } else {
            let end = self.cursor.position();

            self.push(Token {
                kind: TokenKind::Operator(Operator::Logical(LogicalOperator::Not)),
                span: (start as u32, end as u32),
                value: "not",
            })
        }

        Ok(())
    }

    fn identifier(&mut self) -> LexerResult<()> {
        let (start, _) = self.next()?;
        let mut end = start;

        while let Some((e, _)) = self.cursor.next_if(|c| is_token_type!(c, "alphanumeric")) {
            end = e;
        }

        let value = &self.source[start..=end];
        match value {
            "and" => self.push(Token {
                kind: TokenKind::Operator(Operator::Logical(LogicalOperator::And)),
                span: (start as u32, (end + 1) as u32),
                value,
            }),
            "or" => self.push(Token {
                kind: TokenKind::Operator(Operator::Logical(LogicalOperator::Or)),
                span: (start as u32, (end + 1) as u32),
                value,
            }),
            "in" => self.push(Token {
                kind: TokenKind::Operator(Operator::Comparison(ComparisonOperator::In)),
                span: (start as u32, (end + 1) as u32),
                value,
            }),
            "true" => self.push(Token {
                kind: TokenKind::Boolean(true),
                span: (start as u32, (end + 1) as u32),
                value,
            }),
            "false" => self.push(Token {
                kind: TokenKind::Boolean(false),
                span: (start as u32, (end + 1) as u32),
                value,
            }),
            "not" => self.not(start)?,
            _ => self.push(Token {
                kind: Identifier::try_from(value)
                    .map(|identifier| TokenKind::Identifier(identifier))
                    .unwrap_or(TokenKind::Literal),
                span: (start as u32, (end + 1) as u32),
                value,
            }),
        }

        Ok(())
    }
}
