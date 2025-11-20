pub(crate) mod auth;
pub(crate) mod backend;
pub(crate) mod listener;

use crate::nodes::function::v2::module::export_default;
use crate::nodes::function::v2::module::http::auth::HttpConfigAuth;
use crate::nodes::function::v2::module::http::backend::callback::CallbackHttpBackend;
use crate::nodes::function::v2::module::http::backend::{HttpBackend, HttpResponse};
use crate::nodes::function::v2::serde::{rquickjs_conv, JsValue};
use ahash::HashMap;
use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::prelude::{Async, Func, Opt};
use rquickjs::{Ctx, FromJs, Value};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::Display;

async fn execute_http<'js>(
    ctx: Ctx<'js>,
    method: HttpMethod,
    url: String,
    data: Option<serde_json::Value>,
    request: Option<HttpRequestConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    let mut request = request.unwrap_or_default();
    if let Some(data) = data {
        request.data = Some(data);
    }

    if ctx.globals().contains_key("__executeHttp").unwrap_or(false) {
        let backend = CallbackHttpBackend;
        let backend_result = backend.execute_http(ctx, method, url, request).await;
        return backend_result;
    }

    #[cfg(not(target_family = "wasm"))]
    {
        let backend = crate::nodes::function::v2::module::http::backend::native::NativeHttpBackend;
        return backend.execute_http(ctx, method, url, request).await;
    }

    #[cfg(target_family = "wasm")]
    {
        Err(rquickjs::Error::new_from_js(
            "http",
            "HTTP not available in WASM environment without callback handler",
        ))
    }
}

async fn get<'js>(
    ctx: Ctx<'js>,
    url: String,
    config: Opt<HttpRequestConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(ctx, HttpMethod::GET, url, None, config.0).await
}

async fn post<'js>(
    ctx: Ctx<'js>,
    url: String,
    data: JsValue,
    config: Opt<HttpRequestConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(
        ctx,
        HttpMethod::POST,
        url,
        Some(data.0.to_value()),
        config.0,
    )
    .await
}

async fn patch<'js>(
    ctx: Ctx<'js>,
    url: String,
    data: JsValue,
    config: Opt<HttpRequestConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(
        ctx,
        HttpMethod::PATCH,
        url,
        Some(data.0.to_value()),
        config.0,
    )
    .await
}

async fn put<'js>(
    ctx: Ctx<'js>,
    url: String,
    data: JsValue,
    config: Opt<HttpRequestConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(ctx, HttpMethod::PUT, url, Some(data.0.to_value()), config.0).await
}

async fn delete<'js>(
    ctx: Ctx<'js>,
    url: String,
    config: Opt<HttpRequestConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(ctx, HttpMethod::DELETE, url, None, config.0).await
}

async fn head<'js>(
    ctx: Ctx<'js>,
    url: String,
    config: Opt<HttpRequestConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(ctx, HttpMethod::HEAD, url, None, config.0).await
}

pub(crate) struct HttpModule;

impl ModuleDef for HttpModule {
    fn declare<'js>(decl: &Declarations<'js>) -> rquickjs::Result<()> {
        decl.declare("get")?;
        decl.declare("head")?;
        decl.declare("post")?;
        decl.declare("patch")?;
        decl.declare("put")?;
        decl.declare("delete")?;

        decl.declare("default")?;

        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> rquickjs::Result<()> {
        export_default(ctx, exports, |default| {
            default.set("get", Func::from(Async(get)))?;
            default.set("head", Func::from(Async(head)))?;
            default.set("post", Func::from(Async(post)))?;
            default.set("patch", Func::from(Async(patch)))?;
            default.set("put", Func::from(Async(put)))?;
            default.set("delete", Func::from(Async(delete)))?;

            Ok(())
        })
    }
}

#[derive(Display)]
pub(crate) enum HttpMethod {
    GET,
    POST,
    DELETE,
    HEAD,
    PUT,
    PATCH,
}

#[derive(Deserialize, Serialize, Default)]
pub(crate) struct HttpRequestConfig {
    #[serde(default)]
    headers: HashMap<String, StringPrimitive>,
    #[serde(default)]
    params: HashMap<String, StringPrimitive>,
    data: Option<serde_json::Value>,
    auth: Option<HttpConfigAuth>,
}

#[derive(Debug, Clone)]
pub(crate) struct StringPrimitive(pub String);

impl<'de> Deserialize<'de> for StringPrimitive {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let data = match value {
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s,
            _ => return Err(serde::de::Error::custom("Value is not a string")),
        };

        Ok(Self(data))
    }
}

impl Serialize for StringPrimitive {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'js> FromJs<'js> for HttpRequestConfig {
    fn from_js(_ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Self> {
        rquickjs_conv::from_value(value)
    }
}
