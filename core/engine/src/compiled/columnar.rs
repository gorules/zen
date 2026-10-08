use crate::compiled::data::{Data, Layer, Mask, Shape};
use crate::compiled::typed::{ColumnBuilder, Input, Leaf};
use crate::compiled::{CompiledGraph, Source};
use crate::decision_graph::cleaner::{VariableCleaner, ZEN_RESERVED_PROPERTIES};
use crate::decision_graph::walker::GraphWalker;
use crate::model::GraphContent;
use crate::nodes::input::dates::DeclaredDates;
use crate::nodes::NodeHandlerExtensions;
use crate::EvaluationError;
use std::rc::Rc;
use std::sync::Arc;
use crate::compiled::typed::Bits;
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
    pub errors: Vec<Option<Box<EvaluationError>>>,
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
                Err(error) => errors.push(Some(error)),
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

struct Group<'a> {
    rows: Rc<[usize]>,
    leaves: Vec<(Arc<str>, Leaf<'a>)>,
}

impl CompiledGraph {
    pub(crate) fn input_leaf<'a>(column: Column<'a>, rows: usize) -> Leaf<'a> {
        let dated = DeclaredDates::dated;
        let nested = match column.values {
            Values::Any(values) => values.iter().take(rows).any(dated),
            Values::Dict { values: Dictionary::Any(values), .. } | Values::List { child: Dictionary::Any(values), .. } => {
                values.iter().any(dated)
            }
            Values::List {
                child: Dictionary::Text { .. } | Dictionary::Scaled { .. } | Dictionary::Bool { .. },
                ..
            } => false,
            Values::List {
                child: Dictionary::Column(child),
                ..
            } if Self::scalar_struct(child) => false,
            Values::Struct { .. } if Self::scalar_struct(&column) => false,
            Values::List { .. } | Values::Dict { values: Dictionary::Column(_), .. } => {
                (0..rows).any(|row| dated(&column.variable(row)))
            }
            _ => false,
        };
        match nested {
            true => Leaf::Any(
                (0..rows)
                    .map(|row| {
                        let value = column.variable(row);
                        DeclaredDates::prepare(&value, None).unwrap_or(value)
                    })
                    .collect(),
            ),
            false => Leaf::input(Self::validated(column, rows), rows),
        }
    }

    fn scalar_struct(column: &Column) -> bool {
        match column.values {
            Values::Struct { fields, .. } => fields.iter().all(|(_, field)| !matches!(field.values, Values::Any(_) | Values::List { .. } | Values::Struct { .. } | Values::Dict { values: Dictionary::Any(_) | Dictionary::Column(_), .. })),
            _ => false,
        }
    }

    fn input_data<'a>(columns: &Columns<'a>) -> Data<'a> {
        let rows = columns.rows;
        let mut paths: Vec<Arc<str>> = Vec::with_capacity(columns.columns.len());
        let mut leaves: Vec<Leaf<'a>> = Vec::with_capacity(columns.columns.len());
        for (path, column) in &columns.columns {
            paths.push(Arc::from(*path));
            leaves.push(Self::input_leaf(*column, rows));
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

    pub(crate) fn validated<'a>(column: Column<'a>, rows: usize) -> Column<'a> {
        let Values::Utf8 { offsets, data } = column.values else {
            return column;
        };
        let text = offsets
            .get(rows)
            .and_then(|&end| usize::try_from(end).ok())
            .and_then(|end| data.get(..end))
            .and_then(|bytes| std::str::from_utf8(bytes).ok());
        match text {
            Some(data) => Column {
                values: Values::Text { offsets, data },
                validity: column.validity,
            },
            None => column,
        }
    }

    async fn planned_input(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        columns: &Columns<'_>,
    ) -> Option<Vec<(usize, Box<EvaluationError>)>> {
        let step = self.steps.iter().flatten().find(|step| matches!(step.kind, crate::compiled::Kind::Input))?;
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
                        Box::new(EvaluationError::NodeError {
                            node_id: step.node.id.clone(),
                            source: message.into(),
                            trace: None,
                        }),
                    )
                })
                .collect(),
        )
    }

    fn reserved(path: &str) -> bool {
        path.split('.').any(|segment| segment == "$nodes")
    }

    fn leaves<'a>(datas: &[Data<'a>]) -> Option<Vec<(Arc<str>, Leaf<'a>)>> {
        let folded;
        let single = match datas {
            [single] => single,
            [first, ..] if datas.iter().all(Data::patchable) => {
                let mut acc = Data::empty(&first.rows);
                for data in datas {
                    acc = acc.layered(data.patch()?);
                }
                folded = acc;
                &folded
            }
            _ => return None,
        };
        let Shape::Patched { base: None, leaves, .. } = &single.shape else {
            return None;
        };
        let overlapping = leaves.iter().any(|(a, _)| {
            leaves
                .iter()
                .any(|(b, _)| b.strip_prefix(a.as_ref()).is_some_and(|rest| rest.starts_with('.')))
        });
        if overlapping {
            return None;
        }
        Some(
            leaves
                .iter()
                .filter(|(path, _)| !Self::reserved(path))
                .map(|(path, leaf)| (path.clone(), Self::cleaned(leaf)))
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

    fn flattened<'a>(rows: &Rc<[usize]>, datas: &[Data<'a>]) -> Group<'a> {
        let columns: Vec<_> = datas.iter().map(Data::materialized).collect();
        let mut paths: Vec<Arc<str>> = Vec::new();
        let mut values: Vec<Vec<Variable>> = Vec::new();
        for i in 0..rows.len() {
            let result = match columns.as_slice() {
                [single] => GraphWalker::merge_ending(std::iter::once(&single[i])),
                _ => GraphWalker::merge_ending(columns.iter().map(|c| &c[i])),
            };
            VariableCleaner::new().clean(&result);
            let mut entries = Vec::new();
            Self::flatten(&result, "", &mut entries);
            for (path, value) in entries {
                let at = match paths.iter().position(|p| p.as_ref() == path) {
                    Some(at) => at,
                    None => {
                        paths.push(Arc::from(path));
                        values.push(vec![Variable::Null; rows.len()]);
                        values.len() - 1
                    }
                };
                values[at][i] = value;
            }
        }
        Group {
            rows: rows.clone(),
            leaves: paths
                .into_iter()
                .zip(values)
                .map(|(path, column)| (path, Leaf::Any(column.into())))
                .collect(),
        }
    }

    pub(crate) async fn evaluate_columns<'a>(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        columns: &'a Columns<'a>,
    ) -> ColumnarOutput<'a> {
        if let (true, Some(plan)) = (max_depth > 0, self.plan.as_ref()) {
            if let Some(failures) = self.planned_input(content, extensions, columns).await {
                if let Some(output) = plan.evaluate(self, content, extensions, max_depth, columns, failures).await {
                    return output;
                }
            }
        }
        let count = columns.rows;
        let source = Source::Columns(Rc::new(Self::input_data(columns)));
        let state = self.run(content, extensions, max_depth, &source, count, true, crate::compiled::Nesting::default()).await;
        let groups: Vec<Group<'a>> = state
            .endings
            .iter()
            .map(|(rows, datas)| match Self::leaves(datas) {
                Some(leaves) => Group {
                    rows: rows.clone(),
                    leaves,
                },
                None => Self::flattened(rows, datas),
            })
            .collect();
        let identity = |rows: &[usize]| rows.len() == count && rows.iter().enumerate().all(|(i, r)| i == *r);
        let columns = match groups.as_slice() {
            [single] if identity(&single.rows) => single
                .leaves
                .iter()
                .map(|(path, leaf)| (path.clone(), OutputColumn(leaf.clone())))
                .collect(),
            _ => {
                let mut owner: Vec<Option<(usize, usize)>> = vec![None; count];
                for (g, group) in groups.iter().enumerate() {
                    for (i, &row) in group.rows.iter().enumerate() {
                        owner[row] = Some((g, i));
                    }
                }
                let mut paths: Vec<Arc<str>> = Vec::new();
                for group in &groups {
                    for (path, _) in &group.leaves {
                        if !paths.contains(path) {
                            paths.push(path.clone());
                        }
                    }
                }
                let owned = owner
                    .iter()
                    .zip(&state.errors)
                    .all(|(owner, error)| owner.is_some() || error.is_some());
                let mut aligned: Vec<Vec<(*const [usize], bool)>> = vec![Vec::new(); groups.len()];
                let mut passes = |g: usize, leaf: &Leaf<'a>| -> Option<Rc<Input<'a>>> {
                    let (input, positions) = leaf.passed()?;
                    let rows = &groups[g].rows;
                    let ok = match positions {
                        None => rows.len() == input.rows && rows.iter().enumerate().all(|(i, r)| i == *r),
                        Some(positions) => {
                            let key = Rc::as_ptr(positions);
                            match aligned[g].iter().find(|(k, _)| std::ptr::eq(*k, key)) {
                                Some((_, ok)) => *ok,
                                None => {
                                    let ok = positions.as_ref() == rows.as_ref();
                                    aligned[g].push((key, ok));
                                    ok
                                }
                            }
                        }
                    };
                    ok.then(|| input.clone())
                };
                paths
                    .into_iter()
                    .map(|path| {
                        let found: Vec<Option<&Leaf<'a>>> = groups
                            .iter()
                            .map(|g| g.leaves.iter().find(|(p, _)| *p == path).map(|(_, l)| l))
                            .collect();
                        let passed: Option<Vec<Rc<Input<'a>>>> = match owned {
                            true => found.iter().enumerate().map(|(g, leaf)| leaf.and_then(|leaf| passes(g, leaf))).collect(),
                            false => None,
                        };
                        if let Some(first) = passed.as_ref().and_then(|p| p.first().filter(|first| p.iter().all(|x| Rc::ptr_eq(x, first)))) {
                            return (path, OutputColumn(Leaf::Input(first.clone())));
                        }
                        let sources: Vec<Option<Column>> = groups
                            .iter()
                            .map(|g| g.leaves.iter().find(|(p, _)| *p == path).map(|(_, l)| l.column()))
                            .collect();
                        let mut builder = ColumnBuilder::with_capacity(count);
                        for slot in &owner {
                            match slot.and_then(|(g, i)| sources[g].as_ref().map(|column| (column, i))) {
                                Some((column, i)) => builder.push_cell(column, i),
                                None => builder.push_null(),
                            }
                        }
                        (path, OutputColumn(Leaf::typed(builder.finish())))
                    })
                    .collect()
            }
        };
        ColumnarOutput {
            rows: count,
            columns,
            errors: state.errors,
        }
    }
}
