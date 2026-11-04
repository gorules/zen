use super::graph::cell_field;
use super::value::write_str;
use smallvec::SmallVec;
use std::sync::Arc;
use zen_types::decision::{DecisionNode, DecisionNodeKind};
use zen_types::variable::{Variable, VariableMap};

pub type Slot = u32;

pub struct Template {
    pub(crate) nodes: Vec<Option<NodeT>>,
}

pub(crate) struct NodeT {
    pub(crate) node: Arc<DecisionNode>,
    pub(crate) id: Arc<str>,
    pub(crate) rules: Vec<RuleT>,
    pub(crate) statements: Vec<Arc<str>>,
    pub(crate) head: Option<String>,
    pub(crate) key: String,
    pub(crate) results_at: Option<String>,
    pub(crate) touched: Option<Arc<[Arc<str>]>>,
    pub(crate) scoped: bool,
    pub(crate) items: Option<String>,
    pub(crate) looped: bool,
    pub(crate) at: Option<Arc<str>>,
    pub(crate) items_at: Option<Arc<str>>,
    pub(crate) source: String,
    pub(crate) exprs: Vec<(Arc<str>, Arc<str>)>,
}

pub(crate) struct RuleT {
    pub(crate) id: Option<Arc<str>>,
    pub(crate) cells: Vec<(Arc<str>, Arc<str>)>,
    pub(crate) derive: Option<Vec<Arc<str>>>,
    pub(crate) head: String,
    pub(crate) dollar: bool,
    pub(crate) index: usize,
}

impl RuleT {
    fn build(index: usize, entries: Vec<(Arc<str>, Arc<str>)>) -> Self {
        let mut entries = entries;
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|b, a| {
            if a.0 == b.0 {
                a.1 = b.1.clone();
                true
            } else {
                false
            }
        });
        let id = entries
            .iter()
            .find(|(k, _)| &**k == "_id")
            .map(|(_, v)| v.clone());
        let cells: Vec<(Arc<str>, Arc<str>)> = entries
            .iter()
            .filter(|(k, _)| id.is_none() || &**k != "_id")
            .cloned()
            .collect();
        let mut derive = Some(Vec::new());
        for (k, _) in &entries {
            if &**k == "_id" {
                continue;
            }
            match (cell_field(k), derive.as_mut()) {
                (Some(f), Some(list)) => list.push(Arc::from(f)),
                _ => derive = None,
            }
        }
        let dollar = cells.iter().any(|(k, _)| k.starts_with('$'));
        let mut head = String::with_capacity(32 + cells.len() * 24);
        head.push('{');
        if let Some(id) = &id {
            head.push_str("\"id\":");
            write_str(id, &mut head);
            head.push(',');
        }
        head.push_str("\"cells\":");
        if dollar {
            head.push_str("{\"$literal\":");
        }
        head.push('{');
        for (i, (k, v)) in cells.iter().enumerate() {
            if i > 0 {
                head.push(',');
            }
            write_str(k, &mut head);
            head.push(':');
            write_str(v, &mut head);
        }
        head.push('}');
        if dollar {
            head.push('}');
        }
        head.push_str(",\"index\":");
        head.push_str(&index.to_string());
        Self {
            id,
            cells,
            derive,
            head,
            dollar,
            index,
        }
    }
}

