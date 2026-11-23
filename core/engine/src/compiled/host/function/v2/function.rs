use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::future::{poll_fn, Future};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::pin::{pin, Pin};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use crate::compiled::host::function::sleep::sleep;
use crate::compiled::host::function::v2::error::{FunctionError, FunctionResult, ResultExt};
use crate::compiled::host::function::v2::listener::{RuntimeEvent, RuntimeListener};
use crate::compiled::host::function::v2::module::console::{Console, Log};
use crate::compiled::host::function::v2::module::ModuleLoader;
use crate::compiled::host::function::v2::serde::{JsValue, JsValueWithNodes};
use crate::compiled::pinning::JsSlot;
use rquickjs::prelude::Func;
use rquickjs::promise::MaybePromise;
use rquickjs::{
    async_with, AsyncContext, AsyncRuntime, CatchResultExt, Ctx, IntoJs, Module, Object,
    Persistent, Type, Value,
};
use serde::{Deserialize, Serialize};
use std::rc::Rc;
use zen_expression::context::EvaluationContext;
use zen_expression::variable::{ToVariable, Variable};

pub struct FunctionConfig {
    pub(crate) listeners: Option<Vec<Box<dyn RuntimeListener>>>,
    pub(crate) slot: JsSlot,
}

pub struct Function {
    nodes: RefCell<Option<Persistent<rquickjs::Function<'static>>>>,
    rt: Arc<AsyncRuntime>,
    ctx: AsyncContext,
    listeners: Vec<Box<dyn RuntimeListener>>,
    module_loader: ModuleLoader,
    declared: RefCell<HashMap<String, u64>>,
    reused: RefCell<HashMap<String, (u64, String)>>,
    slot: JsSlot,
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
            nodes: RefCell::new(None),
            rt,
            ctx,
            module_loader,
            listeners: config.listeners.unwrap_or_default(),
            declared: RefCell::new(HashMap::new()),
            reused: RefCell::new(HashMap::new()),
            slot: config.slot,
        };

        this.dispatch_event(RuntimeEvent::Startup).await?;
        let slot = this.slot.clone();
        let installed: FunctionResult = async_with!(&this.ctx => |ctx| {
            slot.install(&ctx).catch(&ctx)?;
            Ok(())
        })
        .await;
        installed?;
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

    pub(crate) async fn reuse_module(&self, name: &str, source: &str) -> FunctionResult {
        let module_name = self.shared_module(name, source).await?;
        self.reused
            .borrow_mut()
            .insert(name.to_string(), (create_content_hash(source), module_name));
        Ok(())
    }

    pub(crate) fn reused_module(&self, name: &str, source: &str) -> Option<String> {
        let content_hash = create_content_hash(source);
        self.reused
            .borrow()
            .get(name)
            .filter(|(hash, _)| *hash == content_hash)
            .map(|(_, module_name)| module_name.clone())
    }

    pub(crate) async fn shared_module(&self, name: &str, source: &str) -> FunctionResult<String> {
        let content_hash = create_content_hash(source);
        let declarative_name = format!("node:{name}");
        let module_name = match self.declared.borrow().get(&declarative_name) {
            Some(hash) if *hash == content_hash => return Ok(declarative_name),
            Some(_) => format!("node:{name}.{content_hash:x}"),
            None if self.module_loader.has_module(&declarative_name) => {
                format!("node:{name}.{content_hash:x}")
            }
            None => declarative_name.clone(),
        };
        if self.declared.borrow().get(&module_name) == Some(&content_hash) {
            return Ok(module_name);
        }
        self.register_module(&module_name, source).await?;
        self.declared
            .borrow_mut()
            .insert(module_name.clone(), content_hash);
        Ok(module_name)
    }

    pub(crate) async fn call_handler(
        &self,
        name: &str,
        data: Variable,
        context: &EvaluationContext,
        deadline: Instant,
    ) -> FunctionResult<HandlerResponse> {
        let _entered = self.slot.enter(context.clone());
        let k: FunctionResult<HandlerResponse> = async_with!(&self.ctx => |ctx| {
            self.dispatch_event_inner(&ctx, RuntimeEvent::SoftReset).await?;

            let m: rquickjs::Object = bounded(&ctx, Module::import(&ctx, name).catch(&ctx)?.into_future(), deadline).await?;
            let handler: rquickjs::Function = m.get("handler").catch(&ctx)?;

            let data = self.with_nodes(&ctx, data).catch(&ctx)?;
            let handler_promise: MaybePromise = handler.call((data, 5)).catch(&ctx)?;
            let handler_result: JsValue = bounded(&ctx, handler_promise.into_future(), deadline).await?;

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
        limit: Duration,
        tick: &dyn Fn(),
        context: &EvaluationContext,
    ) -> Vec<FunctionResult<Variable>> {
        let _entered = self.slot.enter(context.clone());
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
            let deadline = Instant::now() + limit;
            let m: rquickjs::Object = bounded(&ctx, Module::import(&ctx, name).catch(&ctx)?.into_future(), deadline).await?;
            let handler: rquickjs::Function = m.get("handler").catch(&ctx)?;
            let mut results = Vec::with_capacity(count);
            self.dispatch_event_inner(&ctx, RuntimeEvent::SoftReset).await?;
            for (input, nodes) in rows {
                tick();
                let deadline = Instant::now() + limit;
                *current.borrow_mut() = nodes.unwrap_or_default();
                let result: FunctionResult<Variable> = async {
                    if config.2 {
                        self.dispatch_event_inner(&ctx, RuntimeEvent::SoftReset).await?;
                    }
                    let input = self.with_nodes(&ctx, input).catch(&ctx)?;
                    let promise: MaybePromise = handler.call((input, 5)).catch(&ctx)?;
                    let value: JsValue = bounded(&ctx, promise.into_future(), deadline).await?;
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
                (0..count)
                    .map(|_| Err(FunctionError::Caught(message.clone())))
                    .collect()
            }
        }
    }

    fn with_nodes<'js>(&self, ctx: &Ctx<'js>, input: Variable) -> rquickjs::Result<Value<'js>> {
        let value = JsValue(input).into_js(ctx)?;
        if !value.is_object() {
            return Ok(value);
        }
        let saved = self.nodes.borrow().clone();
        let factory = match saved {
            Some(saved) => saved.restore(ctx)?,
            None => {
                let factory = JsValueWithNodes::factory(ctx)?;
                *self.nodes.borrow_mut() = Some(Persistent::save(ctx, factory.clone()));
                factory
            }
        };
        JsValueWithNodes::attach(value, &factory)
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

async fn bounded<'js, T>(
    ctx: &Ctx<'js>,
    future: impl Future<Output = rquickjs::Result<T>>,
    deadline: Instant,
) -> FunctionResult<T> {
    let mut future = pin!(future);
    let mut timer: Option<Pin<Box<dyn Future<Output = ()>>>> = None;
    let settled = poll_fn(|cx| loop {
        if let Poll::Ready(result) = future.as_mut().poll(cx) {
            return Poll::Ready(Ok(result));
        }
        let Some(timer) = timer.as_mut() else {
            if ctx.execute_pending_job() {
                let thrown = ctx.catch();
                if !matches!(
                    thrown.type_of(),
                    Type::Uninitialized | Type::Null | Type::Undefined
                ) {
                    return Poll::Ready(Err(Some(thrown)));
                }
                continue;
            }
            if Waiting::any() {
                return Poll::Pending;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            timer = Some(Box::pin(sleep(remaining)));
            continue;
        };
        if Waiting::any() {
            return Poll::Pending;
        }
        return match timer.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(None)),
            Poll::Pending => Poll::Pending,
        };
    })
    .await;
    let caught = match settled {
        Ok(result) => result.catch(ctx).map_err(FunctionError::from),
        Err(Some(thrown)) => Err::<T, _>(ctx.throw(thrown))
            .catch(ctx)
            .map_err(FunctionError::from),
        Err(None) => Err(FunctionError::Caught(TIMED_OUT.to_string())),
    };
    match caught {
        Err(_) if Instant::now() >= deadline => Err(FunctionError::Caught(TIMED_OUT.to_string())),
        other => other,
    }
}

const TIMED_OUT: &str = "Error: interrupted";

thread_local! {
    static WAITING: Cell<usize> = const { Cell::new(0) };
}

pub(crate) struct Waiting;

impl Waiting {
    pub(crate) fn enter() -> Self {
        WAITING.set(WAITING.get() + 1);
        Waiting
    }

    fn any() -> bool {
        WAITING.get() > 0
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITING.set(WAITING.get().saturating_sub(1));
    }
}

fn create_content_hash(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}
