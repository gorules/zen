use crate::handler::custom_node_adapter::CustomNodeAdapter;
use crate::handler::function::function::Function;
use crate::handler::graph::{DecisionGraph, DecisionGraphConfig};
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::loader::DecisionLoader;
use crate::model::DecisionNodeKind;
use crate::util::validator_cache::ValidatorCache;
use anyhow::anyhow;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct DecisionHandler<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> {
    trace: bool,
    loader: Arc<L>,
    adapter: Arc<A>,
    max_depth: u8,
    js_function: Option<Rc<Function>>,
    validator_cache: ValidatorCache,
}

impl<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> DecisionHandler<L, A> {
    pub fn new(
        trace: bool,
        max_depth: u8,
        loader: Arc<L>,
        adapter: Arc<A>,
        js_function: Option<Rc<Function>>,
        validator_cache: ValidatorCache,
    ) -> Self {
        Self {
            trace,
            loader,
            adapter,
            max_depth,
            js_function,
            validator_cache,
        }
    }

    pub fn handle<'s, 'arg, 'recursion>(
        &'s self,
        request: NodeRequest,
    ) -> Pin<Box<dyn Future<Output = NodeResult> + 'recursion>>
    where
        's: 'recursion,
        'arg: 'recursion,
    {
        Box::pin(async move {
            let content = match &request.node.kind {
                DecisionNodeKind::DecisionNode { content } => Ok(content),
                _ => Err(anyhow!("Unexpected node type")),
            }?;

            let sub_decision = self.loader.load(&content.key).await?;
            let sub_tree = DecisionGraph::try_new(DecisionGraphConfig {
                content: sub_decision,
                max_depth: self.max_depth,
                loader: self.loader.clone(),
                adapter: self.adapter.clone(),
                iteration: request.iteration + 1,
                trace: self.trace,
                validator_cache: Some(self.validator_cache.clone()),
            })?
            .with_function(self.js_function.clone());

            let sub_tree_mutex = Arc::new(Mutex::new(sub_tree));

            content
                .transform_attributes
                .run_with(request.input, |input| {
                    let sub_tree_mutex = sub_tree_mutex.clone();

                    async move {
                        let mut sub_tree_ref = sub_tree_mutex.lock().await;

                        sub_tree_ref.reset_graph();
                        sub_tree_ref
                            .evaluate(input)
                            .await
                            .map(|r| NodeResponse {
                                output: r.result,
                                trace_data: serde_json::to_value(r.trace).ok(),
                            })
                            .map_err(|e| e.source)
                    }
                })
                .await
        })
    }
}
