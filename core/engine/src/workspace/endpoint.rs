//! A document as an endpoint: whether it serves one, and the JSON Schemas
//! (draft-07) of its request and response. One place for what the Agent's
//! OpenAPI document, BRMS API docs, MCP tools and the simulator describe.
//!
//! The request has two audiences. The contract is what a caller sends to a
//! host with a feature store: what the host supplies is left out and a
//! reference is the id of what it names. The evaluated request is what the
//! rules are given once the host has filled it in, as a decision log
//! records it: everything the request holds, the host's values included.

use std::collections::BTreeSet;
use std::sync::Arc;

use ahash::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use zen_expression::variable::VariableType;

use crate::model::{DecisionContent, DecisionNodeKind};
use crate::policy::ir::{DataModelIr, PropertyTypeIr};
use crate::policy::raw::BlockDoc;
use crate::workspace::db::Db;
use crate::workspace::types::{InputProperty, ScopeRequest, SuppliedBy};

const DRAFT: &str = "http://json-schema.org/draft-07/schema#";

/// What a document serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndpointKind {
    Graph,
    Policy,
}

/// Who a request schema is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SchemaAudience {
    /// What a caller sends: no host-supplied fields, references as ids.
    Contract,
    /// What the rules are given: the request as the host filled it in
    /// (features, model outputs, computes, the referenced records).
    Evaluated,
}

impl Db {
    /// A graph, or a policy with at least one rule. A document of data
    /// models and dictionaries only serves nothing; nor does an unknown path.
    pub(crate) fn endpoint(&self, path: &str) -> Option<EndpointKind> {
        match self.raw_document(path)?.as_ref() {
            DecisionContent::Graph(_) => Some(EndpointKind::Graph),
            DecisionContent::Policy(_) => {
                let parsed = self.parsed(&Arc::from(path))?;
                (!parsed.policy.rules.is_empty()).then_some(EndpointKind::Policy)
            }
        }
    }

    /// The request's JSON Schema, without `$id` (the caller names it). A
    /// graph's declared input schema wins; a request nothing is known of is
    /// an open object.
    pub(crate) fn request_schema(&self, req: &ScopeRequest, audience: SchemaAudience) -> Value {
        if let Some(declared) = self.declared_graph_schema(&req.policy_path, true) {
            return declared;
        }
        let mut inputs = self.inputs(req);
        if inputs.is_empty() {
            return self.signature_schema(&req.policy_path, true);
        }
        let shape = self.request_shape(&req.policy_path);
        for input in &mut inputs {
            if let Some(pool) = shape.pools.get(&input.path) {
                input.record_references = pool.references.clone();
            }
        }
        from_inputs(&inputs, audience, &shape)
    }

    /// The response's JSON Schema, without `$id`: what the rules write. A
    /// graph's declared output schema wins.
    pub(crate) fn response_schema(&self, req: &ScopeRequest) -> Value {
        if let Some(declared) = self.declared_graph_schema(&req.policy_path, false) {
            return declared;
        }
        let outputs = self.outputs(req);
        if outputs.is_empty() {
            return self.signature_schema(&req.policy_path, false);
        }
        from_properties(
            outputs
                .iter()
                .map(|output| (output.path.as_ref(), &output.resolved_type)),
        )
    }

    /// The schema on a graph's Request (`input`) or Response node, its
    /// dictionaries resolved; `None` when it declares none or names a
    /// dictionary no import makes visible.
    fn declared_graph_schema(&self, path: &str, input: bool) -> Option<Value> {
        let document = self.raw_document(path)?;
        let graph = document.as_graph()?;
        let schema = graph.nodes.iter().find_map(|node| match &node.kind {
            DecisionNodeKind::InputNode { content } if input => content.schema.clone(),
            DecisionNodeKind::OutputNode { content } if !input => content.schema.clone(),
            _ => None,
        })?;
        if !crate::decision_graph::schema_dict::schema_references_dictionary(&schema) {
            return Some(schema.as_ref().clone());
        }
        let dictionaries = self
            .graph_dictionary_blocks(&graph.imports)
            .into_iter()
            .map(|entry| (entry.ir.name.clone(), entry.ir.values().cloned().collect()))
            .collect();
        crate::decision_graph::schema_dict::resolve_schema(&schema, &dictionaries)
            .ok()
            .map(|(resolved, _)| resolved)
    }

