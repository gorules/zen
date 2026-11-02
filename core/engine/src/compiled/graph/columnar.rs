use crate::compiled::graph::data::{Data, Layer};
use crate::compiled::graph::plan::PlanOutcome;
use crate::compiled::graph::CompiledGraph;
use crate::compiled::leaf::Bits;
use crate::compiled::leaf::Leaf;
use crate::compiled::leaf::Mask;
use crate::compiled::output::OutputColumn;
use crate::compiled::shred::{Normalizer, Shredder};
use crate::decision_graph::cleaner::{VariableCleaner, ZEN_RESERVED_PROPERTIES};
use crate::diagnostics::Paths;
use crate::model::GraphContent;
use crate::nodes::input::dates::DeclaredDates;
use crate::nodes::{NodeError, NodeHandlerExtensions};
use crate::EvaluationError;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Columns, Dictionary, Values};
use zen_expression::Variable;

#[derive(Default)]
pub(crate) struct Admitted<'a> {
    pub failures: Vec<(usize, Rc<EvaluationError>)>,
    pub dated: Vec<(usize, Leaf<'a>)>,
}

pub struct ColumnarOutput<'a> {
    pub rows: usize,
    pub columns: Vec<(Arc<str>, OutputColumn<'a>)>,
    pub errors: Vec<Option<Rc<EvaluationError>>>,
    pub(crate) paths: Paths,
}

impl ColumnarOutput<'_> {
    pub(crate) fn from_results(
        results: Vec<Result<crate::DecisionGraphResponse, Box<EvaluationError>>>,
    ) -> Self {
        let rows = results.len();
        let (columns, errors) =
            Shredder::results(results, |response: &crate::DecisionGraphResponse| {
                &response.result
            });
        ColumnarOutput {
            rows,
            columns,
            errors: errors
                .into_iter()
                .map(|error| error.map(Rc::from))
                .collect(),
            paths: Paths {
                hosted: rows,
                ..Paths::default()
            },
        }
    }

    pub fn row(&self, row: usize) -> Variable {
        OutputColumn::row(&self.columns, row)
    }

    #[doc(hidden)]
    pub fn paths(&self) -> Paths {
        self.paths
    }

    pub(crate) fn splice(
        self,
        rows: &[usize],
        results: Vec<Result<crate::DecisionGraphResponse, Box<EvaluationError>>>,
    ) -> Self {
        let mut values: Vec<Result<Variable, Rc<EvaluationError>>> = (0..self.rows)
            .map(|row| match self.errors.get(row).cloned().flatten() {
                Some(error) => Err(error),
                None => Ok(self.row(row)),
            })
            .collect();
        for (&row, result) in rows.iter().zip(results) {
            if let Some(slot) = values.get_mut(row) {
                *slot = result.map(|response| response.result).map_err(Rc::from);
            }
        }
        let (columns, errors) = Shredder::results(values, |value: &Variable| value);
        ColumnarOutput {
            rows: self.rows,
            columns,
            errors,
            paths: Paths {
                hosted: self.paths.hosted + rows.len(),
                ..self.paths
            },
        }
    }
}