impl NodeT {
    fn new(node: Arc<DecisionNode>) -> Self {
        let mut rules = Vec::new();
        let mut statements = Vec::new();
        match &node.kind {
            DecisionNodeKind::DecisionTableNode { content } => {
                for (index, rule) in content.rules.iter().enumerate() {
                    let mut entries: Vec<(Arc<str>, Arc<str>)> = Vec::new();
                    entries.push((
                        Arc::from("_id"),
                        rule.get("_id").cloned().unwrap_or_else(|| Arc::from("")),
                    ));
                    if let Some(description) = rule.get("_description") {
                        entries.push((Arc::from("_description"), description.clone()));
                    }
                    for input in content.inputs.iter() {
                        let (Some(cell), Some(field)) = (rule.get(&input.id), &input.field) else {
                            continue;
                        };
                        entries.push((Arc::from(format!("{field}[{}]", input.id)), cell.clone()));
                    }
                    rules.push(RuleT::build(index, entries));
                }
            }
            DecisionNodeKind::SwitchNode { content } => {
                statements = content.statements.iter().map(|s| s.id.clone()).collect();
            }
            _ => {}
        }
        let exprs = match &node.kind {
            DecisionNodeKind::ExpressionNode { content } => content
                .expressions
                .iter()
                .filter(|e| !e.key.is_empty() && !e.value.is_empty())
                .map(|e| (e.id.clone(), e.key.clone()))
                .collect(),
            _ => Vec::new(),
        };
        let (kind, attrs) = super::native::Kind::of(&node);
        let head = matches!(
            kind,
            super::native::Kind::Input
                | super::native::Kind::Output
                | super::native::Kind::Table
                | super::native::Kind::Expression
                | super::native::Kind::Switch
        )
        .then(|| {
            let mut head = String::with_capacity(64);
            head.push_str("{\"id\":");
            write_str(&node.id, &mut head);
            if node.name != node.id {
                head.push_str(",\"name\":");
                write_str(&node.name, &mut head);
            }
            head.push_str(",\"type\":\"");
            head.push_str(kind.name());
            head.push_str("\",\"perf\":");
            head
        });
        let mut key = String::with_capacity(node.id.len() + 2);
        write_str(&node.id, &mut key);
        let results_at = attrs
            .and_then(|a| a.output_path.as_deref())
            .map(super::core::dotted)
            .filter(|segs| !segs.is_empty())
            .map(|segs| {
                let mut text = String::from(",\"resultsAt\":");
                write_str(&super::core::to_ptr(&segs), &mut text);
                text
            });
        let touched = Self::touched(&node);
        let mut source = String::from("nodes");
        super::core::escape_seg(&node.id, &mut source);
        source.push_str("/output");
        let at = attrs.and_then(|a| a.output_path.clone());
        let items_at = attrs
            .and_then(|a| a.input_field.clone())
            .filter(|f| super::graph::plain_path(f));
        let scoped = attrs.is_some_and(|a| a.input_field.is_some());
        let looped = attrs.is_some_and(|a| {
            matches!(
                a.execution_mode,
                zen_types::decision::TransformExecutionMode::Loop
            )
        });
        let items = attrs
            .and_then(|a| a.input_field.as_deref())
            .filter(|f| super::graph::plain_path(f))
            .map(|f| {
                let mut text = String::from(",\"items\":");
                write_str(&super::core::dot_to_ptr(f), &mut text);
                text
            });
        Self {
            id: node.id.clone(),
            node,
            rules,
            statements,
            head,
            key,
            results_at,
            touched,
            scoped,
            items,
            looped,
            at,
            items_at,
            source,
            exprs,
        }
    }

    fn touched(node: &DecisionNode) -> Option<Arc<[Arc<str>]>> {
        let (attrs, fields): (&zen_types::decision::TransformAttributes, Vec<&str>) =
            match &node.kind {
                DecisionNodeKind::DecisionTableNode { content }
                    if matches!(
                        content.hit_policy,
                        zen_types::decision::DecisionTableHitPolicy::First
                    ) =>
                {
                    (
                        &content.transform_attributes,
                        content.outputs.iter().map(|o| o.write_path().0).collect(),
                    )
                }
                DecisionNodeKind::ExpressionNode { content } => (
                    &content.transform_attributes,
                    content
                        .expressions
                        .iter()
                        .filter(|e| !e.key.is_empty() && !e.value.is_empty())
                        .map(|e| &*e.key)
                        .collect(),
                ),
                _ => return None,
            };
        let single = matches!(
            attrs.execution_mode,
            zen_types::decision::TransformExecutionMode::Single
        );
        if !attrs.pass_through || (!single && attrs.output_path.is_none()) {
            return None;
        }
        let fields: Vec<&str> = match attrs.output_path.as_deref() {
            Some(path) => vec![path],
            None => fields,
        };
        let mut tops: Vec<Arc<str>> = Vec::with_capacity(fields.len());
        for field in fields {
            let top = field.split('.').next().unwrap_or_default();
            if top.is_empty() || top.starts_with('$') {
                return None;
            }
            if !tops.iter().any(|t| &**t == top) {
                tops.push(Arc::from(top));
            }
        }
        Some(tops.into())
    }
}

