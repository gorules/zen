use super::traced::{Run, Traced};
use super::{CompiledGraph, Host, Nesting};
use crate::compiled::content::Content;
use crate::compiled::exact::graph::Row;
use crate::compiled::host::{NodeError, NodeHandlerExtensions, NodeResponse, NodeResult};
use crate::compiled::trace::collect::Fact;
use crate::loader::LoaderError;
use crate::EvaluationError;
use std::cell::{OnceCell, RefCell};
use std::error::Error;
use std::sync::Arc;
use zen_expression::Variable;
use zen_types::decision::TransformAttributes;

pub(crate) struct Call<'a> {
    pub(crate) key: &'a str,
    pub(crate) extensions: &'a NodeHandlerExtensions,
    pub(crate) max_depth: u8,
    pub(crate) iteration: u8,
}

pub(crate) enum Load {
    Graph(Arc<Content>),
    Kind(&'static str),
}

impl Call<'_> {
    pub(crate) async fn load(&self) -> Result<Load, LoaderError> {
        let loader = self.extensions.loader();
        let content = loader.load(self.key).await?;
        let kind = content.kind();
        match content.into_graph_arc() {
            Some(graph) => Ok(Load::Graph(
                self.extensions
                    .prepared
                    .graph(self.key, graph, loader)
                    .await,
            )),
            None => Ok(Load::Kind(kind)),
        }
    }

    pub(super) fn compiled(graph: &Content) -> Result<&CompiledGraph, Box<dyn Error>> {
        let plan = graph
            .compiled_plan
            .as_deref()
            .ok_or_else(Self::uncompiled)?;
        plan.graph().map_err(|invalid| invalid.source())
    }

    pub(super) fn uncompiled() -> Box<dyn Error> {
        "sub-decision is not compiled".into()
    }

    pub(super) fn kind(key: &str, kind: &str) -> Box<dyn Error> {
        format!("sub-decision '{key}' is a {kind}, expected graph").into()
    }

    #[allow(clippy::arc_with_non_send_sync)]
    pub(super) fn sub_extensions(&self, content: &Content) -> NodeHandlerExtensions {
        NodeHandlerExtensions {
            dt_indexes: content.dt_indexes.clone(),
            stripped_functions: content.stripped_functions.clone(),
            validator_cache: Arc::new(OnceCell::from(content.validator_cache.clone())),
            ..self.extensions.clone()
        }
    }

    pub(crate) async fn run(
        &self,
        graph: &Arc<Content>,
        input: Variable,
        parent: Option<Variable>,
    ) -> Result<Variable, Box<dyn Error>> {
        let compiled = Self::compiled(graph)?;
        let content = compiled.schemas(graph, self.extensions.loader()).await;
        let extensions = self.sub_extensions(&content);
        let parents = [parent];
        let inputs = [input];
        let nesting = Nesting {
            parents: Some(&parents),
            iteration: self.iteration + 1,
            completed: None,
            traced: false,
        };
        let state = Box::pin(compiled.run(
            Host {
                content: &content,
                extensions: &extensions,
                max_depth: self.max_depth,
            },
            &inputs,
            nesting,
        ))
        .await;
        let error = state.errors.into_vec().into_iter().next().flatten();
        let result = state.results.into_iter().next().flatten();
        match error {
            Some(error) => Err(error.to_string().into()),
            None => Ok(result.unwrap_or_else(Variable::empty_object)),
        }
    }

    pub(crate) async fn traced(
        &self,
        graph: &Arc<Content>,
        input: Variable,
        parent: Option<Variable>,
    ) -> Result<Run, Box<dyn Error>> {
        let compiled = Self::compiled(graph)?;
        let content = compiled.schemas(graph, self.extensions.loader()).await;
        let extensions = self.sub_extensions(&content);
        let traced = Traced {
            graph: compiled,
            content: &content,
            extensions: &extensions,
            max_depth: self.max_depth,
            iteration: self.iteration + 1,
            parent,
        };
        Ok(Box::pin(traced.run(input)).await)
    }

    pub(crate) async fn node(&self, row: Row<'_>, attributes: &TransformAttributes) -> NodeResult {
        let loaded: RefCell<Option<Arc<Content>>> = RefCell::new(None);
        let loaded = &loaded;
        let id = row.id;
        let nodes = &row.nodes;
        let traced = row.traced();
        let sink = &row;
        row.transform(attributes, |input, _| async move {
            let fault = |source: Box<dyn Error>| NodeError::new(id.clone(), source);
            let cached = loaded.borrow().clone();
            let graph = match cached {
                Some(graph) => graph,
                None => match self.load().await {
                    Err(error) => return Err(fault(error.into())),
                    Ok(Load::Kind(kind)) => return Err(fault(Self::kind(self.key, kind))),
                    Ok(Load::Graph(graph)) => {
                        *loaded.borrow_mut() = Some(graph.clone());
                        graph
                    }
                },
            };
            if !traced {
                let output = self.run(&graph, input, nodes.clone()).await.map_err(fault)?;
                return Ok(NodeResponse {
                    output,
                    trace_data: None,
                });
            }
            match self.traced(&graph, input, nodes.clone()).await {
                Err(source) => Err(fault(source)),
                Ok(run) => {
                    let failed = matches!(&run.result, Err(e) if !matches!(**e, EvaluationError::NodeError { .. }));
                    if !failed {
                        sink.push(Fact::Sub(Box::new(run.level)));
                    }
                    match run.result {
                        Ok(output) => Ok(NodeResponse {
                            output,
                            trace_data: None,
                        }),
                        Err(error) => Err(fault(error.to_string().into())),
                    }
                }
            }
        })
        .await
    }
}
