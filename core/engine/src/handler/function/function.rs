use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rquickjs::promise::MaybePromise;
use rquickjs::{async_with, AsyncContext, AsyncRuntime, CatchResultExt, Ctx, Module};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::handler::function::error::{FunctionError, FunctionResult};
use crate::handler::function::listener::{RuntimeEvent, RuntimeListener};
use crate::handler::function::module::console::{Console, ConsoleListener, Log};
use crate::handler::function::module::ModuleLoader;
use crate::handler::function::serde::JsValue;
use crate::handler::node::NodeResult;

pub struct Function {
    rt: Arc<AsyncRuntime>,
    ctx: AsyncContext,

    listeners: Vec<Box<dyn RuntimeListener>>,
    module_loader: ModuleLoader,
}

impl Function {
    pub async fn create<'js>() -> FunctionResult<Self> {
        let module_loader = ModuleLoader::new();
        let rt = Arc::new(AsyncRuntime::new()?);

        rt.set_loader(module_loader.clone(), module_loader.clone())
            .await;

        let ctx = AsyncContext::full(&rt).await?;
        let this = Self {
            rt,
            ctx,
            module_loader,
            listeners: vec![Box::new(ConsoleListener)],
        };

        this.dispatch_event(RuntimeEvent::Startup).await?;
        Ok(this)
    }

    fn dispatch_event_inner(&self, ctx: &Ctx, event: RuntimeEvent) -> FunctionResult {
        for listener in &self.listeners {
            if let Err(err) = listener.on_event(&ctx, &event) {
                return Err(err.into());
            };
        }

        return Ok(());
    }

    async fn dispatch_event(&self, event: RuntimeEvent) -> FunctionResult {
        self.ctx
            .with(|ctx| self.dispatch_event_inner(&ctx, event))
            .await
    }

    pub fn context(&self) -> &AsyncContext {
        &self.ctx
    }

    pub fn runtime(&self) -> &AsyncRuntime {
        &self.rt
    }

    pub async fn register_module(&self, name: &str, source: &str) -> FunctionResult {
        let maybe_error: Option<FunctionError> = async_with!(&self.ctx => |ctx| {
            if let Err(err) = Module::declare(ctx.clone(), name.as_bytes().to_vec(), source.as_bytes().to_vec()).catch(&ctx) {
                return Some(err.into())
            }

            return None;
        }).await;
        if let Some(err) = maybe_error {
            return Err(err);
        }

        self.module_loader.add_module(name.to_string());
        Ok(())
    }

    pub(crate) async fn call_handler(
        &self,
        name: &str,
        data: JsValue,
    ) -> FunctionResult<HandlerResponse> {
        let k: FunctionResult<HandlerResponse> = async_with!(&self.ctx => |ctx| {
            self.dispatch_event_inner(&ctx, RuntimeEvent::SoftReset)?;

            let m: rquickjs::Object = Module::import(&ctx, name).catch(&ctx)?.into_future().await.catch(&ctx)?;
            let handler: rquickjs::Function = m.get("handler").catch(&ctx)?;

            let handler_promise: MaybePromise = handler.call((data, 5)).catch(&ctx)?;
            let handler_result = handler_promise.into_future::<JsValue>().await.catch(&ctx)?;

            let console = Console::from_context(&ctx).unwrap();
            let logs = console.logs.into_inner();

            Ok(HandlerResponse { data: handler_result.0, logs })
        })
        .await;

        Ok(k?)
    }
}

#[derive(Serialize, Deserialize)]
pub struct HandlerResponse {
    pub logs: Vec<Log>,
    pub data: Value,
}
