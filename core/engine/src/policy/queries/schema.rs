use std::sync::Arc;

use ahash::{HashMap, HashSet};
use zen_expression::variable::VariableType;

use crate::policy::db::Db;
use crate::policy::ir::DataModelIr;
use crate::policy::queries::dependency::{DependencyGraph, PathPrefix};
use crate::policy::queries::scope::PropertyScope;
use crate::policy::types::{
    Entity, EntityField, FieldOrigin, Global, InputProperty, OutputProperty, PropertyKind,
    ScopeRequest,
};

impl Db {
    pub fn entities(&self, req: &ScopeRequest) -> Vec<Entity> {
        let entity_filter = (!req.goals.is_empty()).then(|| {
            self.goal_reachable_entities(&self.unit(&req.policy_path).dep_graph, &req.goals)
        });
        let mut by_entity: HashMap<Arc<str>, Vec<EntityField>> = HashMap::default();

        let fields = self
            .walk_schema_fields(req)
            .into_iter()
            .chain(self.walk_computed_fields(req));
        for (entity, field) in fields {
            if entity_filter.as_ref().is_some_and(|f| !f.contains(&entity)) {
                continue;
            }
            by_entity.entry(entity).or_default().push(field);
        }

        let mut result: Vec<Entity> = by_entity
            .into_iter()
            .map(|(name, mut fields)| {
                fields.sort_by(|a, b| a.name.cmp(&b.name));
                Entity { name, fields }
            })
            .collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    pub fn globals(&self, req: &ScopeRequest) -> Vec<Global> {
        let mut out: Vec<Global> = Vec::new();
        let unit = self.unit(&req.policy_path);
        let visible = &unit.members;
        let entities_map = &unit.entities;
        let goal_filter =
            (!req.goals.is_empty()).then(|| unit.dep_graph.reachable_from(&req.goals));

        for vp in self.walk_visible_properties(&req.policy_path) {
            if !matches!(vp.scope, PropertyScope::Global) {
                continue;
            }
            if let Some(filter) = goal_filter.as_ref() {
                if !filter.contains(&vp.property.name) {
                    continue;
                }
            }
            let mut visited: HashSet<Arc<str>> = HashSet::default();
            let resolved_type =
                DataModelIr::wire_property_type(&vp.property, entities_map, &mut visited);
            out.push(Global {
                name: vp.property.name.clone(),
                resolved_type,
                origin: FieldOrigin::Schema {
                    source: vp.policy_path,
                    kind: vp.property.kind.to_schema_field_kind(vp.property.array),
                },
            });
        }

        let mut seen: HashSet<Arc<str>> = out.iter().map(|g| g.name.clone()).collect();
        let enriched = self.enriched_of_unit(&unit);
        for (path, owner, node) in unit.dep_graph.computed_in(visible) {
            if path.contains('.') {
                continue;
            }
            if !seen.insert(path.clone()) {
                continue;
            }
            if let Some(filter) = goal_filter.as_ref() {
                if !filter.contains(path) {
                    continue;
                }
            }
            out.push(Global {
                name: path.clone(),
                resolved_type: node.resolved_type_in(&enriched.scope, path),
                origin: FieldOrigin::Computed {
                    written_by: owner.clone(),
                    instance_of: unit.computed_instances.get(path).cloned(),
                },
            });
        }

        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn inputs(&self, req: &ScopeRequest) -> Vec<InputProperty> {
        let unit = self.unit(&req.policy_path);
        let visible = &unit.members;
        let entities = &unit.entities;
        let (root_entities, ref_targets) = self.classify_root_entities(visible);

        let mut result: Vec<InputProperty> = self
            .walk_visible_properties(&req.policy_path)
            .into_iter()
            .filter(|vp| match &vp.scope {
                PropertyScope::Entity(entity) => root_entities.contains(entity),
                PropertyScope::Global => true,
            })
            .map(|vp| {
                let mut visited: HashSet<Arc<str>> = HashSet::default();
                InputProperty {
                    path: vp.dotted_path(),
                    resolved_type: DataModelIr::wire_property_type(
                        &vp.property,
                        entities,
                        &mut visited,
                    ),
                }
            })
            .collect();

        for target in &ref_targets {
            if !entities.contains_key(target) {
                continue;
            }
            let mut visited: HashSet<Arc<str>> = HashSet::default();
            let entity_type = DataModelIr::wire_object(target, entities, &mut visited);
            if !matches!(entity_type, VariableType::Any) {
                result.push(InputProperty {
                    path: target.clone(),
                    resolved_type: entity_type.array(),
                });
            }
        }

        if !req.goals.is_empty() {
            let reachable = self.goal_reachable_input_paths(&unit.dep_graph, &req.goals, visible);
            result.retain(|p| {
                reachable.iter().any(|r| {
                    PathPrefix::extends(p.path.as_ref(), r.as_ref())
                        || PathPrefix::extends(r.as_ref(), p.path.as_ref())
                })
            });
        }
        result.sort_by(|a, b| a.path.cmp(&b.path));
        result
    }

    pub fn outputs(&self, req: &ScopeRequest) -> Vec<OutputProperty> {
        let unit = self.unit(&req.policy_path);
        let enriched = self.enriched_of_unit(&unit);
        let goal_filter =
            (!req.goals.is_empty()).then(|| unit.dep_graph.reachable_from(&req.goals));

        let mut result: Vec<OutputProperty> = unit
            .dep_graph
            .computed_in(&unit.members)
            .filter(|(path, _, _)| goal_filter.as_ref().is_none_or(|f| f.contains(*path)))
            .map(|(path, owner, node)| OutputProperty {
                path: path.clone(),
                resolved_type: node.resolved_type_in(&enriched.scope, path),
                kind: PropertyKind::Computed,
                written_by: Some(owner.clone()),
                instance_of: unit.computed_instances.get(path).cloned(),
            })
            .collect();
        result.sort_by(|a, b| a.path.cmp(&b.path));
        result
    }

    fn walk_schema_fields(&self, req: &ScopeRequest) -> Vec<(Arc<str>, EntityField)> {
        let unit = self.unit(&req.policy_path);
        let entities = &unit.entities;
        self.walk_visible_properties(&req.policy_path)
            .into_iter()
            .filter_map(|vp| {
                let PropertyScope::Entity(entity) = vp.scope else {
                    return None;
                };
                let mut visited: HashSet<Arc<str>> = HashSet::default();
                let resolved_type =
                    DataModelIr::wire_property_type(&vp.property, entities, &mut visited);
                let origin = FieldOrigin::Schema {
                    source: vp.policy_path,
                    kind: vp.property.kind.to_schema_field_kind(vp.property.array),
                };
                Some((
                    entity,
                    EntityField {
                        name: vp.property.name,
                        resolved_type,
                        origin,
                    },
                ))
            })
            .collect()
    }

    fn walk_computed_fields(&self, req: &ScopeRequest) -> Vec<(Arc<str>, EntityField)> {
        let unit = self.unit(&req.policy_path);
        let enriched = self.enriched_of_unit(&unit);

        let mut sorted: Vec<(&Arc<str>, &crate::policy::types::BlockRef, &_)> =
            unit.dep_graph.computed_in(&unit.members).collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));

        let mut seen: HashSet<(Arc<str>, Arc<str>)> = HashSet::default();
        sorted
            .into_iter()
            .filter_map(|(path, owner, node)| {
                let (entity, name) = path.split_once('.')?;
                let entity: Arc<str> = Arc::from(entity);
                let name: Arc<str> = Arc::from(name);
                if !seen.insert((entity.clone(), name.clone())) {
                    return None;
                }
                Some((
                    entity,
                    EntityField {
                        name,
                        resolved_type: node.resolved_type_in(&enriched.scope, path),
                        origin: FieldOrigin::Computed {
                            written_by: owner.clone(),
                            instance_of: unit.computed_instances.get(path).cloned(),
                        },
                    },
                ))
            })
            .collect()
    }

