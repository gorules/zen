use std::cell::RefCell;
use std::time::Instant;

use rquickjs::prelude::Rest;
use rquickjs::{Ctx, Value};
use serde::{Deserialize, Serialize};

use crate::handler::function::listener::{RuntimeEvent, RuntimeListener};

#[derive(Default)]
pub struct ConsoleModule;

pub struct ConsoleListener;

impl RuntimeListener for ConsoleListener {
    fn on_event(&self, ctx: &Ctx, event: &RuntimeEvent) -> rquickjs::Result<()> {
        match event {
            RuntimeEvent::Startup => Console::init(ctx)?,
            RuntimeEvent::SoftReset => Console::init(ctx)?,
        }

        Ok(())
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
}
