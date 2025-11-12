pub(crate) mod auth;
pub(crate) mod backend;
pub(crate) mod listener;

use crate::nodes::function::v2::error::ResultExt;
use crate::nodes::function::v2::module::export_default;
use crate::nodes::function::v2::module::http::auth::HttpConfigAuth;
use crate::nodes::function::v2::module::http::backend::callback::CallbackHttpBackend;
use crate::nodes::function::v2::module::http::backend::{HttpBackend, HttpResponse};
use crate::nodes::function::v2::serde::JsValue;
use ahash::HashMap;
use http::Method;
use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::prelude::{Async, Func, Opt};
use rquickjs::{Ctx, FromJs, Value};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

async fn execute_http<'js>(
    ctx: Ctx<'js>,
    method: Method,
    url: String,
    data: Option<JsValue>,
    config: Option<HttpConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    if ctx.globals().contains_key("__executeHttp").unwrap_or(false) {
        let backend = CallbackHttpBackend;
        let backend_result = backend.execute_http(ctx, method, url, data, config).await;
        return backend_result;
    }

    #[cfg(not(target_family = "wasm"))]
    {
        let backend = crate::nodes::function::v2::module::http::backend::native::NativeHttpBackend;
        return backend.execute_http(ctx, method, url, data, config).await;
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
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(ctx, Method::GET, url, None, config.0).await
}

async fn post<'js>(
    ctx: Ctx<'js>,
    url: String,
    data: JsValue,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(ctx, Method::POST, url, Some(data), config.0).await
}

async fn patch<'js>(
    ctx: Ctx<'js>,
    url: String,
    data: JsValue,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(ctx, Method::PATCH, url, Some(data), config.0).await
}

async fn put<'js>(
    ctx: Ctx<'js>,
    url: String,
    data: JsValue,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(ctx, Method::PUT, url, Some(data), config.0).await
}

async fn delete<'js>(
    ctx: Ctx<'js>,
    url: String,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(ctx, Method::DELETE, url, None, config.0).await
}

async fn head<'js>(
    ctx: Ctx<'js>,
    url: String,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    execute_http(ctx, Method::HEAD, url, None, config.0).await
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

#[derive(Deserialize, Serialize)]
pub(crate) struct HttpConfig {
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

impl<'js> FromJs<'js> for HttpConfig {
    fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Self> {
        rquickjs_serde::from_value(value).or_throw(&ctx)
    }
}
