use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use zen_expression::intellisense::ArmTest;
use zen_expression::variable::VariableType;

use crate::policy::blocks::{BlockKind, IntelliSenseSource, ReadFlattener};
use crate::policy::db::{Db, Unit};
use crate::policy::queries::dependency::PathPrefix;
use crate::policy::queries::scope::VariableTypeScope;
use crate::policy::types::{
    ConditionalSchema, DiscriminantVariant, DiscriminatedUnion, ExpressionKind, GuardedProperty,
    InputProperty, OutputProperty, SchemaGroup, ScopeRequest,
};

struct ArmInfo {
    id: Arc<str>,
    condition: Arc<str>,
    test: ArmTest,
    value_reads: Vec<Arc<str>>,
    discriminant_value: Option<Arc<str>>,
}

struct MatchBlockInfo {
    arms: Vec<ArmInfo>,
}

struct DiscriminantInfo<'a> {
    block: &'a MatchBlockInfo,
    property: Arc<str>,
    resolved_type: VariableType,
}

impl Db {
    pub fn conditional_schema(&self, req: &ScopeRequest) -> ConditionalSchema {
        let unit = self.unit(&req.policy_path);
        let inputs = self.inputs(req);
        let outputs = self.outputs(req);
        let blocks = self.collect_match_blocks(&unit);

        match self.clean_discriminant(&unit, &blocks) {
            Some(disc) => self.build_union(req, &unit, &disc, &inputs, &outputs),
            None => self.build_flat(req, &unit, &blocks, &inputs, &outputs),
        }
    }

    fn collect_match_blocks(&self, unit: &Unit) -> Vec<MatchBlockInfo> {
        let mut members: Vec<&Arc<str>> = unit.members.iter().collect();
        members.sort();
        let mut blocks = Vec::new();
        for pp in members {
            let Some(parsed) = self.parsed(pp) else {
                continue;
            };
            for rule in parsed.policy.rules() {
                let BlockKind::Match(m) = &rule.kind else {
                    continue;
                };
                if m.key.is_empty() {
                    continue;
                }
                let arms = m
                    .arms
                    .iter()
                    .filter(|arm| !arm.condition.is_empty())
                    .map(|arm| {
                        let test = IntelliSenseSource::arm_test(
                            &mut self.intellisense().borrow_mut(),
                            &arm.condition,
                        );
                        let discriminant_value = Self::discriminant_value(&test);
                        ArmInfo {
                            id: arm.id.clone(),
                            condition: arm.condition.clone(),
                            test,
                            value_reads: self.flatten_reads(&arm.value),
                            discriminant_value,
                        }
                    })
                    .collect();
                blocks.push(MatchBlockInfo { arms });
            }
        }
        blocks
    }

    fn discriminant_value(test: &ArmTest) -> Option<Arc<str>> {
        match test {
            ArmTest::Enum { values, .. } if values.len() == 1 => {
                Some(Arc::from(values[0].as_ref()))
            }
            ArmTest::Bool { values, .. } if values.len() == 1 => {
                Some(Arc::from(if values[0] { "true" } else { "false" }))
            }
            _ => None,
        }
    }

