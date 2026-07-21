use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};

use crate::policy::ir::PropertyTypeIr;
use crate::policy::linter::Linter;
use crate::policy::queries::dependency::WriteScope;
use crate::policy::queries::path::PathRoot;
use crate::workspace::db::Db;
use crate::workspace::types::{BlockRef, Diagnostic, DiagnosticCode, DiagnosticLocation};

impl Db {
    pub fn compute_policy_diagnostics(&self, path: &Arc<str>) -> Vec<Diagnostic> {
        let mut out: Vec<Diagnostic> = Vec::new();

        if let Some(parsed) = self.parsed(path) {
            out.extend(parsed.diagnostics.iter().cloned());
        }

        let shallow = self.shallow();
        out.extend(shallow.diags_for(path).iter().cloned());

        out.extend(self.graph_diagnostics(path));

        let enriched = self.enriched(path);
        out.extend(
            enriched
                .diagnostics
                .iter()
                .filter(|d| d.is_in(path))
                .cloned(),
        );
        out.extend(
            enriched
                .per_rule
                .iter()
                .filter(|rule| rule.policy_path == *path)
                .flat_map(|rule| rule.diagnostics.iter().cloned()),
        );

        out.extend(self.import_diagnostics(path));

        out.extend(self.data_model_diagnostics(path));

        out.extend(self.dictionary_diagnostics(path));

        out.extend(self.unreachable_reads_diagnostics(path));

        out.extend(self.nested_iteration_diagnostics(path));

        out.extend(Linter::standard().run(self, path));

        out
    }

    fn nested_iteration_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let shallow = self.shallow();
        let unit = self.unit(target);
        let entity_sources = &unit.entity_sources;
        let classifier = &unit.classifier;