    /// A graph's whole input or output type, as its analysis has it; an open
    /// object when that says nothing.
    fn signature_schema(&self, path: &str, input: bool) -> Value {
        self.graph_analysis(&Arc::from(path))
            .and_then(|analysis| {
                let signature = &analysis.signature;
                from_type(if input {
                    &signature.input
                } else {
                    &signature.output
                })
            })
            .unwrap_or_else(|| json!({ "$schema": DRAFT, "type": "object" }))
    }

    /// What the request's entities are beyond their types: their keys, the
    /// pools of referenced records beside a policy's request, and the stored
    /// relationships no request carries.
    fn request_shape(&self, path: &Arc<str>) -> RequestShape {
        if self.is_graph(path) {
            let imports = self.graph_imports(path);
            let blocks = self.graph_entity_blocks(&imports);
            let entities = self.graph_entities(&imports);
            let target = self
                .raw_document(path)
                .and_then(|document| document.as_graph()?.request_target());
            let stored = target
                .and_then(|target| entities.get(&target).cloned())
                .map(|entity| {
                    entity
                        .properties
                        .iter()
                        .filter(|p| p.is_stored())
                        .map(|p| p.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            let paths: Vec<Arc<str>> = blocks.into_iter().map(|b| b.policy_path).collect();
            return RequestShape {
                ids: self.id_schemas(&paths, &entities),
                pools: HashMap::default(),
                stored,
            };
        }

        let unit = self.unit(path);
        let mut members: Vec<Arc<str>> = unit.members.iter().cloned().collect();
        members.sort();
        let targets: HashSet<&Arc<str>> = unit
            .entities
            .values()
            .flat_map(|dm| &dm.properties)
            .filter_map(|p| match &p.kind {
                PropertyTypeIr::Reference { target } => Some(target),
                _ => None,
            })
            .collect();
        // A pool holds the referenced entity's records: their own references
        // are ids, as anywhere in a policy's request.
        let pools = targets
            .into_iter()
            .filter_map(|target| {
                let dm = unit.entities.get(target)?;
                let references = dm
                    .properties
                    .iter()
                    .filter_map(|p| match &p.kind {
                        PropertyTypeIr::Reference { target } => {
                            Some((p.name.clone(), target.clone()))
                        }
                        _ => None,
                    })
                    .collect();
                let optional = dm
                    .properties
                    .iter()
                    .filter(|p| p.optional || p.default.is_some())
                    .map(|p| p.name.to_string())
                    .collect();
                Some((
                    target.clone(),
                    Pool {
                        references,
                        optional,
                    },
                ))
            })
            .collect();
        let stored = self
            .walk_visible_properties(path)
            .into_iter()
            .filter(|vp| vp.property.is_stored())
            .map(|vp| vp.dotted_path())
            .collect();
        RequestShape {
            ids: self.id_schemas(&members, &unit.entities),
            pools,
            stored,
        }
    }

    /// Each entity's id as a reference carries it: a string (ZEN reads
    /// references by string id; a composite key's parts are joined), or the
    /// values of a single enum key. Declared in `paths`, first wins.
    fn id_schemas(
        &self,
        paths: &[Arc<str>],
        entities: &HashMap<Arc<str>, Arc<DataModelIr>>,
    ) -> HashMap<Arc<str>, Value> {
        let mut out: HashMap<Arc<str>, Value> = HashMap::default();
        for path in paths {
            let Some(policy) = self.raw_policy(path) else {
                continue;
            };
            for block in &policy.blocks {
                let BlockDoc::DataModel { data, .. } = block else {
                    continue;
                };
                if out.contains_key(&data.name) {
                    continue;
                }
                let key = match &data.key {
                    Some(Value::String(key)) => Some(key.as_str()),
                    Some(Value::Array(_)) => None,
                    _ => Some("id"),
                };
                let property = key.and_then(|key| {
                    entities
                        .get(&data.name)?
                        .properties
                        .iter()
                        .find(|p| p.name.as_ref() == key)
                });
                let schema = match property.map(|p| (&p.kind, p.array)) {
                    Some((PropertyTypeIr::Enum(values), false)) => json!({ "enum": values }),
                    _ => json!({ "type": "string" }),
                };
                out.insert(data.name.clone(), schema);
            }
        }
        out
    }
}

/// What [`from_inputs`] needs beyond the inputs.
#[derive(Default)]
pub(crate) struct RequestShape {
    /// Each entity's id schema (a string when unknown).
    ids: HashMap<Arc<str>, Value>,
    /// The pools beside a policy's request, by entity.
    pools: HashMap<Arc<str>, Pool>,
    /// Stored relationships: the host finds their members; never sent.
    stored: HashSet<Arc<str>>,
}

/// A pool of a referenced entity's records.
struct Pool {
    /// The reference fields of its records.
    references: std::collections::BTreeMap<Arc<str>, Arc<str>>,
    /// The fields its records may leave out (optional, or with a default).
    optional: HashSet<String>,
}

impl RequestShape {
    /// The schema of a reference: the id of the entity it names.
    fn id_schema(&self, target: &str, nullable: bool) -> Value {
        let mut id = self
            .ids
            .get(target)
            .cloned()
            .unwrap_or_else(|| json!({ "type": "string" }));
        id["description"] = json!(format!("{target} id"));
        if nullable {
            json!({ "anyOf": [id, { "type": "null" }] })
        } else {
            id
        }
    }
}

/// A request schema from the engine's input properties. The contract
/// leaves out what the host supplies; the evaluated request keeps it (but
/// not a stored relationship's members, which no request holds). A field
/// is required unless it is optional, has a default or is nullable; its
/// default is published. A reference sent as an id (always in the
/// contract, and in a policy's request, where pools hold the records) is
/// that id, at the top and inside a relationship's records.
pub(crate) fn from_inputs(
    inputs: &[InputProperty],
    audience: SchemaAudience,
    shape: &RequestShape,
) -> Value {
    let sent: Vec<&InputProperty> = inputs
        .iter()
        .filter(|input| match audience {
            SchemaAudience::Contract => input.supplied_by == SuppliedBy::Request,
            SchemaAudience::Evaluated => !shape.stored.contains(&input.path),
        })
        .collect();
    let mut schema = nested(sent.iter().map(|input| {
        let required = !input.optional && !matches!(input.resolved_type, VariableType::Nullable(_));
        (
            input.path.as_ref(),
            &input.resolved_type,
            required,
            input.default.as_ref(),
        )
    }));

    let as_id = |leaf: &Value| audience == SchemaAudience::Contract || is_string(leaf);
    for input in &sent {
        let pointer = format!(
            "/properties/{}",
            input
                .path
                .split('.')
                .collect::<Vec<_>>()
                .join("/properties/")
        );
        let Some(leaf) = schema.pointer_mut(&pointer) else {
            continue;
        };
        if let Some(target) = &input.reference {
            if as_id(leaf) {
                let nullable =
                    input.optional || matches!(input.resolved_type, VariableType::Nullable(_));
                let default = leaf.get("default").cloned();
                *leaf = shape.id_schema(target, nullable);
                if let Some(default) = default {
                    leaf["default"] = default;
                }
            }
        }
        if let Some(pool) = shape.pools.get(&input.path) {
            pool_items(leaf, pool, audience);
        }
        for (field, target) in &input.record_references {
            let Some(property) = record_schema(leaf)
                .and_then(|record| record.get_mut("properties")?.get_mut(field.as_ref()))
            else {
                continue;
            };
            if as_id(property) {
                let nullable = property.get("anyOf").is_some();
                *property = shape.id_schema(target, nullable);
            }
        }
    }
    schema
}

/// A pool's records are found by their `id`, and may leave out what their
/// entity says is optional; the evaluated request's records carry what the
/// host keeps beside it (its key column), so they stay open.
fn pool_items(schema: &mut Value, pool: &Pool, audience: SchemaAudience) {
    let Some(items) = record_schema(schema) else {
        return;
    };
    if items["properties"].get("id").is_none() {
        items["properties"]["id"] = json!({ "type": "string" });
    }
    let mut required: BTreeSet<String> = items["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str().map(str::to_string))
        .filter(|r| !pool.optional.contains(r))
        .collect();
    required.insert("id".to_string());
    items["required"] = json!(required.into_iter().collect::<Vec<_>>());
    if audience == SchemaAudience::Evaluated {
        if let Some(items) = items.as_object_mut() {
            items.remove("additionalProperties");
        }
    }
}

/// A string, or a nullable one.
fn is_string(schema: &Value) -> bool {
    match schema.get("anyOf").and_then(Value::as_array) {
        Some(options) => options.iter().any(is_string),
        None => schema["type"] == "string",
    }
}

/// The record object of a relationship's schema: itself, its items, or
/// either inside a nullable `anyOf`.
fn record_schema(schema: &mut Value) -> Option<&mut Value> {
    if schema.get("anyOf").is_some() {
        return schema["anyOf"]
            .as_array_mut()?
            .iter_mut()
            .find(|s| s["type"] != "null")
            .and_then(record_schema);
    }
    match schema["type"].as_str() {
        Some("object") => Some(schema),
        Some("array") => record_schema(schema.get_mut("items")?),
        _ => None,
    }
}

/// The schema of an engine type.
pub(crate) fn leaf_schema(variable_type: &VariableType) -> Value {
    match variable_type {
        VariableType::Any => json!({}),
        VariableType::Null => json!({ "type": "null" }),
        VariableType::Bool => json!({ "type": "boolean" }),
        VariableType::String => json!({ "type": "string" }),
        VariableType::Number => json!({ "type": "number" }),
        VariableType::Date => json!({ "type": "string", "format": "date-time" }),
        VariableType::Interval => json!({ "type": "string" }),
        VariableType::Const(value) => json!({ "const": value.as_ref() }),
        VariableType::Enum(name, values) => {
            let mut schema = json!({
                "enum": values.iter().map(|v| v.as_ref()).collect::<Vec<_>>()
            });
            if let Some(name) = name {
                schema["title"] = json!(name.as_ref());
            }
            schema
        }
        VariableType::Array(items) => json!({ "type": "array", "items": leaf_schema(items) }),
        VariableType::Object(fields) => {
            let fields = fields.borrow();
            let mut properties = Map::new();
            let mut required = BTreeSet::new();

            for (key, field) in fields.iter() {
                properties.insert(key.to_string(), leaf_schema(field));
                if !matches!(field, VariableType::Nullable(_)) {
                    required.insert(key.to_string());
                }
            }

            let mut schema = json!({
                "type": "object",
                "properties": properties,
                "additionalProperties": false,
            });
            if !required.is_empty() {
                schema["required"] = json!(required.into_iter().collect::<Vec<_>>());
            }
            schema
        }
        VariableType::Nullable(inner) => {
            json!({ "anyOf": [leaf_schema(inner), { "type": "null" }] })
        }
    }
}

fn set_at_path(root: &mut Value, path: &[&str], leaf: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };

    let mut cursor = root;
    for segment in parents {
        if cursor.get("type").and_then(Value::as_str) != Some("object") {
            cursor["type"] = json!("object");
            cursor["additionalProperties"] = json!(false);
        }
        if !cursor["properties"].is_object() {
            cursor["properties"] = json!({});
        }

        let properties = &mut cursor["properties"];
        if !properties[*segment].is_object() {
            properties[*segment] = json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            });
        }
        cursor = &mut properties[*segment];
    }

    if !cursor["properties"].is_object() {
        cursor["properties"] = json!({});
    }
    cursor["properties"][*last] = leaf;
}

/// A whole type as a root schema; `None` when it says nothing (`any`, or
/// an object without fields), rather than an object that forbids every key.
pub(crate) fn from_type(variable_type: &VariableType) -> Option<Value> {
    match variable_type {
        VariableType::Any => return None,
        VariableType::Object(fields) if fields.borrow().is_empty() => return None,
        _ => {}
    }

    let mut schema = leaf_schema(variable_type);
    schema
        .as_object_mut()?
        .insert("$schema".to_string(), json!(DRAFT));
    Some(schema)
}

/// Dotted property paths stitched into one object schema (as BRMS
/// `buildSchemaFromProperties`); a property is required unless nullable.
pub(crate) fn from_properties<'a>(
    properties: impl IntoIterator<Item = (&'a str, &'a VariableType)>,
) -> Value {
    nested(
        properties
            .into_iter()
            .map(|(path, ty)| (path, ty, !matches!(ty, VariableType::Nullable(_)), None)),
    )
}

