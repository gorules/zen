use super::row::Row;
use super::{Nesting, Source};
use crate::loader::LoaderError;
use crate::model::GraphContent;
use crate::nodes::{NodeError, NodeHandlerExtensions, NodeResponse, NodeResult};
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
    Graph(Arc<GraphContent>),
    Kind(&'static str),
}

impl Call<'_> {
    pub(crate) async fn load(&self) -> Result<Load, LoaderError> {
        let loader = self.extensions.loader();
        let content = loader.load(self.key).await?;
        let kind = content.kind();
        match content.into_graph_arc() {
            Some(graph) => Ok(Load::Graph(GraphContent::loaded(graph, loader).await)),
            None => Ok(Load::Kind(kind)),
        }
    }

    pub(crate) async fn run(
        &self,
        graph: &Arc<GraphContent>,
        input: Variable,
        parent: Option<Variable>,
    ) -> Result<Variable, Box<dyn Error>> {
        let Some(plan) = graph.compiled_plan.as_deref() else {
            return Ok(Variable::Null);
        };
        let compiled = plan.graph().map_err(|invalid| invalid.source())?;
        let content = compiled.schemas(graph, self.extensions.loader()).await;
        let extensions = NodeHandlerExtensions {
            compiled_cache: content.compiled_cache.clone(),
            dt_indexes: content.dt_indexes.clone(),
            validator_cache: Arc::new(OnceCell::from(content.validator_cache.clone())),
            ..self.extensions.clone()
        };
        let parents = [parent];
        let inputs = [input];
        let nesting = Nesting {
            parents: Some(&parents),
            iteration: self.iteration + 1,
        };
        let state = Box::pin(compiled.run(&content, &extensions, self.max_depth, &Source::Rows(&inputs), 1, false, nesting)).await;
        let error = state.errors.into_iter().next().flatten();
        let result = state.results.into_iter().next().flatten();
        match error {
            Some(error) => Err(error.to_string().into()),
            None => Ok(result.unwrap_or_else(Variable::empty_object)),
        }
    }

    pub(crate) async fn node(&self, row: Row<'_>, attributes: &TransformAttributes) -> NodeResult {
        let loaded: RefCell<Option<Arc<GraphContent>>> = RefCell::new(None);
        let loaded = &loaded;
        let id = row.id;
        let nodes = &row.nodes;
        row.transform(attributes, |input, _| async move {
            let fault = |source: Box<dyn Error>| NodeError {
                node_id: id.clone(),
                trace: None,
                source,
            };
            let cached = loaded.borrow().clone();
            let graph = match cached {
                Some(graph) => graph,
                None => match self.load().await {
                    Err(error) => return Err(fault(error.into())),
                    Ok(Load::Kind(kind)) => {
                        return Err(fault(format!("sub-decision '{}' is a {kind}, expected graph", self.key).into()))
                    }
                    Ok(Load::Graph(graph)) => {
                        *loaded.borrow_mut() = Some(graph.clone());
                        graph
                    }
                },
            };
            let output = self.run(&graph, input, nodes.clone()).await.map_err(fault)?;
            Ok(NodeResponse {
                output,
                trace_data: None,
            })
        })
        .await
    }
}
