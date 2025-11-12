use crate::nodes::function::v2::error::ResultExt;
use crate::nodes::function::v2::module::http::{HttpMethod, HttpRequestConfig};
use rquickjs::{Ctx, FromJs, IntoJs, Object, Value};
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

impl<'js> FromJs<'js> for HttpResponse<'js> {
    fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Self> {
        let object = value.as_object().or_throw(&ctx)?;

        Ok(HttpResponse {
            status: object.get("status").or_throw(&ctx)?,
            data: object.get("data").or_throw(&ctx)?,
            headers: object.get("headers").or_throw(&ctx)?,
        })
    }
}

/// Trait for HTTP backend implementations
pub(crate) trait HttpBackend {
    fn execute_http<'js>(
        &self,
        ctx: Ctx<'js>,
        method: HttpMethod,
        url: String,
        config: HttpRequestConfig,
    ) -> Pin<Box<dyn Future<Output = rquickjs::Result<HttpResponse<'js>>> + 'js>>;
}
