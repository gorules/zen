use crate::nodes::function::v2::serde::JsValue;
use anyhow::Context as _;
use rquickjs::{Context, Ctx, Error as QError, FromJs, Module, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Debug;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zen_expression::variable::Variable;

#[derive(Debug, Deserialize)]
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
        let context = Context::full(runtime).context("Failed to create context")?;

        let args_str =
            serde_json::to_string(args).context("Failed to serialize function arguments")?;

        let json_response = context.with(|ctx| -> anyhow::Result<String> {
            Self::prepare(&ctx)?;
            ctx.eval::<String, _>(format!("{source};main({args_str})"))
                .map_err(|e| map_js_error(&ctx, e))
        })?;

        Self::response(&json_response)
    }

    fn prepare(ctx: &Ctx) -> anyhow::Result<()> {
        Module::evaluate(
            ctx.clone(),
            "main",
            "import 'internals'; globalThis.now = Date.now();",
        )
        .map_err(|e| map_js_error(ctx, e))?
        .finish::<()>()
        .map_err(|e| map_js_error(ctx, e))?;

        ctx.globals()
            .set("log", Vec::<isize>::new())
            .map_err(|e| map_js_error(ctx, e))?;
        Ok(())
    }

    fn response(json: &str) -> anyhow::Result<EvaluateResponse> {
        serde_json::from_str(json).context("Failed to parse function result")
    }
}

pub(crate) struct Session {
    runtime: Runtime,
    context: Context,
    base: Instant,
    started: Arc<AtomicU64>,
}

impl Session {
    pub(crate) const ROW: &'static str = "__zenRow";
    const NESTING: usize = 32;

    pub(crate) fn open(source: &str, limit: Duration) -> Option<Self> {
        let runtime = super::runtime::create_runtime().ok()?;
        let base = Instant::now();
        let started = Arc::new(AtomicU64::new(0));
        let watched = started.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || {
            base.elapsed().saturating_sub(Duration::from_nanos(watched.load(Ordering::Relaxed))) > limit
        })));
        let context = Context::full(&runtime).ok()?;
        let ready = context.with(|ctx| Script::prepare(&ctx).is_ok() && ctx.eval::<(), _>(source).is_ok());
        (ready && !runtime.is_job_pending()).then_some(Self {
            runtime,
            context,
            base,
            started,
        })
    }

    pub(crate) fn call<P>(&self, layout: &str, args: &P) -> anyhow::Result<EvaluateResponse>
    where
        P: Serialize,
    {
        let args_str =
            serde_json::to_string(args).context("Failed to serialize function arguments")?;
        self.started.store(self.base.elapsed().as_nanos() as u64, Ordering::Relaxed);
        let json_response = self.context.with(|ctx| -> anyhow::Result<String> {
            let parsed = Self::plain(&args_str)
                .then(|| ctx.json_parse(args_str.as_str()).ok())
                .flatten()
                .is_some_and(|value| ctx.globals().set(Self::ROW, value).is_ok());
            let script = match parsed {
                true => format!("{layout};main(({}))", Self::ROW),
                false => format!("{layout};main({args_str})"),
            };
            ctx.eval::<String, _>(script).map_err(|e| map_js_error(&ctx, e))
        })?;
        Script::response(&json_response)
    }

    fn plain(json: &str) -> bool {
        if json.contains("\"__proto__\"") {
            return false;
        }
        let (mut depth, mut deepest, mut quoted, mut escaped) = (0usize, 0usize, false, false);
        for byte in json.bytes() {
            match (quoted, escaped, byte) {
                (true, true, _) => escaped = false,
                (true, false, b'\\') => escaped = true,
                (true, false, b'"') => quoted = false,
                (true, false, _) => {}
                (false, _, b'"') => quoted = true,
                (false, _, b'{' | b'[') => {
                    depth += 1;
                    deepest = deepest.max(depth);
                }
                (false, _, b'}' | b']') => depth = depth.saturating_sub(1),
                (false, _, _) => {}
            }
        }
        deepest <= Self::NESTING
    }

    pub(crate) fn settled(&self) -> bool {
        !self.runtime.is_job_pending()
    }
}

fn map_js_error(ctx: &Ctx, e: QError) -> anyhow::Error {
    let error = JsValue::from_js(ctx, ctx.catch())
        .map(|v| v.0)
        .unwrap_or(Variable::String((e.to_string().as_str()).into()));

    anyhow::Error::msg(error.to_string())
}

