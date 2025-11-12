use super::{HttpBackend, HttpResponse};
use crate::nodes::function::v2::error::ResultExt;
use crate::nodes::function::v2::module::http::HttpConfig;
use crate::nodes::function::v2::serde::JsValue;
use http::Method;
use rquickjs::promise::MaybePromise;
use rquickjs::{CatchResultExt, Ctx, Object, Value};
use std::future::Future;
use std::pin::Pin;

pub(crate) struct CallbackHttpBackend;

impl HttpBackend for CallbackHttpBackend {
    fn execute_http<'js>(
        &self,
        ctx: Ctx<'js>,
        method: Method,
        url: String,
        data: Option<JsValue>,
        config: Option<HttpConfig>,
    ) -> Pin<Box<dyn Future<Output = rquickjs::Result<HttpResponse<'js>>> + 'js>> {
        Box::pin(async move {
            let execute_http_fn: rquickjs::Function =
                ctx.globals().get("__executeHttp").or_throw(&ctx)?;

            let request_obj = Object::new(ctx.clone())?;
            request_obj.set("method", method.as_str())?;
            request_obj.set("url", url)?;

            if let Some(d) = data {
                request_obj.set("body", d)?;
            }

            if let Some(c) = &config {
                let config_js = rquickjs_serde::to_value(ctx.clone(), c).or_throw(&ctx)?;
                request_obj.set("config", config_js)?;
            }

            let response_promise: MaybePromise = execute_http_fn
                .call((request_obj,))
                .catch(&ctx)
                .or_throw(&ctx)?;

            let response_object: Object = response_promise
                .into_future()
                .await
                .catch(&ctx)
                .or_throw(&ctx)?;

            let status: u16 = response_object.get("status").or_throw(&ctx)?;
            let data: Value = response_object.get("data").or_throw(&ctx)?;
            let headers: Value = response_object.get("headers").or_throw(&ctx)?;

            Ok(HttpResponse {
                data,
                headers,
                status,
            })
        })
    }
}
