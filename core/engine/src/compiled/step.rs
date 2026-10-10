use crate::decision_graph::graph::DecisionGraph;
use crate::decision_graph::schema_dict;
use crate::model::{DecisionNodeKind, GraphContent};
use crate::compiled::schedule::Replay;
use std::sync::{Arc, OnceLock};
use crate::compiled::table::Table;
use zen_expression::lane::{LaneProgram, Programs, SourceInfo};
use zen_expression::ExpressionKind;
use zen_types::decision::{
    DecisionNode, DecisionTableContent, ExpressionNodeContent, SwitchStatementHitPolicy,
    TransformAttributes, TransformExecutionMode,
};
use crate::compiled::{CompiledGraph, Refusal, Invalid};

pub(super) enum Body {
    Expression {
        program: Box<LaneProgram>,
        empty: Box<[bool]>,
        entries: Box<[(Arc<str>, Arc<str>)]>,
        fragments: Box<[Arc<str>]>,
        overlapping: bool,
        walked: bool,
        keys: Arc<[Arc<str>]>,
    },
    Table(Box<Table>),
}

pub(super) enum Hit {
    Uniform(bool),
    Rows(Vec<u64>),
}

pub(super) struct Transform {
    pub(super) input_field: Option<(LaneProgram, Arc<str>)>,
    pub(super) base: Option<Arc<str>>,
    pub(super) looped: bool,
    pub(super) output_path: Option<Arc<str>>,
    pub(super) pass_through: bool,
}

pub(super) enum Kind {
    Input,
    Output,
    Switch {
        first: bool,
        conditions: Box<[(Arc<str>, Condition)]>,
    },
    Node {
        body: Body,
        transform: Box<Transform>,
    },
    Host,
}

pub(super) enum Condition {
    Always,
    Never,
    Program(Box<LaneProgram>),
}

pub(super) struct Step {
    pub(super) node: Arc<DecisionNode>,
    pub(super) kind: Kind,
    pub(super) nodes: bool,
    pub(super) statics: Option<Vec<StaticRead>>,
    pub(super) root: bool,
    pub(super) schema: bool,
    pub(super) sites: Vec<Option<Arc<str>>>,
    pub(super) dynamic: bool,
    eager: bool,
    programs: OnceLock<Arc<Programs>>,
}

pub(super) type StaticRead = (Arc<str>, Arc<str>, Arc<str>);

impl Step {
    pub(super) fn shared_programs(&self) -> Option<&Arc<Programs>> {
        self.eager.then(|| self.programs())
    }

    pub(super) fn programs(&self) -> &Arc<Programs> {
        self.programs.get_or_init(|| {
            let mut sources: Vec<(&str, ExpressionKind)> = Vec::new();
            match &self.node.kind {
                DecisionNodeKind::ExpressionNode { content } => {
                    sources.extend(
                        content
                            .expressions
                            .iter()
                            .filter(|e| !e.key.is_empty() && !e.value.is_empty())
                            .map(|e| (e.value.as_ref(), ExpressionKind::Standard)),
                    );
                    sources.extend(content.transform_attributes.input_field.iter().map(|f| (f.as_ref(), ExpressionKind::Standard)));
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    for rule in content.rules.iter() {
                        for input in content.inputs.iter() {
                            if let Some(cell) = rule.get(&input.id).filter(|c| !c.is_empty()) {
                                let kind = match input.field.is_some() {
                                    true => ExpressionKind::Unary,
                                    false => ExpressionKind::Standard,
                                };
                                sources.push((cell.as_ref(), kind));
                            }
                        }
                        for output in content.outputs.iter() {
                            if let Some(cell) = rule.get(&output.id).filter(|c| !c.is_empty()) {
                                sources.push((cell.as_ref(), ExpressionKind::Standard));
                            }
                        }
                    }
                    sources.extend(content.inputs.iter().filter_map(|i| i.field.as_deref()).map(|f| (f, ExpressionKind::Standard)));
                    sources.extend(content.transform_attributes.input_field.iter().map(|f| (f.as_ref(), ExpressionKind::Standard)));
                }
                DecisionNodeKind::SwitchNode { content } => {
                    sources.extend(
                        content
                            .statements
                            .iter()
                            .filter(|s| !s.condition.is_empty())
                            .map(|s| (s.condition.as_ref(), ExpressionKind::Standard)),
                    );
                }
                _ => {}
            }
            Arc::new(Programs::compile(sources))
        })
    }
}

