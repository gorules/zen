use std::sync::Arc;

use napi::anyhow::anyhow;
use napi_derive::napi;
use serde_json::Value;

use zen_engine::policy;

#[napi(object)]
pub struct PolicyExpressionCursor {
    pub policy_path: String,
    pub block_id: String,
    pub pos: u32,
    #[napi(ts_type = "PolicyCursorTarget")]
    pub target: Value,
}

impl TryFrom<PolicyExpressionCursor> for policy::Cursor {
    type Error = napi::Error;

    fn try_from(c: PolicyExpressionCursor) -> napi::Result<Self> {
        Ok(Self {
            policy_path: c.policy_path.into(),
            block_id: c.block_id.into(),
            pos: c.pos,
            target: serde_json::from_value(c.target)
                .map_err(|e| napi::Error::from_reason(format!("invalid cursor target: {e}")))?,
        })
    }
}

#[allow(dead_code)]
#[napi(object)]
pub struct PolicyFieldKindInfo {
    #[napi(ts_type = "PolicyFieldKind")]
    pub kind: String,
    pub target: Option<String>,
    pub array: Option<bool>,
}

#[napi(object)]
pub struct PolicyDiagnostic {
    #[napi(ts_type = "PolicyDiagnosticCode")]
    pub code: String,
    pub message: String,
    #[napi(ts_type = "PolicySeverity")]
    pub severity: String,
    pub policy_path: String,
    pub block_id: Option<String>,
    #[napi(ts_type = "PolicySpan")]
    pub span: Option<Vec<u32>>,
    pub expression_id: Option<String>,
    #[napi(ts_type = "PolicyCursorTarget")]
    pub target: Option<Value>,
}

impl From<&policy::Diagnostic> for PolicyDiagnostic {
    fn from(d: &policy::Diagnostic) -> Self {
        Self {
            code: serde_json::to_value(&d.code)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            message: d.message.clone(),
            severity: serde_json::to_value(&d.severity)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            policy_path: d.location.policy_path.to_string(),
            block_id: d.location.block_id.as_ref().map(|s| s.to_string()),
            span: d.location.span.map(|(s, e)| vec![s, e]),
            expression_id: d.location.expression_id.as_ref().map(|s| s.to_string()),
            target: d
                .location
                .target
                .as_ref()
                .and_then(|t| serde_json::to_value(t).ok()),
        }
    }
}

#[napi(object)]
pub struct PolicyEntityFieldInfo {
    pub name: String,
    #[napi(ts_type = "PolicyVariableType")]
    pub resolved_type: Value,
    #[napi(ts_type = "PolicyFieldOrigin")]
    pub origin: Value,
}

#[napi(object)]
pub struct PolicyEntityInfo {
    pub name: String,
    pub fields: Vec<PolicyEntityFieldInfo>,
}

#[napi(object)]
pub struct PolicyGlobalInfo {
    pub name: String,
    #[napi(ts_type = "PolicyVariableType")]
    pub resolved_type: Value,
    #[napi(ts_type = "PolicyFieldOrigin")]
    pub origin: Value,
}

#[napi(object)]
pub struct PolicyInputProperty {
    pub path: String,
    #[napi(ts_type = "PolicyVariableType")]
    pub resolved_type: Value,
}

#[napi(object)]
pub struct PolicyOutputProperty {
    pub path: String,
    #[napi(ts_type = "PolicyVariableType")]
    pub resolved_type: Value,
    #[napi(ts_type = "PolicyPropertyKind")]
    pub kind: String,
    pub written_by: Option<PolicyPropertyWriter>,
    pub instance_of: Option<PolicyInstanceOf>,
}

#[napi(object)]
pub struct PolicyInstanceOf {
    pub target: String,
    pub array: bool,
}

#[napi(object)]
pub struct PolicyPropertyWriter {
    pub policy_path: String,
    pub block_id: String,
}

impl From<&policy::BlockRef> for PolicyPropertyWriter {
    fn from(b: &policy::BlockRef) -> Self {
        Self {
            policy_path: b.policy_path.to_string(),
            block_id: b.block_id.to_string(),
        }
    }
}

#[napi(object)]
pub struct PolicyWriteConflict {
    pub path: String,
    pub policies: Vec<String>,
}

#[napi(object)]
pub struct PolicyGuardedProperty {
    pub path: String,
    #[napi(ts_type = "PolicyVariableType")]
    pub resolved_type: Value,
    pub required_when: Option<String>,
}

