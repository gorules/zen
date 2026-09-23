use crate::nodes::definition::NodeHandler;
use crate::nodes::result::NodeResult;
use crate::nodes::NodeContext;
use serde_json::Value;
use zen_types::decision::InputNodeContent;
use zen_types::variable::Variable;

#[derive(Debug, Clone)]
pub struct InputNodeHandler;

pub type InputNodeData = InputNodeContent;
pub type InputNodeTrace = Variable;

impl NodeHandler for InputNodeHandler {
    type NodeData = InputNodeData;
    type TraceData = InputNodeTrace;

    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        if let Some(json_schema) = &ctx.node.schema {
            ctx.validate(json_schema, &ctx.input).or_else(|_| {
                ctx.validate(
                    json_schema,
                    &without_optional_nulls(json_schema, &ctx.input),
                )
            })?;
        };

        ctx.success(ctx.input.clone())
    }
}

fn without_optional_nulls(schema: &Value, value: &Variable) -> Variable {
    if let (Some(props), Some(obj)) = (
        schema.get("properties").and_then(Value::as_object),
        value.as_object(),
    ) {
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut map = obj.borrow().clone();
        let keys: Vec<_> = obj.borrow().iter().map(|(key, _)| key.clone()).collect();
        for key in keys {
            let name: &str = key.as_ref();
            let Some(prop_schema) = props.get(name) else {
                continue;
            };
            let current = map.get(&key).cloned().unwrap_or(Variable::Null);
            if matches!(current, Variable::Null) {
                if !required.contains(&name) {
                    map.remove(&key);
                }
                continue;
            }
            map.insert(key.clone(), without_optional_nulls(prop_schema, &current));
        }
        return Variable::from_object(map);
    }
    if let (Some(items), Some(arr)) = (schema.get("items"), value.as_array()) {
        let stripped = arr
            .borrow()
            .iter()
            .map(|item| without_optional_nulls(items, item))
            .collect();
        return Variable::from_array(stripped);
    }
    value.clone()
}