impl CompiledGraph {
    pub(super) fn compile(content: &GraphContent, eager: bool) -> Result<Self, Refusal> {
        let graph = DecisionGraph::build_graph(content)
            .map_err(|e| Refusal::Invalid(Arc::from(e.to_string()), Invalid::Build(e)))?;
        DecisionGraph::validate_graph(&graph)
            .map_err(|e| Refusal::Invalid(Arc::from("graph fails validation"), Invalid::Validation(e)))?;

        let mut dictionaries = false;
        let mut steps: Vec<Option<Step>> = Vec::new();
        for nid in graph.node_indices() {
            let node = graph[nid].clone();
            let step = Self::step(&node, &mut dictionaries, eager);
            let index = nid.index();
            if steps.len() <= index {
                steps.resize_with(index + 1, || None);
            }
            steps[index] = Some(step);
        }

        let root = Arc::new(Replay::new(&graph).segment(&[]));
        Ok(Self {
            graph,
            steps,
            root,
            dictionaries,
            plan: OnceLock::new(),
        })
    }

    fn transform(attributes: &TransformAttributes) -> Result<(Transform, Vec<Arc<str>>), String> {
        let input_field = attributes
            .input_field
            .as_ref()
            .map(|f| {
                LaneProgram::standard(f)
                    .map(|p| (p, f.clone()))
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        let looped = matches!(attributes.execution_mode, TransformExecutionMode::Loop);
        let base = input_field
            .as_ref()
            .filter(|_| !looped)
            .and_then(|(program, _)| program.path().map(Arc::from));
        Ok((
            Transform {
                input_field,
                base,
                looped,
                output_path: attributes.output_path.clone(),
                pass_through: attributes.pass_through,
            },
            attributes.input_field.iter().cloned().collect(),
        ))
    }

    fn expression_kind(content: &ExpressionNodeContent, sources: &mut Vec<Arc<str>>) -> Result<Kind, String> {
        let entries: Box<[(Arc<str>, Arc<str>)]> = content
            .expressions
            .iter()
            .filter(|e| !e.key.is_empty() && !e.value.is_empty())
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();
        sources.extend(entries.iter().map(|(_, v)| v.clone()));
        let opaque = entries.iter().any(|(_, v)| {
            v.match_indices('$').any(|(at, _)| {
                let rest = &v[at + 1..];
                !rest.starts_with("nodes") && !rest.starts_with('.') && !rest.starts_with('{')
            })
        });
        let overlapping = entries.iter().any(|(key, _)| key.split('.').any(str::is_empty))
            || entries.iter().enumerate().any(|(i, (a, _))| {
            entries.iter().skip(i + 1).any(|(b, _)| {
                a == b
                    || a.strip_prefix(b.as_ref()).is_some_and(|r| r.starts_with('.'))
                    || b.strip_prefix(a.as_ref()).is_some_and(|r| r.starts_with('.'))
            })
        });
        let walked = overlapping && entries.iter().any(|(_, source)| source.contains('$'));
        let mut expanded: Vec<(Arc<str>, Arc<str>, Arc<str>)> = Vec::with_capacity(entries.len());
        for (key, source) in entries.iter() {
            let read = format!("$.{}", key.split('.').next().unwrap_or_default());
            let flat = !opaque && !overlapping && !entries.iter().any(|(_, v)| v.contains(read.as_str()));
            match flat {
                true => Self::expand(key, source, source, &mut expanded),
                false => expanded.push((key.clone(), source.clone(), source.clone())),
            }
        }
        let refs: Vec<(&str, &str)> = expanded.iter().map(|(k, _, v)| (k.as_ref(), v.as_ref())).collect();
        let program = LaneProgram::compile_many(&refs, true).map_err(|e| e.to_string())?;
        let empty: Box<[bool]> = expanded.iter().map(|(_, _, fragment)| fragment.trim() == "{}").collect();
        let fragments: Box<[Arc<str>]> = expanded.iter().map(|(_, _, fragment)| fragment.clone()).collect();
        let entries: Box<[(Arc<str>, Arc<str>)]> =
            expanded.into_iter().map(|(key, source, _)| (key, source)).collect();
        let (transform, extra) = Self::transform(&content.transform_attributes)?;
        sources.extend(extra);
        Ok(Kind::Node {
            transform: Box::new(transform),
            body: Body::Expression {
                program: Box::new(program),
                keys: entries.iter().map(|(k, _)| k.clone()).collect(),
                entries,
                fragments,
                empty,
                overlapping,
                walked,
            },
        })
    }

    fn table_kind(
        content: &DecisionTableContent,
        sources: &mut Vec<Arc<str>>,
        cells: &mut Vec<Arc<str>>,
    ) -> Result<Kind, String> {
        for rule in content.rules.iter() {
            for input in content.inputs.iter() {
                if let Some(cell) = rule.get(&input.id) {
                    match input.field.is_some() {
                        true => cells.push(cell.clone()),
                        false => sources.push(cell.clone()),
                    }
                }
            }
            for output in content.outputs.iter() {
                sources.extend(rule.get(&output.id).cloned());
            }
        }
        sources.extend(content.inputs.iter().filter_map(|i| i.field.clone()));
        let table = Table::compile(content)?;
        let (transform, extra) = Self::transform(&content.transform_attributes)?;
        sources.extend(extra);
        Ok(Kind::Node {
            body: Body::Table(Box::new(table)),
            transform: Box::new(transform),
        })
    }

    fn step(node: &Arc<DecisionNode>, dictionaries: &mut bool, eager: bool) -> Step {
        let mut sources: Vec<Arc<str>> = Vec::new();
        let mut cells: Vec<Arc<str>> = Vec::new();
        let kind = match &node.kind {
            DecisionNodeKind::InputNode { content } => {
                *dictionaries |= content
                    .schema
                    .as_deref()
                    .is_some_and(schema_dict::schema_references_dictionary);
                Kind::Input
            }
            DecisionNodeKind::OutputNode { content } => {
                *dictionaries |= content
                    .schema
                    .as_deref()
                    .is_some_and(schema_dict::schema_references_dictionary);
                Kind::Output
            }
            DecisionNodeKind::SwitchNode { content } => {
                let conditions = content
                    .statements
                    .iter()
                    .map(|s| {
                        sources.push(s.condition.clone());
                        let condition = match s.condition.is_empty() {
                            true => Condition::Always,
                            false => LaneProgram::standard(&s.condition)
                                .map(|p| Condition::Program(Box::new(p)))
                                .unwrap_or(Condition::Never),
                        };
                        (s.id.clone(), condition)
                    })
                    .collect();
                Kind::Switch {
                    first: matches!(content.hit_policy, SwitchStatementHitPolicy::First),
                    conditions,
                }
            }
            DecisionNodeKind::ExpressionNode { .. } | DecisionNodeKind::DecisionTableNode { .. } if !eager => Kind::Host,
            DecisionNodeKind::ExpressionNode { content } => {
                Self::expression_kind(content, &mut sources).unwrap_or(Kind::Host)
            }
            DecisionNodeKind::DecisionTableNode { content } => {
                Self::table_kind(content, &mut sources, &mut cells).unwrap_or(Kind::Host)
            }
            DecisionNodeKind::FunctionNode { .. }
            | DecisionNodeKind::DecisionNode { .. }
            | DecisionNodeKind::CustomNode { .. } => Kind::Host,
        };
        let mut kind = kind;
        if let Kind::Node { body, transform } = &mut kind {
            if let Some(base) = transform.base.clone() {
                let mappable = match &*body {
                    Body::Expression { program, .. } => program
                        .program()
                        .site_keys
                        .iter()
                        .flatten()
                        .all(|key| Self::rebased(&base, key).is_some()),
                    Body::Table(_) => false,
                };
                if !mappable {
                    transform.base = None;
                }
            }
        }
        let mut programs: Vec<&LaneProgram> = Vec::new();
        match &kind {
            Kind::Switch { conditions, .. } => programs.extend(conditions.iter().filter_map(|(_, c)| match c {
                Condition::Program(p) => Some(p.as_ref()),
                _ => None,
            })),
            Kind::Node { body, transform } => {
                match body {
                    Body::Expression { program, .. } => programs.push(program),
                    Body::Table(table) => programs.extend(table.programs()),
                }
                programs.extend(transform.input_field.iter().map(|(p, _)| p));
            }
            Kind::Input | Kind::Output | Kind::Host => {}
        }
        let base = match &kind {
            Kind::Node { transform, .. } => transform.base.clone(),
            _ => None,
        };
        let input_sites = match &kind {
            Kind::Node { transform, .. } => transform.input_field.iter().count(),
            _ => 0,
        };
        let body_programs = programs.len() - input_sites;
        let programs = match base {
            Some(_) => &programs[..body_programs],
            None => &programs[..],
        };
        let sites: Vec<Option<Arc<str>>> = programs
            .iter()
            .enumerate()
            .flat_map(|(index, p)| {
                let base = base.as_deref().filter(|_| index < body_programs);
                p.program().site_keys.iter().map(move |k| {
                    k.as_deref().map(|key| match base {
                        Some(base) => Self::rebased(base, key).unwrap_or_else(|| Arc::from(key)),
                        None => Arc::from(key),
                    })
                })
            })
            .collect();
        let dynamic = programs
            .iter()
            .any(|p| p.program().writes_env || p.program().chain || p.program().opaque());
        let schema = match &node.kind {
            DecisionNodeKind::InputNode { content } => content.schema.is_some(),
            DecisionNodeKind::OutputNode { content } => content.schema.is_some(),
            _ => false,
        };
        Step {
            node: node.clone(),
            eager,
            programs: OnceLock::new(),
            statics: match &kind {
                Kind::Host => None,
                Kind::Node { body: Body::Table(_), .. } => None,
                _ if sources.iter().any(|s| SourceInfo::reads_root(s)) => None,
                _ => Self::statics(&sites),
            },
            nodes: matches!(kind, Kind::Host)
                || sources.iter().any(|s| SourceInfo::reads_nodes(s))
                || cells.iter().any(|s| SourceInfo::reads_nodes_unary(s)),
            root: match &kind {
                Kind::Node {
                    body: Body::Expression { program, .. },
                    transform,
                } => {
                    program.program().keys.iter().any(|k| k == "$" || k.starts_with("$."))
                        || transform.input_field.as_ref().is_some_and(|(_, source)| SourceInfo::reads_root(source))
                }
                _ => {
                    sources.iter().any(|s| SourceInfo::reads_root(s))
                        || cells.iter().any(|s| SourceInfo::reads_root_unary(s))
                }
            },
            schema,
            sites,
            dynamic,
            kind,
        }
    }

    fn expand(key: &Arc<str>, origin: &Arc<str>, source: &str, out: &mut Vec<(Arc<str>, Arc<str>, Arc<str>)>) {
        match LaneProgram::object_fields(source).filter(|fields| !fields.is_empty()) {
            Some(fields) => {
                for (field, fragment) in fields.into_iter().rev() {
                    Self::expand(&Arc::from(format!("{key}.{field}")), origin, &fragment, out);
                }
            }
            None => out.push((key.clone(), origin.clone(), Arc::from(source))),
        }
    }

    pub(super) fn rebased(base: &str, key: &str) -> Option<Arc<str>> {
        if key.starts_with("$nodes") {
            return Some(Arc::from(key));
        }
        match key.strip_prefix('$') {
            Some("") => Some(Arc::from(base)),
            Some(rest) if rest.starts_with('.') => Some(Arc::from(format!("{base}{rest}"))),
            Some(_) => None,
            None => Some(Arc::from(format!("{base}.{key}"))),
        }
    }

    fn statics(sites: &[Option<Arc<str>>]) -> Option<Vec<StaticRead>> {
        if sites.iter().any(Option::is_none) {
            return None;
        }
        let mut out: Vec<StaticRead> = Vec::new();
        for key in sites.iter().flatten() {
            if key.as_ref() == "$nodes" {
                return None;
            }
            let Some(rest) = key.strip_prefix("$nodes.") else {
                continue;
            };
            let (name, path) = rest.split_once('.').unwrap_or((rest, ""));
            if !out.iter().any(|(k, _, _)| k == key) {
                out.push((key.clone(), Arc::from(name), Arc::from(path)));
            }
        }
        Some(out)
    }
}
