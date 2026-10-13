use crate::compiled::CompiledPlan;
use crate::decision_graph::schema_dict;
use crate::loader::DynamicLoader;
use crate::nodes::decision_table::index::TableIndex;
use crate::nodes::function::v2::strip::TypeStripper;
use crate::nodes::validator_cache::ValidatorCache;
use crate::policy::PolicyDocument;
use ahash::{HashMap, HashMapExt};
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;

type ResolvedSchemas = HashMap<Arc<str>, (Arc<serde_json::Value>, u64)>;
use zen_types::decision::{DecisionEdge, DecisionNode, DecisionNodeKind, FunctionNodeContent};
use crate::decision_graph::request_entity::RequestPreparation;

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum DecisionContent {
    Graph(Arc<GraphContent>),
    Policy(PolicyContent),
}

impl<'de> Deserialize<'de> for DecisionContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let is_policy = value
            .as_object()
            .is_some_and(|object| object.contains_key("blocks"));

        let content = if is_policy {
            serde_path_to_error::deserialize::<_, PolicyContent>(value).map(Self::Policy)
        } else {
            serde_path_to_error::deserialize::<_, GraphContent>(value)
                .map(|graph| Self::Graph(Arc::new(graph)))
        };

        content.map_err(serde::de::Error::custom)
    }
}

impl Default for DecisionContent {
    fn default() -> Self {
        Self::Graph(Arc::new(GraphContent::default()))
    }
}

impl DecisionContent {
    pub fn as_graph(&self) -> Option<&GraphContent> {
        match self {
            Self::Graph(g) => Some(g),
            Self::Policy(_) => None,
        }
    }

    pub fn as_policy(&self) -> Option<&PolicyContent> {
        match self {
            Self::Policy(p) => Some(p),
            Self::Graph(_) => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Graph(_) => "graph",
            Self::Policy(_) => "policy",
        }
    }

    pub fn into_graph_arc(self: Arc<Self>) -> Option<Arc<GraphContent>> {
        match self.as_ref() {
            Self::Graph(g) => Some(g.clone()),
            Self::Policy(_) => None,
        }
    }
}

impl From<GraphContent> for DecisionContent {
    fn from(value: GraphContent) -> Self {
        Self::Graph(Arc::new(value))
    }
}

impl From<Arc<GraphContent>> for DecisionContent {
    fn from(value: Arc<GraphContent>) -> Self {
        Self::Graph(value)
    }
}

impl From<PolicyContent> for DecisionContent {
    fn from(value: PolicyContent) -> Self {
        Self::Policy(value)
    }
}

