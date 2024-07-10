use std::str::FromStr;

use reqwest::header::{HeaderMap, HeaderName};
use reqwest::Method;
use rquickjs::{CatchResultExt, Ctx, FromJs, IntoAtom, IntoJs, Object, Value};

use crate::handler::function::error::ResultExt;
use crate::handler::function::serde::JsValue;

#[derive(rquickjs::class::Trace)]
#[rquickjs::class]
pub(crate) struct HttpResponse<'js> {
    #[qjs(get)]
    data: Value<'js>,
    #[qjs(get)]
    headers: Object<'js>,
    #[qjs(get)]
    status: u16,
}

async fn execute_http<'js>(
    ctx: Ctx<'js>,
    method: Method,
    url: String,
    data: Option<JsValue>,
    config: Option<HttpConfig>,
) -> rquickjs::Result<HttpResponse> {
    let client = reqwest::Client::new();
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

    let header_object = rquickjs::Object::new(ctx.clone())
        .catch(&ctx)
        .or_throw(&ctx)?;
    for (key, value) in response.headers() {
        header_object.set(
            key.as_str().into_atom(&ctx)?,
            value.to_str().or_throw(&ctx).into_js(&ctx),
        )?;
    }

    let data: serde_json::Value = response.json().await.unwrap();

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
                    serde_json::Value::Null => None,
                    serde_json::Value::Bool(b) => Some(b.to_string()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    serde_json::Value::String(s) => Some(s),
                    serde_json::Value::Array(_) => None,
                    serde_json::Value::Object(_) => None,
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
                    serde_json::Value::Null => None,
                    serde_json::Value::Bool(b) => Some(b.to_string()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    serde_json::Value::String(s) => Some(s),
                    serde_json::Value::Array(_) => None,
                    serde_json::Value::Object(_) => None,
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

#[rquickjs::module(rename_vars = "camelCase")]
pub mod http_module {
    use reqwest::Method;
    use rquickjs::prelude::Opt;
    use rquickjs::Ctx;

    use crate::handler::function::module::http::{execute_http, HttpConfig, HttpResponse};
    use crate::handler::function::serde::JsValue;

    #[allow(non_snake_case)]
    #[rquickjs::function]
    pub async fn get<'js>(
        ctx: Ctx<'js>,
        url: String,
        config: Opt<HttpConfig>,
    ) -> rquickjs::Result<HttpResponse> {
        execute_http(ctx, Method::GET, url, None, config.0).await
    }

    #[allow(non_snake_case)]
    #[rquickjs::function]
    pub async fn post<'js>(
        ctx: Ctx<'js>,
        url: String,
        data: JsValue,
        config: Opt<HttpConfig>,
    ) -> rquickjs::Result<HttpResponse> {
        execute_http(ctx, Method::POST, url, Some(data), config.0).await
    }

    #[allow(non_snake_case)]
    #[rquickjs::function]
    pub async fn patch<'js>(
        ctx: Ctx<'js>,
        url: String,
        data: JsValue,
        config: Opt<HttpConfig>,
    ) -> rquickjs::Result<HttpResponse> {
        execute_http(ctx, Method::PATCH, url, Some(data), config.0).await
    }

    #[allow(non_snake_case)]
    #[rquickjs::function]
    pub async fn put<'js>(
        ctx: Ctx<'js>,
        url: String,
        data: JsValue,
        config: Opt<HttpConfig>,
    ) -> rquickjs::Result<HttpResponse> {
        execute_http(ctx, Method::PUT, url, Some(data), config.0).await
    }

    #[allow(non_snake_case)]
    #[rquickjs::function]
    pub async fn delete<'js>(
        ctx: Ctx<'js>,
        url: String,
        config: Opt<HttpConfig>,
    ) -> rquickjs::Result<HttpResponse> {
        execute_http(ctx, Method::DELETE, url, None, config.0).await
    }

    #[allow(non_snake_case)]
    #[rquickjs::function]
    pub async fn head<'js>(
        ctx: Ctx<'js>,
        url: String,
        config: Opt<HttpConfig>,
    ) -> rquickjs::Result<HttpResponse> {
        execute_http(ctx, Method::DELETE, url, None, config.0).await
    }
}
