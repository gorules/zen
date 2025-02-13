use bumpalo::Bump;

use crate::{compiler::Compiler, lexer::Lexer, parser::Parser};

pub struct ValidationError {
    pub error_type: String,
    pub source: String,
}

pub fn get_unary_error(expression: &str) -> Option<ValidationError> {
    let mut lexer = Lexer::new();
    let tokens = match lexer.tokenize(expression) {
        Err(e) => {
            return Some(ValidationError {
                error_type: "lexerError".to_string(),
                source: e.to_string(),
            })
        }
        Ok(tokens) => tokens,
    };

    let bump = Bump::new();
    let parser = match Parser::try_new(tokens, &bump) {
        Err(e) => {
            return Some(ValidationError {
                error_type: "parserError".to_string(),
                source: e.to_string(),
            })
        }
        Ok(p) => p.unary(),
    };

    let parser_result = parser.parse();
    match parser_result.error() {
        Err(e) => {
            return Some(ValidationError {
                error_type: "parserError".to_string(),
                source: e.to_string(),
            })
        }
        Ok(n) => n,
    };

    let mut compiler = Compiler::new();
    if let Err(e) = compiler.compile(parser_result.root) {
        return Some(ValidationError {
            error_type: "compilerError".to_string(),
            source: e.to_string(),
        });
    }

    None
}

pub fn get_error(expression: &str) -> Option<ValidationError> {
    let mut lexer = Lexer::new();
    let tokens = match lexer.tokenize(expression) {
        Err(e) => {
            return Some(ValidationError {
                error_type: "lexerError".to_string(),
                source: e.to_string(),
            })
        }
        Ok(tokens) => tokens,
    };

    let bump = Bump::new();
    let parser = match Parser::try_new(tokens, &bump) {
        Err(e) => {
            return Some(ValidationError {
                error_type: "parserError".to_string(),
                source: e.to_string(),
            })
        }
        Ok(p) => p.standard(),
    };

    let parser_result = parser.parse();
    match parser_result.error() {
        Err(e) => {
            return Some(ValidationError {
                error_type: "parserError".to_string(),
                source: e.to_string(),
            })
        }
        Ok(n) => n,
    };

    let mut compiler = Compiler::new();
    if let Err(e) = compiler.compile(parser_result.root) {
        return Some(ValidationError {
            error_type: "compilerError".to_string(),
            source: e.to_string(),
        });
    }

    None
}
