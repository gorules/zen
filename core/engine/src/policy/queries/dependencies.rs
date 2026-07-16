use std::sync::Arc;

use ahash::{HashMap, HashSet, HashSetExt};

use crate::policy::blocks::{IntelliSenseSource, ReadFlattener};
use crate::policy::queries::scope::{EntityForm, VariableTypeScope};
use crate::workspace::db::{Db, Snapshot};
use crate::workspace::types::{BlockRef, DependencyNode};
use zen_expression::variable::VariableType;

impl Db {
    pub fn dependencies(&self, target: &str) -> DependencyNode {
        let snapshot = self.snapshot();
        let unit = self.unit_for_property(target);
        let entity_form = EntityForm::new(&unit.entity_sources);
        let enriched = self.enriched_of_unit(&unit);
        let scope = &enriched.scope;
        let mut visited: HashSet<Arc<str>> = HashSet::new();
        let mut expr_ids_cache: HashMap<BlockRef, HashMap<Arc<str>, HashSet<Arc<str>>>> =
            HashMap::default();
        let mut node_cache: HashMap<Arc<str>, DependencyNode> = HashMap::default();

        if unit.dep_graph.writer_for(target).is_none() {
            if let Some(node) = self.field_dependency_node(
                &unit.dep_graph,
                &snapshot,
                &entity_form,
                scope,
                target,
                &mut visited,
                &mut expr_ids_cache,
                &mut node_cache,
            ) {
                return node;
            }
        }

        let (node, _tainted) = Self::build_dep_node(
            &unit.dep_graph,
            &snapshot.shallow,
            &snapshot.rule_by_ref,
            &entity_form,
            scope,
            target,
            false,
            &mut visited,
            &mut expr_ids_cache,
            &mut node_cache,
        );
        node
    }

