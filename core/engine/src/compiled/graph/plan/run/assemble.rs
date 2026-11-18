use super::store::{Group, Origin, Store, View};
use super::{Host, Run};
use crate::compiled::content::Content;
use crate::compiled::graph::columnar::Admitted;
use crate::compiled::graph::plan::subgraphs::Subgraphs;
use crate::compiled::graph::plan::Plan;
use crate::compiled::graph::{ColumnarOutput, CompiledGraph, Completed, Kind};
use crate::compiled::host::NodeHandlerExtensions;
use crate::compiled::leaf::{Bits, ColumnBuilder, Leaf};
use crate::compiled::output::OutputColumn;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns, Dictionary, Values};

impl<'a> Run<'_, 'a> {
    fn assemble(store: Store<'a>) -> ColumnarOutput<'a> {
        let rows = store.rows;
        let mut store = store;
        let groups = std::mem::take(&mut store.groups);
        let finish = |leaf: &Leaf<'a>, presence: &Option<Rc<[u64]>>| match presence {
            None => leaf.clone(),
            Some(mask) => Leaf::masked(leaf.clone(), mask),
        };
        let columns: Vec<(Arc<str>, Leaf<'a>)> = match groups.as_slice() {
            [Group {
                positions: None,
                leaves,
            }] => leaves
                .iter()
                .map(|(path, leaf, presence)| (path.clone(), finish(leaf, presence)))
                .collect(),
            _ => {
                let mut owner: Vec<Option<(usize, usize)>> = vec![None; rows];
                let mut reach: Vec<Vec<u64>> = Vec::with_capacity(groups.len());
                for (g, Group { positions, .. }) in groups.iter().enumerate() {
                    let mut bits = vec![0u64; rows.div_ceil(64)];
                    match positions {
                        Some(positions) => positions.iter().enumerate().for_each(|(i, &row)| {
                            owner[row] = Some((g, i));
                            Bits::set(&mut bits, row, true);
                        }),
                        None => (0..rows).for_each(|row| {
                            owner[row] = Some((g, row));
                            Bits::set(&mut bits, row, true);
                        }),
                    }
                    reach.push(bits);
                }
                let mut paths: Vec<Arc<str>> = Vec::new();
                for Group { leaves, .. } in &groups {
                    for (path, _, _) in leaves {
                        if !paths.contains(path) {
                            paths.push(path.clone());
                        }
                    }
                }
                paths
                    .into_iter()
                    .map(|path| {
                        let sources: Vec<Origin<'_, 'a>> = groups
                            .iter()
                            .map(|Group { leaves, .. }| {
                                leaves
                                    .iter()
                                    .find(|(p, _, _)| *p == path)
                                    .map(|(_, l, m)| (l, m))
                            })
                            .collect();
                        if let Some(Some((first, _))) = sources.iter().find(|s| s.is_some()) {
                            if sources.iter().flatten().all(|(s, _)| s.same(first)) {
                                if sources.iter().all(|s| s.is_some_and(|(_, m)| m.is_none())) {
                                    return (path, (*first).clone());
                                }
                                let mut combined = vec![0u64; rows.div_ceil(64)];
                                for (source, bits) in sources.iter().zip(&reach) {
                                    match source {
                                        None => {}
                                        Some((_, Some(mask))) => combined
                                            .iter_mut()
                                            .zip(bits.iter().zip(mask.iter()))
                                            .for_each(|(c, (b, m))| *c |= b & m),
                                        Some((_, None)) => {
                                            combined.iter_mut().zip(bits).for_each(|(c, b)| *c |= b)
                                        }
                                    }
                                }
                                return (path, Leaf::masked((*first).clone(), &combined.into()));
                            }
                        }
                        let views: Vec<Option<View>> = sources
                            .iter()
                            .zip(&groups)
                            .map(|(source, Group { positions, .. })| {
                                source.map(|(leaf, presence)| {
                                    match (leaf.scatter_parts(), positions) {
                                        (Some((inner, at)), Some(positions))
                                            if Rc::ptr_eq(at, positions) =>
                                        {
                                            View {
                                                column: inner.column(),
                                                local: true,
                                                presence: presence.as_ref(),
                                                dict: inner.coded_dict(),
                                            }
                                        }
                                        _ => View {
                                            column: leaf.column(),
                                            local: false,
                                            presence: presence.as_ref(),
                                            dict: leaf.coded_dict(),
                                        },
                                    }
                                })
                            })
                            .collect();
                        let located = |row: usize| -> Option<(usize, &Column, usize)> {
                            let (g, i) = owner[row]?;
                            let view = views[g].as_ref()?;
                            if view.presence.is_some_and(|p| !Bits::get(p, row)) {
                                return None;
                            }
                            let at = if view.local { i } else { row };
                            view.column.valid(at).then_some((g, &view.column, at))
                        };
                        let cell = |row: usize| located(row).map(|(_, view, at)| (view, at));
                        if let Some(array) = Self::coded_gather(rows, &views, &located) {
                            return (path, Leaf::typed(array));
                        }
                        let family = |column: &Column| match column.values {
                            Values::Scaled { .. }
                            | Values::I64(_)
                            | Values::Dict {
                                values: Dictionary::Scaled { .. },
                                ..
                            } => 0u8,
                            Values::Bool { .. }
                            | Values::Dict {
                                values: Dictionary::Bool { .. },
                                ..
                            } => 1,
                            Values::Text { .. }
                            | Values::Utf8 { .. }
                            | Values::Dict {
                                values: Dictionary::Text { .. },
                                ..
                            } => 2,
                            _ => 3,
                        };
                        let families: Vec<u8> = views
                            .iter()
                            .flatten()
                            .map(|view| family(&view.column))
                            .collect();
                        let common = families
                            .first()
                            .copied()
                            .filter(|f| families.iter().all(|x| x == f));
                        if let Some(array) = Self::typed_gather(rows, common, &cell) {
                            return (path, Leaf::typed(array));
                        }
                        let mut builder = ColumnBuilder::with_capacity(rows);
                        for row in 0..rows {
                            match cell(row) {
                                Some((view, at)) => builder.push_cell(view, at),
                                None => builder.push_null(),
                            }
                        }
                        (path, Leaf::typed(builder.finish()))
                    })
                    .collect()
            }
        };
        ColumnarOutput {
            rows,
            columns: columns
                .into_iter()
                .map(|(path, leaf)| (path, OutputColumn(CompiledGraph::cleaned(&leaf))))
                .collect(),
            errors: store.errors.take(),
            paths: crate::compiled::diagnostics::Paths {
                typed: store.counts.0,
                laned: store.counts.1,
                hosted: 0,
            },
            traces: None,
        }
    }
}

