use std::future::Future;
use std::pin::Pin;

use rquickjs::Ctx;

use crate::handler::function::error::FunctionResult;

#[derive(Clone, PartialEq)]
pub enum RuntimeEvent {
    Startup,
    SoftReset,
}

pub trait RuntimeListener {
    fn on_event<'js>(
        &self,
        ctx: Ctx<'js>,
        event: RuntimeEvent,
    ) -> Pin<Box<dyn Future<Output = FunctionResult> + 'js>>;
}
