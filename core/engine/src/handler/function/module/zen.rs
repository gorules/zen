use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;

use rquickjs::prelude::{Async, Func, Opt};
use rquickjs::{CatchResultExt, Ctx, Object};

use crate::handler::custom_node_adapter::CustomNodeAdapter;
use crate::handler::function::error::{FunctionResult, ResultExt};
use crate::handler::function::listener::{RuntimeEvent, RuntimeListener};
use crate::handler::function::serde::JsValue;
use crate::handler::graph::{DecisionGraph, DecisionGraphConfig};
use crate::loader::DecisionLoader;

pub(crate) struct ZenListener<Loader, Adapter> {
    pub loader: Arc<Loader>,
    pub adapter: Arc<Adapter>,
}

impl<Loader: DecisionLoader + 'static, Adapter: CustomNodeAdapter + 'static> RuntimeListener
    for ZenListener<Loader, Adapter>
{
    fn on_event<'js>(
        &self,
        ctx: Ctx<'js>,
        event: RuntimeEvent,
    ) -> Pin<Box<dyn Future<Output = FunctionResult> + 'js>> {
        let loader1 = self.loader.clone();
        let loader2 = self.loader.clone();
        let adapter = self.adapter.clone();

        Box::pin(async move {
            if event != RuntimeEvent::Startup {
                return Ok(());
            };

            ctx.globals()
                .set(
                    "__getContent",
                    Func::from(Async(move |ctx: Ctx<'js>, key: String| {
                        let loader = loader1.clone();
                        async move {
                            let load_result = loader.load(key.as_str()).await;
                            let decision_content = load_result.or_throw(&ctx)?;

                            return rquickjs::Result::Ok(JsValue(
                                serde_json::to_value(decision_content.deref()).unwrap(),
                            ));
                        }
                    })),
                )
                .catch(&ctx)?;

            ctx.globals()
                .set(
                    "__evaluate",
                    Func::from(Async(
                        move |ctx: Ctx<'js>,
                              key: String,
                              context: JsValue,
                              opts: Opt<Object<'js>>| {
                            let loader = loader2.clone();
                            let adapter = adapter.clone();

                            async move {
                                let config: Object = ctx.globals().get("config").or_throw(&ctx)?;

                                let iteration: u8 = config.get("iteration").or_throw(&ctx)?;
                                let max_depth: u8 = config.get("maxDepth").or_throw(&ctx)?;
                                let trace = opts
                                    .0
                                    .map(|opt| opt.get::<_, bool>("trace").unwrap_or_default())
                                    .unwrap_or_default();

                                let load_result = loader.load(key.as_str()).await;
                                let decision_content = load_result.or_throw(&ctx)?;
                                let mut sub_tree = DecisionGraph::try_new(DecisionGraphConfig {
                                    content: &decision_content,
                                    max_depth,
                                    loader,
                                    adapter,
                                    iteration: iteration + 1,
                                    trace,
                                })
                                .or_throw(&ctx)?;

                                let response =
                                    sub_tree.evaluate(&context.0).await.or_throw(&ctx)?;
                                return rquickjs::Result::Ok(JsValue(
                                    serde_json::to_value(response).unwrap(),
                                ));
                            }
                        },
                    )),
                )
                .catch(&ctx)?;

            Ok(())
        })
    }
}

#[rquickjs::module(rename_vars = "camelCase")]
pub mod zen_module {
    use crate::handler::function::error::ResultExt;
    use crate::handler::function::serde::JsValue;
    use rquickjs::prelude::Opt;
    use rquickjs::{Ctx, Function, Object};

    #[allow(non_snake_case)]
    #[rquickjs::function]
    pub fn evaluateExpression<'js>(
        ctx: Ctx<'js>,
        expression: String,
        context: JsValue,
    ) -> rquickjs::Result<JsValue> {
        let s =
            zen_expression::evaluate_expression(expression.as_str(), &context.0).or_throw(&ctx)?;

        Ok(JsValue(s))
    }

    #[allow(non_snake_case)]
    #[rquickjs::function]
    pub fn evaluateUnaryExpression<'js>(
        ctx: Ctx<'js>,
        expression: String,
        context: JsValue,
    ) -> rquickjs::Result<bool> {
        let s = zen_expression::evaluate_unary_expression(expression.as_str(), &context.0)
            .or_throw(&ctx)?;

        Ok(s)
    }

    #[allow(non_snake_case)]
    #[rquickjs::function]
    pub fn evaluate<'js>(
        ctx: Ctx<'js>,
        key: String,
        context: JsValue,
        opts: Opt<Object<'js>>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let s: Function = ctx.globals().get("__evaluate").or_throw(&ctx)?;
        let result: rquickjs::Value = s.call((key, context, opts)).or_throw(&ctx)?;
        Ok(result)
    }

    #[allow(non_snake_case)]
    #[rquickjs::function]
    pub fn get<'js>(ctx: Ctx<'js>, key: String) -> rquickjs::Result<rquickjs::Value<'js>> {
        let s: Function = ctx.globals().get("__getContent").or_throw(&ctx)?;
        let result: rquickjs::Value = s.call((key,)).or_throw(&ctx)?;
        Ok(result)
    }
}
