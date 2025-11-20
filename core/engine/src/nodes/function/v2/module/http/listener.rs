use crate::nodes::function::http_handler::{DynamicHttpHandler, HttpHandlerRequest};
use crate::nodes::function::v2::error::{FunctionResult, ResultExt};
use crate::nodes::function::v2::listener::{RuntimeEvent, RuntimeListener};
use crate::nodes::function::v2::serde::rquickjs_conv;
use rquickjs::prelude::{Async, Func};
use rquickjs::{CatchResultExt, Ctx};
use std::future::Future;
use std::pin::Pin;

pub(crate) struct HttpListener {
    pub http_handler: DynamicHttpHandler,
}

impl RuntimeListener for HttpListener {
    fn on_event<'js>(
        &self,
        ctx: Ctx<'js>,
        event: RuntimeEvent,
    ) -> Pin<Box<dyn Future<Output = FunctionResult> + 'js>> {
        let http_handler = self.http_handler.clone();

        Box::pin(async move {
            if event != RuntimeEvent::Startup {
                return Ok(());
            }

            let Some(http_handler) = http_handler.clone() else {
                return Ok(());
            };

            ctx.globals()
                .set(
                    "__executeHttp",
                    Func::from(Async(move |ctx: Ctx<'js>, request_obj: rquickjs::Value| {
                        let http_handler = http_handler.clone();
                        let request_result =
                            rquickjs_conv::from_value::<HttpHandlerRequest>(request_obj);

                        async move {
                            let request = request_result?;
                            let response = http_handler.handle(request).await.or_throw(&ctx)?;
                            let response_object = rquickjs_conv::to_value(ctx.clone(), response)?;
                            rquickjs::Result::Ok(response_object)
                        }
                    })),
                )
                .catch(&ctx)?;

            Ok(())
        })
    }
}