    fn classify_root_entities(
        &self,
        visible: &HashSet<Arc<str>>,
    ) -> (HashSet<Arc<str>>, HashSet<Arc<str>>) {
        let parsed: Vec<_> = visible.iter().filter_map(|pp| self.parsed(pp)).collect();
        let models = parsed
            .iter()
            .flat_map(|p| p.policy.data_models().map(|(_, dm)| dm));
        DataModelIr::classify_roots(models)
    }

    pub(crate) fn goal_reachable_input_paths(
        &self,
        graph: &DependencyGraph,
        goals: &[Arc<str>],
        visible: &HashSet<Arc<str>>,
    ) -> HashSet<Arc<str>> {
        graph
            .reachable_from(goals)
            .iter()
            .filter(|p| {
                graph.node_map.get(p.as_ref()).is_some_and(|&idx| {
                    match &graph.graph[idx].written_by {
                        None => true,
                        Some(owner) => !visible.contains(&owner.policy_path),
                    }
                })
            })
            .cloned()
            .collect()
    }

    fn goal_reachable_entities(
        &self,
        graph: &DependencyGraph,
        goals: &[Arc<str>],
    ) -> HashSet<Arc<str>> {
        graph
            .reachable_from(goals)
            .iter()
            .filter_map(|p| p.split('.').next().map(Arc::<str>::from))
            .collect()
    }
}
