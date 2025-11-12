use super::{HttpBackend, HttpResponse};
use crate::nodes::function::v2::error::ResultExt;
use crate::nodes::function::v2::module::http::{HttpMethod, HttpRequestConfig};
use crate::nodes::http_handler::HttpHandlerRequest;
use rquickjs::promise::MaybePromise;
use rquickjs::{CatchResultExt, Ctx};
use std::future::Future;
use std::pin::Pin;

pub(crate) struct CallbackHttpBackend;

impl HttpBackend for CallbackHttpBackend {
    fn execute_http<'js>(
        &self,
        ctx: Ctx<'js>,
        method: HttpMethod,
        url: String,
        config: HttpRequestConfig,
    ) -> Pin<Box<dyn Future<Output = rquickjs::Result<HttpResponse<'js>>> + 'js>> {
        Box::pin(async move {
            let execute_http_fn: rquickjs::Function =
                ctx.globals().get("__executeHttp").or_throw(&ctx)?;

            let http_request = HttpHandlerRequest {
                url,
                method: method.to_string(),
                body: config.data,
                headers: config.headers.into_iter().map(|(k, v)| (k, v.0)).collect(),
                params: config.params.into_iter().map(|(k, v)| (k, v.0)).collect(),
                auth: config
                    .auth
                    .map(serde_json::to_value)
                    .transpose()
                    .or_throw(&ctx)?,
            };

            let http_request_js =
                rquickjs_serde::to_value(ctx.clone(), http_request).or_throw(&ctx)?;

            let response_promise: MaybePromise = execute_http_fn
                .call((http_request_js,))
                .catch(&ctx)
                .or_throw(&ctx)?;

            let response: HttpResponse = response_promise
                .into_future()
                .await
                .catch(&ctx)
                .or_throw(&ctx)?;

            Ok(response)
        })
    }
}