impl Template {
    pub fn new<I: IntoIterator<Item = Option<Arc<DecisionNode>>>>(nodes: I) -> Self {
        Self {
            nodes: nodes.into_iter().map(|n| n.map(NodeT::new)).collect(),
        }
    }

    pub(crate) fn node(&self, slot: Slot) -> Option<&NodeT> {
        self.nodes.get(slot as usize)?.as_ref()
    }

    pub fn refs(&self, slot: Slot, rule: u32, input: &Variable) -> Refs {
        let Some(node) = self.node(slot) else {
            return Refs::new();
        };
        let Some(rule) = node.rules.get(rule as usize) else {
            return Refs::new();
        };
        let env = super::native::table_env(&node.node, input);
        rule.cells
            .iter()
            .filter_map(|(k, _)| super::graph::cell_field(k))
            .filter(|f| !f.is_empty())
            .map(|f| (Arc::from(f), super::native::dotted_or_null(&env, f)))
            .collect()
    }

    pub fn refs_with(
        &self,
        slot: Slot,
        rule: u32,
        mut value: impl FnMut(&str) -> Option<Option<Variable>>,
    ) -> Option<Refs> {
        let rule = self.node(slot)?.rules.get(rule as usize)?;
        let mut refs = Refs::new();
        for (k, _) in &rule.cells {
            let Some(field) = super::graph::cell_field(k).filter(|f| !f.is_empty()) else {
                continue;
            };
            if let Some(v) = value(field)? {
                refs.push((Arc::from(field), v));
            }
        }
        Some(refs)
    }

    pub fn touched(&self, slot: Slot) -> Option<&Arc<[Arc<str>]>> {
        self.node(slot)?.touched.as_ref()
    }

    pub fn statement(&self, slot: Slot, id: &str) -> Option<u32> {
        self.node(slot)?
            .statements
            .iter()
            .position(|s| &**s == id)
            .map(|p| p as u32)
    }
}

pub type Refs = SmallVec<[(Arc<str>, Variable); 4]>;

#[derive(Clone)]
pub struct Hit {
    pub rule: u32,
    pub refs: Option<Refs>,
}

#[derive(Clone)]
pub enum Fact {
    None,
    Data(Variable),
    Results(Vec<(Arc<str>, Variable)>),
    Hit(Hit),
    Hits(Vec<Hit>),
    Branches(SmallVec<[u32; 4]>),
    Sub(Box<Level>),
    Items(Vec<Fact>),
}

#[derive(Clone)]
pub struct Record {
    pub slot: Slot,
    pub parents: SmallVec<[Slot; 4]>,
    pub input: Variable,
    pub output: Variable,
    pub ns: u64,
    pub fact: Fact,
}

#[derive(Clone)]
pub struct Level {
    pub template: Arc<Template>,
    pub records: Vec<Record>,
}

impl Level {
    pub fn with_capacity(template: Arc<Template>, capacity: usize) -> Self {
        Self {
            template,
            records: Vec::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, record: Record) {
        self.records.push(record);
    }

    pub fn strip(&mut self, key: &str) {
        for r in &mut self.records {
            r.fact.strip(key);
        }
    }

    pub fn values(&self, visit: &mut dyn FnMut(&Variable)) {
        for r in &self.records {
            visit(&r.input);
            visit(&r.output);
            r.fact.values(visit);
        }
    }

    pub fn values_mut(&mut self, visit: &mut dyn FnMut(&mut Variable)) {
        for r in &mut self.records {
            visit(&mut r.input);
            visit(&mut r.output);
            r.fact.values_mut(visit);
        }
    }
}

impl Fact {
    fn strip(&mut self, key: &str) {
        match self {
            Fact::Results(m) => m.retain(|(k, _)| &**k != key),
            Fact::Hit(h) => h.strip(key),
            Fact::Hits(hs) => hs.iter_mut().for_each(|h| h.strip(key)),
            Fact::Sub(level) => level.strip(key),
            Fact::Items(items) => items.iter_mut().for_each(|f| f.strip(key)),
            Fact::None | Fact::Data(_) | Fact::Branches(_) => {}
        }
    }

