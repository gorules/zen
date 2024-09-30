use crate::handler::function::serde::JsValue;
use anyhow::Context as _;
use rquickjs::{Context, Ctx, Error as QError, FromJs, Module, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Debug;
use std::rc::Rc;
use zen_expression::variable::Variable;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateResponse {
    pub output: Variable,
    pub log: Vec<Value>,
}

pub struct Script {
    runtime: Runtime,
}

impl Script {
    pub fn new(runtime: Runtime) -> Self {
        Self { runtime }
    }

    pub async fn call<P>(&mut self, source: &str, args: &P) -> anyhow::Result<EvaluateResponse>
    where
        P: Serialize,
    {
        let runtime = &self.runtime;
        let context = Context::full(&runtime).context("Failed to create context")?;

        let args_str =
            serde_json::to_string(args).context("Failed to serialize function arguments")?;

        let json_response = context.with(|ctx| -> anyhow::Result<String> {
            Module::evaluate(
                ctx.clone(),
                "main",
                "import 'internals'; globalThis.now = Date.now();",
            )
            .unwrap()
            .finish::<()>()
            .unwrap();

            let _ = ctx
                .globals()
                .set("log", Vec::<isize>::new())
                .map_err(|e| map_js_error(&ctx, e))?;

            ctx.eval::<String, _>(format!("{source};main({args_str})"))
                .map_err(|e| map_js_error(&ctx, e))
        })?;

        serde_json::from_str(json_response.as_str()).context("Failed to parse function result")
    }
}

fn map_js_error(ctx: &Ctx, e: QError) -> anyhow::Error {
    let error = JsValue::from_js(&ctx, ctx.catch())
        .map(|v| v.0)
        .unwrap_or(Variable::String(Rc::from(e.to_string().as_str())));

    anyhow::Error::msg(error.to_string())
}
