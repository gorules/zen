use crate::model::{DecisionNodeKind, GraphContent};
use crate::nodes::function::v2::FunctionV2NodeHandler;
use crate::nodes::input::dates::DeclaredDates;
use crate::nodes::NodeError;
use crate::nodes::variable_json::{Guards, VariableNode};
use crate::nodes::{NodeContextConfig, NodeHandlerExtensions};
use crate::compiled::graph::schedule::Execute;
use std::sync::Arc;
use crate::compiled::graph::data::{Col, Data};
use std::rc::Rc;
use zen_expression::lane::LaneRunner;
use zen_expression::Variable;
use zen_types::decision::FunctionNodeContent;
use crate::compiled::graph::{Body, CompiledGraph, Kind, NodeColumn, State, Step, data, plan};

impl CompiledGraph {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::compiled::graph) async fn execute<'a>(
        &self,
        event: &Execute,
        rows: &Rc<[usize]>,
        inputs: &[Variable],
        state: &mut State<'a>,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        with_nodes: bool,
        max_depth: u8,
    ) {
        let Some(step) = self.step_at(event.node) else {
            return;
        };
        let data = match step.kind {
            Kind::Input => Data::plain(rows.clone(), rows.iter().map(|&r| inputs[r].clone()).collect()),
            _ => Self::input_of(state, &event.parents, rows),
        };
        let (nodes, extra) = self.node_inputs(step, state, &event.visible, rows, with_nodes, step.column_path(rows.len()));

        if matches!(step.kind, Kind::Host) {
            let materialized = data.materialized();
            let results = Self::hosted(step, &materialized, &nodes, extensions, max_depth, state.nesting.iteration, state.errors.len() > 1).await;
            let mut values = Vec::with_capacity(rows.len());
            for (i, result) in results.into_iter().enumerate() {
                match result {
                    Ok(v) => values.push(v),
                    Err(error) => {
                        Self::fault(state, rows[i], error);
                        values.push(Variable::Null);
                    }
                }
            }
            state.nodes[event.node.index()].push(Rc::new(Data::plain(rows.clone(), values.into())));
            return;
        }

        let (output, failures): (Data<'a>, Vec<(usize, String)>) = match &step.kind {
            Kind::Switch { .. } | Kind::Host => (data, Vec::new()),
            Kind::Output if !step.schema => (data, Vec::new()),
            Kind::Input if !step.schema && Self::unresolved(content, &step.node) => {
                let values: Col = data
                    .materialized()
                    .iter()
                    .map(|value| DeclaredDates::prepare(value, None).unwrap_or_else(|| value.clone()))
                    .collect();
                (Data::plain(rows.clone(), values), Vec::new())
            }
            Kind::Input | Kind::Output => {
                let (values, failures) = self.bounded(step, &data.materialized(), &nodes, content, extensions).await;
                (Data::plain(rows.clone(), values.into()), failures)
            }
            Kind::Node { .. } => self.node(step, rows, &data, &nodes, &extra),
        };

        for (i, message) in failures {
            Self::fail(state, rows[i], &step.node, message);
        }
        state.nodes[event.node.index()].push(Rc::new(output));
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
            if let Some(results) = Self::nested(step, content, inputs, nodes, extensions, max_depth, iteration).await {
                return results;
            }
        }
        let single = iteration > 0 && inputs.len() == 1;
        if let (DecisionNodeKind::FunctionNode { content: FunctionNodeContent::Version2(content) }, false) = (&step.node.kind, single) {
            let config = NodeContextConfig {
                max_depth,
                trace: false,
                ..Default::default()
            };
            let inputs = inputs.iter().enumerate().map(|(i, value)| (value.clone(), nodes.get(i).cloned().flatten())).collect();
            return FunctionV2NodeHandler::batch(&step.node.id, content, extensions, &config, iteration, inputs).await;
        }
        let mut results = Vec::with_capacity(inputs.len());
        for (i, value) in inputs.iter().enumerate() {
            let nodes = nodes.get(i).cloned().flatten();
            results.push(Self::host(step, value.clone(), nodes, extensions, max_depth, iteration, batched).await);
        }
        results
    }

    pub(in crate::compiled::graph) async fn bounded(
        &self,
        step: &Step,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        content: &Arc<GraphContent>,
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
                        Kind::Input => DeclaredDates::prepare(value, Some(schema)).unwrap_or_else(|| value.clone()),
                        _ => value.clone(),
                    });
                    continue;
                }
            }
            match self.boundary(step, value.clone(), nodes, content, extensions).await {
                Ok(v) => values.push(v),
                Err(e) => {
                    failures.push((i, e));
                    values.push(Variable::Null);
                }
            }
        }
        (values, failures)
    }

    pub(in crate::compiled::graph) fn rowwise<'a>(&self, runner: &mut LaneRunner, step: &Step, data: Data<'a>, nodes: &[Option<Variable>]) -> (Data<'a>, Vec<(usize, String)>) {
        let rows: Rc<[usize]> = (0..data.len()).collect();
        if Self::foreign_input(step, &data) {
            plan::census::Census::mark("walked");
            let mut failures = Vec::new();
            let values: Col = data
                .materialized()
                .iter()
                .enumerate()
                .map(|(i, value)| match Self::walked(step, value.clone(), nodes.get(i).cloned().flatten(), true) {
                    Ok(output) => output,
                    Err(message) => {
                        failures.push((i, message));
                        Variable::Null
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
        Self::RUNNER.with_borrow_mut(|runner| self.node_with(runner, step, rows, data, nodes, extra))
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
            let overlapping = matches!(body, Body::Expression { overlapping: true, .. });
            match (rebase, &transform.output_path, wide) {
                (Some(rebase), None, true) if !overlapping => {
                    plan::census::Census::mark("layered");
                    Self::bound(step, data, nodes, extra, rebase, |bound| {
                        Self::layered(runner, body, transform.pass_through, data, bound, None)
                    })
                }
                (Some(rebase), Some(path), true) if keyed && expression && !overlapping => {
                    plan::census::Census::mark("layered-path");
                    Self::bound(step, data, nodes, extra, rebase, |bound| {
                        Self::layered(runner, body, transform.pass_through, data, bound, Some(path))
                    })
                }
                (Some(None), Some(path), true) if keyed => {
                    plan::census::Census::mark("placed");
                    Self::bound(step, data, nodes, extra, None, |bound| {
                        Self::placed(runner, body, path, transform.pass_through, data, bound)
                    })
                }
                (None, Some(path), true)
                    if transform.looped
                        && !step.nodes
                        && transform.input_field.as_ref().is_some_and(|(program, _)| program.path().is_some()) =>
                {
                    plan::census::Census::mark("looped");
                    Self::looped(runner, step, body, transform, path, data)
                }
                _ => {
                    plan::census::Census::mark("transformed");
                    let materialized = data.materialized();
                    let results = Self::transformed(runner, step, body, transform, &materialized, nodes);
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
        matches!(&step.kind, Kind::Node { body: Body::Expression { walked: true, .. }, .. })
            || matches!(&data.shape, data::Shape::Plain(values) if values.iter().any(Self::foreign))
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
            Kind::Input | Kind::Host => false,
        }
    }

    pub(in crate::compiled::graph) fn execute_now<'a>(&self, step: &Step, event: &Execute, rows: &Rc<[usize]>, state: &mut State<'a>, with_nodes: bool) {
        let data = Self::input_of(state, &event.parents, rows);
        let foreign = Self::foreign_input(step, &data);
        let (output, failures) = match &step.kind {
            Kind::Node { .. } if foreign => {
                let values = data.materialized();
                let nodes: Vec<Option<Variable>> = match step.nodes && with_nodes {
                    true => self.nodes_of(state, &event.visible, rows).into_iter().map(Some).collect(),
                    false => Vec::new(),
                };
                let mut failures = Vec::new();
                let outputs: Vec<Variable> = values
                    .iter()
                    .enumerate()
                    .map(|(i, value)| match Self::walked(step, value.clone(), nodes.get(i).cloned().flatten(), true) {
                        Ok(output) => output,
                        Err(message) => {
                            failures.push((i, message));
                            Variable::Null
                        }
                    })
                    .collect();
                (Data::plain(rows.clone(), outputs.into()), failures)
            }
            Kind::Node { .. } => {
                let (nodes, extra) = self.node_inputs(step, state, &event.visible, rows, with_nodes, step.column_path(rows.len()));
                self.node(step, rows, &data, &nodes, &extra)
            }
            _ => (data, Vec::new()),
        };
        for (i, message) in failures {
            Self::fail(state, rows[i], &step.node, message);
        }
        state.nodes[event.node.index()].push(Rc::new(output));
    }
}
