use crate::compiled::graph::data::{Data, Layer, Mask};
use crate::compiled::shred::Normalizer;
use crate::compiled::leaf::Leaf;
use crate::compiled::graph::CompiledGraph;
use crate::decision_graph::cleaner::{VariableCleaner, ZEN_RESERVED_PROPERTIES};
use crate::model::GraphContent;
use crate::nodes::NodeHandlerExtensions;
use crate::EvaluationError;
use std::rc::Rc;
use std::sync::Arc;
use crate::compiled::leaf::Bits;
use zen_expression::lane::{Column, Columns, Dictionary, Values};
use zen_expression::Variable;

pub struct OutputColumn<'a>(pub(crate) Leaf<'a>);

pub struct RecordsView<'c> {
    pub offsets: &'c [i32],
    pub valid: Vec<u64>,
    pub fields: Vec<(&'c str, Column<'c>, &'c [u64])>,
}

impl OutputColumn<'_> {
    pub fn column(&self) -> Column<'_> {
        self.0.column()
    }

    pub fn records(&self) -> Option<RecordsView<'_>> {
        let records = match &self.0 {
            Leaf::Records(records) => records,
            _ => return None,
        };
        Some(RecordsView {
            offsets: records.offsets(),
            valid: records.validity().to_vec(),
            fields: records.fields().iter().map(|(key, leaf, present)| (key.as_str(), leaf.column(), present.as_ref())).collect(),
        })
    }

    pub fn get(&self, row: usize) -> Variable {
        self.0.get(row)
    }
}

pub struct ColumnarOutput<'a> {
    pub rows: usize,
    pub columns: Vec<(Arc<str>, OutputColumn<'a>)>,
    pub errors: Vec<Option<Rc<EvaluationError>>>,
    pub typed: usize,
    pub laned: usize,
}

impl ColumnarOutput<'_> {
    pub(crate) fn from_results(results: Vec<Result<crate::DecisionGraphResponse, Box<EvaluationError>>>) -> Self {
        let rows = results.len();
        let mut paths: Vec<Arc<str>> = Vec::new();
        let mut values: Vec<Vec<Variable>> = Vec::new();
        let mut errors = Vec::with_capacity(rows);
        for (row, result) in results.into_iter().enumerate() {
            match result {
                Ok(response) => {
                    let mut entries = Vec::new();
                    CompiledGraph::flatten(&response.result, "", &mut entries);
                    for (path, value) in entries {
                        let at = match paths.iter().position(|p| p.as_ref() == path) {
                            Some(at) => at,
                            None => {
                                paths.push(Arc::from(path));
                                values.push(vec![Variable::Null; rows]);
                                values.len() - 1
                            }
                        };
                        values[at][row] = value;
                    }
                    errors.push(None);
                }
                Err(error) => errors.push(Some(Rc::from(error))),
            }
        }
        ColumnarOutput {
            rows,
            columns: paths
                .into_iter()
                .zip(values)
                .map(|(path, column)| (path, OutputColumn(Leaf::Any(column.into()))))
                .collect(),
            errors,
            typed: 0,
            laned: 0,
        }
    }

    pub fn row(&self, row: usize) -> Variable {
        let object = Variable::empty_object();
        for (path, column) in &self.columns {
            let value = column.get(row);
            match (path.is_empty(), value) {
                (_, Variable::Null) => {}
                (true, value) => return value,
                (false, value) => {
                    object.dot_insert(path, value);
                }
            }
        }
        object
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
        let masked = columns.columns.iter().any(|(_, column)| column.validity.is_some());
        let present = masked.then(|| {
            columns
                .columns
                .iter()
                .zip(&leaves)
                .map(|((_, column), leaf)| match (leaf, column.validity) {
                    (Leaf::Input(..), _) => Mask::Valid(leaf.clone()),
                    (_, Some((bits, offset))) => Mask::Bits(Bits::window(bits, offset, rows).into()),
                    (_, None) => Mask::Bits(Bits::ones(rows).into()),
                })
                .collect()
        });
        let layer = Layer::new(paths.into(), leaves, present);
        Data::record(&(0..rows).collect::<Rc<[usize]>>(), layer, None)
    }

    async fn planned_input(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        columns: &Columns<'_>,
    ) -> Option<Vec<(usize, Rc<EvaluationError>)>> {
        let step = self.steps.iter().flatten().find(|step| matches!(step.kind, crate::compiled::graph::Kind::Input))?;
        if !Self::columnar_input(step, content, extensions) {
            return None;
        }
        if !step.schema && Self::validator(step, content, extensions).is_none() {
            return Some(Vec::new());
        }
        let data = Self::input_data(columns);
        let failures = self.validate_input(step, &data, &[], content, extensions).await;
        Some(
            failures
                .into_iter()
                .map(|(row, message)| {
                    (
                        row,
                        Rc::new(EvaluationError::NodeError {
                            node_id: step.node.id.clone(),
                            source: message.into(),
                            trace: None,
                        }),
                    )
                })
                .collect(),
        )
    }

    fn reserved_inside(value: &Variable) -> bool {
        match value {
            Variable::Object(map) => {
                let map = map.borrow();
                ZEN_RESERVED_PROPERTIES.iter().any(|key| map.get_str(key).is_some())
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
                Values::Dict { values: Dictionary::Any(values), .. } => values.iter().any(Self::reserved_inside),
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

    fn flatten(value: &Variable, prefix: &str, out: &mut Vec<(String, Variable)>) {
        match value {
            Variable::Object(map) => {
                let start = out.len();
                for (key, child) in map.borrow().iter() {
                    if key.is_empty() || key.contains('.') {
                        out.truncate(start);
                        out.push((prefix.to_string(), value.clone()));
                        return;
                    }
                    let path = match prefix.is_empty() {
                        true => key.to_string(),
                        false => format!("{prefix}.{key}"),
                    };
                    Self::flatten(child, &path, out);
                }
            }
            Variable::Null => {}
            other => out.push((prefix.to_string(), other.clone())),
        }
    }

    pub(crate) async fn evaluate_columns<'a>(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        columns: &'a Columns<'a>,
    ) -> ColumnarOutput<'a> {
        if max_depth > 0 {
            if let Some(failures) = self.planned_input(content, extensions, columns).await {
                if let Some(output) = self.plan().evaluate(self, content, extensions, max_depth, columns, failures).await {
                    return output;
                }
            }
        }
        let inputs = Self::input_data(columns).materialized();
        ColumnarOutput::from_results(self.evaluate(content, extensions, max_depth, &inputs).await)
    }
}