        for rule_analysis in shallow.rules_for(target) {
            let mut flagged: HashSet<Arc<str>> = HashSet::default();
            for write in &rule_analysis.writes {
                let PathRoot::Entity { entity, .. } = classifier.classify(&write.path) else {
                    continue;
                };
                let Some(src) = entity_sources.get(&entity) else {
                    continue;
                };
                let root = src.path.split('.').next().unwrap_or_default();
                if root == entity.as_ref() || !entity_sources.contains_key(root) {
                    continue;
                }
                if !flagged.insert(entity.clone()) {
                    continue;
                }
                out.push(Diagnostic::error(
                    DiagnosticCode::UnsupportedNestedIteration,
                    DiagnosticLocation::block(
                        rule_analysis.policy_path.clone(),
                        rule_analysis.block_id.clone(),
                    ),
                    format!(
                        "cannot write to entity '{entity}': its collection '{}' is nested inside iterated entity '{root}'; only one level of relationship nesting is evaluated",
                        src.path
                    ),
                ));
            }
        }
        out
    }

    fn unreachable_reads_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let shallow = self.shallow();
        let unit = self.unit(target);
        let entity_sources = &unit.entity_sources;
        let classifier = &unit.classifier;
        let rule_index = self.rule_by_ref();

        for rule_analysis in shallow.rules_for(target) {
            let block_ref = BlockRef {
                policy_path: rule_analysis.policy_path.clone(),
                block_id: rule_analysis.block_id.clone(),
            };
            let Some(rule) = rule_index.get(&block_ref) else {
                continue;
            };
            let write_scope = rule.write_scope(&classifier);

            for read in &rule_analysis.reads {
                if read.via_alias {
                    continue;
                }
                let PathRoot::Entity {
                    entity: read_entity,
                    ..
                } = classifier.classify(&read.path)
                else {
                    continue;
                };
                if !entity_sources.contains_key(&read_entity) {
                    continue;
                }
                let reachable = matches!(&write_scope, WriteScope::Entity(e) if e.as_ref() == read_entity.as_ref());
                if reachable {
                    continue;
                }
                let context = match &write_scope {
                    WriteScope::Entity(e) => format!("entity '{e}'"),
                    WriteScope::Global => "globals".to_string(),
                    WriteScope::Empty | WriteScope::Mixed => "this block".to_string(),
                };
                out.push(Diagnostic::error(
                    DiagnosticCode::UnreachableEntityRead,
                    DiagnosticLocation::expression(
                        rule_analysis.policy_path.clone(),
                        rule_analysis.block_id.clone(),
                        read.expression_id.clone().unwrap_or_else(|| Arc::from("")),
                        read.span,
                    ),
                    format!(
                        "cannot read '{}' from {context}: entity '{read_entity}' is iterated; aggregate it with map/some/every/sum",
                        read.path
                    ),
                ));
            }
        }
        out
    }

    fn graph_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        use crate::policy::queries::dependency::PathPrefix;

        let mut out = Vec::new();
        let shallow = self.shallow();
        let unit = self.unit(target);
        let data_model_paths = &unit.data_model_paths;
        let visible = &unit.members;
        let mut first_writer: HashMap<Arc<str>, BlockRef> = HashMap::new();
        let mut all_writes: Vec<(BlockRef, bool, Arc<str>)> = Vec::new();

        let mut sorted_members: Vec<&Arc<str>> = visible.iter().collect();
        sorted_members.sort();
        for rule in sorted_members.iter().flat_map(|m| shallow.rules_for(m)) {
            let in_target = rule.is_in(target);
            let block_ref = BlockRef {
                policy_path: rule.policy_path.clone(),
                block_id: rule.block_id.clone(),
            };
            let block = self.block_ir(&block_ref);

            for write in &rule.writes {
                let wtarget = block
                    .as_ref()
                    .and_then(|b| b.kind.write_target(&write.path));

                if let Some(matched) = data_model_paths.matches_prefix(&write.path) {
                    if in_target {
                        out.push(Diagnostic::error(
                            DiagnosticCode::InputOverride,
                            DiagnosticLocation::block(
                                rule.policy_path.clone(),
                                rule.block_id.clone(),
                            )
                            .maybe_target(wtarget.clone()),
                            format!(
                                "cannot write to '{}': '{}' is defined as a DataModel input",
                                write.path, matched
                            ),
                        ));
                    }
                    continue;
                }

                match first_writer.get(&write.path) {
                    Some(existing) if in_target => {
                        out.push(Diagnostic::error(
                            DiagnosticCode::DuplicateWriter,
                            DiagnosticLocation::block(
                                rule.policy_path.clone(),
                                rule.block_id.clone(),
                            )
                            .maybe_target(wtarget.clone()),
                            format!(
                                "property '{}' is written by both block '{}' (in '{}') and block '{}' (in '{}')",
                                write.path,
                                existing.block_id,
                                existing.policy_path,
                                rule.block_id,
                                rule.policy_path
                            ),
                        ));
                    }
                    Some(_) => {}
                    None => {
                        first_writer.insert(write.path.clone(), block_ref.clone());
                    }
                }

                all_writes.push((block_ref.clone(), in_target, write.path.clone()));
            }

            for write in &rule.writes {
                if !in_target {
                    continue;
                }
                let conflict = rule.reads.iter().find(|r| {
                    data_model_paths.matches_prefix(&r.path).is_none()
                        && PathPrefix::extends(&r.path, &write.path)
                });
                if let Some(read) = conflict {
                    let wtarget = block
                        .as_ref()
                        .and_then(|b| b.kind.write_target(&write.path));
                    let message = if read.path == write.path {
                        format!("block reads and writes the same property '{}'", write.path)
                    } else {
                        format!(
                            "block writes '{}' while reading the overlapping path '{}' — it would read a partially-built object",
                            write.path, read.path
                        )
                    };
                    out.push(Diagnostic::error(
                        DiagnosticCode::SelfReferencingWrite,
                        DiagnosticLocation::block(rule.policy_path.clone(), rule.block_id.clone())
                            .maybe_target(wtarget),
                        message,
                    ));
                }
            }
        }

        let mut containers: Vec<Arc<str>> = Vec::new();
        let mut seen: HashSet<Arc<str>> = HashSet::default();
        for (_, _, candidate) in &all_writes {
            if !seen.insert(candidate.clone()) {
                continue;
            }
            let has_nested = all_writes
                .iter()
                .any(|(_, _, w)| w != candidate && PathPrefix::extends(candidate, w));
            if has_nested {
                containers.push(candidate.clone());
            }
        }
        containers.sort();
        for container in containers {
            let mut blocks: Vec<(&BlockRef, bool)> = Vec::new();
            for (block_ref, in_t, write) in &all_writes {
                if PathPrefix::extends(&container, write)
                    && !blocks.iter().any(|(b, _)| *b == block_ref)
                {
                    blocks.push((block_ref, *in_t));
                }
            }
            let Some((owner, _)) = blocks.iter().find(|(_, in_t)| *in_t) else {
                continue;
            };
            let cross_policy = blocks
                .iter()
                .any(|(b, _)| b.policy_path != owner.policy_path);
            let names: Vec<String> = blocks
                .iter()
                .map(|(b, _)| {
                    if cross_policy {
                        format!("{}:{}", b.policy_path, b.block_id)
                    } else {
                        b.block_id.to_string()
                    }
                })
                .collect();
            out.push(Diagnostic::error(
                DiagnosticCode::PartialObjectWrite,
                DiagnosticLocation::block(owner.policy_path.clone(), owner.block_id.clone()),
                format!(
                    "object '{}' is written as a whole and also written into via nested paths ({}); the whole-object write overwrites the nested writes — assemble it in one place or merge explicitly",
                    container,
                    names.join(", ")
                ),
            ));
        }

        let graph = &unit.dep_graph;
        let cyclic = graph.cyclic_paths();
        let target_in_cycle = cyclic.iter().any(|path| {
            graph
                .writer_for(path)
                .is_some_and(|owner| owner.policy_path == *target)
        });
        if target_in_cycle {
            out.push(Diagnostic::error(
                DiagnosticCode::CyclicDependency,
                DiagnosticLocation::policy(target.clone()),
                "cyclic dependency detected among computed properties",
            ));
        }

        out
    }

    fn import_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let Some(parsed) = self.parsed(target) else {
            return out;
        };
        let all_paths = self.path_set();

        for imported in parsed.policy.imports() {
            if !all_paths.contains(imported) {
                out.push(Diagnostic::error(
                    DiagnosticCode::ImportNotFound,
                    DiagnosticLocation::policy(target.clone()),
                    format!("imported policy '{}' not found in workspace", imported),
                ));
            }
        }

        if let Some(cycles) = self.import_cycles().get(target) {
            out.extend(cycles.iter().cloned());
        }
        out
    }

    fn data_model_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let mut seen: HashMap<
            (Option<Arc<str>>, Arc<str>),
            (Arc<str>, Arc<str>, PropertyTypeIr, bool, bool),
        > = HashMap::default();
        let unit = self.unit(target);
        let all_dms = &unit.data_models;
        let known_entities: HashSet<Arc<str>> = all_dms
            .iter()
            .filter(|e| !e.ir.scope.is_global())
            .map(|e| e.ir.name.clone())
            .collect();
        let global_property_names: HashSet<Arc<str>> = all_dms
            .iter()
            .filter(|e| e.ir.scope.is_global())
            .flat_map(|e| e.ir.properties.iter().map(|p| p.name.clone()))
            .collect();

        for entry in all_dms {
            let policy_path = &entry.policy_path;
            let block_id = &entry.block_id;
            let dm = &entry.ir;
            let is_global = dm.scope.is_global();

            if !is_global && global_property_names.contains(&dm.name) && policy_path == target {
                out.push(Diagnostic::error(
                    DiagnosticCode::DataModelCollision,
                    DiagnosticLocation::block(policy_path.clone(), block_id.clone()),
                    format!(
                        "entity name '{}' collides with a global property of the same name",
                        dm.name
                    ),
                ));
            }

            for prop in &dm.properties {
                if is_global && known_entities.contains(&prop.name) && policy_path == target {
                    out.push(Diagnostic::error(
                        DiagnosticCode::DataModelCollision,
                        DiagnosticLocation::expression(
                            policy_path.clone(),
                            block_id.clone(),
                            prop.id.clone(),
                            None,
                        ),
                        format!(
                            "global property '{}' collides with an entity of the same name",
                            prop.name
                        ),
                    ));
                }

                let key = if is_global {
                    (None, prop.name.clone())
                } else {
                    (Some(dm.name.clone()), prop.name.clone())
                };
                if let Some((prev_policy, prev_block, prev_kind, prev_array, prev_optional)) =
                    seen.get(&key).cloned()
                {
                    let conflicts = !prop.kind.same_shape_as(&prev_kind)
                        || prev_array != prop.array
                        || prev_optional != prop.optional;
                    if conflicts && policy_path == target {
                        let location = if is_global {
                            format!("global property '{}'", prop.name)
                        } else {
                            format!("property '{}' in entity '{}'", prop.name, dm.name)
                        };
                        out.push(Diagnostic::error(
                            DiagnosticCode::DataModelCollision,
                            DiagnosticLocation::expression(
                                policy_path.clone(),
                                block_id.clone(),
                                prop.id.clone(),
                                None,
                            ),
                            format!(
                                "{location} conflicts with definition in '{prev_policy}' (block '{prev_block}')"
                            ),
                        ));
                    }
                } else {
                    seen.insert(
                        key,
                        (
                            policy_path.clone(),
                            block_id.clone(),
                            prop.kind.clone(),
                            prop.array,
                            prop.optional,
                        ),
                    );
                }

                if let PropertyTypeIr::Relationship { target: t }
                | PropertyTypeIr::Reference { target: t } = &prop.kind
                {
                    let dictionary_target =
                        matches!(prop.kind, PropertyTypeIr::Relationship { .. })
                            && unit.dictionaries.contains_key(t);
                    if !known_entities.contains(t) && !dictionary_target && policy_path == target {
                        let owner = if is_global {
                            format!("global property '{}'", prop.name)
                        } else {
                            format!("property '{}' in entity '{}'", prop.name, dm.name)
                        };
                        out.push(Diagnostic::error(
                            DiagnosticCode::UnknownDataModelTarget,
                            DiagnosticLocation::expression(
                                policy_path.clone(),
                                block_id.clone(),
                                prop.id.clone(),
                                None,
                            ),
                            format!("{owner} references unknown entity '{t}'"),
                        ));
                    }
                }
            }
        }

        out
    }

    fn dictionary_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let unit = self.unit(target);
        let known_entities: HashSet<Arc<str>> = unit
            .data_models
            .iter()
            .filter(|e| !e.ir.scope.is_global())
            .map(|e| e.ir.name.clone())
            .collect();
        let global_property_names: HashSet<Arc<str>> = unit
            .data_models
            .iter()
            .filter(|e| e.ir.scope.is_global())
            .flat_map(|e| e.ir.properties.iter().map(|p| p.name.clone()))
            .collect();

        let mut first_by_name: HashMap<Arc<str>, (Arc<str>, Arc<str>)> = HashMap::default();
        for entry in &unit.dictionary_blocks {
            let name = &entry.ir.name;
            if let Some((prev_policy, prev_block)) = first_by_name.get(name) {
                if entry.policy_path == *target {
                    out.push(Diagnostic::error(
                        DiagnosticCode::DataModelCollision,
                        DiagnosticLocation::block(
                            entry.policy_path.clone(),
                            entry.block_id.clone(),
                        ),
                        format!(
                            "dictionary '{name}' is already defined in '{prev_policy}' (block '{prev_block}')"
                        ),
                    ));
                }
                continue;
            }
            first_by_name.insert(
                name.clone(),
                (entry.policy_path.clone(), entry.block_id.clone()),
            );

            if entry.policy_path != *target {
                continue;
            }
            if known_entities.contains(name) {
                out.push(Diagnostic::error(
                    DiagnosticCode::DataModelCollision,
                    DiagnosticLocation::block(entry.policy_path.clone(), entry.block_id.clone()),
                    format!("dictionary name '{name}' collides with an entity of the same name"),
                ));
            }
            if global_property_names.contains(name) {
                out.push(Diagnostic::error(
                    DiagnosticCode::DataModelCollision,
                    DiagnosticLocation::block(entry.policy_path.clone(), entry.block_id.clone()),
                    format!(
                        "dictionary name '{name}' collides with a global property of the same name"
                    ),
                ));
            }
        }

        out
    }
}