impl CompiledGraph {
    fn request_data<'a>(columns: &'a Columns<'a>) -> super::trace::Source<'a> {
        use crate::compiled::graph::data::{Data, Layer};
        use crate::compiled::leaf::Mask;
        use zen_expression::Variable;
        let rows = columns.rows;
        let scalars = |values: &[Variable]| {
            values.iter().all(|value| {
                !matches!(
                    value,
                    Variable::Array(_) | Variable::Object(_) | Variable::Dynamic(_)
                )
            })
        };
        let scalar = |column: &Column<'a>| match column.values {
            Values::List { .. } => false,
            Values::Any(values) => scalars(values),
            Values::Dict {
                values: Dictionary::Any(values),
                ..
            } => scalars(values),
            _ => true,
        };
        let clean = columns
            .columns
            .iter()
            .all(|(path, _)| !path.split('.').any(|part| part.starts_with('$')));
        let paths: Arc<[Arc<str>]> = columns
            .columns
            .iter()
            .map(|(path, _)| Arc::from(*path))
            .collect();
        let leaves: Vec<Leaf<'a>> = columns
            .columns
            .iter()
            .map(|(_, column)| match scalar(column) {
                true => Leaf::input(*column, rows),
                false => Leaf::Any((0..rows).map(|row| column.variable(row)).collect()),
            })
            .collect();
        let present: Vec<Option<Vec<u64>>> = columns
            .columns
            .iter()
            .map(|(_, column)| match column.validity {
                None => None,
                Some(_) => {
                    let bits = Bits::of(rows, |row| {
                        column.valid(row) || !matches!(column.variable(row), Variable::Null)
                    });
                    (0..rows).any(|row| !Bits::get(&bits, row)).then_some(bits)
                }
            })
            .collect();
        let masks = present.iter().any(Option::is_some).then(|| {
            present
                .into_iter()
                .map(|bits| Mask::Bits(bits.unwrap_or_else(|| Bits::ones(rows)).into()))
                .collect()
        });
        let all: Rc<[usize]> = (0..rows).collect();
        let containers = columns
            .columns
            .iter()
            .zip(&leaves)
            .filter(|((_, column), _)| !scalar(column))
            .map(|(_, leaf)| leaf.clone())
            .collect();
        super::trace::Source::Derived {
            data: Rc::new(Data::record(&all, Layer::new(paths, leaves, masks), None)),
            reserved: crate::compiled::trace::batch::Reserved::new(clean, containers, rows),
        }
    }

    pub(super) fn shared_rows(columns: &Columns<'_>) -> Rc<[zen_expression::Variable]> {
        use zen_expression::Variable;
        use zen_types::symbol::Symbol;
        let shapes: Vec<(Box<[Symbol]>, bool)> = columns
            .columns
            .iter()
            .map(|(path, column)| {
                let nested = matches!(column.values, Values::Any(_))
                    && columns
                        .columns
                        .iter()
                        .any(|(other, _)| zen_types::path::DotPath::under(other, path));
                (path.split('.').map(Symbol::from).collect(), nested)
            })
            .collect();
        (0..columns.rows)
            .map(|row| {
                let object = Variable::empty_object();
                for ((_, column), (segments, nested)) in columns.columns.iter().zip(&shapes) {
                    let value = match nested {
                        true => column.variable(row).depth_clone(usize::MAX),
                        false => column.variable(row),
                    };
                    if !matches!(value, Variable::Null) || column.valid(row) {
                        Self::shared_insert(&object, segments, value);
                    }
                }
                object
            })
            .collect()
    }

    fn flat_scalars(columns: &Columns<'_>) -> bool {
        columns
            .columns
            .iter()
            .all(|(path, column)| !path.contains('.') && !matches!(column.values, Values::Any(_)))
    }

    fn shared_insert(
        target: &zen_expression::Variable,
        segments: &[zen_types::symbol::Symbol],
        value: zen_expression::Variable,
    ) {
        use zen_expression::Variable;
        let Some((last, head)) = segments.split_last() else {
            return;
        };
        let mut current = target.shallow_clone();
        for part in head {
            let Variable::Object(object) = &current else {
                return;
            };
            let existing = object.borrow().get(part).map(Variable::shallow_clone);
            let next = match existing {
                Some(existing) => existing,
                None => {
                    let created = Variable::empty_object();
                    object
                        .borrow_mut()
                        .insert(part.clone(), created.shallow_clone());
                    created
                }
            };
            current = next;
        }
        if let Variable::Object(object) = current {
            object.borrow_mut().insert(last.clone(), value);
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn plan_traces<'a>(
        &self,
        content: &Arc<Content>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        columns: &Columns<'_>,
        trace: super::PlanTrace<'a>,
        output: &ColumnarOutput<'_>,
        deferred: &[usize],
        format: crate::compiled::trace::TraceFormat,
    ) -> crate::compiled::trace::TraceBatch<'a> {
        let rows = columns.rows;
        let mut traces = crate::compiled::trace::batch::Traces::new(
            self.template().clone(),
            self.steps.len(),
            rows,
        );
        traces.events = trace.events;
        let requests = match trace.source {
            super::trace::Source::Rows { requests, .. } => {
                crate::compiled::trace::Requests::Owned(requests.to_vec())
            }
            super::trace::Source::Derived { data, reserved } => {
                traces.derived = Some(reserved);
                crate::compiled::trace::Requests::derived(data)
            }
        };
        let mut failures: Vec<Option<(Arc<str>, String)>> = vec![None; rows];
        for row in 0..rows {
            let errored = output.errors.get(row).is_some_and(Option::is_some);
            if !errored && !deferred.contains(&row) {
                continue;
            }
            extensions.context.restart(&[row]);
            let fresh = NodeHandlerExtensions {
                function_runtime: Default::default(),
                context: extensions.context.at(row),
                ..extensions.clone()
            };
            let traced = crate::compiled::traced::Traced {
                graph: self,
                content,
                extensions: &fresh,
                max_depth,
                iteration: 0,
                parent: None,
            };
            let request = requests.get(row).unwrap_or(zen_expression::Variable::Null);
            let run = Box::pin(traced.run(request.deep_clone())).await;
            failures[row] = crate::compiled::trace::failure(&run.result);
            if let Some(slot) = traces.replaced.get_mut(row) {
                *slot = Some(run.recording);
            }
        }
        crate::compiled::trace::TraceBatch::with_failures(
            traces,
            requests,
            Vec::new(),
            failures,
            format,
            &extensions.context,
        )
    }
}

