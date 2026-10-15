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
    /// Rows decided one at a time (the rest as columns).
    pub hosted: usize,
    /// Why (a policy's; a document not compiled says so).
    pub fallbacks: Vec<crate::compiled::policy::Fallback>,
    /// Why each row decided as it did: a policy's root matches and tables
    /// (asked for with `trace`; rows decided one by one have none).
    pub reasons: Vec<crate::compiled::policy::Reasons>,
    /// Rows read whole: their value as decided, where columns drop part.
    pub(crate) exact: Vec<Option<Variable>>,
}

impl<'a> ColumnarOutput<'a> {
    pub(crate) fn from_results(
        results: Vec<Result<crate::DecisionGraphResponse, Box<EvaluationError>>>,
        reason: crate::compiled::policy::FallbackReason,
    ) -> Self {
        let rows = results.len();
        let mut paths: Vec<Arc<str>> = Vec::new();
        let mut values: Vec<Vec<Variable>> = Vec::new();
        let mut errors = Vec::with_capacity(rows);
        let mut exact: Vec<Option<Variable>> = Vec::new();
        for (row, result) in results.into_iter().enumerate() {
            match result {
                Ok(response) => {
                    if crate::compiled::shred::Shredder::lossy(&response.result) {
                        if exact.is_empty() {
                            exact = vec![None; rows];
                        }
                        exact[row] = Some(response.result.clone());
                    }
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
            hosted: rows,
            fallbacks: vec![crate::compiled::policy::Fallback { reason, rows }],
            reasons: Vec::new(),
            exact,
        }
    }

    /// A policy's columnar output as a document's: the same columns, each
    /// failure as the engine's error.
    pub(crate) fn from_policy(output: crate::compiled::policy::PolicyColumnarOutput<'a>) -> Self {
        ColumnarOutput {
            rows: output.rows,
            columns: output.columns,
            errors: output
                .errors
                .into_iter()
                .map(|error| error.map(|error| Rc::new(EvaluationError::Policy(error))))
                .collect(),
            typed: output.typed,
            laned: output.laned,
            hosted: output.hosted,
            fallbacks: output.fallbacks,
            reasons: output.reasons,
            exact: output.exact,
        }
    }

    /// The row's value kept whole, where its columns can't say it (a
    /// property written as null, an empty object): read it, not them.
    pub fn whole(&self, row: usize) -> Option<&Variable> {
        self.exact.get(row).and_then(Option::as_ref)
    }

    pub fn row(&self, row: usize) -> Variable {
        if let Some(Some(value)) = self.exact.get(row) {
            return value.clone();
        }
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

    /// The input as the input node gives it on: each column at a date its
    /// schema declares converted (a text that parses becomes the date, any
    /// other value stays). The columns as given when the schema declares
    /// none; `None` when a declared date is not a column's own (inside a
    /// list or a record) or an input holds a date already (the node turns
    /// those back into text): decided by rows.
    fn dated_input<'a>(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        columns: &'a Columns<'a>,
        store: &'a crate::ColumnStore<'a>,
    ) -> Option<(&'a Columns<'a>, bool)> {
        use crate::nodes::input::dates::DeclaredDates;
        let step = self.steps.iter().flatten().find(|step| matches!(step.kind, crate::compiled::graph::Kind::Input))?;
        let Some((_, schema)) = Self::validator(step, content, extensions) else {
            return Some((columns, false));
        };
        if !DeclaredDates::mentions(&schema) {
            return Some((columns, false));
        }
        let rows = columns.rows;
        let mut converted = Columns::new(rows);
        for (path, column) in &columns.columns {
            if path.is_empty() {
                return None;
            }
            // The schema at the column's path, through `properties`.
            let mut at: Option<&serde_json::Value> = Some(&schema);
            for segment in path.split('.') {
                at = at
                    .and_then(serde_json::Value::as_object)
                    .and_then(|o| DeclaredDates::structure(o, "properties"))
                    .and_then(serde_json::Value::as_object)
                    .and_then(|p| p.get(segment));
            }
            let declared = at.is_some_and(DeclaredDates::declared);
            let scalar = !matches!(column.values, Values::List { .. } | Values::Struct { .. } | Values::Any(_));
            let holds_dates = matches!(column.values, Values::Any(values) if values.iter().any(DeclaredDates::dated));
            if holds_dates || (!scalar && at.is_some_and(DeclaredDates::mentions)) {
                return None;
            }
            let column = match declared {
                false => *column,
                true => {
                    let values: Vec<Variable> = (0..rows)
                        .map(|row| match column.variable(row) {
                            Variable::String(text) => zen_expression::DateValue::from_text(&text).unwrap_or(Variable::String(text)),
                            other => other,
                        })
                        .collect();
                    Column {
                        values: Values::Any(store.kept(values)),
                        validity: column.validity,
                    }
                }
            };
            converted = converted.column(path, column);
        }
        Some((store.keep_columns(converted), true))
    }

    /// A request typed by an entity (`target`), prepared as columns as each
    /// row's is (dates converted, defaults filled, under the entity's name):
    /// the columns nodes read, and the rows that do not fit (their error,
    /// from their own preparation). `None` when the request is not typed by
    /// an entity; `Some(None)` when its columns can't be prepared (decided
    /// by rows).
    #[allow(clippy::type_complexity)]
    async fn entity_input<'a>(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        columns: &'a Columns<'a>,
        store: &'a crate::ColumnStore<'a>,
    ) -> Option<Option<(&'a Columns<'a>, Vec<(usize, Rc<EvaluationError>)>)>> {
        use crate::compiled::policy::FallbackReason;
        let step = self.steps.iter().flatten().find(|step| matches!(step.kind, crate::compiled::graph::Kind::Input))?;
        let prepared = match Self::request(step, content, extensions).await? {
            Ok(prepared) => prepared,
            Err(_) => return Some(None),
        };
        let target = prepared.target();
        let rows = columns.rows;
        let mut under = Columns::new(rows);
        for (path, column) in &columns.columns {
            if path.is_empty() {
                return Some(None);
            }
            under = under.column(store.text(&format!("{target}.{path}")), *column);
        }
        let Some((converted, mut forced)) = crate::compiled::policy::columnar::prepare::convert(prepared.schema(), &under, store) else {
            return Some(None);
        };

        // A reference sent as its id (not its record): the row says so.
        for (field, _) in prepared.references() {
            let Some(at) = converted.find(&format!("{target}.{field}")) else {
                continue;
            };
            let column = converted.columns[at].1;
            for (row, forced) in forced.iter_mut().enumerate() {
                let id = match column.variable(row) {
                    Variable::String(_) => true,
                    Variable::Array(items) => items.borrow().iter().any(|item| matches!(item, Variable::String(_))),
                    _ => false,
                };
                if id {
                    forced.get_or_insert(FallbackReason::Reference);
                }
            }
        }
        // Rows the columns can't vouch for: prepared alone, their error kept.
        let sure = prepared.schema().sure_prepared(&converted);
        let mut failures = Vec::new();
        for row in 0..rows {
            if forced[row].is_none() && sure.as_ref().is_some_and(|sure| sure[row]) {
                continue;
            }
            match prepared.prepare(&columns.row(row)) {
                Err(message) => failures.push((
                    row,
                    Rc::new(EvaluationError::NodeError {
                        node_id: step.node.id.clone(),
                        source: message.into(),
                        trace: None,
                    }),
                )),
                // Fits, though its columns could not say it: by rows.
                Ok(_) if forced[row].is_some() => return Some(None),
                Ok(_) => {}
            }
        }
        // The request as nodes read it: the entity's columns, unwrapped.
        let prefix = format!("{target}.");
        let mut input = Columns::new(rows);
        for (path, column) in &converted.columns {
            let Some(rest) = path.strip_prefix(prefix.as_str()) else {
                return Some(None);
            };
            input = input.column(rest, *column);
        }
        Some(Some((store.keep_columns(input), failures)))
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
        store: &'a crate::ColumnStore<'a>,
    ) -> ColumnarOutput<'a> {
        use crate::compiled::policy::FallbackReason;
        // Why the rows go by row: the request not prepared as columns, or
        // the nodes not planned so.
        let mut why = FallbackReason::Unplanned;
        if max_depth > 0 {
            // A request typed by an entity: prepared as columns.
            if let Some(prepared) = self.entity_input(content, extensions, columns, store).await {
                match prepared {
                    // Given on as prepared: the entity as policies read it.
                    Some((input, failures)) => {
                        if let Some(output) = self.plan().evaluate(self, content, extensions, max_depth, input, true, failures).await {
                            return output;
                        }
                    }
                    None => why = FallbackReason::Converted,
                }
            } else if let Some(failures) = self.planned_input(content, extensions, columns).await {
                // Validated as given; the dates the schema declares
                // converted, as the input node converts each row's.
                match self.dated_input(content, extensions, columns, store) {
                    Some((input, prepared)) => {
                        if let Some(output) = self.plan().evaluate(self, content, extensions, max_depth, input, prepared, failures).await {
                            return output;
                        }
                    }
                    None => why = FallbackReason::Converted,
                }
            }
        }
        let inputs = Self::input_data(columns).materialized();
        ColumnarOutput::from_results(self.evaluate(content, extensions, max_depth, &inputs).await, why)
    }
}
