use crate::compiled::host::decision_table::index::TableIndex;
use crate::compiled::host::function::v2::strip::TypeStripper;
use crate::compiled::CompiledPlan;
use crate::decision_graph::schema_dict;
use crate::loader::DynamicLoader;
use crate::model::{DecisionNodeKind, FunctionNodeContent, GraphContent};
use ahash::{HashMap, HashMapExt};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct Content {
    graph: GraphContent,
    pub(crate) compiled_plan: Option<Arc<CompiledPlan>>,
    pub(crate) schema_failures: Option<Arc<HashMap<Arc<str>, Arc<str>>>>,
}

impl Deref for Content {
    type Target = GraphContent;

    fn deref(&self) -> &GraphContent {
        &self.graph
    }
}

impl DerefMut for Content {
    fn deref_mut(&mut self) -> &mut GraphContent {
        &mut self.graph
    }
}

impl Content {
    pub(crate) fn of(graph: &GraphContent) -> Self {
        Self {
            graph: graph.clone(),
            compiled_plan: None,
            schema_failures: None,
        }
    }

    pub(crate) fn compiled_verdict(&self) -> Option<Result<(), String>> {
        self.compiled_plan.as_deref().map(CompiledPlan::verdict)
    }

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

    pub(crate) fn hosted(&self) -> (Content, Arc<CompiledPlan>) {
        let mut owned = self.clone();
        let plan = Arc::new(CompiledPlan::hosted(&owned));
        owned.compiled_plan = Some(plan.clone());
        (owned, plan)
    }

    pub(crate) async fn loaded(graph: Arc<Content>, loader: &DynamicLoader) -> Arc<Content> {
        if graph.compiled_plan.is_some()
            && graph.resolved_schemas.is_some()
            && !graph.schemas_failed()
        {
            return graph;
        }
        let mut owned = (*graph).clone();
        owned.prepare();
        let _ = owned.resolve_schemas(loader).await;
        Arc::new(owned)
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

    pub(crate) async fn resolve_schemas(&mut self, loader: &DynamicLoader) -> Result<(), String> {
        if self.resolved_schemas.is_some() && !self.schemas_failed() {
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

        let mut resolved = HashMap::with_capacity(referencing.len());
        let mut failures = HashMap::new();
        let mut first = None;
        if !referencing.is_empty() {
            let dictionaries = schema_dict::load_import_dictionaries(loader, &self.imports).await;
            for (id, schema) in referencing {
                match dictionaries
                    .as_ref()
                    .map_err(Clone::clone)
                    .and_then(|dictionaries| schema_dict::resolve_schema(&schema, dictionaries))
                {
                    Ok((value, salt)) => {
                        resolved.insert(id, (Arc::new(value), salt));
                    }
                    Err(message) => {
                        failures.insert(id, Arc::from(message.as_str()));
                        first.get_or_insert(message);
                    }
                }
            }
        }

        self.resolved_schemas = Some(Arc::new(resolved.into_iter().collect()));
        self.schema_failures = Some(Arc::new(failures));
        first.map_or(Ok(()), Err)
    }

    pub(crate) fn schemas_failed(&self) -> bool {
        self.schema_failures
            .as_ref()
            .is_some_and(|failures| !failures.is_empty())
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
        self.stripped_functions
            .replace(Arc::new(stripped.into_iter().collect()));
    }
}
