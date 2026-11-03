use crate::compiled::diagnostics::Paths;
use crate::compiled::output::OutputColumn;
use crate::compiled::shred::Shredder;
use crate::compiled::ColumnarOutput;
use crate::workspace::types::{EvaluationError, EvaluationResult};
use std::rc::Rc;
use std::sync::Arc;

pub(super) fn output<'a>(
    rows: usize,
    columns: Vec<(Arc<str>, OutputColumn<'a>)>,
    errors: Vec<Option<EvaluationError>>,
    paths: Paths,
) -> ColumnarOutput<'a> {
    ColumnarOutput {
        rows,
        columns,
        errors: errors
            .into_iter()
            .map(|error| error.map(|error| Rc::new(crate::EvaluationError::Policy(error))))
            .collect(),
        paths,
    }
}

pub(super) fn from_results<'a>(
    rows: usize,
    results: Vec<Result<EvaluationResult, EvaluationError>>,
) -> ColumnarOutput<'a> {
    let (columns, errors) = Shredder::results(results, |result: &EvaluationResult| &result.output);
    output(
        rows,
        columns,
        errors,
        Paths {
            hosted: rows,
            ..Paths::default()
        },
    )
}
