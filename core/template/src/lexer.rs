use std::iter::{Enumerate, Peekable};
use std::str::Chars;

#[derive(Debug, PartialOrd, PartialEq)]
pub(crate) enum Token<'source> {
    Text(&'source str),
    OpenBracket,
    CloseBracket,
}

pub(crate) struct Lexer<'source> {
    cursor: Peekable<Enumerate<Chars<'source>>>,
    source: &'source str,
    tokens: Vec<Token<'source>>,
    text_start: Option<usize>,
}

impl<'source, T> From<T> for Lexer<'source>
where
    T: Into<&'source str>,
{
    fn from(value: T) -> Self {
        let source: &'source str = value.into();

        Self {
            source,
            cursor: source.chars().enumerate().peekable(),
            tokens: Default::default(),
            text_start: None,
        }
    }
}

impl<'source> Lexer<'source> {
    pub fn collect(mut self) -> Vec<Token<'source>> {
        while let Some((index, char)) = self.cursor.next() {
            if char == '{' && matches!(self.cursor.peek(), Some((_, '{'))) {
                self.flush(index);

                self.cursor.next();
                self.tokens.push(Token::OpenBracket);
            } else if char == '}' && matches!(self.cursor.peek(), Some((_, '}'))) {
                self.flush(index);

                self.cursor.next();
                self.tokens.push(Token::CloseBracket);
            } else {
                self.text_start.get_or_insert(index);
            }
        }

        self.flush(self.source.len());
        self.tokens
    }

    fn flush(&mut self, index: usize) {
        if let Some(start) = self.text_start {
            self.tokens.push(Token::Text(&self.source[start..index]));
            self.text_start = None;
        }
    }
}