impl CompiledGraph {
    fn input_data<'a>(columns: &Columns<'a>) -> Data<'a> {
        let rows = columns.rows;
        let mut paths: Vec<Arc<str>> = Vec::with_capacity(columns.columns.len());
        let mut leaves: Vec<Leaf<'a>> = Vec::with_capacity(columns.columns.len());
        for (path, column) in &columns.columns {
            paths.push(Arc::from(*path));
            leaves.push(Normalizer::input(*column, rows));
        }
        let masked = columns
            .columns
            .iter()
            .any(|(_, column)| column.validity.is_some());
        let present = masked.then(|| {
            columns
                .columns
                .iter()
                .zip(&leaves)
                .map(|((_, column), leaf)| match (leaf, column.validity) {
                    (Leaf::Input(..), _) => Mask::Valid(leaf.clone()),
                    (_, Some((bits, offset))) => {
                        Mask::Bits(Bits::window(bits, offset, rows).into())
                    }
                    (_, None) => Mask::Bits(Bits::ones(rows).into()),
                })
                .collect()
        });
        let layer = Layer::new(paths.into(), leaves, present);
        Data::record(&(0..rows).collect::<Rc<[usize]>>(), layer, None)
    }

    async fn planned_input<'a>(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        columns: &Columns<'_>,
    ) -> Option<Admitted<'a>> {
        let step = self
            .steps
            .iter()
            .flatten()
            .find(|step| matches!(step.kind, crate::compiled::graph::Kind::Input))?;
        if !Self::columnar_input(step, content, extensions) {
            return None;
        }
        let validator = Self::validator(step, content, extensions);
        if !step.schema && validator.is_none() {
            return Some(Admitted::default());
        }
        let data = Self::input_data(columns);
        let failures = self
            .validate_input(step, &data, &[], content, extensions)
            .await;
        Some(Admitted {
            failures: failures
                .into_iter()
                .map(|(row, message)| {
                    (
                        row,
                        Rc::new(NodeError::new(step.node.id.clone(), message).into()),
                    )
                })
                .collect(),
            dated: validator
                .map(|(_, schema)| Self::dated(&schema, columns))
                .unwrap_or_default(),
        })
    }

    fn dated<'a>(schema: &serde_json::Value, columns: &Columns<'_>) -> Vec<(usize, Leaf<'a>)> {
        if !DeclaredDates::mentions(schema) {
            return Vec::new();
        }
        columns
            .columns
            .iter()
            .enumerate()
            .filter_map(|(index, (path, column))| {
                let schema =
                    DeclaredDates::at(schema, path).filter(|s| DeclaredDates::mentions(s))?;
                let values: Rc<[Variable]> = (0..columns.rows)
                    .map(|row| {
                        let value = column.variable(row);
                        DeclaredDates::prepare(&value, Some(schema)).unwrap_or(value)
                    })
                    .collect();
                Some((index, Leaf::Any(values)))
            })
            .collect()
    }

    fn reserved_inside(value: &Variable) -> bool {
        match value {
            Variable::Object(map) => {
                let map = map.borrow();
                ZEN_RESERVED_PROPERTIES
                    .iter()
                    .any(|key| map.get_str(key).is_some())
                    || map.iter().any(|(_, child)| Self::reserved_inside(child))
            }
            Variable::Array(items) => items.borrow().iter().any(Self::reserved_inside),
            _ => false,
        }
    }

    pub(crate) fn cleaned<'a>(leaf: &Leaf<'a>) -> Leaf<'a> {
        if !leaf.objects() {
            return leaf.clone();
        }
        let reserved = match leaf {
            Leaf::Any(values) => values.iter().any(Self::reserved_inside),
            _ => match leaf.column().values {
                Values::Any(values) => values.iter().any(Self::reserved_inside),
                Values::Dict {
                    values: Dictionary::Any(values),
                    ..
                } => values.iter().any(Self::reserved_inside),
                _ => (0..leaf.len()).any(|row| Self::reserved_inside(&leaf.get(row))),
            },
        };
        if !reserved {
            return leaf.clone();
        }
        let mut cleaner = VariableCleaner::new();
        let values: Vec<Variable> = (0..leaf.len())
            .map(|row| {
                let value = leaf.get(row).deep_clone();
                cleaner.clean(&value);
                value
            })
            .collect();
        Leaf::Any(values.into())
    }

    pub(crate) async fn evaluate_columns<'a>(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        columns: &'a Columns<'a>,
    ) -> ColumnarOutput<'a> {
        if max_depth > 0 {
            if let Some(admitted) = self.planned_input(content, extensions, columns).await {
                match self
                    .plan()
                    .evaluate(self, content, extensions, max_depth, columns, admitted)
                    .await
                {
                    PlanOutcome::Skipped => {}
                    PlanOutcome::Done(output) => return output,
                    PlanOutcome::Partial {
                        output,
                        rows,
                        completed,
                    } => {
                        let inputs = Self::input_data(columns).materialized();
                        let inputs: Vec<Variable> =
                            rows.iter().map(|&row| inputs[row].clone()).collect();
                        let completed = completed.subset(&rows);
                        let results = self
                            .resumed(content, extensions, max_depth, &inputs, Some(&completed))
                            .await;
                        return output.splice(&rows, results);
                    }
                }
            }
        }
        let inputs = Self::input_data(columns).materialized();
        ColumnarOutput::from_results(self.evaluate(content, extensions, max_depth, &inputs).await)
    }
}
