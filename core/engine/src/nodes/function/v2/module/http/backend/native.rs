use super::{HttpBackend, HttpResponse};
use crate::nodes::function::v2::error::ResultExt;
use crate::nodes::function::v2::module::http::auth::{HttpConfigAuth, IamAuth};
use crate::nodes::function::v2::module::http::HttpConfig;
use crate::nodes::function::v2::serde::JsValue;
use crate::ZEN_CONFIG;
use ::http::Request as HttpRequest;
use reqwest::{Body, Method, Request, Url};
use rquickjs::{CatchResultExt, Ctx, IntoAtom, IntoJs, Object};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use zen_expression::variable::Variable;

pub(crate) struct NativeHttpBackend;

impl HttpBackend for NativeHttpBackend {
    fn execute_http<'js>(
        &self,
        ctx: Ctx<'js>,
        method: Method,
        url: String,
        data: Option<JsValue>,
        config: Option<HttpConfig>,
    ) -> Pin<Box<dyn Future<Output = rquickjs::Result<HttpResponse<'js>>> + 'js>> {
        Box::pin(async move { execute_http_native(ctx, method, url, data, config).await })
    }
}

async fn execute_http_native<'js>(
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
        headers: header_object.into_value(),
        status,
    })
}
