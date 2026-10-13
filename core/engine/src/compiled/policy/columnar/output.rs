use crate::compiled::graph::columnar::OutputColumn;
use crate::compiled::shred::Shredder;
use crate::workspace::types::{EvaluationError, EvaluationResult};
use std::sync::Arc;
use zen_expression::Variable;
use super::Sheet;

pub struct PolicyColumnarOutput<'a> {
    pub rows: usize,
    pub columns: Vec<(Arc<str>, OutputColumn<'a>)>,
    pub errors: Vec<Option<EvaluationError>>,
    pub hosted: usize,
    pub typed: usize,
    pub laned: usize,
    /// Rows read whole: their value as decided, where columns drop part.
    pub(crate) exact: Vec<Option<Variable>>,
}

impl PolicyColumnarOutput<'_> {
    pub fn row(&self, row: usize) -> Variable {
        if let Some(Some(value)) = self.exact.get(row) {
            return value.clone();
        }
        let object = Variable::empty_object();
        for (path, column) in &self.columns {
            let value = column.get(row);
            if !matches!(value, Variable::Null) {
                object.dot_insert(path, value);
            }
        }
        object
    }

    pub(super) fn from_results<'a>(rows: usize, results: Vec<Result<EvaluationResult, EvaluationError>>) -> PolicyColumnarOutput<'a> {
        let mut shredder = Shredder::new(rows);
        let mut errors = Vec::with_capacity(rows);
        for (row, result) in results.into_iter().enumerate() {
            match result {
                Ok(result) => {
                    shredder.row(&result.output, row);
                    errors.push(None);
                }
                Err(error) => errors.push(Some(error)),
            }
        }
        let exact = shredder.exact();
        PolicyColumnarOutput {
            rows,
            columns: Sheet::columns(shredder.finish()),
            errors,
            hosted: rows,
            typed: 0,
            laned: 0,
            exact,
        }
    }
}
