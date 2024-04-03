use std::iter::Peekable;
use std::slice::Iter;

use crate::error::TemplateRenderError;
use serde_json::Value;
use zen_expression::Isolate;

use crate::parser::Node;

#[derive(Debug, PartialEq)]
pub(crate) enum InterpreterResult<'a> {
    String(&'a str),
    Value(Value),
}

#[derive(Debug)]
pub(crate) struct Interpreter<'source, 'nodes> {
    cursor: Peekable<Iter<'nodes, Node<'source>>>,
    isolate: Isolate<'source>,
    results: Vec<InterpreterResult<'source>>,
}

impl<'source, 'nodes, T> From<T> for Interpreter<'source, 'nodes>
where
    T: Into<&'nodes [Node<'source>]>,
{
    fn from(value: T) -> Self {
        let nodes = value.into();
        let cursor = nodes.iter().peekable();

        Self {
            cursor,
            isolate: Isolate::new(),
            results: Default::default(),
        }
    }
}

impl<'source, 'nodes> Interpreter<'source, 'nodes> {
    pub(crate) fn collect_for(mut self, context: &Value) -> Result<Value, TemplateRenderError> {
        self.isolate.set_environment(context);

        while let Some(node) = self.cursor.next() {
            match node {
                Node::Text(data) => self.text(data),
                Node::Expression(data) => self.expression(data)?,
            }
        }

        match self.results.len() {
            0 => Ok(Value::Null),
            1 => {
                let item = self.results.remove(0);
                match item {
                    InterpreterResult::Value(val) => Ok(val),
                    InterpreterResult::String(str) => Ok(Value::String(str.to_string())),
                }
            }
            _ => {
                let string_data = self
                    .results
                    .into_iter()
                    .map(|item| match item {
                        InterpreterResult::String(str) => str.to_string(),
                        InterpreterResult::Value(value) => value_to_string(value),
                    })
                    .collect::<String>();

                Ok(Value::String(string_data))
            }
        }
    }

    fn text(&mut self, data: &'source str) {
        self.results.push(InterpreterResult::String(data));
    }

    fn expression(&mut self, data: &'source str) -> Result<(), TemplateRenderError> {
        let result = self.isolate.run_standard(data)?;
        self.results.push(InterpreterResult::Value(result));
        Ok(())
    }
}

fn value_to_string(value: Value) -> String {
    match value {
        Value::Null => String::from("null"),
        Value::Bool(b) => match b {
            true => String::from("true"),
            false => String::from("false"),
        },
        Value::Number(n) => n.to_string(),
        Value::String(s) => s,
        Value::Array(arr) => Value::Array(arr).to_string(),
        Value::Object(obj) => Value::Object(obj).to_string(),
    }
}