    fn values_mut(&mut self, visit: &mut dyn FnMut(&mut Variable)) {
        match self {
            Fact::None | Fact::Branches(_) => {}
            Fact::Data(v) => visit(v),
            Fact::Results(m) => m.iter_mut().for_each(|(_, v)| visit(v)),
            Fact::Hit(h) => h.refs.iter_mut().flatten().for_each(|(_, v)| visit(v)),
            Fact::Hits(hs) => hs
                .iter_mut()
                .for_each(|h| h.refs.iter_mut().flatten().for_each(|(_, v)| visit(v))),
            Fact::Sub(level) => level.values_mut(visit),
            Fact::Items(items) => items.iter_mut().for_each(|f| f.values_mut(visit)),
        }
    }

    fn values(&self, visit: &mut dyn FnMut(&Variable)) {
        match self {
            Fact::None | Fact::Branches(_) => {}
            Fact::Data(v) => visit(v),
            Fact::Results(m) => m.iter().for_each(|(_, v)| visit(v)),
            Fact::Hit(h) => h.refs.iter().flatten().for_each(|(_, v)| visit(v)),
            Fact::Hits(hs) => hs
                .iter()
                .for_each(|h| h.refs.iter().flatten().for_each(|(_, v)| visit(v))),
            Fact::Sub(level) => level.values(visit),
            Fact::Items(items) => items.iter().for_each(|f| f.values(visit)),
        }
    }
}

fn object(entries: Vec<(&str, Variable)>) -> Variable {
    let mut map = VariableMap::with_capacity(entries.len());
    for (k, v) in entries {
        map.insert(k.into(), v);
    }
    Variable::from_object(map)
}

impl Hit {
    fn strip(&mut self, key: &str) {
        if let Some(refs) = self.refs.as_mut() {
            refs.retain(|(k, _)| &**k != key);
        }
    }

    fn legacy(&self, rule: Option<&RuleT>, scope: Option<&Variable>) -> Variable {
        let mut refs = VariableMap::with_capacity(4);
        match (&self.refs, rule) {
            (Some(list), _) => {
                for (k, v) in list {
                    refs.insert((&**k).into(), v.clone());
                }
            }
            (None, Some(rule)) => {
                for (k, v) in super::native::derived(rule, scope) {
                    refs.insert(k.into(), v);
                }
            }
            (None, None) => {}
        }
        let mut cells = VariableMap::with_capacity(4);
        if let Some(rule) = rule {
            if let Some(id) = &rule.id {
                cells.insert("_id".into(), Variable::String((&**id).into()));
            }
            for (k, v) in &rule.cells {
                cells.insert((&**k).into(), Variable::String((&**v).into()));
            }
        }
        object(vec![
            (
                "index",
                Variable::Number(rust_decimal::Decimal::from(self.rule)),
            ),
            ("reference_map", Variable::from_object(refs)),
            ("rule", Variable::from_object(cells)),
        ])
    }
}

impl Fact {
    fn legacy(&self, node: Option<&NodeT>, input: &Variable) -> Variable {
        let rule = |h: &Hit| node.and_then(|n| n.rules.get(h.rule as usize));
        let scope = || node.and_then(|n| super::native::table_scope(&n.node, input, None));
        match self {
            Fact::None => Variable::Null,
            Fact::Data(v) => v.clone(),
            Fact::Results(m) => {
                let mut map = VariableMap::with_capacity(m.len());
                for (k, v) in m {
                    map.insert((&**k).into(), object(vec![("result", v.clone())]));
                }
                Variable::from_object(map)
            }
            Fact::Hit(h) => h.legacy(rule(h), scope().as_ref()),
            Fact::Hits(hs) => {
                let scope = scope();
                Variable::from_array(
                    hs.iter()
                        .map(|h| h.legacy(rule(h), scope.as_ref()))
                        .collect(),
                )
            }
            Fact::Branches(b) => {
                let statements = b
                    .iter()
                    .map(|i| {
                        let id = node
                            .and_then(|n| n.statements.get(*i as usize))
                            .map(|s| Variable::String((&**s).into()))
                            .unwrap_or(Variable::Null);
                        object(vec![("id", id)])
                    })
                    .collect();
                object(vec![("statements", Variable::from_array(statements))])
            }
            Fact::Sub(level) => level.legacy(),
            Fact::Items(items) => Variable::from_array(
                items
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| !matches!(f, Fact::None))
                    .map(|(j, f)| match f {
                        Fact::Hit(h) => {
                            let scope = node
                                .and_then(|n| super::native::table_scope(&n.node, input, Some(j)));
                            h.legacy(rule(h), scope.as_ref())
                        }
                        Fact::Hits(hs) => {
                            let scope = node
                                .and_then(|n| super::native::table_scope(&n.node, input, Some(j)));
                            Variable::from_array(
                                hs.iter()
                                    .map(|h| h.legacy(rule(h), scope.as_ref()))
                                    .collect(),
                            )
                        }
                        other => other.legacy(node, input),
                    })
                    .collect(),
            ),
        }
    }
}