impl Plan {
    #[allow(clippy::too_many_arguments)]
    pub async fn evaluate<'a>(
        &self,
        graph: &CompiledGraph,
        content: &Arc<Content>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        columns: &'a Columns<'a>,
        admitted: Admitted<'a>,
        traced: Option<crate::compiled::trace::TraceFormat>,
    ) -> PlanOutcome<'a> {
        if let Some(resolved) = &content.resolved_schemas {
            let touched = graph.steps.iter().flatten().any(|step| {
                !matches!(step.kind, Kind::Input | Kind::Output)
                    && resolved.contains_key(&step.node.id)
            });
            if touched {
                return PlanOutcome::Skipped;
            }
        }
        let Admitted { failures, dated } = admitted;
        let indexes: Vec<usize> = dated.iter().map(|(index, _)| *index).collect();
        let subgraphs = match Subgraphs::wanted(graph) {
            true => Some(Arc::new(Subgraphs::resolve(graph, extensions).await)),
            false => None,
        };
        let Some((layout, root)) = self.root(graph, columns, subgraphs.as_ref(), &indexes) else {
            return PlanOutcome::Skipped;
        };
        let Ok(root) = root.as_ref().as_ref() else {
            return PlanOutcome::Skipped;
        };
        if traced.is_some() {
            let hosted = graph.steps.iter().flatten().any(|step| {
                matches!(step.kind, Kind::Host(_) | Kind::Switch { .. }) || step.draws()
            }) || root.ops.iter().any(|op| match op {
                crate::compiled::graph::plan::Op::Child(_) => true,
                crate::compiled::graph::plan::Op::Rows(rows) => graph
                    .step_at(rows.node)
                    .is_none_or(|step| matches!(step.kind, Kind::Host(_))),
                _ => false,
            });
            if hosted {
                if std::env::var("ZEN_TRACE_COLUMNAR_DEBUG").is_ok_and(|v| v != "0") {
                    eprintln!("columnar plan not traced: hosted node");
                }
                return PlanOutcome::Untraced;
            }
        }
        crate::compiled::trace::count_run();
        let overlapping = graph.steps.iter().flatten().any(|step| {
            matches!(
                &step.kind,
                Kind::Node {
                    body: crate::compiled::graph::Body::Expression {
                        overlapping: true,
                        ..
                    },
                    ..
                }
            )
        });
        let rowwise = graph.steps.iter().flatten().any(|step| {
            step.nodes || matches!(&step.kind, Kind::Node { transform, .. } if transform.looped)
        }) || root.ops.iter().any(|op| {
            matches!(
                op,
                crate::compiled::graph::plan::Op::Rows(_)
                    | crate::compiled::graph::plan::Op::Child(_)
            )
        });
        let mut store = Store::new(root, columns, dated);
        for (row, error) in failures {
            store.errors.set(row, error);
        }
        let mut run = Run {
            graph,
            layout: &layout,
            runner: CompiledGraph::RUNNER.with_borrow_mut(std::mem::take),
            host: Host {
                content,
                extensions,
                max_depth,
            },
            store,
            settled: true,
            failures: Vec::new(),
            shreds: Vec::new(),
            completed: Completed::default(),
            iteration: 0,
            parents: None,
            trace: traced.map(|_| {
                let lazy = !overlapping && !rowwise;
                let source = match (lazy && indexes.is_empty(), indexes.is_empty()) {
                    (true, _) => CompiledGraph::request_data(columns),
                    (false, plain) => super::trace::Source::Rows {
                        requests: CompiledGraph::shared_rows(columns),
                        shared: plain && CompiledGraph::flat_scalars(columns),
                    },
                };
                Box::new(super::PlanTrace::new(graph.steps.len(), source, lazy))
            }),
        };
        run.walk(root.clone()).await;
        let Run {
            runner,
            store,
            completed,
            trace,
            ..
        } = run;
        if let Some(reason) = trace.as_ref().and_then(|t| t.refused) {
            if std::env::var("ZEN_TRACE_COLUMNAR_DEBUG").is_ok_and(|v| v != "0") {
                eprintln!("columnar plan not traced: {reason}");
            }
            CompiledGraph::RUNNER.with_borrow_mut(|slot| *slot = runner);
            return PlanOutcome::Untraced;
        }
        CompiledGraph::RUNNER.with_borrow_mut(|slot| *slot = runner);
        let rows = store.errors.deferred();
        let output = Run::assemble(store);
        let output = match (traced, trace) {
            (Some(format), Some(trace)) => {
                let batch = graph
                    .plan_traces(
                        content, extensions, max_depth, columns, *trace, &output, &rows, format,
                    )
                    .await;
                output.traced(batch)
            }
            _ => output,
        };
        match rows.is_empty() {
            true => PlanOutcome::Done(output),
            false => PlanOutcome::Partial {
                output,
                rows,
                completed,
            },
        }
    }
}

pub(crate) enum PlanOutcome<'a> {
    Skipped,
    Untraced,
    Done(ColumnarOutput<'a>),
    Partial {
        output: ColumnarOutput<'a>,
        rows: Vec<usize>,
        completed: Completed,
    },
}
