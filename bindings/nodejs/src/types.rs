use napi::JsObject;
use napi_derive::napi;
use serde_json::Value;

#[napi(object)]
pub struct DecisionNode {
    pub id: String,
    pub name: String,
    #[napi(js_name = "type")]
    pub kind: String,
    pub content: CustomNodeContent,
}

#[napi(object)]
pub struct CustomNodeContent {
    pub component: String,
    /// Config is where custom data is kept. Usually in JSON format.
    pub config: Value,
}

#[napi(object)]
pub struct ZenEngineHandlerRequest {
    pub input: JsObject,
    pub node: DecisionNode,
}

#[napi(object)]
pub struct ZenEngineHandlerResponse {
    pub output: JsObject,
    pub trace: JsObject,
}
