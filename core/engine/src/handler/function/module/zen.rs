use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::handler::custom_node_adapter::CustomNodeAdapter;
use crate::handler::function::error::{FunctionResult, ResultExt};
use crate::handler::function::listener::{RuntimeEvent, RuntimeListener};
use crate::handler::function::module::export_default;
use crate::handler::function::serde::JsValue;
use crate::handler::graph::{DecisionGraph, DecisionGraphConfig};
use crate::loader::DecisionLoader;
use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::prelude::{Async, Func, Opt};
use rquickjs::{CatchResultExt, Ctx, Function, Object};

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
        let loader = self.loader.clone();
        let adapter = self.adapter.clone();

        Box::pin(async move {
            if event != RuntimeEvent::Startup {
                return Ok(());
            };

            ctx.globals()
                .set(
                    "__evaluate",
                    Func::from(Async(
                        move |ctx: Ctx<'js>,
                              key: String,
                              context: JsValue,
                              opts: Opt<Object<'js>>| {
                            let loader = loader.clone();
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
                                    content: decision_content,
                                    max_depth,
                                    loader,
                                    adapter,
                                    iteration: iteration + 1,
                                    trace,
                                    validator_cache: None,
                                })
                                .or_throw(&ctx)?;

                                let response = sub_tree.evaluate(context.0).await.or_throw(&ctx)?;
                                let k = serde_json::to_value(response).or_throw(&ctx)?.into();

                                return rquickjs::Result::Ok(JsValue(k));
                            }
                        },
                    )),
                )
                .catch(&ctx)?;

            Ok(())
        })
    }
}

fn evaluate_expression<'js>(
    ctx: Ctx<'js>,
    expression: String,
    context: JsValue,
) -> rquickjs::Result<JsValue> {
    let s = zen_expression::evaluate_expression(expression.as_str(), context.0).or_throw(&ctx)?;

    Ok(JsValue(s))
}

fn evaluate_unary_expression<'js>(
    ctx: Ctx<'js>,
    expression: String,
    context: JsValue,
) -> rquickjs::Result<bool> {
    let s =
        zen_expression::evaluate_unary_expression(expression.as_str(), context.0).or_throw(&ctx)?;

    Ok(s)
}

fn evaluate<'js>(
    ctx: Ctx<'js>,
    key: String,
    context: JsValue,
    opts: Opt<Object<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let s: Function = ctx.globals().get("__evaluate").or_throw(&ctx)?;
    let result: rquickjs::Value = s.call((key, context, opts)).or_throw(&ctx)?;
    Ok(result)
}

pub struct ZenModule;

impl ModuleDef for ZenModule {
    fn declare<'js>(decl: &Declarations<'js>) -> rquickjs::Result<()> {
        decl.declare("evaluateExpression")?;
        decl.declare("evaluateUnaryExpression")?;
        decl.declare("evaluate")?;

        decl.declare("default")?;

        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
        export_default(ctx, exports, |default| {
            default.set("evaluateExpression", Func::from(evaluate_expression))?;
            default.set(
                "evaluateUnaryExpression",
                Func::from(evaluate_unary_expression),
            )?;
            default.set("evaluate", Func::from(evaluate))?;

            Ok(())
        })
    }
}
