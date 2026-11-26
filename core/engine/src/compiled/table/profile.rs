use crate::policy::blocks::DecisionTableIr;
use ahash::HashMap;
use std::sync::Arc;
use zen_expression::typed::{Hit as Typed, TableRule, TableSpec};
use zen_types::decision::{DecisionTableContent, DecisionTableHitPolicy};
use zen_types::path::DotPath;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Resolve {
    Row,
    Column,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Rows {
    Objects,
    Fields,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Empty {
    Absent,
    Fill,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Errors {
    Lenient,
    Strict,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Profile {
    pub resolve: Resolve,
    pub rows: Rows,
    pub empty: Empty,
    pub errors: Errors,
    pub trim: bool,
}

impl Profile {
    pub const GRAPH: Profile = Profile {
        resolve: Resolve::Row,
        rows: Rows::Objects,
        empty: Empty::Absent,
        errors: Errors::Lenient,
        trim: true,
    };

    pub const POLICY: Profile = Profile {
        resolve: Resolve::Column,
        rows: Rows::Fields,
        empty: Empty::Fill,
        errors: Errors::Strict,
        trim: true,
    };

    pub fn strict(&self) -> bool {
        self.errors == Errors::Strict
    }
}

type Rule = HashMap<Arc<str>, Arc<str>>;

pub(crate) struct InputIr<'t> {
    pub id: &'t Arc<str>,
    pub field: Option<&'t str>,
}

pub(crate) struct OutputIr<'t> {
    pub id: &'t Arc<str>,
    pub path: &'t str,
    pub collect: bool,
}

pub(crate) struct TableIr<'t> {
    pub profile: Profile,
    pub collect: bool,
    pub inputs: Vec<InputIr<'t>>,
    pub outputs: Vec<OutputIr<'t>>,
    pub rules: Vec<&'t Rule>,
}

impl<'t> TableIr<'t> {
    const FILLED: usize = 64;

    pub fn graph(content: &'t DecisionTableContent) -> Self {
        let mut inputs: Vec<InputIr<'t>> = Vec::with_capacity(content.inputs.len());
        for input in content.inputs.iter() {
            let field = match input.field.as_deref() {
                Some(field) if field.trim() == "$" => {
                    Self::previous(content, &inputs, &input.id).or(Some(field))
                }
                field => field,
            };
            inputs.push(InputIr {
                id: &input.id,
                field,
            });
        }
        Self {
            profile: Profile::GRAPH,
            collect: matches!(content.hit_policy, DecisionTableHitPolicy::Collect),
            inputs,
            outputs: content
                .outputs
                .iter()
                .map(|output| {
                    let (path, collect) = output.write_path();
                    OutputIr {
                        id: &output.id,
                        path,
                        collect,
                    }
                })
                .collect(),
            rules: content.rules.iter().collect(),
        }
    }

    fn previous(
        content: &'t DecisionTableContent,
        before: &[InputIr<'t>],
        id: &Arc<str>,
    ) -> Option<&'t str> {
        let earlier = before
            .iter()
            .rev()
            .find(|input| input.field.is_some_and(|f| !f.trim().is_empty()))?;
        let field = earlier.field?;
        if field.trim() == "$" || field.contains("rand") {
            return None;
        }
        let filled = |rule: &Rule, id: &Arc<str>| rule.get(id).is_some_and(|cell| !cell.is_empty());
        content
            .rules
            .iter()
            .all(|rule| !filled(rule, id) || filled(rule, earlier.id))
            .then_some(field)
    }

    pub fn policy(ir: &'t DecisionTableIr) -> Self {
        Self {
            profile: Profile::POLICY,
            collect: ir.outputs.iter().any(|o| !o.field.is_empty())
                && ir
                    .outputs
                    .iter()
                    .filter(|o| !o.field.is_empty())
                    .all(|o| o.collect),
            inputs: ir
                .inputs
                .iter()
                .map(|input| InputIr {
                    id: &input.id,
                    field: input.field.as_deref(),
                })
                .collect(),
            outputs: ir
                .outputs
                .iter()
                .map(|o| OutputIr {
                    id: &o.id,
                    path: &o.field,
                    collect: o.collect,
                })
                .collect(),
            rules: ir.rules.iter().collect(),
        }
    }

    pub fn cell(&self, rule: &'t Rule, id: &str) -> Option<&'t str> {
        rule.get(id)
            .map(Arc::as_ref)
            .filter(|cell| !cell.is_empty())
    }

    pub fn cells(&self, id: &str) -> Vec<Option<&'t str>> {
        self.rules.iter().map(|rule| self.cell(rule, id)).collect()
    }

    pub fn used(&self, id: &str) -> bool {
        self.rules.iter().any(|rule| self.cell(rule, id).is_some())
    }

    pub fn written(&self) -> impl Iterator<Item = &OutputIr<'t>> {
        self.outputs.iter().filter(|o| !o.path.is_empty())
    }

    pub fn filled(&self) -> bool {
        self.typable()
            && self.written().all(|o| {
                self.rules
                    .iter()
                    .all(|rule| self.cell(rule, o.id).is_some())
            })
    }

    pub fn typable(&self) -> bool {
        self.rules.len() <= Self::FILLED
            && self.written().next().is_some()
            && self.written().all(|o| !o.collect)
            && self.inputs.iter().all(|input| {
                input.field.is_some_and(|f| !f.trim().is_empty()) || !self.used(input.id)
            })
    }

    pub fn gathers(&self) -> bool {
        self.profile.strict()
            && self.rules.len() <= Self::FILLED
            && self.written().any(|o| o.collect)
            && self.written().all(|o| {
                self.written()
                    .all(|other| other.path != o.path || other.collect == o.collect)
            })
            && self.inputs.iter().all(|input| {
                input.field.is_some_and(|f| !f.trim().is_empty()) || !self.used(input.id)
            })
    }

    pub fn gathered(&self, computed: impl Fn(usize, usize) -> bool) -> Option<TableSpec> {
        if !self.gathers() {
            return None;
        }
        let inputs: Vec<&InputIr> = self.inputs.iter().filter(|input| self.used(input.id)).collect();
        let rules = self
            .rules
            .iter()
            .enumerate()
            .map(|(at, rule)| TableRule {
                cells: inputs
                    .iter()
                    .map(|input| self.cell(rule, input.id).map(str::to_string))
                    .collect(),
                values: self
                    .written()
                    .enumerate()
                    .map(|(column, o)| {
                        self.cell(rule, o.id)
                            .filter(|_| computed(at, column))
                            .map(str::to_string)
                    })
                    .collect(),
            })
            .collect();
        Some(TableSpec {
            inputs: inputs.iter().map(|input| input.field.map(str::to_string)).collect(),
            outputs: Vec::new(),
            rules,
            hit: Typed::Strict,
            selector: false,
            collect: true,
        })
    }

    pub fn spec(&self) -> Option<TableSpec> {
        if self.collect || self.written().any(|o| o.collect) {
            return None;
        }
        self.specified(false)
    }

    pub fn collected(&self) -> Option<TableSpec> {
        if !(self.collect || self.written().any(|o| o.collect)) || self.profile.strict() {
            return None;
        }
        self.specified(true)
    }

    fn specified(&self, collect: bool) -> Option<TableSpec> {
        let strict = self.profile.strict();
        let mut paths: Vec<&str> = Vec::new();
        for output in self.written() {
            if strict || !paths.contains(&output.path) {
                paths.push(output.path);
            }
        }
        let inputs: Vec<&InputIr> = self
            .inputs
            .iter()
            .filter(|input| !strict || self.used(input.id))
            .collect();
        match strict {
            true if !self.typable() => return None,
            true => {}
            false => {
                let nested = paths
                    .iter()
                    .any(|a| paths.iter().any(|b| DotPath::under(b, a)));
                let plain = paths.iter().all(|path| {
                    path.split('.')
                        .all(|segment| !segment.is_empty() && !segment.contains(['$', '[', ']']))
                });
                let blank = self
                    .inputs
                    .iter()
                    .any(|input| input.field.is_some_and(|field| field.trim().is_empty()))
                    || self.rules.iter().any(|rule| {
                        rule.values()
                            .any(|cell| !cell.is_empty() && cell.trim().is_empty())
                    });
                if nested || !plain || blank {
                    return None;
                }
            }
        }
        let rules = self
            .rules
            .iter()
            .map(|rule| TableRule {
                cells: inputs
                    .iter()
                    .map(|input| self.cell(rule, input.id).map(str::to_string))
                    .collect(),
                values: match strict {
                    _ if collect => Vec::new(),
                    true => self
                        .written()
                        .map(|o| self.cell(rule, o.id).map(str::to_string))
                        .collect(),
                    false => paths
                        .iter()
                        .map(|path| {
                            self.written()
                                .filter(|o| o.path == *path)
                                .filter_map(|o| self.cell(rule, o.id))
                                .last()
                                .map(str::to_string)
                        })
                        .collect(),
                },
            })
            .collect();
        Some(TableSpec {
            inputs: inputs
                .iter()
                .map(|input| input.field.map(str::to_string))
                .collect(),
            outputs: match collect {
                true => Vec::new(),
                false => paths.iter().map(|path| path.to_string()).collect(),
            },
            rules,
            hit: match strict {
                true => Typed::Strict,
                false => Typed::Lenient,
            },
            selector: !strict && !collect,
            collect,
        })
    }
}