/// Dotted paths nested into one object schema: (path, type, required,
/// default). A top-level field is required when a property under it is.
fn nested<'a>(
    properties: impl IntoIterator<Item = (&'a str, &'a VariableType, bool, Option<&'a Value>)>,
) -> Value {
    let mut root = json!({
        "$schema": DRAFT,
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    });

    let mut required = BTreeSet::new();
    for (path, variable_type, is_required, default) in properties {
        let segments: Vec<&str> = path.split('.').collect();
        let mut leaf = leaf_schema(variable_type);
        if let Some(default) = default {
            leaf["default"] = default.clone();
        }
        set_at_path(&mut root, &segments, leaf);

        if let (Some(first), true) = (segments.first(), is_required) {
            required.insert(first.to_string());
        }
    }

    if !required.is_empty() {
        root["required"] = json!(required.into_iter().collect::<Vec<_>>());
    }

    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn object(fields: &[(&str, VariableType)]) -> VariableType {
        let map = fields
            .iter()
            .map(|(key, value)| (Rc::from(*key), value.clone()))
            .collect();
        VariableType::Object(Rc::new(RefCell::new(map)))
    }

    fn input(
        path: &str,
        optional: bool,
        supplied_by: SuppliedBy,
        default: Option<Value>,
    ) -> InputProperty {
        InputProperty {
            path: path.into(),
            resolved_type: VariableType::Number,
            optional,
            supplied_by,
            default,
            reference: None,
            record_references: Default::default(),
        }
    }

    #[test]
    fn dotted_paths_nest_into_objects() {
        let cart = VariableType::Number;
        let name = VariableType::String;
        let schema = from_properties([("customer.name", &name), ("cart.total", &cart)]);

        assert!(schema.get("$id").is_none());
        assert_eq!(schema["$schema"], json!(DRAFT));
        assert_eq!(schema["required"], json!(["cart", "customer"]));
        assert_eq!(
            schema["properties"]["customer"]["properties"]["name"],
            json!({ "type": "string" })
        );
        assert_eq!(
            schema["properties"]["cart"]["properties"]["total"],
            json!({ "type": "number" })
        );
        assert_eq!(
            schema["properties"]["customer"]["additionalProperties"],
            json!(false)
        );
    }

    #[test]
    fn nullable_top_level_properties_are_not_required() {
        let optional = VariableType::Nullable(Rc::new(VariableType::String));
        let required = VariableType::Number;
        let schema = from_properties([("note", &optional), ("amount", &required)]);

        assert_eq!(schema["required"], json!(["amount"]));
        assert_eq!(
            schema["properties"]["note"],
            json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
        );
    }

    #[test]
    fn object_leaf_marks_non_nullable_fields_required() {
        let value = object(&[
            ("id", VariableType::Number),
            (
                "note",
                VariableType::Nullable(Rc::new(VariableType::String)),
            ),
        ]);
        let schema = from_properties([("customer", &value)]);

        let customer = &schema["properties"]["customer"];
        assert_eq!(customer["required"], json!(["id"]));
        assert_eq!(customer["properties"]["id"], json!({ "type": "number" }));
    }

    #[test]
    fn any_becomes_an_open_schema() {
        let any = VariableType::Any;
        let schema = from_properties([("passthrough", &any)]);

        assert_eq!(schema["properties"]["passthrough"], json!({}));
    }

    #[test]
    fn uninformative_types_have_no_schema() {
        assert_eq!(from_type(&VariableType::Any), None);
        assert_eq!(from_type(&object(&[])), None);
        let typed = from_type(&object(&[("total", VariableType::Number)])).unwrap();
        assert_eq!(typed["$schema"], json!(DRAFT));
        assert_eq!(typed["required"], json!(["total"]));
    }

    #[test]
    fn requests_leave_out_what_the_host_supplies() {
        let inputs = [
            input("transaction.amount", false, SuppliedBy::Request, None),
            input("transaction.txn_count_7d", false, SuppliedBy::Host, None),
            input(
                "customer.limit",
                true,
                SuppliedBy::Request,
                Some(json!(100)),
            ),
            input("note", true, SuppliedBy::Request, None),
        ];
        let shape = RequestShape::default();
        let schema = from_inputs(&inputs, SchemaAudience::Contract, &shape);
        assert_eq!(schema["required"], json!(["transaction"]), "{schema}");
        let transaction = &schema["properties"]["transaction"]["properties"];
        assert!(transaction.get("amount").is_some());
        assert!(
            transaction.get("txn_count_7d").is_none(),
            "a feature is not a request field: {schema}"
        );
        assert_eq!(
            schema["properties"]["customer"]["properties"]["limit"]["default"],
            json!(100)
        );
        assert!(schema["properties"].get("note").is_some());

        // The rules are given the host's values too.
        let evaluated = from_inputs(&inputs, SchemaAudience::Evaluated, &shape);
        assert_eq!(
            evaluated["properties"]["transaction"]["properties"]["txn_count_7d"],
            json!({ "type": "number" })
        );
    }

    #[test]
    fn references_are_sent_as_ids() {
        let record = object(&[("id", VariableType::Number), ("note", VariableType::String)]);
        let line = object(&[("qty", VariableType::Number), ("product", record.clone())]);
        let customer = InputProperty {
            path: "customer".into(),
            resolved_type: record,
            optional: false,
            supplied_by: SuppliedBy::Request,
            default: None,
            reference: Some("customer".into()),
            record_references: Default::default(),
        };
        let lines = InputProperty {
            path: "lines".into(),
            resolved_type: VariableType::Array(Rc::new(line)),
            optional: false,
            supplied_by: SuppliedBy::Request,
            default: None,
            reference: None,
            record_references: [("product".into(), "product".into())].into_iter().collect(),
        };
        let inputs = [customer, lines];
        let shape = RequestShape::default();
        let schema = from_inputs(&inputs, SchemaAudience::Contract, &shape);
        assert_eq!(
            schema["properties"]["customer"],
            json!({ "type": "string", "description": "customer id" }),
            "{schema}"
        );
        let item = &schema["properties"]["lines"]["items"]["properties"];
        assert_eq!(
            item["product"],
            json!({ "type": "string", "description": "product id" }),
            "{schema}"
        );
        assert_eq!(item["qty"], json!({ "type": "number" }));

        // Evaluated: a reference holding its record stays the record.
        let evaluated = from_inputs(&inputs, SchemaAudience::Evaluated, &shape);
        assert_eq!(evaluated["properties"]["customer"]["type"], json!("object"));
        assert_eq!(
            evaluated["properties"]["lines"]["items"]["properties"]["product"]["type"],
            json!("object")
        );
    }

    #[test]
    fn a_nullable_record_inside_a_relationship_is_found() {
        let record = object(&[(
            "product",
            VariableType::Nullable(Rc::new(VariableType::String)),
        )]);
        let lines = InputProperty {
            path: "lines".into(),
            resolved_type: VariableType::Nullable(Rc::new(VariableType::Array(Rc::new(record)))),
            optional: true,
            supplied_by: SuppliedBy::Request,
            default: None,
            reference: None,
            record_references: [("product".into(), "product".into())].into_iter().collect(),
        };
        let schema = from_inputs(
            &[lines],
            SchemaAudience::Evaluated,
            &RequestShape::default(),
        );
        assert_eq!(
            schema["properties"]["lines"]["anyOf"][0]["items"]["properties"]["product"],
            json!({ "anyOf": [{ "type": "string", "description": "product id" }, { "type": "null" }] }),
            "{schema}"
        );
    }
}
