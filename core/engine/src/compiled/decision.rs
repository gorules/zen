use crate::compiled::content::Content;
use crate::compiled::host::NodeHandlerExtensions;
use crate::compiled::pinning;
use crate::compiled::prepared::Prepared;
use crate::compiled::traced::Traced;
use crate::compiled::{ColumnarOutput, CompiledGraph, CompiledPlan, Invalid};
use crate::decision_graph::graph::DecisionGraphResponse;
use crate::engine::EvaluationOptions;
use crate::loader::DynamicLoader;
use crate::model::GraphContent;
use crate::nodes::custom::DynamicCustomNode;
use crate::nodes::function::http_handler::DynamicHttpHandler;
use crate::EvaluationError;
use std::cell::OnceCell;
use std::sync::{Arc, OnceLock};
use zen_expression::clock::EvalContext;
use zen_expression::lane::Columns;
use zen_expression::variable::Variable;

type Planned = (Arc<Content>, Arc<CompiledPlan>);

#[derive(Debug)]
pub(crate) struct CompiledDecision {
    source: Arc<GraphContent>,
    content: Arc<Content>,
    prepared: OnceLock<Planned>,
    hosted: OnceLock<Planned>,
    loader: DynamicLoader,
    adapter: DynamicCustomNode,
    http_handler: DynamicHttpHandler,
    children: Arc<Prepared>,
}

impl CompiledDecision {
    pub(crate) fn new(
        source: Arc<GraphContent>,
        loader: DynamicLoader,
        adapter: DynamicCustomNode,
        http_handler: DynamicHttpHandler,
        children: Arc<Prepared>,
    ) -> Self {
        Self {
            content: Arc::new(Content::of(&source)),
            source,
            prepared: OnceLock::new(),
            hosted: OnceLock::new(),
            loader,
            adapter,
            http_handler,
            children,
        }
    }

    pub(crate) fn holds(&self, content: &Arc<GraphContent>) -> bool {
        Arc::ptr_eq(&self.source, content)
    }

    pub(crate) async fn evaluate_pinned(
        &self,
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let eval = pinning::single(&options);
        self.evaluate_with_opts(context, options, &eval).await
    }

    pub(crate) async fn evaluate_batch_pinned(
        &self,
        contexts: &[Variable],
        options: EvaluationOptions,
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        let eval = pinning::batch(&options);
        self.evaluate_batch(contexts, options, &eval).await
    }

