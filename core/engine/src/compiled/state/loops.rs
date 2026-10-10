use std::sync::Arc;
use crate::compiled::bound::Bound;
use crate::compiled::data::{Data, Layer, Mask};
use crate::compiled::leaf::{ColumnBuilder, Leaf};
use std::rc::Rc;
use zen_expression::lane::{LaneProgram, LaneRunner};
use zen_expression::{Scope, Variable};
use crate::compiled::exact::graph::Row;
use crate::compiled::{Body, CompiledGraph, Kind, Step, Transform, leaf};

impl CompiledGraph {
    pub(in crate::compiled) fn transformed(
        runner: &mut LaneRunner,
        step: &Step,
        body: &Body,
        transform: &Transform,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
    ) -> Vec<Result<Variable, String>> {
        let effective: Vec<Result<Variable, String>> = match &transform.input_field {
            None => inputs.iter().cloned().map(Ok).collect(),
            Some((program, _)) => {
                let scopes: Vec<Scope> = inputs
                    .iter()
                    .enumerate()
                    .map(|(row, i)| Self::scope(i, nodes.get(row).and_then(Option::as_ref)))
                    .collect();
                runner
                    .evaluate(program, &scopes)
                    .into_iter()
                    .map(|r| r.map_err(|e| format!("Failed to evaluate expression: {e}")))
                    .collect()
            }
        };

        let mut outputs: Vec<Result<Variable, String>> = match transform.looped {
            false => {
                let rows: Vec<usize> = (0..inputs.len()).filter(|&r| effective[r].is_ok()).collect();
                let body_inputs: Vec<Variable> = rows
                    .iter()
                    .filter_map(|&r| effective[r].as_ref().ok().cloned())
                    .collect();
                let body_nodes: Vec<Option<Variable>> = rows.iter().map(|&r| nodes.get(r).cloned().flatten()).collect();
                let mut produced = Self::items(runner, step, body, &body_inputs, &body_nodes).into_iter();
                effective
                    .into_iter()
                    .map(|e| e.and_then(|_| produced.next().unwrap_or_else(|| Ok(Variable::Null))))
                    .collect()
            }
            true => {
                let mut owners: Vec<usize> = Vec::new();
                let mut items: Vec<Variable> = Vec::new();
                let mut item_nodes: Vec<Option<Variable>> = Vec::new();
                let mut shaped: Vec<Result<usize, String>> = Vec::with_capacity(inputs.len());
                for (row, value) in effective.iter().enumerate() {
                    match value {
                        Err(e) => shaped.push(Err(e.clone())),
                        Ok(value) => match value.as_array() {
                            None => shaped.push(Err("Expected an array".to_string())),
                            Some(array) => {
                                let array = array.borrow();
                                shaped.push(Ok(array.len()));
                                for item in array.iter() {
                                    owners.push(row);
                                    items.push(item.clone());
                                    item_nodes.push(nodes.get(row).cloned().flatten());
                                }
                            }
                        },
                    }
                }
                let mut produced = Self::items(runner, step, body, &items, &item_nodes).into_iter();
                let mut item_inputs = items.into_iter();
                shaped
                    .into_iter()
                    .map(|shape| {
                        let count = shape?;
                        let mut collected = Vec::with_capacity(count);
                        let mut failure: Option<String> = None;
                        for _ in 0..count {
                            let output = produced.next().unwrap_or_else(|| Ok(Variable::Null));
                            let item = item_inputs.next().unwrap_or(Variable::Null);
                            match (output, failure.is_some()) {
                                (_, true) => {}
                                (Err(e), false) => failure = Some(e),
                                (Ok(out), false) => collected.push(match transform.pass_through {
                                    true => item.clone().merge_clone(&out),
                                    false => out,
                                }),
                            }
                        }
                        match failure {
                            Some(e) => Err(e),
                            None => Ok(Variable::from_array(collected)),
                        }
                    })
                    .collect()
            }
        };

        for (row, output) in outputs.iter_mut().enumerate() {
            let Ok(value) = output else {
                continue;
            };
            if let Some(path) = &transform.output_path {
                let wrapped = Variable::empty_object();
                wrapped.dot_insert(path, value.clone());
                *value = wrapped;
            }
            if transform.pass_through {
                let mut node_input = inputs[row].clone();
                *value = node_input.merge_clone(value);
            }
        }
        outputs
    }

