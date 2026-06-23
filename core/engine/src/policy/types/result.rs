use std::sync::Arc;
use std::time::Duration;

use ahash::HashMap;
use serde::Serialize;
use zen_expression::intellisense::completion::Completion as _Completion;
use zen_expression::variable::{Variable, VariableType};

pub type Completion = _Completion;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationResult {
    pub output: Variable,
    #[serde(serialize_with = "EvaluationResult::serialize_duration_micros")]
    pub duration: Duration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<Trace>,
}

impl EvaluationResult {
    fn serialize_duration_micros<S: serde::Serializer>(
        d: &Duration,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_u128(d.as_micros())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Trace {
    pub engine_version: Arc<str>,
    pub properties: HashMap<Arc<str>, Variable>,
    pub executions: Vec<BlockExecution>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockExecution {
    pub block_id: Arc<str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_path: Option<Arc<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_path: Option<Arc<str>>,
    pub trace: BlockTrace,
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub operand_values: HashMap<Arc<str>, Variable>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<WriteTrace>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<Arc<str>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTrace {
    pub path: Arc<str>,
    pub value: Variable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BlockTrace {
    Assertion {
        result: bool,
        conditions: Vec<ConditionTrace>,
    },
    DecisionTable {
        matched_rows: Vec<u32>,
        evaluations: Vec<HashMap<Arc<str>, Variable>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extras: Option<DecisionTableExtras>,
    },
    Expression {
        property: Arc<str>,
        value: Variable,
    },
    Match {
        #[serde(skip_serializing_if = "Option::is_none")]
        matched_arm: Option<Arc<str>>,
        value: Variable,
        arms: Vec<ConditionTrace>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableExtras {
    pub input_pass: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionTrace {
    pub id: Arc<str>,
    pub result: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteConflict {
    pub path: Arc<str>,
    pub policies: Vec<Arc<str>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputProperty {
    pub path: Arc<str>,
    pub resolved_type: VariableType,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputProperty {
    pub path: Arc<str>,
    pub resolved_type: VariableType,
    pub kind: PropertyKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written_by: Option<BlockRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_of: Option<InstanceTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceTarget {
    pub target: Arc<str>,
    pub array: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, strum::Display)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "camelCase")]
pub enum PropertyKind {
    Input,
    Computed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardedProperty {
    pub path: Arc<str>,
    pub resolved_type: VariableType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_when: Option<Arc<str>>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaGroup {
    pub inputs: Vec<GuardedProperty>,
    pub outputs: Vec<GuardedProperty>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscriminantVariant {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Arc<str>>,
    pub arm: Arc<str>,
    pub group: SchemaGroup,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscriminatedUnion {
    pub property: Arc<str>,
    pub resolved_type: VariableType,
    pub variants: Vec<DiscriminantVariant>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ConditionalSchema {
    Union {
        common: SchemaGroup,
        union: DiscriminatedUnion,
    },
    Flat {
        common: SchemaGroup,
        conditional: SchemaGroup,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockRef {
    pub policy_path: Arc<str>,
    pub block_id: Arc<str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyNode {
    pub property: Arc<str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written_by: Option<BlockRef>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unresolved: bool,
    pub resolved_type: VariableType,
    pub deps: Vec<DependencyNode>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    pub name: Arc<str>,
    pub fields: Vec<EntityField>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Global {
    pub name: Arc<str>,
    pub resolved_type: VariableType,
    pub origin: FieldOrigin,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityField {
    pub name: Arc<str>,
    pub resolved_type: VariableType,
    pub origin: FieldOrigin,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "origin",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FieldOrigin {
    Schema {
        source: Arc<str>,
        #[serde(rename = "fieldKind")]
        kind: SchemaFieldKind,
    },
    Computed {
        written_by: BlockRef,
        #[serde(skip_serializing_if = "Option::is_none")]
        instance_of: Option<InstanceTarget>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SchemaFieldKind {
    Scalar,
    Enum { values: Vec<Arc<str>>, array: bool },
    Relationship { target: Arc<str>, array: bool },
    Reference { target: Arc<str>, array: bool },
}