    pub(crate) async fn evaluate_columns_pinned<'a>(
        &self,
        columns: &'a Columns<'a>,
        options: EvaluationOptions,
    ) -> ColumnarOutput<'a> {
        let eval = pinning::batch(&options);
        self.evaluate_columns(columns, options, &eval).await
    }

    pub(crate) async fn trace_pair_pinned(
        &self,
        context: Variable,
    ) -> Option<(
        crate::compiled::trace::TraceDocument,
        crate::compiled::trace::TraceDocument,
    )> {
        let options = EvaluationOptions {
            trace: crate::compiled::trace::TraceFormat::V1,
            ..Default::default()
        };
        let eval = pinning::single(&options);
        self.trace_pair(context, &eval).await
    }

    pub(crate) async fn evaluate_with_opts(
        &self,
        context: Variable,
        options: EvaluationOptions,
        eval: &EvalContext,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        if options.trace.is_on() {
            return self.traced(context, options, eval).await;
        }

        let (compiled, content) = Self::compiled(self.single()).map_err(Invalid::evaluation)?;
        let content = compiled.schemas(content, &self.loader).await;
        compiled
            .evaluate_one(
                &content,
                &self.extensions(&content, eval),
                options.max_depth,
                context,
            )
            .await
    }

    pub(crate) async fn evaluate_batch(
        &self,
        contexts: &[Variable],
        options: EvaluationOptions,
        eval: &EvalContext,
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        self.evaluate_batched(contexts, options, eval).await
    }

    async fn evaluate_batched(
        &self,
        contexts: &[Variable],
        options: EvaluationOptions,
        eval: &EvalContext,
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        let owned: Vec<Variable> = contexts.iter().map(Variable::deep_clone).collect();
        if options.trace.is_on() {
            let start = std::time::Instant::now();
            return match self.traced_batch(&owned, contexts, options, eval).await {
                Ok((results, batch)) => {
                    batch.responses(results, &format!("{:.1?}", start.elapsed()))
                }
                Err(invalid) => owned.iter().map(|_| Err(invalid.evaluation())).collect(),
            };
        }

        match Self::compiled(self.prepared()) {
            Ok((compiled, content)) => {
                let content = compiled.schemas(content, &self.loader).await;
                compiled
                    .evaluate(
                        &content,
                        &self.extensions(&content, eval),
                        options.max_depth,
                        &owned,
                    )
                    .await
            }
            Err(invalid) => owned.iter().map(|_| Err(invalid.evaluation())).collect(),
        }
    }

    pub(crate) async fn evaluate_columns<'a>(
        &self,
        columns: &'a Columns<'a>,
        options: EvaluationOptions,
        eval: &EvalContext,
    ) -> ColumnarOutput<'a> {
        self.evaluate_columned(columns, options, eval).await
    }

    async fn evaluate_columned<'a>(
        &self,
        columns: &'a Columns<'a>,
        options: EvaluationOptions,
        eval: &EvalContext,
    ) -> ColumnarOutput<'a> {
        if options.trace.is_on() {
            if let Ok((compiled, content)) = Self::compiled(self.prepared()) {
                let content = compiled.schemas(content, &self.loader).await;
                if let Some(output) = compiled
                    .evaluate_columns_traced(
                        &content,
                        &self.extensions(&content, eval),
                        options.max_depth,
                        columns,
                        options.trace,
                    )
                    .await
                {
                    crate::compiled::trace::count_columnar(true, columns.rows);
                    return output;
                }
            }
            crate::compiled::trace::count_columnar(false, columns.rows);
            eval.clock().reset();
            let contexts: Vec<Variable> = (0..columns.rows).map(|row| columns.row(row)).collect();
            let owned: Vec<Variable> = contexts.iter().map(Variable::deep_clone).collect();
            return match self.traced_batch(&owned, &contexts, options, eval).await {
                Ok((results, batch)) => {
                    let results = results
                        .into_iter()
                        .map(|result| {
                            result.map(|result| DecisionGraphResponse {
                                performance: String::new(),
                                result,
                                trace: None,
                            })
                        })
                        .collect();
                    ColumnarOutput::from_results(results).traced(batch)
                }
                Err(invalid) => ColumnarOutput::from_results(
                    owned.iter().map(|_| Err(invalid.evaluation())).collect(),
                ),
            };
        }
        match Self::compiled(self.prepared()) {
            Ok((compiled, content)) => {
                let content = compiled.schemas(content, &self.loader).await;
                compiled
                    .evaluate_columns(
                        &content,
                        &self.extensions(&content, eval),
                        options.max_depth,
                        columns,
                    )
                    .await
            }
            Err(_) => {
                let contexts: Vec<Variable> =
                    (0..columns.rows).map(|row| columns.row(row)).collect();
                let options = EvaluationOptions {
                    trace: crate::compiled::trace::TraceFormat::Off,
                    ..options
                };
                let results = self.evaluate_batched(&contexts, options, eval).await;
                ColumnarOutput::from_results(results)
            }
        }
    }

    pub(crate) async fn traced_batch(
        &self,
        owned: &[Variable],
        requests: &[Variable],
        options: EvaluationOptions,
        eval: &EvalContext,
    ) -> Result<
        (
            Vec<Result<Variable, Box<EvaluationError>>>,
            crate::compiled::trace::TraceBatch,
        ),
        &Invalid,
    > {
        let (compiled, content) = Self::compiled(self.prepared())?;
        let content = compiled.schemas(content, &self.loader).await;
        let (results, traces) = compiled
            .traced_batch(
                &content,
                &self.extensions(&content, eval),
                options.max_depth,
                owned,
                requests,
            )
            .await;
        let batch = crate::compiled::trace::TraceBatch::new(
            traces,
            requests.to_vec(),
            owned.to_vec(),
            &results,
            options.trace,
            eval,
        );
        Ok((results, batch))
    }

    async fn traced(
        &self,
        context: Variable,
        options: EvaluationOptions,
        eval: &EvalContext,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let start = std::time::Instant::now();
        let request = crate::compiled::trace::Request::take(&context);
        let run = self.run(context, options.max_depth, eval).await?;
        let evaluation = pinning::evaluation(eval);
        crate::compiled::trace::collected(options.trace, request, run, start, Some(&evaluation))
    }

    async fn run(
        &self,
        context: Variable,
        max_depth: u8,
        eval: &EvalContext,
    ) -> Result<crate::compiled::traced::Run, Box<EvaluationError>> {
        let (compiled, content) = Self::compiled(self.tracing()).map_err(Invalid::evaluation)?;
        let content = compiled.schemas(content, &self.loader).await;
        let extensions = self.extensions(&content, eval);
        let traced = Traced {
            graph: compiled,
            content: &content,
            extensions: &extensions,
            max_depth,
            iteration: 0,
            parent: None,
        };
        Ok(traced.run(context).await)
    }

    pub(crate) async fn trace_pair(
        &self,
        context: Variable,
        eval: &EvalContext,
    ) -> Option<(
        crate::compiled::trace::TraceDocument,
        crate::compiled::trace::TraceDocument,
    )> {
        let request = crate::compiled::trace::Request::take(&context);
        let run = self.run(context, 10, eval).await.ok()?;
        let evaluation = pinning::evaluation(eval);
        crate::compiled::trace::pair(request, run, Some(&evaluation))
    }

    fn prepared(&self) -> (&Arc<Content>, &CompiledPlan) {
        let (content, plan) = self.prepared.get_or_init(|| {
            let mut owned = (*self.content).clone();
            let plan = owned.prepare();
            (Arc::new(owned), plan)
        });
        (content, plan)
    }

    fn hosted(&self) -> (&Arc<Content>, &CompiledPlan) {
        let (content, plan) = self.hosted.get_or_init(|| {
            let (content, plan) = self.content.hosted();
            (Arc::new(content), plan)
        });
        (content, plan)
    }

    fn single(&self) -> (&Arc<Content>, &CompiledPlan) {
        match (self.prepared.get(), self.hosted.get()) {
            (None, None) => self.hosted(),
            _ => self.prepared(),
        }
    }

    fn tracing(&self) -> (&Arc<Content>, &CompiledPlan) {
        match self.prepared.get() {
            None => self.hosted(),
            _ => self.prepared(),
        }
    }

    fn compiled<'c>(
        (content, plan): (&'c Arc<Content>, &'c CompiledPlan),
    ) -> Result<(&'c CompiledGraph, &'c Arc<Content>), &'c Invalid> {
        plan.graph().map(|compiled| (compiled, content))
    }

    #[allow(clippy::arc_with_non_send_sync)]
    fn extensions(&self, content: &Content, eval: &EvalContext) -> NodeHandlerExtensions {
        NodeHandlerExtensions {
            function_runtime: Default::default(),
            loader: self.loader.clone(),
            custom_node: self.adapter.clone(),
            http_handler: self.http_handler.clone(),
            dt_indexes: content.dt_indexes.clone(),
            stripped_functions: content.stripped_functions.clone(),
            validator_cache: Arc::new(OnceCell::from(content.validator_cache.clone())),
            prepared: self.children.clone(),
            eval: eval.clone(),
        }
    }

    pub(crate) fn compile(&self) {
        self.prepared();
    }

    pub(crate) fn plan_verdict(&self, columns: &Columns) -> Option<Result<(), String>> {
        let (compiled, _) = Self::compiled(self.prepared()).ok()?;
        Some(compiled.plan_verdict(columns))
    }

    pub(crate) fn degraded(&self) -> Option<Vec<(Arc<str>, Arc<str>)>> {
        let (compiled, _) = Self::compiled(self.prepared()).ok()?;
        Some(compiled.degraded())
    }

    pub(crate) fn compiled_verdict(&self) -> Option<Result<(), String>> {
        self.prepared().0.compiled_verdict()
    }
}