    pub(in crate::compiled) fn looped<'a>(
        runner: &mut LaneRunner,
        step: &Step,
        body: &Body,
        transform: &Transform,
        path: &Arc<str>,
        data: &Data<'a>,
    ) -> (Data<'a>, Vec<(usize, String)>) {
        let source = transform
            .input_field
            .as_ref()
            .and_then(|(program, _)| program.path())
            .unwrap_or_default();
        let lists = data.subtree(source);
        let mut items: Vec<Variable> = Vec::new();
        let mut spans: Vec<Option<(usize, usize)>> = Vec::with_capacity(lists.len());
        let mut failures = Vec::new();
        for (row, list) in lists.iter().enumerate() {
            match list.as_array() {
                Some(array) => {
                    let start = items.len();
                    items.extend(array.borrow().iter().cloned());
                    spans.push(Some((start, items.len())));
                }
                None => {
                    failures.push((row, "Expected an array".to_string()));
                    spans.push(None);
                }
            }
        }
        let produced = Self::items(runner, step, body, &items, &[]);
        let mut values = Vec::with_capacity(spans.len());
        for (row, span) in spans.into_iter().enumerate() {
            let Some((start, end)) = span else {
                values.push(Variable::Null);
                continue;
            };
            let mut collected = Vec::with_capacity(end - start);
            let mut failure = None;
            for (item, output) in items[start..end].iter().zip(&produced[start..end]) {
                match output {
                    Ok(out) => collected.push(match transform.pass_through {
                        true => item.clone().merge_clone(out),
                        false => out.clone(),
                    }),
                    Err(e) => {
                        failure = Some(e.clone());
                        break;
                    }
                }
            }
            match failure {
                Some(e) => {
                    failures.push((row, e));
                    values.push(Variable::Null);
                }
                None => values.push(Variable::from_array(collected)),
            }
        }
        let layer = Layer::new(Arc::from([path.clone()]), vec![Leaf::Any(values.into())], None);
        let output = match transform.pass_through {
            true => data.layered(layer),
            false => Data::record(&data.rows, layer, None),
        };
        (output, failures)
    }

    pub(in crate::compiled) fn items(
        runner: &mut LaneRunner,
        step: &Step,
        body: &Body,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
    ) -> Vec<Result<Variable, String>> {
        if inputs.iter().any(Self::foreign) {
            let kept: Vec<usize> = (0..inputs.len()).filter(|&i| !Self::foreign(&inputs[i])).collect();
            let kept_inputs: Vec<Variable> = kept.iter().map(|&i| inputs[i].clone()).collect();
            let kept_nodes: Vec<Option<Variable>> = kept.iter().map(|&i| nodes.get(i).cloned().flatten()).collect();
            let mut produced = Self::items(runner, step, body, &kept_inputs, &kept_nodes).into_iter();
            return inputs
                .iter()
                .enumerate()
                .map(|(i, input)| match Self::foreign(input) {
                    true => Self::walked(step, input.clone(), nodes.get(i).cloned().flatten(), false),
                    false => produced.next().unwrap_or(Ok(Variable::Null)),
                })
                .collect();
        }
        match Self::item_data(step, body, inputs, nodes) {
            Some(data) => {
                let (output, failures) = Self::item_run(runner, step, body, &data);
                let values = output.materialized();
                let mut results: Vec<Result<Variable, String>> = values.iter().cloned().map(Ok).collect();
                for (item, message) in failures.into_iter().rev() {
                    if let Some(result) = results.get_mut(item) {
                        *result = Err(message);
                    }
                }
                results
            }
            None => Self::body(runner, body, inputs, nodes),
        }
    }

    pub(in crate::compiled) fn foreign(value: &Variable) -> bool {
        !matches!(value, Variable::Null | Variable::Object(_))
    }

    pub(in crate::compiled) fn walked(step: &Step, input: Variable, nodes: Option<Variable>, transforms: bool) -> Result<Variable, String> {
        let row = Row {
            id: &step.node.id,
            input,
            nodes,
            programs: step.shared_programs(),
            trace: false,
        };
        row.now(&step.node, None, transforms)
            .map(|response| response.output)
            .map_err(|error| error.source.to_string())
    }

    pub(in crate::compiled) fn item_keys(step: &Step, body: &Body) -> Option<Vec<Arc<str>>> {
        if step.root || step.dynamic || matches!(&step.kind, Kind::Node { transform, .. } if transform.base.is_some()) {
            return None;
        }
        let programs: Vec<&LaneProgram> = match body {
            Body::Expression { program, .. } => vec![program],
            Body::Table(table) => table.programs(),
        };
        let mut keys: Vec<Arc<str>> = Vec::new();
        for program in &programs {
            let unary = matches!(program.kind(), zen_expression::ExpressionKind::Unary);
            for key in program.program().site_keys.iter() {
                let key = key.as_deref()?;
                match key.starts_with('$') {
                    true if unary && (key == "$" || key.starts_with("$.")) => {}
                    true => return None,
                    false if !keys.iter().any(|k| k.as_ref() == key) => keys.push(Arc::from(key)),
                    false => {}
                }
            }
        }
        let overlapping = keys.iter().any(|a| {
            keys.iter()
                .any(|b| b.strip_prefix(a.as_ref()).is_some_and(|rest| rest.starts_with('.')))
        });
        (!overlapping).then_some(keys)
    }

    pub(in crate::compiled) fn item_run<'a>(runner: &mut LaneRunner, step: &Step, body: &Body, data: &Data<'a>) -> (Data<'a>, Vec<(usize, String)>) {
        Self::bound(step, data, &[], &[], None, |bound| Self::layered(runner, body, false, data, bound, None))
    }

    fn item_data<'a>(step: &Step, body: &Body, inputs: &[Variable], nodes: &[Option<Variable>]) -> Option<Data<'a>> {
        if inputs.len() < 2 || nodes.iter().any(Option::is_some) {
            return None;
        }
        let keys = Self::item_keys(step, body)?;
        let mut leaves = Vec::with_capacity(keys.len());
        let mut present = Vec::with_capacity(keys.len());
        for key in &keys {
            let mut builder = ColumnBuilder::with_capacity(inputs.len());
            let mut mask = Vec::with_capacity(inputs.len());
            for item in inputs {
                let value = Data::lookup(item, key);
                mask.push(value.is_some());
                builder.push_variable(value.unwrap_or(Variable::Null));
            }
            leaves.push(Leaf::typed(builder.finish()));
            present.push(Mask::Bits(leaf::Bits::of(mask.len(), |row| mask[row]).into()));
        }
        let rows: Rc<[usize]> = (0..inputs.len()).collect();
        Some(Data::record(&rows, Layer::new(keys.into(), leaves, Some(present)), None))
    }

    fn body(
        runner: &mut LaneRunner,
        body: &Body,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
    ) -> Vec<Result<Variable, String>> {
        let scopes: Vec<Scope> = inputs
            .iter()
            .enumerate()
            .map(|(row, i)| Self::scope(i, nodes.get(row).and_then(Option::as_ref)))
            .collect();
        match body {
            Body::Expression { program, entries, .. } => {
                let mut out: Vec<Result<Variable, String>> = vec![Ok(Variable::Null); scopes.len()];
                runner.evaluate_many(program, &scopes, None, |row, result| {
                    out[row] = match result {
                        Some(Ok(values)) => {
                            let object = Variable::empty_object();
                            for ((key, _), value) in entries.iter().zip(values) {
                                object.dot_insert(key, value);
                            }
                            Ok(object)
                        }
                        Some(Err((stage, _))) => Err(format!(
                            r#"Failed to evaluate expression: "{}""#,
                            entries.get(stage).map(|(_, v)| v.as_ref()).unwrap_or_default()
                        )),
                        None => Ok(Variable::Null),
                    };
                });
                out
            }
            Body::Table(table) => table
                .evaluate(runner, &Bound::plain(scopes))
                .into_iter()
                .map(|o| Ok(table.value(o)))
                .collect(),
        }
    }
}
