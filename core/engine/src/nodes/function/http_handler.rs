use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub trait HttpHandler: Debug + Send + Sync {
    fn handle(
        &self,
        request: HttpHandlerRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HttpHandlerResponse, String>> + Send + '_>>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpHandlerRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub body: Option<Value>,
    #[serde(default)]
    pub headers: Option<Value>,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpHandlerResponse {
    pub status: u16,
    pub headers: Value,
    pub data: Value,
}

pub type DynamicHttpHandler = Option<Arc<dyn HttpHandler + Send + Sync>>;
