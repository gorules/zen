use std::iter::Peekable;
use std::rc::Rc;
use std::slice::Iter;

use crate::error::TemplateRenderError;
use crate::parser::Node;
use zen_expression::variable::Variable;
use zen_expression::Isolate;

#[derive(Debug, PartialEq)]
pub(crate) enum InterpreterResult<'a> {
    String(&'a str),
    Variable(Variable),
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
    pub(crate) fn collect_for(
        mut self,
        context: Variable,
    ) -> Result<Variable, TemplateRenderError> {
        self.isolate.set_environment(context);

        while let Some(node) = self.cursor.next() {
            match node {
                Node::Text(data) => self.text(data),
                Node::Expression(data) => self.expression(data)?,
            }
        }

        match self.results.len() {
            0 => Ok(Variable::Null),
            1 => {
                let item = self.results.remove(0);
                match item {
                    InterpreterResult::Variable(val) => Ok(val),
                    InterpreterResult::String(str) => Ok(Variable::String(Rc::from(str))),
                }
            }
            _ => {
                let string_data = self
                    .results
                    .into_iter()
                    .map(|item| match item {
                        InterpreterResult::String(str) => str.to_string(),
                        InterpreterResult::Variable(value) => var_to_string(value),
                    })
                    .collect::<String>();

                Ok(Variable::String(Rc::from(string_data.as_str())))
            }
        }
    }

    fn text(&mut self, data: &'source str) {
        self.results.push(InterpreterResult::String(data));
    }

    fn expression(&mut self, data: &'source str) -> Result<(), TemplateRenderError> {
        let result = self.isolate.run_standard(data)?;
        self.results.push(InterpreterResult::Variable(result));
        Ok(())
    }
}

fn var_to_string(var: Variable) -> String {
    match var {
        Variable::String(s) => s.to_string(),
        _ => var.to_string(),
    }
}
