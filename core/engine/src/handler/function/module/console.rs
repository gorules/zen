use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use crate::handler::function::error::{FunctionResult, ResultExt};
use crate::handler::function::listener::{RuntimeEvent, RuntimeListener};
use rquickjs::prelude::Rest;
use rquickjs::{Ctx, Object, Value};
use serde::{Deserialize, Serialize};

pub(crate) struct ConsoleListener;

impl RuntimeListener for ConsoleListener {
    fn on_event<'js>(
        &self,
        ctx: Ctx<'js>,
        event: RuntimeEvent,
    ) -> Pin<Box<dyn Future<Output = FunctionResult> + 'js>> {
        Box::pin(async move {
            match event {
                RuntimeEvent::Startup => Console::init(&ctx)?,
                RuntimeEvent::SoftReset => Console::init(&ctx)?,
            }

            Ok(())
        })
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Log {
    lines: Vec<String>,
    ms_since_run: usize,
}

#[derive(rquickjs::class::Trace, Clone)]
#[rquickjs::class]
pub struct Console {
    #[qjs(skip_trace)]
    pub logs: RefCell<Vec<Log>>,
    #[qjs(skip_trace)]
    created_at: Instant,
}

#[rquickjs::methods(rename_all = "camelCase")]
impl Console {
    fn new() -> Self {
        Self {
            logs: Default::default(),
            created_at: Instant::now(),
        }
    }

    #[qjs(skip)]
    pub fn init(ctx: &Ctx) -> rquickjs::Result<()> {
        ctx.globals().set("console", Self::new())?;
        Ok(())
    }

    #[qjs(skip)]
    pub fn from_context(ctx: &Ctx) -> rquickjs::Result<Self> {
        let obj: Self = ctx.globals().get("console")?;
        Ok(obj)
    }

    pub fn log<'js>(&self, ctx: Ctx<'js>, args: Rest<Value<'js>>) -> rquickjs::Result<()> {
        let config: Object = ctx.globals().get("config").or_throw(&ctx)?;
        let trace: bool = config.get("trace").or_throw(&ctx)?;
        if !trace {
            return Ok(());
        }

        let step1 = args
            .0
            .into_iter()
            .map(|arg| ctx.json_stringify(arg))
            .collect::<Result<Vec<Option<rquickjs::String<'js>>>, _>>()?;

        let step2 = step1
            .into_iter()
            .map(|s| s.map(|i| i.to_string()).transpose())
            .collect::<Result<Vec<Option<String>>, _>>()?;

        let step3 = step2
            .into_iter()
            .map(|s| s.unwrap_or_default())
            .collect::<Vec<String>>();

        let mut logs = self.logs.borrow_mut();
        logs.push(Log {
            lines: step3,
            ms_since_run: self.created_at.elapsed().as_millis() as usize,
        });

        Ok(())
    }

    pub async fn sleep(&self, duration_ms: u64) -> rquickjs::Result<()> {
        tokio::time::sleep(Duration::from_millis(duration_ms)).await;
        Ok(())
    }
}
