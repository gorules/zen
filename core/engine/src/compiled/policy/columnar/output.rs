use crate::compiled::columnar::OutputColumn;
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
}

impl PolicyColumnarOutput<'_> {
    pub fn row(&self, row: usize) -> Variable {
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
                    shredder.visit(&result.output, 0, row);
                    errors.push(None);
                }
                Err(error) => errors.push(Some(error)),
            }
        }
        PolicyColumnarOutput {
            rows,
            columns: Sheet::columns(shredder.finish()),
            errors,
            hosted: rows,
            typed: 0,
            laned: 0,
        }
    }
}
