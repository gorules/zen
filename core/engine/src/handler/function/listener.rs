use std::future::Future;
use std::pin::Pin;

use rquickjs::Ctx;

use crate::handler::function::error::FunctionResult;

#[derive(Clone, PartialEq)]
pub(crate) enum RuntimeEvent {
    Startup,
    SoftReset,
}

pub(crate) trait RuntimeListener {
    fn on_event<'js>(
        &self,
        ctx: Ctx<'js>,
        event: RuntimeEvent,
    ) -> Pin<Box<dyn Future<Output = FunctionResult> + 'js>>;
}
