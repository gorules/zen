use ahash::HashMap;
use napi::bindgen_prelude::Promise;
use napi::threadsafe_function::ThreadsafeFunction;
use napi::Status;
use napi_derive::napi;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use zen_engine::nodes::http_handler::{
    HttpHandler, HttpHandlerRequest as EngineHttpHandlerRequest,
    HttpHandlerResponse as EngineHttpHandlerResponse,
};

pub(crate) type HttpHandlerTsfn = Arc<
    ThreadsafeFunction<
        ZenHttpHandlerRequest,
        Promise<ZenHttpHandlerResponse>,
        ZenHttpHandlerRequest,
        Status,
        false,
        true,
    >,
>;

#[derive(Default)]
pub(crate) struct NodeHttpHandler {
    function: Option<HttpHandlerTsfn>,
}

impl Debug for NodeHttpHandler {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "NodeHttpHandler")
    }
}

impl NodeHttpHandler {
    pub fn new(tsf: HttpHandlerTsfn) -> Self {
        Self {
            function: Some(tsf),
        }
    }
}

impl HttpHandler for NodeHttpHandler {
    fn handle(
        &self,
        request: EngineHttpHandlerRequest,
    ) -> Pin<Box<dyn Future<Output = Result<EngineHttpHandlerResponse, String>> + Send + '_>> {
        let napi_request = ZenHttpHandlerRequest {
            method: request.method,
            url: request.url,
            body: request.body,
            headers: request.headers,
            params: request.params,
        };

        Box::pin(async move {
            let Some(function) = &self.function else {
                return Err("HTTP handler function is undefined".to_string());
            };

            let promise: Promise<ZenHttpHandlerResponse> = function
                .clone()
                .call_async(napi_request)
                .await
                .map_err(|err| err.reason.to_string())?;

            let response = promise.await.map_err(|err| err.reason.to_string())?;
            Ok(EngineHttpHandlerResponse {
                status: response.status,
                headers: response.headers.into(),
                data: response.data.into(),
            })
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[napi(object)]
pub struct ZenHttpHandlerRequest {
    pub method: String,
    pub url: String,
    pub body: Option<Value>,
    pub headers: HashMap<String, String>,
    pub params: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[napi(object)]
pub struct ZenHttpHandlerResponse {
    pub status: u16,
    pub headers: Value,
    pub data: Value,
}
