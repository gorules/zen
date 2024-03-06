mod interpreter;
mod lexer;
mod parser;

use serde_json::Value;

use crate::interpreter::Interpreter;
use crate::lexer::Lexer;
use crate::parser::Parser;

pub fn render(template: &str, context: &Value) -> Value {
    let tokens = Lexer::from(template).collect();
    let nodes = Parser::from(tokens.as_slice()).collect();

    Interpreter::from(nodes.as_slice()).collect_for(context)
}