    fn clean_discriminant<'a>(
        &self,
        unit: &Unit,
        blocks: &'a [MatchBlockInfo],
    ) -> Option<DiscriminantInfo<'a>> {
        let mut clean: Vec<DiscriminantInfo<'a>> = Vec::new();
        for block in blocks {
            let Some(path) = Self::shared_enum_bool_path(&block.arms) else {
                continue;
            };
            let dotted: Arc<str> = Arc::from(
                path.iter()
                    .map(|s| s.as_ref())
                    .collect::<Vec<&str>>()
                    .join("."),
            );
            if !self.is_input_derived(unit, &dotted) {
                continue;
            }
            let resolved_type = self.resolve_in_unit(unit, &dotted);
            if !matches!(resolved_type, VariableType::Enum(..) | VariableType::Bool) {
                continue;
            }
            clean.push(DiscriminantInfo {
                block,
                property: dotted,
                resolved_type,
            });
        }
        match clean.len() {
            1 => clean.into_iter().next(),
            _ => None,
        }
    }

    fn shared_enum_bool_path(arms: &[ArmInfo]) -> Option<Vec<Rc<str>>> {
        if arms.is_empty() {
            return None;
        }
        let mut shared: Option<&Vec<Rc<str>>> = None;
        for arm in arms {
            let path = match &arm.test {
                ArmTest::Enum { path, .. } | ArmTest::Bool { path, .. } => path,
                _ => return None,
            };
            match shared {
                None => shared = Some(path),
                Some(p) if p == path => {}
                Some(_) => return None,
            }
        }
        shared.cloned()
    }

    fn is_input_derived(&self, unit: &Unit, path: &str) -> bool {
        match unit.dep_graph.node_map.get(path) {
            Some(&idx) => match &unit.dep_graph.graph[idx].written_by {
                None => true,
                Some(owner) => !unit.members.contains(&owner.policy_path),
            },
            None => true,
        }
    }

    fn resolve_in_unit(&self, unit: &Unit, path: &str) -> VariableType {
        let enriched = self.enriched_of_unit(unit);
        let resolved_type = enriched.scope.resolve_at(path);
        let (resolved, _) = resolved_type.unwrap_nullable();
        resolved.shallow_clone()
    }

    fn flatten_reads(&self, src: &Arc<str>) -> Vec<Arc<str>> {
        if src.is_empty() {
            return Vec::new();
        }
        let analysis = IntelliSenseSource::reads_only(
            &mut self.intellisense().borrow_mut(),
            src,
            ExpressionKind::Standard,
        );
        let mut out = Vec::new();
        ReadFlattener::extend_from_deps(&analysis.reads, &None, &mut out);
        out.into_iter()
            .filter(|r| !r.via_alias && !r.unresolved)
            .map(|r| r.path)
            .collect()
    }

    fn schema_roots(&self, unit: &Unit, req: &ScopeRequest) -> Vec<Arc<str>> {
        if !req.goals.is_empty() {
            return req.goals.clone();
        }
        unit.dep_graph
            .computed_in(&unit.members)
            .map(|(path, _, _)| path.clone())
            .collect()
    }

    fn build_union(
        &self,
        req: &ScopeRequest,
        unit: &Unit,
        disc: &DiscriminantInfo,
        inputs: &[InputProperty],
        outputs: &[OutputProperty],
    ) -> ConditionalSchema {
        let graph = &unit.dep_graph;
        let r_all = graph.reachable_from(&self.schema_roots(unit, req));

        let cones: Vec<HashSet<Arc<str>>> = disc
            .block
            .arms
            .iter()
            .map(|arm| graph.reachable_from(&arm.value_reads))
            .collect();

        let mut forced: HashSet<Arc<str>> = HashSet::new();
        forced.insert(disc.property.clone());
        for arm in &disc.block.arms {
            forced.extend(self.flatten_reads(&arm.condition));
        }
        let inter = Self::intersection(&cones);
        let union: HashSet<Arc<str>> = cones.iter().flatten().cloned().collect();

        let common_set: HashSet<Arc<str>> = r_all
            .iter()
            .filter(|p| !union.contains(*p))
            .cloned()
            .chain(inter)
            .chain(forced)
            .collect();

        let variants = disc
            .block
            .arms
            .iter()
            .zip(&cones)
            .map(|(arm, cone)| {
                let variant_set: HashSet<Arc<str>> =
                    cone.difference(&common_set).cloned().collect();
                DiscriminantVariant {
                    value: arm.discriminant_value.clone(),
                    arm: arm.id.clone(),
                    group: Self::group_for(&variant_set, inputs, outputs, |_| None),
                }
            })
            .collect();

        ConditionalSchema::Union {
            common: Self::group_for(&common_set, inputs, outputs, |_| None),
            union: DiscriminatedUnion {
                property: disc.property.clone(),
                resolved_type: disc.resolved_type.shallow_clone(),
                variants,
            },
        }
    }

    fn build_flat(
        &self,
        req: &ScopeRequest,
        unit: &Unit,
        blocks: &[MatchBlockInfo],
        inputs: &[InputProperty],
        outputs: &[OutputProperty],
    ) -> ConditionalSchema {
        let graph = &unit.dep_graph;
        let r_all = graph.reachable_from(&self.schema_roots(unit, req));

        let mut union: HashSet<Arc<str>> = HashSet::new();
        let mut forced: HashSet<Arc<str>> = HashSet::new();
        let mut guards: HashMap<Arc<str>, Vec<Arc<str>>> = HashMap::new();
        for block in blocks {
            for arm in &block.arms {
                forced.extend(self.flatten_reads(&arm.condition));
                for path in graph.reachable_from(&arm.value_reads) {
                    union.insert(path.clone());
                    guards.entry(path).or_default().push(arm.condition.clone());
                }
            }
        }

        let common_set: HashSet<Arc<str>> = r_all
            .iter()
            .filter(|p| !union.contains(*p))
            .cloned()
            .chain(forced)
            .collect();
        let conditional_set: HashSet<Arc<str>> = union.difference(&common_set).cloned().collect();

        let guard_for = |path: &str| -> Option<Arc<str>> {
            let mut conditions: Vec<&Arc<str>> = guards
                .iter()
                .filter(|(p, _)| PathPrefix::extends(path, p) || PathPrefix::extends(p, path))
                .flat_map(|(_, c)| c.iter())
                .collect();
            conditions.sort();
            conditions.dedup();
            (!conditions.is_empty()).then(|| {
                Arc::from(
                    conditions
                        .iter()
                        .map(|c| c.as_ref())
                        .collect::<Vec<&str>>()
                        .join(" or "),
                )
            })
        };

        ConditionalSchema::Flat {
            common: Self::group_for(&common_set, inputs, outputs, |_| None),
            conditional: Self::group_for(&conditional_set, inputs, outputs, guard_for),
        }
    }

    fn intersection(cones: &[HashSet<Arc<str>>]) -> HashSet<Arc<str>> {
        let Some((first, rest)) = cones.split_first() else {
            return HashSet::new();
        };
        first
            .iter()
            .filter(|p| rest.iter().all(|c| c.contains(*p)))
            .cloned()
            .collect()
    }

    fn group_for(
        set: &HashSet<Arc<str>>,
        inputs: &[InputProperty],
        outputs: &[OutputProperty],
        guard: impl Fn(&str) -> Option<Arc<str>>,
    ) -> SchemaGroup {
        let in_set = |path: &str| {
            set.iter()
                .any(|p| PathPrefix::extends(path, p) || PathPrefix::extends(p, path))
        };
        SchemaGroup {
            inputs: inputs
                .iter()
                .filter(|i| in_set(&i.path))
                .map(|i| GuardedProperty {
                    path: i.path.clone(),
                    resolved_type: i.resolved_type.shallow_clone(),
                    required_when: guard(&i.path),
                })
                .collect(),
            outputs: outputs
                .iter()
                .filter(|o| in_set(&o.path))
                .map(|o| GuardedProperty {
                    path: o.path.clone(),
                    resolved_type: o.resolved_type.shallow_clone(),
                    required_when: guard(&o.path),
                })
                .collect(),
        }
    }
}
