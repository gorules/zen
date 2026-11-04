use crate::compiled::content::Content;
use crate::compiled::graph::data::{Col, Data};
use crate::compiled::graph::schedule::Execute;
use crate::compiled::graph::tap::Tap;
use crate::compiled::graph::traces::{Encoding, Facts, Value};
use crate::compiled::graph::{data, Body, CompiledGraph, Kind, NodeColumn, State, Step};
use crate::compiled::host::function::v1::{Batch, FunctionV1NodeHandler};
use crate::compiled::host::function::v2::FunctionV2NodeHandler;
use crate::compiled::host::input::dates::DeclaredDates;
use crate::compiled::host::variable_json::{Guards, VariableNode};
use crate::compiled::host::NodeError;
use crate::compiled::host::{NodeContextConfig, NodeHandlerExtensions};
use crate::model::DecisionNodeKind;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use zen_expression::lane::LaneRunner;
use zen_expression::Variable;
use zen_types::decision::FunctionNodeContent;

impl CompiledGraph {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::compiled::graph) async fn execute<'a>(
        &self,
        event: &Execute,
        rows: &Rc<[usize]>,
        inputs: &[Variable],
        state: &mut State<'a>,
        content: &Arc<Content>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
    ) {
        let Some(step) = self.step_at(event.node) else {
            return;
        };
        let start = Instant::now();
        let data = Rc::new(match step.kind {
            Kind::Input => Data::plain(
                rows.clone(),
                rows.iter().map(|&r| inputs[r].clone()).collect(),
            ),
            _ => Self::input_of(state, &event.parents, rows),
        });
        let (nodes, extra) = self.node_inputs(
            step,
            state,
            &event.visible,
            rows,
            step.column_path(rows.len()),
        );

        if matches!(step.kind, Kind::Host(_)) {
            let materialized = match state.traces.is_some() {
                true => Self::merged_rows(state, &event.parents, rows).into(),
                false => data.materialized(),
            };
            let batched = state.errors.len() > 1;
            let traced = state.traces.is_some();
            let origin = crate::compiled::pinning::rows_origin(rows);
            let replayed = state
                .nesting
                .completed
                .and_then(|completed| completed.replay(event.node, rows));
            let (results, facts) = match (replayed, traced) {
                (_, true) => {
                    let nodes = match step.nodes && state.with_nodes && nodes.is_empty() {
                        true => self
                            .nodes_of(state, &event.visible, rows)
                            .into_iter()
                            .map(Some)
                            .collect(),
                        false => nodes,
                    };
                    let (results, facts) = crate::compiled::pinning::originated(
                        origin,
                        Self::hosted_traced(
                            step,
                            &materialized,
                            &nodes,
                            extensions,
                            max_depth,
                            state.nesting.iteration,
                        ),
                    )
                    .await;
                    (results, Facts::Rows(facts))
                }
                (None, false) => (
                    crate::compiled::pinning::originated(
                        origin,
                        Self::hosted(
                            step,
                            &materialized,
                            &nodes,
                            extensions,
                            max_depth,
                            state.nesting.iteration,
                            batched,
                        ),
                    )
                    .await,
                    Facts::None,
                ),
                (Some(replayed), false) => {
                    let missing: Vec<usize> =
                        (0..rows.len()).filter(|&i| replayed[i].is_none()).collect();
                    let inputs: Vec<Variable> =
                        missing.iter().map(|&i| materialized[i].clone()).collect();
                    let parents: Vec<Option<Variable>> = missing
                        .iter()
                        .map(|&i| nodes.get(i).cloned().flatten())
                        .collect();
                    let mut fresh = match missing.is_empty() {
                        true => Vec::new(),
                        false => {
                            let at: Rc<[usize]> = missing.iter().map(|&i| rows[i]).collect();
                            crate::compiled::pinning::originated(
                                crate::compiled::pinning::rows_origin(&at),
                                Self::hosted(
                                    step,
                                    &inputs,
                                    &parents,
                                    extensions,
                                    max_depth,
                                    state.nesting.iteration,
                                    batched,
                                ),
                            )
                            .await
                        }
                    }
                    .into_iter();
                    (
                        replayed
                            .into_iter()
                            .map(|done| match done {
                                Some(value) => Ok(value),
                                None => fresh.next().unwrap_or(Ok(Variable::Null)),
                            })
                            .collect(),
                        Facts::None,
                    )
                }
            };
            let mut values = Vec::with_capacity(rows.len());
            for (i, result) in results.into_iter().enumerate() {
                match result {
                    Ok(v) => values.push(v),
                    Err(error) => {
                        state.errors.fault(rows[i], error);
                        values.push(Variable::Null);
                    }
                }
            }
            let output = Rc::new(Data::plain(rows.clone(), values.into()));
            state.nodes[event.node.index()].push(output.clone());
            Self::trace_event(
                state,
                event,
                rows,
                true,
                Some(Value::Data(output)),
                start,
                facts,
                Encoding::Generic,
            );
            return;
        }

        let (output, failures): (Rc<Data<'a>>, Vec<(usize, String)>) = match &step.kind {
            Kind::Switch { .. } | Kind::Host(_) => (data.clone(), Vec::new()),
            Kind::Output if !step.schema => (data.clone(), Vec::new()),
            Kind::Input if !step.schema && Self::unresolved(content, &step.node) => {
                let values: Col = data
                    .materialized()
                    .iter()
                    .map(|value| {
                        DeclaredDates::prepare(value, None).unwrap_or_else(|| value.clone())
                    })
                    .collect();
                (Rc::new(Data::plain(rows.clone(), values)), Vec::new())
            }
            Kind::Input | Kind::Output => {
                let (values, failures) = self
                    .bounded(step, &data.materialized(), &nodes, content, extensions)
                    .await;
                (Rc::new(Data::plain(rows.clone(), values.into())), failures)
            }
            Kind::Node { .. } => {
                let (output, failures) = self.node(step, rows, &data, &nodes, &extra);
                (Rc::new(output), failures)
            }
        };

        for (i, message) in failures {
            state.errors.fail(rows[i], &step.node, message);
        }
        state.nodes[event.node.index()].push(output.clone());
        if state.traces.is_some() {
            let encoding = self.encoding(step, event, &data);
            let (input, output) = match step.kind {
                Kind::Input => (false, Some(Value::Data(output))),
                Kind::Output => (true, Some(Value::Null)),
                _ => (true, Some(Value::Data(output))),
            };
            Self::trace_event(
                state,
                event,
                rows,
                input,
                output,
                start,
                Facts::None,
                encoding,
            );
        }
    }

    pub(in crate::compiled::graph) async fn hosted(
        step: &Step,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        iteration: u8,
        batched: bool,
    ) -> Vec<Result<Variable, NodeError>> {
        if let DecisionNodeKind::DecisionNode { content } = &step.node.kind {
            if let Some(results) = Self::nested(
                step, content, inputs, nodes, extensions, max_depth, iteration,
            )
            .await
            {
                return results;
            }
        }
        let single = iteration > 0 && inputs.len() == 1;
        if let (
            DecisionNodeKind::FunctionNode {
                content: FunctionNodeContent::Version2(content),
            },
            false,
        ) = (&step.node.kind, single)
        {
            let config = NodeContextConfig {
                max_depth,
                trace: false,
                ..Default::default()
            };
            let inputs = inputs
                .iter()
                .enumerate()
                .map(|(i, value)| (value.clone(), nodes.get(i).cloned().flatten()))
                .collect();
            return FunctionV2NodeHandler::batch(
                &step.node.id,
                content,
                extensions,
                &config,
                iteration,
                inputs,
            )
            .await;
        }
        if let DecisionNodeKind::FunctionNode {
            content: FunctionNodeContent::Version1(source),
        } = &step.node.kind
        {
            if let Some(batch) = FunctionV1NodeHandler::batch(&step.node.id, source, inputs.len()) {
                return Self::scripted(
                    step, batch, inputs, nodes, extensions, max_depth, iteration, batched,
                )
                .await;
            }
        }
        let mut results = Vec::with_capacity(inputs.len());
        for (i, value) in inputs.iter().enumerate() {
            let nodes = nodes.get(i).cloned().flatten();
            results.push(
                crate::compiled::pinning::at_row(
                    i,
                    Self::host(
                        step,
                        value.clone(),
                        nodes,
                        extensions,
                        max_depth,
                        iteration,
                        batched,
                    ),
                )
                .await,
            );
        }
        results
    }

    #[allow(clippy::too_many_arguments)]
    async fn scripted(
        step: &Step,
        mut batch: Batch,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        iteration: u8,
        batched: bool,
    ) -> Vec<Result<Variable, NodeError>> {
        let mut results = Vec::with_capacity(inputs.len());
        for (i, value) in inputs.iter().enumerate() {
            let nodes = nodes.get(i).cloned().flatten();
            match crate::compiled::pinning::at_row_now(i, || batch.call(value, nodes.as_ref())) {
                Some(result) => results.push(result),
                None => results.push(
                    crate::compiled::pinning::at_row(
                        i,
                        Self::host(
                            step,
                            value.clone(),
                            nodes,
                            extensions,
                            max_depth,
                            iteration,
                            batched,
                        ),
                    )
                    .await,
                ),
            }
        }
        results
    }

    pub(in crate::compiled::graph) async fn bounded(
        &self,
        step: &Step,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        content: &Arc<Content>,
        extensions: &NodeHandlerExtensions,
    ) -> (Vec<Variable>, Vec<(usize, String)>) {
        let validator = Self::validator(step, content, extensions);
        let mut values = Vec::with_capacity(inputs.len());
        let mut failures = Vec::new();
        for (i, value) in inputs.iter().enumerate() {
            let nodes = nodes.get(i).cloned().flatten();
            if let Some((validator, schema)) = &validator {
                if validator.is_valid(VariableNode::new(value, &Guards::default())) {
                    values.push(match step.kind {
                        Kind::Input => DeclaredDates::prepare(value, Some(schema))
                            .unwrap_or_else(|| value.clone()),
                        _ => value.clone(),
                    });
                    continue;
                }
            }
            match self
                .boundary(step, value.clone(), nodes, content, extensions)
                .await
            {
                Ok(v) => values.push(v),
                Err(e) => {
                    failures.push((i, e));
                    values.push(Variable::Null);
                }
            }
        }
        (values, failures)
    }

    pub(in crate::compiled::graph) fn rowwise<'a>(
        &self,
        runner: &mut LaneRunner,
        step: &Step,
        data: Data<'a>,
        nodes: &[Option<Variable>],
        mut facts: Option<&mut Vec<crate::compiled::trace::collect::Fact>>,
    ) -> (Data<'a>, Vec<(usize, String)>) {
        let rows: Rc<[usize]> = (0..data.len()).collect();
        if Self::foreign_input(step, &data) {
            crate::compiled::census::Census::mark("walked");
            let mut failures = Vec::new();
            let values: Col = data
                .materialized()
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    let node = nodes.get(i).cloned().flatten();
                    let result =
                        crate::compiled::pinning::at_row_now(i, || match facts.as_deref_mut() {
                            Some(facts) => {
                                let (result, fact) = Self::walked_traced(step, value.clone(), node);
                                facts.push(fact);
                                result
                            }
                            None => Self::walked(step, value.clone(), node, true),
                        });
                    match result {
                        Ok(output) => output,
                        Err(message) => {
                            failures.push((i, message));
                            Variable::Null
                        }
                    }
                })
                .collect();
            return (Data::plain(rows, values), failures);
        }
        self.node_with(runner, step, &rows, &data, nodes, &[])
    }

    fn node<'a>(
        &self,
        step: &Step,
        rows: &Rc<[usize]>,
        data: &Data<'a>,
        nodes: &[Option<Variable>],
        extra: &[NodeColumn<'a>],
    ) -> (Data<'a>, Vec<(usize, String)>) {
        Self::RUNNER
            .with_borrow_mut(|runner| self.node_with(runner, step, rows, data, nodes, extra))
    }

    fn node_with<'a>(
        &self,
        runner: &mut LaneRunner,
        step: &Step,
        rows: &Rc<[usize]>,
        data: &Data<'a>,
        nodes: &[Option<Variable>],
        extra: &[NodeColumn<'a>],
    ) -> (Data<'a>, Vec<(usize, String)>) {
        let Kind::Node { body, transform } = &step.kind else {
            return (Data::plain(rows.clone(), Rc::from(Vec::new())), Vec::new());
        };
        {
            let wide = rows.len() > 1;
            let rebase = match (&transform.input_field, &transform.base, transform.looped) {
                (None, _, false) => Some(None),
                (Some(_), Some(base), false) => Some(Some(base.as_ref())),
                _ => None,
            };
            let keyed = !matches!(body, Body::Expression { keys, .. } if keys.is_empty());
            let expression = matches!(body, Body::Expression { .. });
            let overlapping = matches!(
                body,
                Body::Expression {
                    overlapping: true,
                    ..
                }
            );
            match (rebase, &transform.output_path, wide) {
                (Some(rebase), None, true) if !overlapping => {
                    crate::compiled::census::Census::mark("layered");
                    Self::bound(step, data, nodes, extra, rebase, |bound| {
                        Self::layered(runner, body, transform.pass_through, data, bound, None)
                    })
                }
                (Some(rebase), Some(path), true) if keyed && expression && !overlapping => {
                    crate::compiled::census::Census::mark("layered-path");
                    Self::bound(step, data, nodes, extra, rebase, |bound| {
                        Self::layered(
                            runner,
                            body,
                            transform.pass_through,
                            data,
                            bound,
                            Some(path),
                        )
                    })
                }
                (Some(None), Some(path), true) if keyed => {
                    crate::compiled::census::Census::mark("placed");
                    Self::bound(step, data, nodes, extra, None, |bound| {
                        Self::placed(runner, body, path, transform.pass_through, data, bound)
                    })
                }
                (None, Some(path), true)
                    if transform.looped
                        && !step.nodes
                        && transform
                            .input_field
                            .as_ref()
                            .is_some_and(|(program, _)| program.path().is_some()) =>
                {
                    crate::compiled::census::Census::mark("looped");
                    let _origin = zen_expression::clock::streamed()
                        .then(|| zen_expression::clock::rows(rows.clone()));
                    Self::looped(runner, step, body, transform, path, data)
                }
                _ => {
                    crate::compiled::census::Census::mark("transformed");
                    let _origin = zen_expression::clock::streamed()
                        .then(|| zen_expression::clock::rows(rows.clone()));
                    let materialized = data.materialized();
                    let results =
                        Self::transformed(runner, step, body, transform, &materialized, nodes);
                    let mut failures = Vec::new();
                    let values: Col = results
                        .into_iter()
                        .enumerate()
                        .map(|(i, r)| match r {
                            Ok(v) => v,
                            Err(e) => {
                                failures.push((i, e));
                                Variable::Null
                            }
                        })
                        .collect();
                    (Data::plain(rows.clone(), values), failures)
                }
            }
        }
    }

    fn foreign_input(step: &Step, data: &Data) -> bool {
        let based = match &step.kind {
            Kind::Node { transform, .. } if !transform.looped => transform.base.as_deref(),
            _ => None,
        };
        matches!(
            &step.kind,
            Kind::Node {
                body: Body::Expression { walked: true, .. },
                ..
            }
        ) || matches!(&data.shape, data::Shape::Plain(values) if values.iter().any(Self::foreign))
            || Self::foreign_base(data, based)
    }

    fn foreign_base(data: &Data, rebase: Option<&str>) -> bool {
        let Some(base) = rebase else {
            return false;
        };
        let column = data.column_at(base);
        (0..data.len()).any(|row| Self::foreign(&column.get(row)))
    }

    pub(in crate::compiled::graph) fn immediate(step: &Step) -> bool {
        match &step.kind {
            Kind::Switch { .. } | Kind::Node { .. } => true,
            Kind::Output => !step.schema,
            Kind::Input | Kind::Host(_) => false,
        }
    }

    pub(in crate::compiled::graph) fn execute_now<'a>(
        &self,
        step: &Step,
        event: &Execute,
        rows: &Rc<[usize]>,
        state: &mut State<'a>,
    ) {
        let start = Instant::now();
        let traced = state.traces.is_some();
        let data = Rc::new(Self::input_of(state, &event.parents, rows));
        let foreign = Self::foreign_input(step, &data)
            || (traced && matches!(step.kind, Kind::Node { .. }) && step.draws());
        if traced {
            Tap::arm(Self::wants_fields(step));
        }
        let mut walked: Option<Vec<crate::compiled::trace::collect::Fact>> = None;
        let (output, failures) = match &step.kind {
            Kind::Node { .. } if foreign => {
                let values = data.materialized();
                let nodes: Vec<Option<Variable>> = match step.nodes && state.with_nodes {
                    true => self
                        .nodes_of(state, &event.visible, rows)
                        .into_iter()
                        .map(Some)
                        .collect(),
                    false => Vec::new(),
                };
                let mut failures = Vec::new();
                let mut facts = traced.then(|| Vec::with_capacity(values.len()));
                let _origin = zen_expression::clock::streamed()
                    .then(|| zen_expression::clock::rows(rows.clone()));
                let outputs: Vec<Variable> = values
                    .iter()
                    .enumerate()
                    .map(|(i, value)| {
                        let node = nodes.get(i).cloned().flatten();
                        let result =
                            crate::compiled::pinning::at_row_now(i, || match facts.as_mut() {
                                Some(facts) => {
                                    let (result, fact) =
                                        Self::walked_traced(step, value.clone(), node);
                                    facts.push(fact);
                                    result
                                }
                                None => Self::walked(step, value.clone(), node, true),
                            });
                        match result {
                            Ok(output) => output,
                            Err(message) => {
                                failures.push((i, message));
                                Variable::Null
                            }
                        }
                    })
                    .collect();
                walked = facts;
                (Rc::new(Data::plain(rows.clone(), outputs.into())), failures)
            }
            Kind::Node { .. } => {
                let (nodes, extra) = self.node_inputs(
                    step,
                    state,
                    &event.visible,
                    rows,
                    step.column_path(rows.len()),
                );
                let (output, failures) = self.node(step, rows, &data, &nodes, &extra);
                (Rc::new(output), failures)
            }
            _ => (data.clone(), Vec::new()),
        };
        let tap = match traced {
            true => Tap::take(),
            false => None,
        };
        for (i, message) in failures {
            state.errors.fail(rows[i], &step.node, message);
        }
        state.nodes[event.node.index()].push(output.clone());
        if traced && !matches!(step.kind, Kind::Switch { .. }) {
            let facts = match (&step.kind, walked) {
                (Kind::Node { .. }, Some(walked)) => Facts::Rows(walked),
                (Kind::Node { .. }, None) => self.tapped(step, tap, state, event, rows, &data),
                _ => Facts::None,
            };
            let encoding = self.encoding(step, event, &data);
            let output = match step.kind {
                Kind::Output => Value::Null,
                _ => Value::Data(output),
            };
            Self::trace_event(
                state,
                event,
                rows,
                true,
                Some(output),
                start,
                facts,
                encoding,
            );
        }
    }
}