    #[allow(clippy::too_many_arguments)]
    fn field_dependency_node(
        &self,
        graph: &crate::policy::queries::dependency::DependencyGraph,
        snapshot: &Snapshot,
        entity_form: &EntityForm,
        scope: &VariableType,
        target: &str,
        visited: &mut HashSet<Arc<str>>,
        expr_ids_cache: &mut HashMap<BlockRef, HashMap<Arc<str>, HashSet<Arc<str>>>>,
        node_cache: &mut HashMap<Arc<str>, DependencyNode>,
    ) -> Option<DependencyNode> {
        let target_type = scope.resolve_at(target).to_acyclic();
        let segments: Vec<&str> = target.split('.').collect();
        if segments.len() < 2 {
            return None;
        }
        let (prefix, owner, tail_start) = (1..segments.len()).rev().find_map(|i| {
            let prefix = segments[..i].join(".");
            graph
                .writer_for(&prefix)
                .cloned()
                .map(|owner| (prefix, owner, i))
        })?;
        let tail: Vec<&str> = segments[tail_start..].to_vec();

        let Some(block) = snapshot.rule_by_ref.get(&owner) else {
            return None;
        };

        let undecomposable = DependencyNode {
            property: Arc::from(target),
            written_by: Some(owner.clone()),
            unresolved: true,
            resolved_type: target_type.clone(),
            deps: Vec::new(),
        };

        let value_exprs = block.kind.write_value_expressions(&prefix);
        if value_exprs.is_empty() {
            return Some(undecomposable);
        }

        let mut flat: Vec<crate::policy::blocks::PropertyRead> = Vec::new();
        let mut navigated = false;
        {
            let is = self.intellisense();
            let mut is = is.borrow_mut();
            for expr in &value_exprs {
                if let Some(reads) = IntelliSenseSource::field_reads(&mut is, expr, &tail) {
                    navigated = true;
                    ReadFlattener::extend_from_deps(&reads, &None, &mut flat);
                }
            }
        }
        if !navigated {
            return Some(undecomposable);
        }

        let mut seen: HashSet<Arc<str>> = HashSet::new();
        let mut deps: Vec<DependencyNode> = Vec::new();
        for read in flat {
            if read.path.as_ref() == "$" || !seen.insert(read.path.clone()) {
                continue;
            }
            let (child, _tainted) = Self::build_dep_node(
                graph,
                &snapshot.shallow,
                &snapshot.rule_by_ref,
                entity_form,
                scope,
                &read.path,
                read.unresolved,
                visited,
                expr_ids_cache,
                node_cache,
            );
            deps.push(child);
        }
        deps.sort_by(|a, b| a.property.cmp(&b.property));

        Some(DependencyNode {
            property: Arc::from(target),
            written_by: Some(owner),
            unresolved: false,
            resolved_type: target_type,
            deps,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_dep_node(
        graph: &crate::policy::queries::dependency::DependencyGraph,
        shallow: &crate::policy::queries::dependency::ShallowAnalyses,
        rule_by_ref: &HashMap<BlockRef, Arc<crate::policy::blocks::Block>>,
        entity_form: &crate::policy::queries::scope::EntityForm,
        scope: &VariableType,
        target: &str,
        target_unresolved: bool,
        visited: &mut HashSet<Arc<str>>,
        expr_ids_cache: &mut HashMap<BlockRef, HashMap<Arc<str>, HashSet<Arc<str>>>>,
        node_cache: &mut HashMap<Arc<str>, DependencyNode>,
    ) -> (DependencyNode, bool) {
        let property: Arc<str> = if graph.writer_for(target).is_some() {
            Arc::from(target)
        } else {
            match entity_form.rewrite(target) {
                Some(ef) if graph.writer_for(&ef).is_some() => Arc::from(ef),
                _ => Arc::from(target),
            }
        };
        if let Some(cached) = node_cache.get(&property) {
            return (cached.clone(), false);
        }
        let written_by = graph.writer_for(&property).cloned();

        let unresolved = written_by.is_none() && target_unresolved;

        let resolved_type = graph
            .node_map
            .get(property.as_ref())
            .map(|&idx| graph.graph[idx].resolved_type_in(scope, &property))
            .unwrap_or_else(|| scope.resolve_at(&property).to_acyclic());

        if !visited.insert(property.clone()) {
            return (
                DependencyNode {
                    property,
                    written_by,
                    unresolved,
                    resolved_type,
                    deps: Vec::new(),
                },
                true,
            );
        }
        let Some(owner) = &written_by else {
            visited.remove(&property);
            let node = DependencyNode {
                property: property.clone(),
                written_by: None,
                unresolved,
                resolved_type,
                deps: Vec::new(),
            };
            node_cache.insert(property, node.clone());
            return (node, false);
        };

        let block_expr_ids = expr_ids_cache.entry(owner.clone()).or_insert_with(|| {
            rule_by_ref
                .get(owner)
                .map(|block| block.kind.write_dependency_expr_ids())
                .unwrap_or_default()
        });
        let unfiltered = block_expr_ids.is_empty();
        let allowed: HashSet<Arc<str>> = match block_expr_ids.get(&property) {
            Some(ids) => ids.clone(),
            None => block_expr_ids
                .iter()
                .filter(|(k, _)| {
                    crate::policy::queries::dependency::PathPrefix::extends(&property, k)
                })
                .flat_map(|(_, ids)| ids.iter().cloned())
                .collect(),
        };

        let direct_paths: Vec<(Arc<str>, bool)> = shallow
            .for_block(owner)
            .map(|r| {
                let mut by_path: HashMap<Arc<str>, bool> = HashMap::default();
                for read in r.reads.iter().filter(|read| match &read.expression_id {
                    _ if unfiltered => true,
                    Some(id) => allowed.contains(id),
                    None => true,
                }) {
                    by_path
                        .entry(read.path.clone())
                        .and_modify(|u| *u = *u && read.unresolved)
                        .or_insert(read.unresolved);
                }
                let mut out: Vec<(Arc<str>, bool)> = by_path.into_iter().collect();
                out.sort_by(|a, b| a.0.cmp(&b.0));
                out
            })
            .unwrap_or_default();

        let mut tainted = false;
        let deps: Vec<DependencyNode> = direct_paths
            .into_iter()
            .map(|(path, child_unresolved)| {
                let (child, child_tainted) = Self::build_dep_node(
                    graph,
                    shallow,
                    rule_by_ref,
                    entity_form,
                    scope,
                    &path,
                    child_unresolved,
                    visited,
                    expr_ids_cache,
                    node_cache,
                );
                tainted |= child_tainted;
                child
            })
            .collect();

        visited.remove(&property);
        let node = DependencyNode {
            property: property.clone(),
            written_by,
            unresolved,
            resolved_type,
            deps,
        };
        if !tainted {
            node_cache.insert(property, node.clone());
        }
        (node, tainted)
    }
}
