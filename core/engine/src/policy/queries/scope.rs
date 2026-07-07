use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use petgraph::stable_graph::StableDiGraph;
use zen_expression::variable::{Variable, VariableType};

use crate::policy::blocks::InstanceSource;
use crate::policy::db::{Db, Snapshot};
use crate::policy::ir::{DataModelIr, DictionaryIr, ParsedPolicy, Property, PropertyTypeIr};
use crate::policy::queries::dependency::DependencyGraph;
use crate::policy::types::InstanceTarget;

#[derive(Debug, Clone)]
pub struct EntitySource {
    pub path: Arc<str>,
    pub owner: Option<Arc<str>>,
}

pub type EntitySources = HashMap<Arc<str>, EntitySource>;

pub(crate) struct EntityForm {
    iter_sources: Vec<(Arc<str>, Arc<str>)>,
}

impl EntityForm {
    pub(crate) fn new(entity_sources: &EntitySources) -> Self {
        let mut iter_sources: Vec<(Arc<str>, Arc<str>)> = entity_sources
            .iter()
            .filter(|(entity, src)| src.path.as_ref() != entity.as_ref())
            .map(|(entity, src)| (src.path.clone(), entity.clone()))
            .collect();
        iter_sources.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        Self { iter_sources }
    }

    pub(crate) fn rewrite(&self, path: &str) -> Option<String> {
        let mut current = path.to_string();
        let mut changed_at_least_once = false;
        for _ in 0..crate::policy::MAX_RECURSION_DEPTH {
            let mut changed = false;
            for (src, entity) in &self.iter_sources {
                let src_len = src.len();
                if current.len() > src_len
                    && current.as_bytes().get(src_len) == Some(&b'.')
                    && current.starts_with(src.as_ref())
                {
                    let next = format!("{}.{}", entity, &current[src_len + 1..]);
                    if next != current {
                        current = next;
                        changed = true;
                        changed_at_least_once = true;
                        break;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        changed_at_least_once.then_some(current)
    }
}

pub trait PathSegments {
    fn to_dotted(&self) -> String;
}

impl PathSegments for [Rc<str>] {
    fn to_dotted(&self) -> String {
        self.iter()
            .map(|s| s.as_ref())
            .collect::<Vec<_>>()
            .join(".")
    }
}

#[derive(Debug, Clone)]
pub struct ReferenceField {
    pub path: Arc<str>,
    pub target: Arc<str>,
    pub array: bool,
}

pub struct ImportGraph {
    pub graph: StableDiGraph<Arc<str>, ()>,
    pub node_map: HashMap<Arc<str>, petgraph::graph::NodeIndex>,
}

pub struct EntityGraph {
    models: HashMap<Arc<str>, Arc<DataModelIr>>,
    globals: HashMap<Arc<str>, Property>,
    entity_sources: Arc<EntitySources>,
    computed: HashMap<Arc<str>, InstanceTarget>,
}

impl EntityGraph {
    pub fn contains(&self, name: &str) -> bool {
        self.models.contains_key(name)
    }

    pub fn next_entity(&self, current: &str, field: &str) -> Option<Arc<str>> {
        if let Some(dm) = self.models.get(current) {
            for prop in &dm.properties {
                if prop.name.as_ref() == field {
                    return match &prop.kind {
                        PropertyTypeIr::Relationship { target }
                        | PropertyTypeIr::Reference { target } => Some(target.clone()),
                        _ => None,
                    };
                }
            }
        }
        if let Some(EntitySource {
            owner: Some(owner), ..
        }) = self.entity_sources.get(current)
        {
            if owner.as_ref() == field {
                return Some(owner.clone());
            }
        }
        self.computed
            .get(format!("{current}.{field}").as_str())
            .map(|t| t.target.clone())
    }

    pub fn next_entity_for_global(&self, name: &str) -> Option<Arc<str>> {
        if let Some(prop) = self.globals.get(name) {
            return match &prop.kind {
                PropertyTypeIr::Relationship { target } | PropertyTypeIr::Reference { target } => {
                    Some(target.clone())
                }
                _ => None,
            };
        }
        self.computed
            .get(name)
            .filter(|_| !name.contains('.'))
            .map(|t| t.target.clone())
    }

    pub fn global_property(&self, name: &str) -> Option<&Property> {
        self.globals.get(name)
    }

    pub fn resolve_path_to_element(&self, path: &[Rc<str>]) -> Option<Arc<str>> {
        let first = path.first()?;
        let root_str: &str = first.as_ref();
        let (mut current, start_idx) = if self.models.contains_key(root_str) {
            (Arc::<str>::from(root_str), 1)
        } else if let Some(target) = self.next_entity_for_global(root_str) {
            (target, 1)
        } else {
            return None;
        };
        for segment in &path[start_idx..] {
            current = self.next_entity(&current, segment.as_ref())?;
        }
        Some(current)
    }

    pub(crate) fn resolve_instance_targets(
        &self,
        dep_graph: &DependencyGraph,
        pool_roots: &HashSet<Arc<str>>,
    ) -> HashMap<Arc<str>, InstanceTarget> {
        let sources: HashMap<&str, &InstanceSource> = dep_graph
            .graph
            .node_weights()
            .filter(|n| n.written_by.is_some())
            .filter_map(|n| n.instance_source.as_ref().map(|s| (n.path.as_ref(), s)))
            .collect();

        let mut memo: HashMap<Arc<str>, Option<InstanceTarget>> = HashMap::new();
        let mut visiting: HashSet<Arc<str>> = HashSet::new();
        for path in sources.keys() {
            self.resolve_instance(path, &sources, pool_roots, &mut memo, &mut visiting);
        }
        memo.into_iter()
            .filter_map(|(path, target)| target.map(|t| (path, t)))
            .collect()
    }

    fn resolve_instance(
        &self,
        path: &str,
        sources: &HashMap<&str, &InstanceSource>,
        pool_roots: &HashSet<Arc<str>>,
        memo: &mut HashMap<Arc<str>, Option<InstanceTarget>>,
        visiting: &mut HashSet<Arc<str>>,
    ) -> Option<InstanceTarget> {
        if let Some(known) = memo.get(path) {
            return known.clone();
        }
        let key: Arc<str> = Arc::from(path);
        if !visiting.insert(key.clone()) {
            return None;
        }
        let result = sources.get(path).copied().and_then(|src| {
            let mut target =
                self.resolve_source_path(&src.path, sources, pool_roots, memo, visiting)?;
            if src.element {
                if !target.array {
                    return None;
                }
                target.array = false;
            }
            Some(target)
        });
        visiting.remove(&key);
        memo.insert(key, result.clone());
        result
    }

    fn resolve_source_path(
        &self,
        dotted: &str,
        sources: &HashMap<&str, &InstanceSource>,
        pool_roots: &HashSet<Arc<str>>,
        memo: &mut HashMap<Arc<str>, Option<InstanceTarget>>,
        visiting: &mut HashSet<Arc<str>>,
    ) -> Option<InstanceTarget> {
        let mut segments = dotted.split('.');
        let root = segments.next()?;

        let mut current = if self.models.contains_key(root) {
            InstanceTarget {
                target: Arc::from(root),
                array: pool_roots.contains(root),
            }
        } else if let Some(prop) = self.globals.get(root) {
            match &prop.kind {
                PropertyTypeIr::Relationship { target } | PropertyTypeIr::Reference { target } => {
                    InstanceTarget {
                        target: target.clone(),
                        array: prop.array,
                    }
                }
                _ => return None,
            }
        } else {
            self.resolve_instance(root, sources, pool_roots, memo, visiting)?
        };

        let mut cumulative = String::from(root);
        for segment in segments {
            cumulative.push('.');
            cumulative.push_str(segment);
            if current.array {
                return None;
            }
            current = match self.declared_hop(&current.target, segment) {
                Some(hop) => hop,
                None => self.resolve_instance(&cumulative, sources, pool_roots, memo, visiting)?,
            };
        }
        Some(current)
    }

    fn declared_hop(&self, entity: &str, field: &str) -> Option<InstanceTarget> {
        let dm = self.models.get(entity)?;
        let prop = dm.properties.iter().find(|p| p.name.as_ref() == field)?;
        match &prop.kind {
            PropertyTypeIr::Relationship { target } | PropertyTypeIr::Reference { target } => {
                Some(InstanceTarget {
                    target: target.clone(),
                    array: prop.array,
                })
            }
            _ => None,
        }
    }

    pub(crate) fn register_computed(&mut self, targets: &HashMap<Arc<str>, InstanceTarget>) {
        self.computed = targets.clone();
    }
}

impl Snapshot {
    fn iter_data_models(
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
    ) -> impl Iterator<Item = (Arc<str>, &DataModelIr)> {
        all_parsed.iter().flat_map(|(path, p)| {
            p.policy
                .data_models()
                .map(move |(_, dm)| (path.clone(), dm))
        })
    }

    pub(crate) fn compute_dictionary_map(
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
    ) -> HashMap<Arc<str>, Arc<DictionaryIr>> {
        let mut sorted: Vec<(&Arc<str>, &Arc<ParsedPolicy>)> = all_parsed.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        let mut out: HashMap<Arc<str>, Arc<DictionaryIr>> = HashMap::new();
        for (_, parsed) in sorted {
            for (_, dict) in parsed.policy.dictionaries() {
                out.entry(dict.name.clone()).or_insert_with(|| dict.clone());
            }
        }
        out
    }

    pub(crate) fn compute_entity_graph(
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
        entity_sources: &Arc<EntitySources>,
    ) -> EntityGraph {
        let mut models: HashMap<Arc<str>, Arc<DataModelIr>> = HashMap::new();
        let mut globals: HashMap<Arc<str>, Property> = HashMap::new();
        for (_, dm) in Self::iter_data_models(all_parsed) {
            if dm.scope.is_global() {
                for prop in &dm.properties {
                    globals
                        .entry(prop.name.clone())
                        .or_insert_with(|| prop.clone());
                }
            } else {
                models
                    .entry(dm.name.clone())
                    .or_insert_with(|| Arc::new(dm.clone()));
            }
        }
        EntityGraph {
            models,
            globals,
            entity_sources: entity_sources.clone(),
            computed: HashMap::new(),
        }
    }

    pub(crate) fn compute_base_scope(
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
        entity_sources: &EntitySources,
    ) -> VariableType {
        let mut entity_map: HashMap<Arc<str>, VariableType> = HashMap::new();
        let dictionaries = Self::compute_dictionary_map(all_parsed);

        let mut models: Vec<(Arc<str>, &DataModelIr)> =
            Self::iter_data_models(all_parsed).collect();
        models.sort_by(|a, b| a.1.name.cmp(&b.1.name).then_with(|| a.0.cmp(&b.0)));

        for (_, dm) in models.iter().filter(|(_, dm)| !dm.scope.is_global()) {
            if let Some(existing) = entity_map.get(&dm.name) {
                dm.merge_scalar_fields_into(existing);
            } else {
                let entity_type =
                    VariableType::Object(Rc::new(RefCell::new(dm.build_scalar_fields())));
                entity_map.insert(dm.name.clone(), entity_type);
            }
        }

        for (_, dm) in models.iter().filter(|(_, dm)| !dm.scope.is_global()) {
            dm.wire_relationships(&entity_map, &dictionaries);
        }

        for (entity_name, source) in entity_sources.iter() {
            let EntitySource {
                owner: Some(owner_name),
                ..
            } = source
            else {
                continue;
            };
            let (Some(entity_type), Some(owner_type)) = (
                entity_map.get(entity_name.as_ref()),
                entity_map.get(owner_name.as_ref()),
            ) else {
                continue;
            };
            let VariableType::Object(ref entity_obj) = entity_type else {
                continue;
            };
            entity_obj
                .borrow_mut()
                .entry(Rc::from(owner_name.as_ref()))
                .or_insert_with(|| owner_type.shallow_clone());
        }

        let mut scope_fields: HashMap<Rc<str>, VariableType> = HashMap::new();
        for (name, entity_type) in &entity_map {
            scope_fields.insert(Rc::from(name.as_ref()), entity_type.shallow_clone());
        }

        for (_, dm) in models.iter().filter(|(_, dm)| dm.scope.is_global()) {
            for prop in &dm.properties {
                let key = Rc::from(prop.name.as_ref());
                let value_type = prop.build_global_type(&entity_map, &dictionaries);
                scope_fields.entry(key).or_insert(value_type);
            }
        }

        VariableType::Object(Rc::new(RefCell::new(scope_fields)))
    }

    pub(crate) fn compute_entity_sources(
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
    ) -> EntitySources {
        let mut sources: EntitySources = HashMap::new();
        let mut all_entities: HashSet<Arc<str>> = HashSet::new();
        let mut referenced_only: HashSet<Arc<str>> = HashSet::new();

        let mut sorted: Vec<(Arc<str>, &DataModelIr)> =
            Self::iter_data_models(all_parsed).collect();
        sorted.sort_by(|a, b| a.1.name.cmp(&b.1.name).then_with(|| a.0.cmp(&b.0)));

        for (_, dm) in &sorted {
            if !dm.scope.is_global() {
                all_entities.insert(dm.name.clone());
            }
            for prop in &dm.properties {
                match &prop.kind {
                    PropertyTypeIr::Relationship { target } => {
                        let (path, owner) = if dm.scope.is_global() {
                            (Arc::from(prop.name.as_ref()), None)
                        } else {
                            (
                                Arc::from(format!("{}.{}", dm.name, prop.name)),
                                Some(dm.name.clone()),
                            )
                        };
                        sources
                            .entry(target.clone())
                            .or_insert(EntitySource { path, owner });
                    }
                    PropertyTypeIr::Reference { target } => {
                        if !sources.contains_key(target) {
                            referenced_only.insert(target.clone());
                        }
                    }
                    _ => {}
                }
            }
        }

        let mut referenced_sorted: Vec<Arc<str>> = referenced_only.into_iter().collect();
        referenced_sorted.sort();
        for entity in &referenced_sorted {
            sources
                .entry(entity.clone())
                .or_insert_with(|| EntitySource {
                    path: entity.clone(),
                    owner: None,
                });
        }
        sources
    }

    pub(crate) fn compute_reference_fields(
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
    ) -> Vec<ReferenceField> {
        Self::iter_data_models(all_parsed)
            .flat_map(|(_, dm)| {
                let is_global = dm.scope.is_global();
                let dm_name = dm.name.clone();
                dm.properties.iter().filter_map(move |prop| {
                    let PropertyTypeIr::Reference { target } = &prop.kind else {
                        return None;
                    };
                    let path: Arc<str> = if is_global {
                        Arc::from(prop.name.as_ref())
                    } else {
                        Arc::from(format!("{}.{}", dm_name, prop.name))
                    };
                    Some(ReferenceField {
                        path,
                        target: target.clone(),
                        array: prop.array,
                    })
                })
            })
            .collect()
    }

    pub(crate) fn compute_import_graph(
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
    ) -> ImportGraph {
        let mut graph = StableDiGraph::new();
        let mut node_map = HashMap::new();

        for path in all_parsed.keys() {
            let idx = graph.add_node(path.clone());
            node_map.insert(path.clone(), idx);
        }

        for (path, p) in all_parsed.iter() {
            let Some(&src) = node_map.get(path) else {
                continue;
            };
            for imported in p.policy.imports() {
                if let Some(&dst) = node_map.get(imported.as_ref()) {
                    graph.add_edge(src, dst, ());
                }
            }
        }

        ImportGraph { graph, node_map }
    }
}

impl Db {
    pub fn visible_policies(&self, path: &str) -> Arc<HashSet<Arc<str>>> {
        Arc::new(self.unit(path).members.clone())
    }

    pub(crate) fn visible_entities(
        &self,
        policy_path: &str,
    ) -> Arc<HashMap<Arc<str>, Arc<DataModelIr>>> {
        Arc::new(self.unit(policy_path).entities.clone())
    }

    pub(crate) fn walk_visible_properties(&self, policy_path: &str) -> Vec<VisibleProperty> {
        let visible = self.visible_policies(policy_path);
        let mut sorted: Vec<Arc<str>> = visible.iter().cloned().collect();
        sorted.sort();
        let mut out: Vec<VisibleProperty> = Vec::new();
        let mut seen: HashSet<(Option<Arc<str>>, Arc<str>)> = HashSet::default();
        for pp in &sorted {
            let Some(parsed) = self.parsed(pp) else {
                continue;
            };
            for (_, dm) in parsed.policy.data_models() {
                for prop in &dm.properties {
                    let scope_key: Option<Arc<str>> = if dm.scope.is_global() {
                        None
                    } else {
                        Some(dm.name.clone())
                    };
                    if seen.insert((scope_key.clone(), prop.name.clone())) {
                        let scope = match scope_key {
                            Some(entity) => PropertyScope::Entity(entity),
                            None => PropertyScope::Global,
                        };
                        out.push(VisibleProperty {
                            policy_path: pp.clone(),
                            scope,
                            property: prop.clone(),
                        });
                    }
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct DataModelEntry {
    pub policy_path: Arc<str>,
    pub block_id: Arc<str>,
    pub ir: Arc<DataModelIr>,
}

#[derive(Debug, Clone)]
pub(crate) struct VisibleProperty {
    pub policy_path: Arc<str>,
    pub scope: PropertyScope,
    pub property: Property,
}

#[derive(Debug, Clone)]
pub(crate) enum PropertyScope {
    Entity(Arc<str>),
    Global,
}

impl VisibleProperty {
    pub(crate) fn dotted_path(&self) -> Arc<str> {
        match &self.scope {
            PropertyScope::Entity(entity) => {
                Arc::from(format!("{}.{}", entity, self.property.name))
            }
            PropertyScope::Global => self.property.name.clone(),
        }
    }
}

impl DataModelIr {
    fn build_scalar_fields(&self) -> HashMap<Rc<str>, VariableType> {
        let mut fields: HashMap<Rc<str>, VariableType> = HashMap::new();
        for prop in &self.properties {
            let Some(base) = prop.kind.as_scalar() else {
                continue;
            };
            let mut final_type = if prop.array { base.array() } else { base };
            if prop.optional {
                final_type = VariableType::Nullable(Rc::new(final_type));
            }
            fields.insert(Rc::from(prop.name.as_ref()), final_type);
        }
        fields
    }

    fn merge_scalar_fields_into(&self, existing: &VariableType) {
        let VariableType::Object(ref obj) = existing else {
            return;
        };
        for prop in &self.properties {
            let Some(base) = prop.kind.as_scalar() else {
                continue;
            };
            let mut new_type = if prop.array { base.array() } else { base };
            if prop.optional {
                new_type = VariableType::Nullable(Rc::new(new_type));
            }
            let key = Rc::from(prop.name.as_ref());
            obj.borrow_mut().entry(key).or_insert(new_type);
        }
    }

    fn wire_relationships(
        &self,
        entity_map: &HashMap<Arc<str>, VariableType>,
        dictionaries: &HashMap<Arc<str>, Arc<DictionaryIr>>,
    ) {
        for prop in &self.properties {
            let target_name = match &prop.kind {
                PropertyTypeIr::Relationship { target } | PropertyTypeIr::Reference { target } => {
                    target
                }
                _ => continue,
            };

            let target_type = match entity_map.get(target_name.as_ref()) {
                Some(target_entity) => target_entity.shallow_clone(),
                None => match dictionaries.get(target_name.as_ref()) {
                    Some(dict) if matches!(prop.kind, PropertyTypeIr::Relationship { .. }) => {
                        dict.enum_type()
                    }
                    _ => continue,
                },
            };

            let mut final_type = if prop.array {
                target_type.array()
            } else {
                target_type
            };
            if prop.optional {
                final_type = VariableType::Nullable(Rc::new(final_type));
            }

            if let Some(VariableType::Object(ref obj)) = entity_map.get(&self.name) {
                let key = Rc::from(prop.name.as_ref());
                obj.borrow_mut().entry(key).or_insert(final_type);
            }
        }
    }
}

impl PropertyTypeIr {
    fn as_scalar(&self) -> Option<VariableType> {
        match self {
            PropertyTypeIr::String => Some(VariableType::String),
            PropertyTypeIr::Enum(values) => Some(VariableType::Enum(
                None,
                crate::policy::ir::enum_values_to_rc(values),
            )),
            PropertyTypeIr::Number => Some(VariableType::Number),
            PropertyTypeIr::Boolean => Some(VariableType::Bool),
            PropertyTypeIr::Date => Some(VariableType::Date),
            PropertyTypeIr::Relationship { .. } | PropertyTypeIr::Reference { .. } => None,
        }
    }
}

impl Property {
    pub(crate) fn build_global_type(
        &self,
        entity_map: &HashMap<Arc<str>, VariableType>,
        dictionaries: &HashMap<Arc<str>, Arc<DictionaryIr>>,
    ) -> VariableType {
        let inner = match &self.kind {
            PropertyTypeIr::String | PropertyTypeIr::Date => VariableType::String,
            PropertyTypeIr::Enum(values) => {
                VariableType::Enum(None, crate::policy::ir::enum_values_to_rc(values))
            }
            PropertyTypeIr::Number => VariableType::Number,
            PropertyTypeIr::Boolean => VariableType::Bool,
            PropertyTypeIr::Relationship { target } | PropertyTypeIr::Reference { target } => {
                match entity_map.get(target.as_ref()) {
                    Some(t) => t.shallow_clone(),
                    None => match dictionaries.get(target.as_ref()) {
                        Some(dict) if matches!(self.kind, PropertyTypeIr::Relationship { .. }) => {
                            dict.enum_type()
                        }
                        _ => VariableType::Any,
                    },
                }
            }
        };
        let mut final_type = if self.array { inner.array() } else { inner };
        if self.optional {
            final_type = VariableType::Nullable(Rc::new(final_type));
        }
        final_type
    }
}

pub trait VariableTypeScope {
    fn resolve_at(&self, path: &str) -> VariableType;

    fn insert_at_path(&self, path: &str, value_type: &VariableType, allow_fill: bool) -> bool;

    fn with_dollar(&self, field_type: &VariableType) -> VariableType;

    fn to_acyclic(&self) -> VariableType;

    fn break_cycles(&self);
}

impl VariableTypeScope for VariableType {
    fn resolve_at(&self, path: &str) -> VariableType {
        let mut current = self.shallow_clone();
        for segment in path.split('.') {
            current = current.get(segment);
        }
        current
    }

    fn insert_at_path(&self, path: &str, value_type: &VariableType, allow_fill: bool) -> bool {
        let segments: Vec<&str> = path.split('.').collect();
        if segments.is_empty() {
            return false;
        }

        let mut current = self.shallow_clone();
        for &segment in segments[..segments.len() - 1].iter() {
            let next = current.get(segment);
            match &next {
                VariableType::Object(_) => current = next,
                VariableType::Any if allow_fill => {
                    let VariableType::Object(ref obj) = current else {
                        return false;
                    };
                    let new_obj = VariableType::empty_object();
                    obj.borrow_mut()
                        .insert(Rc::from(segment), new_obj.shallow_clone());
                    current = new_obj;
                }
                _ => return false,
            }
        }

        let VariableType::Object(ref obj) = current else {
            return false;
        };
        let final_key = segments[segments.len() - 1];
        obj.borrow_mut()
            .insert(Rc::from(final_key), value_type.shallow_clone());
        true
    }

    fn with_dollar(&self, field_type: &VariableType) -> VariableType {
        let VariableType::Object(ref obj) = self else {
            return self.shallow_clone();
        };
        let mut fields: HashMap<Rc<str>, VariableType> = obj.borrow().clone();
        fields.insert(Variable::dollar_key(), field_type.shallow_clone());
        VariableType::Object(Rc::new(RefCell::new(fields)))
    }

    fn to_acyclic(&self) -> VariableType {
        let mut visited: HashSet<*const ()> = HashSet::default();
        AcyclicCloner::clone_type(self, &mut visited)
    }

    fn break_cycles(&self) {
        let mut visited: HashSet<*const ()> = HashSet::default();
        let mut cells = Vec::new();
        let mut stack: Vec<VariableType> = vec![self.shallow_clone()];
        while let Some(current) = stack.pop() {
            match current {
                VariableType::Object(obj) => {
                    if !visited.insert(Rc::as_ptr(&obj) as *const ()) {
                        continue;
                    }
                    stack.extend(obj.borrow().values().map(VariableType::shallow_clone));
                    cells.push(obj);
                }
                VariableType::Array(inner) | VariableType::Nullable(inner) => {
                    stack.push(inner.shallow_clone());
                }
                _ => {}
            }
        }
        for cell in cells {
            cell.borrow_mut().clear();
        }
    }
}

struct AcyclicCloner;

impl AcyclicCloner {
    fn clone_type(t: &VariableType, visited: &mut HashSet<*const ()>) -> VariableType {
        match t {
            VariableType::Any
            | VariableType::Null
            | VariableType::Bool
            | VariableType::String
            | VariableType::Number
            | VariableType::Date
            | VariableType::Interval => t.shallow_clone(),
            VariableType::Const(c) => VariableType::Const(c.clone()),
            VariableType::Enum(name, values) => VariableType::Enum(name.clone(), values.clone()),
            VariableType::Array(inner) => {
                VariableType::Array(Rc::new(Self::clone_type(inner, visited)))
            }
            VariableType::Nullable(inner) => {
                VariableType::Nullable(Rc::new(Self::clone_type(inner, visited)))
            }
            VariableType::Object(obj) => {
                let ptr = Rc::as_ptr(obj) as *const ();
                if !visited.insert(ptr) {
                    return VariableType::Any;
                }
                let mut fields: HashMap<Rc<str>, VariableType> = HashMap::new();
                for (k, v) in obj.borrow().iter() {
                    fields.insert(k.clone(), Self::clone_type(v, visited));
                }
                visited.remove(&ptr);
                VariableType::Object(Rc::new(RefCell::new(fields)))
            }
        }
    }
}