impl Level {
    pub fn legacy(&self) -> Variable {
        let mut map = VariableMap::with_capacity(self.records.len());
        for (order, r) in self.records.iter().enumerate() {
            let node = self.template.node(r.slot);
            let id = node.map(|n| n.id.clone()).unwrap_or_else(|| Arc::from(""));
            let name = node
                .map(|n| n.node.name.clone())
                .unwrap_or_else(|| id.clone());
            let performance = format!("{:.1?}", std::time::Duration::from_nanos(r.ns));
            let entry = object(vec![
                ("id", Variable::String((&*id).into())),
                ("name", Variable::String((&*name).into())),
                ("input", r.input.clone()),
                ("output", r.output.clone()),
                (
                    "order",
                    Variable::Number(rust_decimal::Decimal::from(order)),
                ),
                ("performance", Variable::String(performance.as_str().into())),
                ("traceData", r.fact.legacy(node, &r.input)),
            ]);
            map.insert((&*id).into(), entry);
        }
        Variable::from_object(map)
    }
}

pub enum PolicyFact {
    Expression {
        property: Arc<str>,
        value: Variable,
    },
    Match {
        arms: Vec<(Arc<str>, bool)>,
        matched: Option<Arc<str>>,
        value: Variable,
    },
    Assertion {
        conditions: Vec<(Arc<str>, bool)>,
        result: bool,
    },
    Table {
        rows: Vec<u32>,
        evaluations: Vec<Vec<(Arc<str>, Variable)>>,
        input_pass: Option<String>,
    },
}

impl PolicyFact {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            PolicyFact::Expression { .. } => "expression",
            PolicyFact::Match { .. } => "match",
            PolicyFact::Assertion { .. } => "assertion",
            PolicyFact::Table { .. } => "decisionTable",
        }
    }
}

pub struct PolicyExecution {
    pub block: Arc<str>,
    pub policy: Option<Arc<str>>,
    pub member: Option<Arc<str>>,
    pub fact: PolicyFact,
    pub operands: Vec<(Arc<str>, Variable)>,
    pub writes: Vec<(Arc<str>, Variable)>,
    pub reads: Vec<Arc<str>>,
}

pub struct PolicyLevel {
    pub engine_version: Option<Arc<str>>,
    pub executions: Vec<PolicyExecution>,
    pub properties: Option<Vec<(Arc<str>, Variable)>>,
}

impl PolicyLevel {
    pub(crate) fn simple(&self) -> bool {
        self.executions
            .iter()
            .all(|e| e.operands.is_empty() && e.writes.is_empty() && e.reads.is_empty())
    }
}
