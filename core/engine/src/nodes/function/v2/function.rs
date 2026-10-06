use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use crate::nodes::function::v2::error::{FunctionError, FunctionResult, ResultExt};
use crate::nodes::function::v2::listener::{RuntimeEvent, RuntimeListener};
use crate::nodes::function::v2::module::console::{Console, Log};
use crate::nodes::function::v2::module::ModuleLoader;
use crate::nodes::function::v2::serde::{JsValue, JsValueWithNodes};
use rquickjs::promise::MaybePromise;
use rquickjs::prelude::Func;
use rquickjs::{async_with, AsyncContext, AsyncRuntime, CatchResultExt, Ctx, Module, Object};
use std::rc::Rc;
use serde::{Deserialize, Serialize};
use zen_expression::variable::{ToVariable, Variable};

pub struct FunctionConfig {
    pub(crate) listeners: Option<Vec<Box<dyn RuntimeListener>>>,
}

pub struct Function {
    rt: Arc<AsyncRuntime>,
    ctx: AsyncContext,
    listeners: Vec<Box<dyn RuntimeListener>>,
    module_loader: ModuleLoader,
    declared: RefCell<HashMap<String, u64>>,
}

impl Debug for Function {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Function")
    }
}

impl Function {
    pub async fn create<'js>(config: FunctionConfig) -> FunctionResult<Self> {
        let module_loader = ModuleLoader::new();
        let rt = Arc::new(AsyncRuntime::new()?);

        rt.set_loader(module_loader.clone(), module_loader.clone())
            .await;

        let ctx = AsyncContext::full(&rt).await?;
        let this = Self {
            rt,
            ctx,
            module_loader,
            listeners: config.listeners.unwrap_or_default(),
            declared: RefCell::new(HashMap::new()),
        };

        this.dispatch_event(RuntimeEvent::Startup).await?;
        Ok(this)
    }

    async fn dispatch_event_inner(&self, ctx: &Ctx<'_>, event: RuntimeEvent) -> FunctionResult {
        for listener in &self.listeners {
            if let Err(err) = listener.on_event(ctx.clone(), event.clone()).await {
                return Err(err.into());
            };
        }

        Ok(())
    }

    async fn dispatch_event(&self, event: RuntimeEvent) -> FunctionResult {
        async_with!(&self.ctx => |ctx| {
            self.dispatch_event_inner(&ctx, event).await
        })
        .await
    }

    pub fn context(&self) -> &AsyncContext {
        &self.ctx
    }

    pub fn runtime(&self) -> &AsyncRuntime {
        &self.rt
    }

    pub fn suggest_module_name<'a>(&self, name: &str, source: &str) -> String {
        let declarative_name = format!("node:{name}");

        if self.module_loader.has_module(&declarative_name) {
            let content_hash = create_content_hash(source);
            format!("node:{name}.{content_hash:x}")
        } else {
            declarative_name
        }
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

    pub(crate) async fn shared_module(&self, name: &str, source: &str) -> FunctionResult<String> {
        let content_hash = create_content_hash(source);
        let declarative_name = format!("node:{name}");
        let module_name = match self.declared.borrow().get(&declarative_name) {
            Some(hash) if *hash == content_hash => return Ok(declarative_name),
            Some(_) => format!("node:{name}.{content_hash:x}"),
            None if self.module_loader.has_module(&declarative_name) => format!("node:{name}.{content_hash:x}"),
            None => declarative_name.clone(),
        };
        if self.declared.borrow().get(&module_name) == Some(&content_hash) {
            return Ok(module_name);
        }
        self.register_module(&module_name, source).await?;
        self.declared.borrow_mut().insert(module_name.clone(), content_hash);
        Ok(module_name)
    }

    pub(crate) async fn call_handler(
        &self,
        name: &str,
        data: JsValueWithNodes,
    ) -> FunctionResult<HandlerResponse> {
        let k: FunctionResult<HandlerResponse> = async_with!(&self.ctx => |ctx| {
            self.dispatch_event_inner(&ctx, RuntimeEvent::SoftReset).await?;

            let m: rquickjs::Object = Module::import(&ctx, name).catch(&ctx)?.into_future().await.catch(&ctx)?;
            let handler: rquickjs::Function = m.get("handler").catch(&ctx)?;

            let handler_promise: MaybePromise = handler.call((data, 5)).catch(&ctx)?;
            let handler_result = handler_promise.into_future::<JsValue>().await.catch(&ctx)?;

            let console = Console::from_context(&ctx).or_throw(&ctx)?;
            let logs = console.logs.into_inner();

            Ok(HandlerResponse { data: handler_result.0, logs })
        })
        .await;

        Ok(k?)
    }

    pub(crate) async fn call_rows(
        &self,
        name: &str,
        config: (u8, u8, bool),
        rows: Vec<(Variable, Option<Variable>)>,
        tick: &dyn Fn(),
    ) -> Vec<FunctionResult<Variable>> {
        let count = rows.len();
        let current: Rc<RefCell<Variable>> = Rc::new(RefCell::new(Variable::Null));
        let shared = current.clone();
        let results: FunctionResult<Vec<FunctionResult<Variable>>> = async_with!(&self.ctx => |ctx| {
            let settings = Object::new(ctx.clone()).catch(&ctx)?;
            settings.prop("iteration", config.0).catch(&ctx)?;
            settings.prop("maxDepth", config.1).catch(&ctx)?;
            settings.prop("trace", config.2).catch(&ctx)?;
            ctx.globals().set("config", settings).catch(&ctx)?;
            ctx.globals()
                .set("__getNodesData", Func::from(move || JsValue(shared.borrow().clone())))
                .catch(&ctx)?;
            let m: rquickjs::Object = Module::import(&ctx, name).catch(&ctx)?.into_future().await.catch(&ctx)?;
            let handler: rquickjs::Function = m.get("handler").catch(&ctx)?;
            let mut results = Vec::with_capacity(count);
            self.dispatch_event_inner(&ctx, RuntimeEvent::SoftReset).await?;
            for (input, nodes) in rows {
                tick();
                *current.borrow_mut() = nodes.unwrap_or_default();
                let result: FunctionResult<Variable> = async {
                    if config.2 {
                        self.dispatch_event_inner(&ctx, RuntimeEvent::SoftReset).await?;
                    }
                    let promise: MaybePromise = handler.call((JsValueWithNodes(JsValue(input)), 5)).catch(&ctx)?;
                    let value = promise.into_future::<JsValue>().await.catch(&ctx)?;
                    Ok(value.0)
                }
                .await;
                results.push(result);
            }
            Ok(results)
        })
        .await;
        match results {
            Ok(results) => results,
            Err(error) => {
                let message = error.to_string();
                (0..count).map(|_| Err(FunctionError::Caught(message.clone()))).collect()
            }
        }
    }

    pub(crate) async fn extract_logs(&self) -> Vec<Log> {
        let logs: Option<Vec<Log>> = async_with!(&self.ctx => |ctx| {
            let console = Console::from_context(&ctx).ok()?;
            Some(console.logs.into_inner())
        })
        .await;

        logs.unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize, ToVariable)]
pub struct HandlerResponse {
    pub logs: Vec<Log>,
    pub data: Variable,
}

fn create_content_hash(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}