#[napi(object)]
pub struct PolicySchemaGroup {
    pub inputs: Vec<PolicyGuardedProperty>,
    pub outputs: Vec<PolicyGuardedProperty>,
}

#[napi(object)]
pub struct PolicyDiscriminantVariant {
    pub value: Option<String>,
    pub arm: String,
    pub group: PolicySchemaGroup,
}

#[napi(object)]
pub struct PolicyDiscriminatedUnion {
    pub property: String,
    #[napi(ts_type = "PolicyVariableType")]
    pub resolved_type: Value,
    pub variants: Vec<PolicyDiscriminantVariant>,
}

#[napi(object)]
pub struct PolicyConditionalSchema {
    #[napi(ts_type = "\"union\" | \"flat\"")]
    pub kind: String,
    pub common: PolicySchemaGroup,
    pub union: Option<PolicyDiscriminatedUnion>,
    pub conditional: Option<PolicySchemaGroup>,
}

impl From<policy::GuardedProperty> for PolicyGuardedProperty {
    fn from(p: policy::GuardedProperty) -> Self {
        Self {
            path: p.path.to_string(),
            resolved_type: variable_type_to_json(&p.resolved_type),
            required_when: p.required_when.map(|w| w.to_string()),
        }
    }
}

impl From<policy::SchemaGroup> for PolicySchemaGroup {
    fn from(g: policy::SchemaGroup) -> Self {
        Self {
            inputs: g.inputs.into_iter().map(Into::into).collect(),
            outputs: g.outputs.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<policy::DiscriminantVariant> for PolicyDiscriminantVariant {
    fn from(v: policy::DiscriminantVariant) -> Self {
        Self {
            value: v.value.map(|s| s.to_string()),
            arm: v.arm.to_string(),
            group: v.group.into(),
        }
    }
}

impl From<policy::DiscriminatedUnion> for PolicyDiscriminatedUnion {
    fn from(u: policy::DiscriminatedUnion) -> Self {
        Self {
            property: u.property.to_string(),
            resolved_type: variable_type_to_json(&u.resolved_type),
            variants: u.variants.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<policy::ConditionalSchema> for PolicyConditionalSchema {
    fn from(schema: policy::ConditionalSchema) -> Self {
        match schema {
            policy::ConditionalSchema::Union { common, union } => Self {
                kind: "union".to_string(),
                common: common.into(),
                union: Some(union.into()),
                conditional: None,
            },
            policy::ConditionalSchema::Flat {
                common,
                conditional,
            } => Self {
                kind: "flat".to_string(),
                common: common.into(),
                union: None,
                conditional: Some(conditional.into()),
            },
        }
    }
}

fn entity_field_to_info(f: &policy::EntityField) -> PolicyEntityFieldInfo {
    PolicyEntityFieldInfo {
        name: f.name.to_string(),
        resolved_type: variable_type_to_json(&f.resolved_type),
        origin: serde_json::to_value(&f.origin).expect("FieldOrigin serializes"),
    }
}

#[napi(object)]
pub struct PolicyInspectResult {
    #[napi(ts_type = "PolicySpan")]
    pub span: Vec<u32>,
    #[napi(ts_type = "PolicyVariableType")]
    pub kind: Value,
    pub label: String,
}

#[napi(object)]
pub struct PolicyPrepareRenameResult {
    #[napi(ts_type = "PolicyRenameTarget")]
    pub target: Value,
    #[napi(ts_type = "PolicySpan")]
    pub span: Vec<u32>,
}

#[napi(object)]
pub struct PolicyCompletion {
    pub label: String,
    pub kind: String,
    pub detail: String,
    pub info: String,
}

#[napi(object)]
pub struct PolicyEvaluateRequest {
    pub policy_path: String,
    #[napi(ts_type = "unknown")]
    pub input: Value,
    pub goals: Option<Vec<String>>,
    pub trace: Option<bool>,
}

#[napi(object)]
pub struct PolicyScopeRequest {
    pub policy_path: String,
    pub goals: Option<Vec<String>>,
}

#[napi(object)]
pub struct PolicyRenameRequest {
    #[napi(ts_type = "PolicyRenameTarget")]
    pub target: Value,
    pub new_name: String,
}

#[napi(object)]
pub struct PolicyUpdateBlockRequest {
    pub policy_path: String,
    #[napi(ts_type = "unknown")]
    pub block: Value,
}

#[napi(object)]
pub struct PolicyRemoveBlockRequest {
    pub policy_path: String,
    pub block_id: String,
}

fn goals_to_arc(goals: Option<Vec<String>>) -> Vec<Arc<str>> {
    goals
        .unwrap_or_default()
        .into_iter()
        .map(Arc::from)
        .collect()
}

fn resolve_diagnostic_cap(max: Option<u32>) -> usize {
    match max {
        None => 100,
        Some(0) => usize::MAX,
        Some(n) => n as usize,
    }
}

fn variable_type_to_json(vt: &zen_expression::variable::VariableType) -> Value {
    use zen_expression::variable::VariableType;

    match vt {
        VariableType::Any => serde_json::json!({ "type": "any" }),
        VariableType::Null => serde_json::json!({ "type": "null" }),
        VariableType::Bool => serde_json::json!({ "type": "bool" }),
        VariableType::String => serde_json::json!({ "type": "string" }),
        VariableType::Number => serde_json::json!({ "type": "number" }),
        VariableType::Date => serde_json::json!({ "type": "date" }),
        VariableType::Interval => serde_json::json!({ "type": "interval" }),
        VariableType::Const(c) => serde_json::json!({ "type": "const", "value": c.as_ref() }),
        VariableType::Enum(name, values) => {
            let vals: Vec<&str> = values.iter().map(|v| v.as_ref()).collect();
            serde_json::json!({
                "type": "enum",
                "name": name.as_ref().map(|n| n.as_ref()),
                "values": vals,
            })
        }
        VariableType::Array(inner) => serde_json::json!({
            "type": "array",
            "items": variable_type_to_json(inner),
        }),
        VariableType::Object(obj) => {
            let fields: serde_json::Map<std::string::String, Value> = obj
                .borrow()
                .iter()
                .map(|(k, v)| (k.to_string(), variable_type_to_json(v)))
                .collect();
            serde_json::json!({
                "type": "object",
                "fields": fields,
            })
        }
        VariableType::Nullable(inner) => serde_json::json!({
            "type": "nullable",
            "inner": variable_type_to_json(inner),
        }),
    }
}

fn dependency_node_to_json(node: &policy::DependencyNode) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("property".into(), Value::String(node.property.to_string()));
    if let Some(writer) = &node.written_by {
        obj.insert(
            "writtenBy".into(),
            serde_json::json!({
                "policyPath": writer.policy_path.as_ref(),
                "blockId": writer.block_id.as_ref(),
            }),
        );
    }
    if node.unresolved {
        obj.insert("unresolved".into(), Value::Bool(true));
    }
    obj.insert(
        "resolvedType".into(),
        variable_type_to_json(&node.resolved_type),
    );
    obj.insert(
        "deps".into(),
        Value::Array(node.deps.iter().map(dependency_node_to_json).collect()),
    );
    Value::Object(obj)
}

impl From<PolicyScopeRequest> for policy::ScopeRequest {
    fn from(r: PolicyScopeRequest) -> Self {
        Self {
            policy_path: r.policy_path.into(),
            goals: goals_to_arc(r.goals),
        }
    }
}

#[napi]
pub struct PolicyWorkspace {
    inner: policy::PolicyWorkspace,
}

#[napi]
impl PolicyWorkspace {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: policy::PolicyWorkspace::new(),
        }
    }

    #[napi]
    pub fn set_policy(&mut self, path: String, document: Value) -> napi::Result<()> {
        let doc: policy::PolicyDocument = serde_json::from_value(document)
            .map_err(|e| anyhow!("Invalid policy document: {e}"))?;
        self.inner.set_policy(path, doc);
        Ok(())
    }

    #[napi]
    pub fn remove_policy(&mut self, path: String) -> bool {
        self.inner.remove_policy(&path)
    }

    #[napi]
    pub fn update_block(&mut self, req: PolicyUpdateBlockRequest) -> napi::Result<()> {
        use zen_engine::policy::BlockDoc;

        let current = self.inner.get_policy(&req.policy_path).ok_or_else(|| {
            napi::Error::from_reason(format!("policy '{}' not found", req.policy_path))
        })?;
        let new_block: BlockDoc =
            serde_json::from_value(req.block).map_err(|e| anyhow!("Invalid block: {e}"))?;
        let new_id = new_block
            .id()
            .ok_or_else(|| napi::Error::from_reason("block is missing 'id'"))?
            .to_string();

        let mut doc = (*current).clone();
        match doc
            .blocks
            .iter()
            .position(|b| b.id() == Some(new_id.as_str()))
        {
            Some(pos) => doc.blocks[pos] = new_block,
            None => doc.blocks.push(new_block),
        }
        self.inner.set_policy(req.policy_path, doc);
        Ok(())
    }

    #[napi]
    pub fn remove_block(&mut self, req: PolicyRemoveBlockRequest) -> bool {
        let Some(current) = self.inner.get_policy(&req.policy_path) else {
            return false;
        };
        let mut doc = (*current).clone();
        let Some(pos) = doc
            .blocks
            .iter()
            .position(|b| b.id() == Some(req.block_id.as_str()))
        else {
            return false;
        };
        doc.blocks.remove(pos);
        self.inner.set_policy(req.policy_path, doc);
        true
    }

    #[napi]
    pub fn policy_paths(&self) -> Vec<String> {
        self.inner
            .policy_paths()
            .into_iter()
            .map(|p| p.to_string())
            .collect()
    }

    #[napi]
    pub fn diagnostics(
        &self,
        policy_path: String,
        max_diagnostics: Option<u32>,
    ) -> Vec<PolicyDiagnostic> {
        let cap = resolve_diagnostic_cap(max_diagnostics);
        self.inner
            .diagnostics(&policy_path)
            .iter()
            .take(cap)
            .map(PolicyDiagnostic::from)
            .collect()
    }

    #[napi]
    pub fn all_diagnostics(&self, max_diagnostics: Option<u32>) -> Vec<PolicyDiagnostic> {
        let cap = resolve_diagnostic_cap(max_diagnostics);
        self.inner
            .all_diagnostics()
            .iter()
            .take(cap)
            .map(PolicyDiagnostic::from)
            .collect()
    }

    #[napi]
    pub fn entities(&self, req: PolicyScopeRequest) -> Vec<PolicyEntityInfo> {
        self.inner
            .entities(&req.into())
            .into_iter()
            .map(|e| PolicyEntityInfo {
                name: e.name.to_string(),
                fields: e.fields.iter().map(entity_field_to_info).collect(),
            })
            .collect()
    }

    #[napi]
    pub fn globals(&self, req: PolicyScopeRequest) -> Vec<PolicyGlobalInfo> {
        self.inner
            .globals(&req.into())
            .into_iter()
            .map(|g| PolicyGlobalInfo {
                name: g.name.to_string(),
                resolved_type: variable_type_to_json(&g.resolved_type),
                origin: serde_json::to_value(&g.origin).expect("FieldOrigin serializes"),
            })
            .collect()
    }

    #[napi]
    pub fn inputs(&self, req: PolicyScopeRequest) -> Vec<PolicyInputProperty> {
        self.inner
            .inputs(&req.into())
            .into_iter()
            .map(|p| PolicyInputProperty {
                path: p.path.to_string(),
                resolved_type: variable_type_to_json(&p.resolved_type),
            })
            .collect()
    }

    #[napi]
    pub fn outputs(&self, req: PolicyScopeRequest) -> Vec<PolicyOutputProperty> {
        self.inner
            .outputs(&req.into())
            .into_iter()
            .map(|p| PolicyOutputProperty {
                path: p.path.to_string(),
                resolved_type: variable_type_to_json(&p.resolved_type),
                kind: p.kind.to_string(),
                written_by: p.written_by.as_ref().map(PolicyPropertyWriter::from),
                instance_of: p.instance_of.as_ref().map(|i| PolicyInstanceOf {
                    target: i.target.to_string(),
                    array: i.array,
                }),
            })
            .collect()
    }

    #[napi]
    pub fn conditional_schema(&self, req: PolicyScopeRequest) -> PolicyConditionalSchema {
        self.inner.conditional_schema(&req.into()).into()
    }

    #[napi]
    pub fn inspect(
        &self,
        cursor: PolicyExpressionCursor,
    ) -> napi::Result<Option<PolicyInspectResult>> {
        let cursor: policy::Cursor = cursor.try_into()?;
        Ok(self
            .inner
            .inspect(&cursor)
            .map(|result| PolicyInspectResult {
                span: vec![result.span.0, result.span.1],
                kind: variable_type_to_json(&result.kind),
                label: result.label,
            }))
    }

    #[napi]
    pub fn completions(
        &self,
        cursor: PolicyExpressionCursor,
    ) -> napi::Result<Vec<PolicyCompletion>> {
        let cursor: policy::Cursor = cursor.try_into()?;
        Ok(self
            .inner
            .completions(&cursor)
            .into_iter()
            .map(|c| PolicyCompletion {
                label: c.label,
                kind: serde_json::to_value(&c.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                detail: c.detail,
                info: c.info,
            })
            .collect())
    }

    #[napi]
    pub fn prepare_rename(
        &self,
        cursor: PolicyExpressionCursor,
    ) -> napi::Result<Option<PolicyPrepareRenameResult>> {
        let cursor: policy::Cursor = cursor.try_into()?;
        Ok(self
            .inner
            .prepare_rename(&cursor)
            .map(|result| PolicyPrepareRenameResult {
                target: serde_json::to_value(&result.target).expect("RenameTarget serializes"),
                span: vec![result.span.0, result.span.1],
            }))
    }

    #[napi(ts_return_type = "PolicyEngineEdit[]")]
    pub fn rename(&self, req: PolicyRenameRequest) -> napi::Result<Vec<Value>> {
        let target: policy::RenameTarget = serde_json::from_value(req.target)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(self
            .inner
            .rename(&target, &req.new_name)
            .into_iter()
            .map(|e| serde_json::to_value(e).expect("EngineEdit serializes"))
            .collect())
    }

    #[napi(ts_return_type = "PolicyReferenceSite[]")]
    pub fn references(&self, target: Value) -> napi::Result<Vec<Value>> {
        let target: policy::RenameTarget =
            serde_json::from_value(target).map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(self
            .inner
            .references(&target)
            .into_iter()
            .map(|s| serde_json::to_value(s).expect("ReferenceSite serializes"))
            .collect())
    }

    #[napi(ts_return_type = "unknown")]
    pub fn input_skeleton(&self, req: PolicyScopeRequest) -> Value {
        let inner = policy::ScopeRequest {
            policy_path: req.policy_path.into(),
            goals: goals_to_arc(req.goals),
        };
        self.inner.input_skeleton(&inner)
    }

    #[napi(ts_return_type = "PolicyDependencyNode")]
    pub fn dependencies(&self, target: String) -> Value {
        dependency_node_to_json(&self.inner.dependencies(&target))
    }

    #[napi(ts_return_type = "PolicyEvaluationResult")]
    pub fn evaluate(&self, req: PolicyEvaluateRequest) -> napi::Result<Value> {
        let inner_req = policy::EvaluateRequest {
            policy_path: req.policy_path.into(),
            input: req.input.into(),
            goals: goals_to_arc(req.goals),
            trace: req.trace.unwrap_or(false),
        };
        Self::eval_to_value(self.inner.evaluate(&inner_req))
    }

    #[napi(ts_return_type = "PolicyEvaluationResult")]
    pub fn enhance_trace(&self, req: PolicyEvaluateRequest) -> napi::Result<Value> {
        let inner_req = policy::EvaluateRequest {
            policy_path: req.policy_path.into(),
            input: req.input.into(),
            goals: goals_to_arc(req.goals),
            trace: true,
        };
        Self::eval_to_value(self.inner.enhance_trace(&inner_req))
    }

    fn eval_to_value(
        result: Result<policy::EvaluationResult, policy::EvaluationError>,
    ) -> napi::Result<Value> {
        match result {
            Ok(result) => {
                serde_json::to_value(&result).map_err(|e| napi::Error::from_reason(e.to_string()))
            }
            Err(policy::EvaluationError::ExpressionFailed {
                partial_trace,
                policy_path,
                block_id,
                expression,
                source,
            }) => {
                let mut obj = serde_json::Map::new();
                if let Some(trace) = partial_trace {
                    let trace = serde_json::to_value(&*trace)
                        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
                    obj.insert("trace".to_string(), trace);
                }
                obj.insert(
                    "error".to_string(),
                    serde_json::json!({
                        "policyPath": policy_path.to_string(),
                        "blockId": block_id.to_string(),
                        "expression": expression.to_string(),
                        "message": source.to_string(),
                    }),
                );
                Ok(Value::Object(obj))
            }
            Err(e) => Err(napi::Error::from_reason(e.to_string())),
        }
    }

    #[napi]
    pub fn component_members(&self, policy: String) -> Vec<String> {
        self.inner
            .component_members(&policy)
            .into_iter()
            .map(|p| p.to_string())
            .collect()
    }

    #[napi]
    pub fn cross_component_write_conflicts(&self) -> Vec<PolicyWriteConflict> {
        self.inner
            .cross_component_write_conflicts()
            .into_iter()
            .map(|c| PolicyWriteConflict {
                path: c.path.to_string(),
                policies: c.policies.into_iter().map(|p| p.to_string()).collect(),
            })
            .collect()
    }
}
