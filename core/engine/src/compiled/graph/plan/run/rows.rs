use super::store::{Flat, Layered, Store};
use super::Run;
use crate::compiled::census::Census;
use crate::compiled::graph::data::{Data, Layer};
use crate::compiled::graph::plan::view::{Node, Ref, M};
use crate::compiled::graph::plan::{RowsCond, RowsOp, SegPlan};
use crate::compiled::graph::{CompiledGraph, Condition, Kind};
use crate::compiled::leaf::Mask;
use crate::compiled::leaf::{Bits, Leaf};
use crate::compiled::shred::Shredder;
use crate::decision_graph::cleaner::VariableCleaner;
use crate::model::DecisionNodeKind;
use crate::nodes::NodeError;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use zen_expression::Variable;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;

impl<'a> Run<'_, 'a> {
    pub(super) fn flattened(&mut self, r: Ref, positions: Option<&Rc<[usize]>>) -> Vec<Flat<'a>> {
        let leaf = self.store.leaf(r.slot);
        let rows = self.store.rows;
        let mut shredder = Shredder::new(rows);
        let at: Box<dyn Iterator<Item = usize>> = match positions {
            Some(positions) => Box::new(positions.iter().copied()),
            None => Box::new(0..rows),
        };
        for row in at {
            if !self.store.errors.ok(row) {
                continue;
            }
            let value = leaf.get(row);
            VariableCleaner::new().clean(&value);
            shredder.visit(&value, 0, row);
        }
        shredder.finish()
    }

    pub(super) fn materialize(&mut self, seg: &SegPlan, view: &Node) -> Leaf<'a> {
        if let Some(leaf) = view
            .leaf
            .filter(|leaf| leaf.present == M::All && view.fields.is_empty())
        {
            return self.store.leaf(leaf.slot);
        }
        let shape = self.store.shape(seg, view);
        Leaf::Any(Store::objects(&shape, self.store.rows).into())
    }

    pub(super) fn layer(
        &mut self,
        seg: &SegPlan,
        node: &Node,
        prefix: &str,
        out: &mut Vec<Layered<'a>>,
    ) -> Option<Vec<u64>> {
        match (node.leaf, node.fields.is_empty()) {
            (Some(_), false) => None,
            (Some(r), true) => {
                let bits = self.store.mask(seg, r.present);
                out.push((Arc::from(prefix), self.store.leaf(r.slot), bits.clone()));
                Some(bits.to_vec())
            }
            (None, empty) => {
                let mut any = vec![0u64; self.store.words];
                for (key, child) in &node.fields {
                    let path = match prefix.is_empty() {
                        true => key.to_string(),
                        false => format!("{prefix}.{key}"),
                    };
                    let bits = self.layer(seg, child, &path, out)?;
                    any.iter_mut().zip(&bits).for_each(|(a, b)| *a |= b);
                }
                let obj = self.store.mask(seg, node.obj.unwrap_or(M::None));
                let agrees = match (empty, prefix.is_empty()) {
                    (_, true) => obj
                        .iter()
                        .zip(self.store.ones.iter())
                        .all(|(o, one)| o == one),
                    (true, false) => Bits::none(&obj),
                    (false, false) => obj.as_ref() == any.as_slice(),
                };
                agrees.then_some(any)
            }
        }
    }

    fn columnar(
        &mut self,
        seg: &SegPlan,
        view: &Node,
        positions: Option<&Rc<[usize]>>,
    ) -> Option<Data<'a>> {
        let mut leaves = Vec::new();
        self.layer(seg, view, "", &mut leaves)?;
        if leaves.iter().any(|(path, _, _)| path.is_empty()) {
            return None;
        }
        let rows = positions.map_or(self.store.rows, |p| p.len());
        let mut paths = Vec::with_capacity(leaves.len());
        let mut columns = Vec::with_capacity(leaves.len());
        let mut present = Vec::with_capacity(leaves.len());
        for (path, leaf, bits) in leaves {
            paths.push(path);
            let mask = Mask::Bits(bits);
            match positions {
                Some(positions) => {
                    columns.push(leaf.pick(positions));
                    present.push(mask.pick(positions));
                }
                None => {
                    columns.push(leaf);
                    present.push(mask);
                }
            }
        }
        let local: Rc<[usize]> = (0..rows).collect();
        Some(Data::record(
            &local,
            Layer::new(paths.into(), columns, Some(present)),
            None,
        ))
    }

    fn settle(&mut self, results: Vec<Result<Variable, NodeError>>, at: &[usize]) -> Vec<Variable> {
        results
            .into_iter()
            .zip(at)
            .map(|(result, &row)| match result {
                Ok(value) => value,
                Err(error) => {
                    self.store.errors.fault(row, error);
                    Variable::Null
                }
            })
            .collect()
    }

    pub(super) fn inputs(
        &mut self,
        seg: &SegPlan,
        input: Option<&Node>,
        nodes: &Option<Vec<(Arc<str>, Ref)>>,
        positions: Option<&Rc<[usize]>>,
    ) -> (Vec<usize>, Vec<Variable>, Vec<Option<Variable>>) {
        let at: Vec<usize> = match positions {
            Some(positions) => positions.to_vec(),
            None => (0..self.store.rows).collect(),
        };
        let inputs = match input {
            Some(view) => {
                let leaf = self.materialize(seg, view);
                at.iter().map(|&row| leaf.get(row)).collect()
            }
            None => Vec::new(),
        };
        let objects = match nodes {
            None => Vec::new(),
            Some(nodes) => {
                let leaves: Vec<(Symbol, Leaf<'a>)> = nodes
                    .iter()
                    .map(|(name, r)| (Symbol::from(name.as_ref()), self.store.leaf(r.slot)))
                    .collect();
                let entry = self.parents.and_then(|_| self.graph.input_name());
                at.iter()
                    .map(|&row| {
                        let mut map = VariableMap::with_capacity(leaves.len());
                        for ((original, _), (name, leaf)) in nodes.iter().zip(&leaves) {
                            let value = leaf.get(row);
                            let value = match (&entry, self.parents) {
                                (Some(entry), Some(parents)) if original == entry => {
                                    match parents(row) {
                                        Some(parent) => {
                                            let view = value.depth_clone(1);
                                            view.dot_insert(Variable::nodes_key().as_ref(), parent);
                                            view
                                        }
                                        None => value,
                                    }
                                }
                                _ => value,
                            };
                            map.insert(name.clone(), value);
                        }
                        Some(Variable::from_object(map))
                    })
                    .collect()
            }
        };
        (at, inputs, objects)
    }

    pub(super) async fn rows(
        &mut self,
        seg: &SegPlan,
        op: &RowsOp,
        positions: Option<&Rc<[usize]>>,
    ) {
        let graph = self.graph;
        let Some(step) = graph.step_at(op.node) else {
            return;
        };
        let data = match step.kind {
            Kind::Node { .. } | Kind::Output => self.columnar(seg, &op.view, positions),
            _ => None,
        };
        if let (Kind::Output, Some(data)) = (&step.kind, &data) {
            let nodes = self.inputs(seg, None, &op.nodes, positions).2;
            Census::mark("validate");
            let failures = graph
                .validate_input(step, data, &nodes, self.host.content, self.host.extensions)
                .await;
            for (i, message) in failures {
                let row = positions.map_or(i, |p| p[i]);
                self.store.errors.fail(row, &step.node, Rc::from(message));
            }
            return;
        }
        if let (Kind::Host(_), DecisionNodeKind::DecisionNode { content }) =
            (&step.kind, &step.node.kind)
        {
            let count = positions.map_or(self.store.rows, |p| p.len());
            if CompiledGraph::batchable(&content.transform_attributes, count) {
                Census::mark("batched");
                let values = match CompiledGraph::resolve(content, self.host.extensions).await {
                    Err(failure) => {
                        let error = failure.shared(step, &content.key);
                        match positions {
                            Some(positions) => positions
                                .iter()
                                .for_each(|&row| self.store.errors.share(row, &error)),
                            None => (0..count).for_each(|row| self.store.errors.share(row, &error)),
                        }
                        vec![Variable::Null; count]
                    }
                    Ok(graph) => {
                        let (at, inputs, nodes) =
                            self.inputs(seg, Some(&op.view), &op.nodes, positions);
                        let results = CompiledGraph::batch(
                            step,
                            content,
                            &graph,
                            &inputs,
                            &nodes,
                            self.host.extensions,
                            self.host.max_depth,
                            self.iteration,
                        )
                        .await;
                        {
                            self.completed
                                .record(op.node, self.store.rows, &at, &results);
                            self.settle(results, &at)
                        }
                    }
                };
                let leaf = self.store.place(Leaf::Any(values.into()), positions);
                self.store.slots[op.out as usize] = Some(leaf);
                return;
            }
        }
        let (at, inputs, nodes) = self.inputs(
            seg,
            data.is_none().then_some(&op.view),
            &op.nodes,
            positions,
        );
        let values: Vec<Variable> = match &step.kind {
            Kind::Host(_) => {
                Census::mark("hosted");
                let results = match &step.node.kind {
                    DecisionNodeKind::CustomNode { content } => {
                        CompiledGraph::custom(step, content, inputs, nodes, self.host.extensions)
                            .await
                    }
                    _ => {
                        CompiledGraph::hosted(
                            step,
                            &inputs,
                            &nodes,
                            self.host.extensions,
                            self.host.max_depth,
                            self.iteration,
                            self.store.rows > 1,
                        )
                        .await
                    }
                };
                {
                    self.completed
                        .record(op.node, self.store.rows, &at, &results);
                    self.settle(results, &at)
                }
            }
            Kind::Input | Kind::Output => {
                Census::mark("bounded");
                let (values, failures) = graph
                    .bounded(
                        step,
                        &inputs,
                        &nodes,
                        self.host.content,
                        self.host.extensions,
                    )
                    .await;
                for (i, message) in failures {
                    self.store.errors.fail(at[i], &step.node, Rc::from(message));
                }
                values
            }
            _ => {
                let data =
                    data.unwrap_or_else(|| Data::plain((0..inputs.len()).collect(), inputs.into()));
                let (output, failures) = graph.rowwise(&mut self.runner, step, data, &nodes);
                for (i, message) in failures {
                    self.store.errors.fail(at[i], &step.node, Rc::from(message));
                }
                if !op.keys.is_empty() {
                    for (path, slot, present) in &op.keys {
                        let (leaf, bits) = match present {
                            M::All => (output.column_at(path), None),
                            _ => {
                                let (leaf, bits) = output.present(path);
                                (leaf, Some(bits))
                            }
                        };
                        let leaf = self.store.place(leaf, positions);
                        self.store.slots[*slot as usize] = Some(leaf);
                        if let Some(bits) = bits {
                            let bits = self.store.spread(&bits.into(), positions);
                            self.store.produce(*present, bits);
                        }
                    }
                    return;
                }
                output.materialized().to_vec()
            }
        };
        let leaf = self.store.place(Leaf::Any(values.into()), positions);
        self.store.slots[op.out as usize] = Some(leaf);
    }

    pub(super) fn counted(&self, op: &RowsOp, positions: Option<&Rc<[usize]>>) {
        let kind = self.graph.step_at(op.node).map_or("?", |step| step.label());
        let rows = positions.map_or(self.store.rows, |p| p.len());
        Census::record(
            format!("{kind} | {} | {}", op.reason, Census::path()),
            rows,
            Census::elapsed(),
        );
    }

    pub(super) fn rows_condition(
        &mut self,
        seg: &SegPlan,
        op: &RowsCond,
        positions: Option<&Rc<[usize]>>,
    ) -> Vec<u64> {
        if !Census::on() {
            return self.conditioned(seg, op, positions);
        }
        let start = Instant::now();
        let bits = self.conditioned(seg, op, positions);
        let rows = positions.map_or(self.store.rows, |p| p.len());
        Census::record(
            format!("switch | {} | rows", op.reason),
            rows,
            start.elapsed().as_nanos(),
        );
        bits
    }

    fn conditioned(
        &mut self,
        seg: &SegPlan,
        op: &RowsCond,
        positions: Option<&Rc<[usize]>>,
    ) -> Vec<u64> {
        let mut bits = vec![0u64; self.store.rows.div_ceil(64)];
        let Some(Kind::Switch { conditions, .. }) =
            self.graph.step_at(op.node).map(|step| &step.kind)
        else {
            return bits;
        };
        let Some((_, Condition::Program(program))) = conditions.get(op.index) else {
            return bits;
        };
        let view = Node {
            leaf: Some(op.input),
            obj: None,
            fields: Vec::new(),
        };
        let (at, inputs, nodes) = self.inputs(seg, Some(&view), &op.nodes, positions);
        let scopes: Vec<zen_expression::Scope> = inputs
            .iter()
            .enumerate()
            .map(|(i, input)| CompiledGraph::scope(input, nodes.get(i).and_then(Option::as_ref)))
            .collect();
        for (result, &row) in self.runner.evaluate(program, &scopes).into_iter().zip(&at) {
            if matches!(result, Ok(Variable::Bool(true))) {
                Bits::set(&mut bits, row, true);
            }
        }
        bits
    }
}