impl From<Arc<PolicyDocument>> for PolicyContent {
    fn from(value: Arc<PolicyDocument>) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GraphContent {
    pub nodes: Vec<Arc<DecisionNode>>,
    pub edges: Vec<Arc<DecisionEdge>>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<Arc<str>>,

    #[serde(skip)]
    #[cfg_attr(not(feature = "driver"), allow(clippy::disallowed_types))]
    pub compiled_cache: Option<Arc<zen_expression::OpcodeCache>>,

    #[serde(skip)]
    pub stripped_functions: Option<Arc<HashMap<Arc<str>, Arc<str>>>>,

    #[serde(skip)]
    pub resolved_schemas: Option<Arc<ResolvedSchemas>>,

    /// The Request node's entity preparation (`target`), loaded once.
    #[serde(skip)]
    pub(crate) request_preparation: RequestPreparationCache,

    #[serde(skip)]
    pub(crate) validator_cache: ValidatorCache,

    #[serde(skip)]
    pub(crate) dt_indexes: Option<Arc<HashMap<Arc<str>, TableIndex>>>,

    #[serde(skip)]
    pub(crate) compiled_plan: Option<Arc<CompiledPlan>>,

    #[serde(skip)]
    pub(crate) schema_failures: Option<Arc<HashMap<Arc<str>, Arc<str>>>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct PolicyContent(pub Arc<PolicyDocument>);

impl GraphContent {
    pub fn compiled_verdict(&self) -> Option<Result<(), &str>> {
        self.compiled_plan.as_deref().map(CompiledPlan::verdict)
    }
}

impl GraphContent {
    pub(crate) fn prepare(&mut self) -> Arc<CompiledPlan> {
        self.compile_functions();
        self.build_dt_indexes();
        match &self.compiled_plan {
            Some(plan) => plan.clone(),
            None => {
                let plan = Arc::new(CompiledPlan::compile(self));
                self.compiled_plan = Some(plan.clone());
                plan
            }
        }
    }

    pub(crate) fn hosted(&self) -> (GraphContent, Arc<CompiledPlan>) {
        let mut owned = self.clone();
        let plan = Arc::new(CompiledPlan::hosted(&owned));
        owned.compiled_plan = Some(plan.clone());
        (owned, plan)
    }

    pub(crate) async fn loaded(graph: Arc<GraphContent>, loader: &DynamicLoader) -> Arc<GraphContent> {
        if graph.compiled_plan.is_some() && graph.resolved_schemas.is_some() {
            return graph;
        }
        let mut owned = (*graph).clone();
        owned.prepare();
        let _ = owned.resolve_schemas(loader).await;
        Arc::new(owned)
    }

    pub fn compile(&mut self) {
        self.prepare();
        #[cfg(feature = "driver")]
        self.build_opcode_cache();
    }

    pub(crate) fn is_compiled(&self) -> bool {
        #[cfg(feature = "driver")]
        if self.compiled_cache.is_none() {
            return false;
        }
        self.compiled_plan.is_some()
    }

    fn build_dt_indexes(&mut self) {
        if self.dt_indexes.is_some() {
            return;
        }

        let indexes: HashMap<Arc<str>, TableIndex> = self
            .nodes
            .iter()
            .filter_map(|node| match &node.kind {
                DecisionNodeKind::DecisionTableNode { content } => {
                    TableIndex::build(&content.inputs, &content.rules)
                        .map(|index| (node.id.clone(), index))
                }
                _ => None,
            })
            .collect();

        self.dt_indexes = Some(Arc::new(indexes));
    }

    /// The entity a Request node names (`target`): the host passes a flat
    /// body under this name.
    pub fn request_target(&self) -> Option<Arc<str>> {
        self.nodes.iter().find_map(|node| match &node.kind {
            DecisionNodeKind::InputNode { content } => content.target.clone(),
            _ => None,
        })
    }

    pub async fn resolve_schemas(&mut self, loader: &DynamicLoader) -> Result<(), String> {
        if let Some(target) = self.request_target() {
            self.request_preparation
                .get_or_load(loader, &self.imports, &target)
                .await?;
        }
        if self.resolved_schemas.is_some() {
            return Ok(());
        }

        let mut referencing: Vec<(Arc<str>, Arc<serde_json::Value>)> = Vec::new();
        for node in &self.nodes {
            let schema = match &node.kind {
                DecisionNodeKind::InputNode { content } => content.schema.as_ref(),
                DecisionNodeKind::OutputNode { content } => content.schema.as_ref(),
                _ => None,
            };
            if let Some(schema) = schema {
                if schema_dict::schema_references_dictionary(schema) {
                    referencing.push((node.id.clone(), schema.clone()));
                }
            }
        }

        if referencing.is_empty() {
            self.resolved_schemas = Some(Arc::new(HashMap::new()));
            return Ok(());
        }

        let dictionaries = schema_dict::load_import_dictionaries(loader, &self.imports).await?;
        let mut resolved = HashMap::with_capacity(referencing.len());
        for (id, schema) in referencing {
            let (value, salt) = schema_dict::resolve_schema(&schema, &dictionaries)?;
            resolved.insert(id, (Arc::new(value), salt));
        }

        self.resolved_schemas = Some(Arc::new(resolved));
        Ok(())
    }

    /// A request typed by an entity (`target`), prepared as a policy's
    /// request is: by the graph's preparation, loaded through its imports
    /// once. Not with a schema too.
    #[cfg(feature = "driver")]
    pub(crate) async fn request_input(
        &self,
        loader: &DynamicLoader,
        content: &zen_types::decision::InputNodeContent,
        context: &zen_expression::Variable,
    ) -> Result<zen_expression::Variable, String> {
        let Some(target) = &content.target else {
            return Ok(context.clone());
        };
        if content.schema.is_some() {
            return Err(
                "the request is typed twice: by a schema and by an entity (`target`); keep one"
                    .to_string(),
            );
        }
        let prepared = self
            .request_preparation
            .get_or_load(loader, &self.imports, target)
            .await?;
        prepared.prepare(context)
    }

    pub(crate) async fn validation_schema(
        &self,
        loader: &DynamicLoader,
        node_id: &str,
        schema: Option<&serde_json::Value>,
    ) -> Result<Option<(Arc<serde_json::Value>, u64)>, String> {
        let Some(schema) = schema else {
            return Ok(None);
        };
        if let Some(resolved) = &self.resolved_schemas {
            return Ok(resolved.get(node_id).cloned());
        }
        if !schema_dict::schema_references_dictionary(schema) {
            return Ok(None);
        }

        let dictionaries = schema_dict::load_import_dictionaries(loader, &self.imports).await?;
        schema_dict::resolve_schema(schema, &dictionaries)
            .map(|resolved| Some((Arc::new(resolved.0), resolved.1)))
    }

    fn compile_functions(&mut self) {
        if self.stripped_functions.is_some() {
            return;
        }
        let mut stripped: HashMap<Arc<str>, Arc<str>> = HashMap::new();
        for node in &self.nodes {
            let DecisionNodeKind::FunctionNode {
                content: FunctionNodeContent::Version2(function),
            } = &node.kind
            else {
                continue;
            };
            stripped
                .entry(function.source.clone())
                .or_insert_with(|| TypeStripper::strip(&function.source));
        }
        self.stripped_functions.replace(Arc::new(stripped));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_graph_error_mentions_field() {
        let error = serde_json::from_str::<DecisionContent>(r#"{"nodes":[],"edges":"bad"}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("edges"), "{error}");
    }

    #[test]
    fn malformed_policy_error_mentions_inner_field() {
        let json = r#"{"blocks":[{"type":"assertion","id":"b1","props":{"data":{}}}]}"#;
        let error = serde_json::from_str::<DecisionContent>(json)
            .unwrap_err()
            .to_string();
        assert!(error.contains("output"), "{error}");
    }

    #[test]
    fn compile_strips_function_sources() {
        let json = r#"{"nodes":[{"id":"f","name":"f","type":"functionNode","content":{"source":"export const handler = (input: { age: number }) => ({ total: input.age });"}}],"edges":[]}"#;
        let mut content: GraphContent = serde_json::from_str(json).unwrap();
        content.compile();
        let stripped = content.stripped_functions.as_ref().unwrap();
        assert_eq!(stripped.len(), 1);
        let value = stripped.values().next().unwrap();
        assert!(!value.contains(": number"), "{value}");
    }

    #[test]
    fn valid_graph_routes_to_graph_variant() {
        let content: DecisionContent = serde_json::from_str(r#"{"nodes":[],"edges":[]}"#).unwrap();
        assert!(content.as_graph().is_some());
    }

    #[test]
    fn valid_policy_routes_to_policy_variant() {
        let content: DecisionContent = serde_json::from_str(r#"{"blocks":[]}"#).unwrap();
        assert!(content.as_policy().is_some());
    }
}

/// A graph's Request entity preparation, loaded once: by `resolve_schemas`,
/// or by the first evaluation that needs it (so content made without a
/// caching loader doesn't load its imports on every request). Shared by the
/// clones of a graph (a decision evaluates prepared and hosted copies of its
/// content), and kept per (target, imports), so a clone that changes either
/// loads its own. Equal whatever it holds, as the other caches of a graph are.
#[derive(Default, Debug, Clone)]
pub(crate) struct RequestPreparationCache(Arc<std::sync::Mutex<Option<LoadedRequest>>>);

type LoadedRequest = (Arc<str>, Vec<Arc<str>>, Arc<RequestPreparation>);

impl RequestPreparationCache {
    /// The preparation for `target`, loaded through `imports` the first time.
    /// A failure isn't kept: the next call loads again.
    pub(crate) async fn get_or_load(
        &self,
        loader: &DynamicLoader,
        imports: &[Arc<str>],
        target: &Arc<str>,
    ) -> Result<Arc<RequestPreparation>, String> {
        if let Some(prepared) = self.get(imports, target) {
            return Ok(prepared);
        }
        let loaded = Arc::new(RequestPreparation::load(loader, imports, target).await?);
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some((target.clone(), imports.to_vec(), loaded.clone()));
        }
        Ok(loaded)
    }

    fn get(&self, imports: &[Arc<str>], target: &Arc<str>) -> Option<Arc<RequestPreparation>> {
        let slot = self.0.lock().ok()?;
        let (loaded_target, loaded_imports, prepared) = slot.as_ref()?;
        (loaded_target == target && loaded_imports.as_slice() == imports).then(|| prepared.clone())
    }
}

impl PartialEq for RequestPreparationCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
