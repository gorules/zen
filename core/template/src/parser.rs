use crate::error::{ParserError, TemplateRenderError};
use crate::lexer::Token;
use std::iter::Peekable;
use std::slice::Iter;

#[derive(Debug, PartialOrd, PartialEq)]
pub(crate) enum Node<'a> {
    Text(&'a str),
    Expression(&'a str),
}

#[derive(Debug, PartialOrd, PartialEq)]
enum ParserState {
    Text,
    Expression,
}

pub(crate) struct Parser<'source, 'tokens> {
    cursor: Peekable<Iter<'tokens, Token<'source>>>,
    state: ParserState,
    nodes: Vec<Node<'source>>,
}

impl<'source, 'tokens, T> From<T> for Parser<'source, 'tokens>
where
    T: Into<&'tokens [Token<'source>]>,
{
    fn from(value: T) -> Self {
        let tokens = value.into();
        let cursor = tokens.iter().peekable();

        Self {
            cursor,
            nodes: Default::default(),
            state: ParserState::Text,
        }
    }
}

impl<'source, 'tokens> Parser<'source, 'tokens> {
    pub(crate) fn collect(mut self) -> Result<Vec<Node<'source>>, TemplateRenderError> {
        while let Some(token) = self.cursor.next() {
            match token {
                Token::Text(text) => self.text(text),
                Token::OpenBracket => self.open_bracket()?,
                Token::CloseBracket => self.close_bracket()?,
            }
        }

        Ok(self.nodes)
    }

    fn text(&mut self, data: &'source str) {
        match self.state {
            ParserState::Text => self.nodes.push(Node::Text(data)),
            ParserState::Expression => self.nodes.push(Node::Expression(data)),
        }
    }

    fn open_bracket(&mut self) -> Result<(), ParserError> {
        if self.state == ParserState::Expression {
            return Err(ParserError::OpenBracket);
        }

        self.state = ParserState::Expression;
        Ok(())
    }

    fn close_bracket(&mut self) -> Result<(), ParserError> {
        if self.state != ParserState::Expression {
            return Err(ParserError::CloseBracket);
        }

        self.state = ParserState::Text;
        Ok(())
    }
}
