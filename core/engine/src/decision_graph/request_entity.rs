//! A graph's Request node typed by an entity (`target`): the request is that
//! entity, flat (`{ txn_id, amount, customer }`), and is prepared as a
//! policy's is before the first node runs: dates converted and checked
//! against the entity. A reference holds the record it names (the host places
//! it, e.g. the Agent with the record's features); an id alone does not fit.

use std::fmt;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use zen_expression::variable::{Variable, VariableMap};

use crate::loader::DynamicLoader;
use crate::policy::ir::{DataModelIr, DictionaryIr, Policy, PropertyTypeIr};
use crate::policy::validator::InputSchema;
use crate::workspace::graph::with_inline_references;

pub(crate) struct RequestPreparation {
    target: Arc<str>,
    schema: InputSchema,
    /// The target's references: field, the entity it names.
    references: Vec<(Arc<str>, Arc<str>)>,
}

impl fmt::Debug for RequestPreparation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestPreparation")
            .field("target", &self.target)
            .finish()
    }
}

impl RequestPreparation {
    /// The preparation for `target`, from the entities and dictionaries the
    /// graph's imports make visible (the imports and what they import; the
    /// first of a name wins, as in the editor's analysis).
    pub(crate) async fn load(
        loader: &DynamicLoader,
        imports: &[Arc<str>],
        target: &Arc<str>,
    ) -> Result<Self, String> {
        let mut entities: HashMap<Arc<str>, Arc<DataModelIr>> = HashMap::new();
        let mut dictionaries: HashMap<Arc<str>, Arc<DictionaryIr>> = HashMap::new();
        let mut visited: HashSet<Arc<str>> = HashSet::new();
        let mut queue: Vec<Arc<str>> = imports.to_vec();
        queue.reverse();
        while let Some(key) = queue.pop() {
            if !visited.insert(key.clone()) {
                continue;
            }
            let loaded = loader
                .load(key.as_ref())
                .await
                .map_err(|error| format!("failed to load imported policy '{key}': {error:?}"))?;
            let Some(policy) = loaded.as_policy() else {
                continue;
            };
            let parsed = Policy::parse(&key, &policy.0);
            for (_, dm) in parsed.policy.entity_data_models() {
                if !dm.name.is_empty() {
                    entities
                        .entry(dm.name.clone())
                        .or_insert_with(|| Arc::new(dm.clone()));
                }
            }
            for block in &parsed.policy.dictionaries {
                dictionaries
                    .entry(block.ir.name.clone())
                    .or_insert_with(|| block.ir.clone());
            }
            let mut next: Vec<Arc<str>> = parsed.policy.imports().to_vec();
            next.reverse();
            queue.extend(next);
        }
        if !entities.contains_key(target) {
            return Err(format!(
                "no entity `{target}` visible from this graph's imports; import the policy that defines it"
            ));
        }
        let references = entities
            .get(target)
            .map(|dm| {
                dm.properties
                    .iter()
                    .filter_map(|prop| match &prop.kind {
                        PropertyTypeIr::Reference { target: named }
                            if entities.contains_key(named) =>
                        {
                            Some((prop.name.clone(), named.clone()))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // References carry their records here: checked as those entities.
        let inline = with_inline_references(&entities);
        let schema = InputSchema::for_request(
            Arc::new(inline),
            target.clone(),
            std::iter::empty(),
            dictionaries,
        );
        Ok(Self {
            target: target.clone(),
            schema,
            references,
        })
    }

    /// The request as nodes read it, or why it does not fit the entity.
    pub(crate) fn prepare(&self, input: &Variable) -> Result<Variable, String> {
        if !matches!(input, Variable::Object(_)) {
            return Err(format!(
                "the request is not a valid `{}`: expected an object, got {}",
                self.target,
                input.type_name()
            ));
        }
        // A reference sent as its id: say so, and what it carries instead.
        for (field, named) in &self.references {
            let ids: Vec<String> = match input.dot(field.as_ref()) {
                Some(Variable::String(id)) => vec![id.to_string()],
                Some(Variable::Array(items)) => items
                    .borrow()
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect(),
                _ => continue,
            };
            let Some(first) = ids.first() else {
                continue;
            };
            return Err(format!(
                "the request is not a valid `{}`: '{field}' is the id \"{first}\", but a reference carries its record: send the `{named}` itself (e.g. {{ \"id\": \"{first}\", … }}); a host such as the Agent fills it in from the id",
                self.target
            ));
        }
        // Checked and converted as the entity under its name, as policies are.
        let mut root = VariableMap::new();
        root.insert(
            zen_types::symbol::Symbol::from(self.target.as_ref()),
            input.clone(),
        );
        let wrapped = Variable::from_object(root);
        let wrapped = self.schema.convert_dates(&wrapped).unwrap_or(wrapped);
        let errors = self.schema.validate(&wrapped);
        if !errors.is_empty() {
            let prefix = format!("{}.", self.target);
            let listed: Vec<String> = errors
                .iter()
                .map(|e| {
                    let path = e.path.strip_prefix(&prefix).unwrap_or(&e.path);
                    format!("'{path}': expected {}, got {}", e.expected, e.got)
                })
                .collect();
            return Err(format!(
                "the request is not a valid `{}`: {}",
                self.target,
                listed.join("; ")
            ));
        }
        Ok(wrapped
            .dot(self.target.as_ref())
            .unwrap_or_else(|| input.clone()))
    }
}
