mod auth;

use crate::nodes::function::v2::error::ResultExt;
use crate::nodes::function::v2::module::export_default;
use crate::nodes::function::v2::module::http::auth::{HttpConfigAuth, IamAuth};
use crate::nodes::function::v2::serde::JsValue;
use crate::ZEN_CONFIG;
use ::http::Request as HttpRequest;
use ahash::HashMap;
use reqwest::{Body, Method, Request, Url};
use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::prelude::{Async, Func, Opt};
use rquickjs::{CatchResultExt, Ctx, IntoAtom, IntoJs, Object, Value};
use serde::{Deserialize, Deserializer};
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use zen_expression::variable::Variable;

pub(crate) struct HttpResponse<'js> {
    data: Value<'js>,
    headers: Object<'js>,
    status: u16,
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

async fn execute_http<'js>(
    ctx: Ctx<'js>,
    method: Method,
    url: String,
    data: Option<JsValue>,
    config: Option<HttpConfig>,
) -> rquickjs::Result<HttpResponse<'js>> {
    static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = HTTP_CLIENT.get_or_init(|| reqwest::Client::new()).clone();

    let mut url = Url::parse(&url).or_throw(&ctx)?;
    if let Some(config) = &config {
        for (k, v) in &config.params {
            url.query_pairs_mut().append_pair(k.as_str(), v.0.as_str());
        }
    }

    let mut request_builder = HttpRequest::builder().method(method).uri(url.as_str());
    if let Some(config) = &config {
        for (k, v) in &config.headers {
            request_builder = request_builder.header(k.as_str(), v.0.as_str());
        }
    }

    let auth_method = config
        .as_ref()
        .filter(|_| ZEN_CONFIG.http_auth.load(Ordering::Relaxed))
        .and_then(|c| c.auth.clone());

    let request_data_opt = config
        .and_then(|c| c.data)
        .and_then(|_| data.map(|d| d.0.to_value()));

    let http_request = match request_data_opt {
        None => request_builder.body(Body::default()).or_throw(&ctx)?,
        Some(request_data) => {
            let request_body_json = serde_json::to_vec(&request_data).or_throw(&ctx)?;
            request_builder
                .body(Body::from(request_body_json))
                .or_throw(&ctx)?
        }
    };

    let request = match auth_method {
        Some(HttpConfigAuth::Iam(IamAuth::Aws(config))) => {
            config.build_request(http_request).await.or_throw(&ctx)?
        }
        Some(HttpConfigAuth::Iam(IamAuth::Azure(config))) => {
            config.build_request(http_request).await.or_throw(&ctx)?
        }
        Some(HttpConfigAuth::Iam(IamAuth::Gcp(config))) => {
            config.build_request(http_request).await.or_throw(&ctx)?
        }
        None => Request::try_from(http_request).or_throw(&ctx)?,
    };

    // Apply auth
    let response = client.execute(request).await.or_throw(&ctx)?;
    let status = response.status().as_u16();
    let header_object = Object::new(ctx.clone()).catch(&ctx).or_throw(&ctx)?;
    for (key, value) in response.headers() {
        header_object.set(
            key.as_str().into_atom(&ctx)?,
            value.to_str().or_throw(&ctx).into_js(&ctx),
        )?;
    }

    let data: Variable = response.json().await.or_throw(&ctx)?;

    Ok(HttpResponse {
        data: JsValue(data).into_js(&ctx)?,
        headers: header_object,
        status,
    })
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
    execute_http(ctx, Method::DELETE, url, None, config.0).await
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

#[derive(Deserialize)]
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
