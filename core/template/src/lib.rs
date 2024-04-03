mod error;
mod interpreter;
mod lexer;
mod parser;

use serde_json::Value;

use crate::interpreter::Interpreter;
use crate::lexer::Lexer;
use crate::parser::Parser;

pub use crate::error::{ParserError, TemplateRenderError};

pub fn render(template: &str, context: &Value) -> Result<Value, TemplateRenderError> {
    let tokens = Lexer::from(template.trim()).collect();
    let nodes = Parser::from(tokens.as_slice()).collect()?;

    Interpreter::from(nodes.as_slice()).collect_for(context)
}
