use crate::compiled::graph::columnar::OutputColumn;
use crate::compiled::shred::Shredder;
use crate::workspace::types::{EvaluationError, EvaluationResult};
use std::sync::Arc;
use zen_expression::Variable;
use super::Sheet;

/// Why rows left the columns for the row path (each row counted once, by
/// the first reason it met).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FallbackReason {
    /// Its reference names no item of its pool (or is not a string).
    Reference,
    /// Its date does not parse.
    Date,
    /// Its dates converted row by row (the input's shape is not one the
    /// columns prepare).
    Converted,
    /// An entity it iterates is read row by row.
    Entity,
    /// A block failed on its columns (decided again by row, to say why).
    Failed { block: Option<Arc<str>> },
    /// A block runs by row for it (a member's table, an entity handed over).
    Hosted { block: Arc<str> },
    /// The graph's nodes are not planned as columns: every row decided one
    /// by one.
    Unplanned,
    /// The document is not compiled: every row decided one by one.
    Uncompiled,
}

/// Why rows decided as they did, per block (asked for with `trace`): what a
/// row's trace would say of its root blocks. Rows left to the row path have
/// none here (trace them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reasons {
    /// A match: each row's arm (its id), `None` where no arm matched.
    Match { block: Arc<str>, arms: Vec<Option<Arc<str>>> },
    /// A decision table: each row's matched rules (indices, from 0).
    Table { block: Arc<str>, rules: Vec<Vec<u32>> },
}

/// Rows that left the columns for one reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    pub reason: FallbackReason,
    pub rows: usize,
}

/// Counts fallbacks by reason, in the order reasons first come.
#[derive(Debug, Default, Clone)]
pub(crate) struct Fallbacks(pub(crate) Vec<Fallback>);

impl Fallbacks {
    pub(crate) fn add(&mut self, reason: FallbackReason) {
        match self.0.iter_mut().find(|f| f.reason == reason) {
            Some(found) => found.rows += 1,
            None => self.0.push(Fallback { reason, rows: 1 }),
        }
    }
}

pub struct PolicyColumnarOutput<'a> {
    pub rows: usize,
    pub columns: Vec<(Arc<str>, OutputColumn<'a>)>,
    pub errors: Vec<Option<EvaluationError>>,
    pub hosted: usize,
    /// Why the hosted rows were (see [`FallbackReason`]).
    pub fallbacks: Vec<Fallback>,
    /// Why each row decided as it did (asked for; else empty).
    pub reasons: Vec<Reasons>,
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
            fallbacks: vec![Fallback { reason: FallbackReason::Uncompiled, rows }],
            reasons: Vec::new(),
            typed: 0,
            laned: 0,
            exact,
        }
    }
}
