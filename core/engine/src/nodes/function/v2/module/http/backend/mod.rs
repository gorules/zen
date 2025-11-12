use crate::nodes::function::v2::module::http::HttpConfig;
use crate::nodes::function::v2::serde::JsValue;
use http::Method;
use rquickjs::{Ctx, IntoJs, Object, Value};
use std::future::Future;
use std::pin::Pin;

#[cfg(not(target_family = "wasm"))]
pub(crate) mod native;

pub(crate) mod callback;

#[derive(Debug)]
pub(crate) struct HttpResponse<'js> {
    pub data: Value<'js>,
    pub headers: Value<'js>,
    pub status: u16,
}

impl<'js> IntoJs<'js> for HttpResponse<'js> {
    fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        let object = Object::new(ctx.clone())?;
        object.set("data", self.data)?;
        object.set("headers", self.headers)?;
        object.set("status", self.status)?;

        Ok(object.into_value())
    }
}

/// Trait for HTTP backend implementations
pub(crate) trait HttpBackend {
    fn execute_http<'js>(
        &self,
        ctx: Ctx<'js>,
        method: Method,
        url: String,
        data: Option<JsValue>,
        config: Option<HttpConfig>,
    ) -> Pin<Box<dyn Future<Output = rquickjs::Result<HttpResponse<'js>>> + 'js>>;
}
