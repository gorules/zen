mod error;
mod interpreter;
mod lexer;
mod parser;

use crate::interpreter::Interpreter;
use crate::lexer::Lexer;
use crate::parser::Parser;
use zen_expression::variable::Variable;

pub use crate::error::{ParserError, TemplateRenderError};

pub fn render(template: &str, context: Variable) -> Result<Variable, TemplateRenderError> {
    let tokens = Lexer::from(template.trim()).collect();
    let nodes = Parser::from(tokens.as_slice()).collect()?;

    Interpreter::from(nodes.as_slice()).collect_for(context)
}
