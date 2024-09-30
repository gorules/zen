use crate::handler::function::error::ResultExt;
use crate::handler::function::module::export_default;
use crate::handler::function::serde::JsValue;
use reqwest::header::{HeaderMap, HeaderName};
use reqwest::Method;
use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::prelude::{Async, Func, Opt};
use rquickjs::{CatchResultExt, Ctx, FromJs, IntoAtom, IntoJs, Object, Value};
use std::str::FromStr;
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
) -> rquickjs::Result<HttpResponse> {
    static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

    let client = HTTP_CLIENT.get_or_init(|| reqwest::Client::new()).clone();
    let mut builder = client.request(method, url);
    if let Some(data) = data {
        builder = builder.json(&data.0);
    }

    if let Some(config) = config {
        builder = builder
            .headers(config.headers)
            .query(config.params.as_slice());

        if let Some(data) = config.data {
            builder = builder.json(&data.0);
        }
    }

    let response = builder.send().await.or_throw(&ctx)?;
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

#[derive(Default)]
pub(crate) struct HttpConfig {
    headers: HeaderMap,
    params: Vec<(String, String)>,
    data: Option<JsValue>,
}

impl<'js> FromJs<'js> for HttpConfig {
    fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Self> {
        let object = value.into_object().or_throw(ctx)?;
        let headers_obj: Option<Object<'js>> = object.get("headers").or_throw(ctx)?;
        let headers = if let Some(headers_obj) = headers_obj {
            let mut header_map = HeaderMap::with_capacity(headers_obj.len());
            for result in headers_obj.into_iter() {
                let Ok((key, value)) = result else {
                    continue;
                };

                let value = JsValue::from_js(ctx, value)?;
                let str_value = match value.0 {
                    Variable::Null => None,
                    Variable::Bool(b) => Some(b.to_string()),
                    Variable::Number(n) => Some(n.to_string()),
                    Variable::String(s) => Some(s.to_string()),
                    Variable::Array(_) => None,
                    Variable::Object(_) => None,
                };

                let key_value = key.to_string()?;
                let key = HeaderName::from_str(key_value.as_str()).or_throw(&ctx)?;
                if let Some(str_value) = str_value {
                    header_map.insert(key, str_value.parse().or_throw(&ctx)?);
                }
            }

            header_map
        } else {
            HeaderMap::default()
        };

        let params_obj: Option<Object<'js>> = object.get("params").or_throw(ctx)?;
        let params = if let Some(params_obj) = params_obj {
            let mut params = Vec::with_capacity(params_obj.len());
            for result in params_obj.into_iter() {
                let Ok((key, value)) = result else {
                    continue;
                };

                let value = JsValue::from_js(ctx, value)?;
                let str_value = match value.0 {
                    Variable::Null => None,
                    Variable::Bool(b) => Some(b.to_string()),
                    Variable::Number(n) => Some(n.to_string()),
                    Variable::String(s) => Some(s.to_string()),
                    Variable::Array(_) => None,
                    Variable::Object(_) => None,
                };

                let key = key.to_string()?;
                if let Some(str_value) = str_value {
                    params.push((key, str_value));
                }
            }

            params
        } else {
            Vec::default()
        };

        let data_obj: Option<Value<'js>> = object.get("data").ok();
        let data = if let Some(data_obj) = data_obj {
            Some(
                JsValue::from_js(&ctx, data_obj)
                    .catch(&ctx)
                    .or_throw(&ctx)?,
            )
        } else {
            None
        };

        Ok(Self {
            headers,
            params,
            data,
        })
    }
}

async fn get<'js>(
    ctx: Ctx<'js>,
    url: String,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse> {
    execute_http(ctx, Method::GET, url, None, config.0).await
}

async fn post<'js>(
    ctx: Ctx<'js>,
    url: String,
    data: JsValue,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse> {
    execute_http(ctx, Method::POST, url, Some(data), config.0).await
}

async fn patch<'js>(
    ctx: Ctx<'js>,
    url: String,
    data: JsValue,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse> {
    execute_http(ctx, Method::PATCH, url, Some(data), config.0).await
}

async fn put<'js>(
    ctx: Ctx<'js>,
    url: String,
    data: JsValue,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse> {
    execute_http(ctx, Method::PUT, url, Some(data), config.0).await
}

async fn delete<'js>(
    ctx: Ctx<'js>,
    url: String,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse> {
    execute_http(ctx, Method::DELETE, url, None, config.0).await
}

async fn head<'js>(
    ctx: Ctx<'js>,
    url: String,
    config: Opt<HttpConfig>,
) -> rquickjs::Result<HttpResponse> {
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
