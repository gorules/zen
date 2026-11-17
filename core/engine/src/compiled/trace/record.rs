use super::batch::Facts;
use super::collect::{Fact, Hit, Level, Record, Slot, Template};
use crate::compiled::leaf::Leaf;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::Variable;

pub(crate) type Results = (Arc<[Arc<str>]>, Vec<Leaf<'static>>);
pub(crate) type Objects = (Arc<[Arc<str>]>, Rc<[Variable]>);
pub(crate) type RuleHits = Vec<SmallVec<[u32; 4]>>;

pub(crate) struct FieldColumn {
    pub(crate) name: Arc<str>,
    pub(crate) values: Vec<Variable>,
    pub(crate) failed: Vec<bool>,
}

#[derive(Default)]
pub(crate) struct KernelFacts {
    first: Option<Vec<Option<u32>>>,
    hits: Option<RuleHits>,
    results: Option<Results>,
    rows: Option<Vec<Fact>>,
    objects: Option<Objects>,
    fields: Option<Vec<FieldColumn>>,
    calls: usize,
}

pub(crate) struct Probe {
    fields: bool,
    facts: RefCell<KernelFacts>,
}

pub(crate) enum NodeShape {
    Hosted,
    Expression {
        keyed: bool,
    },
    Table {
        first: bool,
        derived: bool,
    },
    Loop {
        spans: Option<Rc<[(u32, u32)]>>,
        inner: LoopBody,
    },
}

pub(crate) enum LoopBody {
    Results,
    FirstDerived,
    Other,
}

impl Probe {
    pub(crate) fn new(fields: bool) -> Rc<Self> {
        Rc::new(Self {
            fields,
            facts: RefCell::new(KernelFacts::default()),
        })
    }

    pub(crate) fn wants_fields(&self) -> bool {
        self.fields
    }

    pub(crate) fn record_fields(&self, fields: Vec<FieldColumn>) {
        self.facts.borrow_mut().fields = Some(fields);
    }

    pub(crate) fn record_hits(&self, hits: Option<RuleHits>) {
        let mut facts = self.facts.borrow_mut();
        facts.calls += 1;
        facts.hits = hits;
    }

    pub(crate) fn record_first(&self, rules: Vec<Option<u32>>) {
        let mut facts = self.facts.borrow_mut();
        facts.calls += 1;
        facts.first = Some(rules);
    }

    pub(crate) fn record_results(&self, keys: &Arc<[Arc<str>]>, leaves: &[Leaf<'_>]) {
        let owned: Option<Vec<Leaf<'static>>> = leaves
            .iter()
            .map(|leaf| match leaf {
                Leaf::Typed(array) => Some(Leaf::Typed(array.clone())),
                Leaf::Any(values) => Some(Leaf::Any(values.clone())),
                _ => None,
            })
            .collect();
        let mut facts = self.facts.borrow_mut();
        facts.calls += 1;
        facts.results = owned.map(|leaves| (keys.clone(), leaves));
    }

    pub(crate) fn record_row_results(&self, keys: &Arc<[Arc<str>]>, rows: Vec<Vec<Variable>>) {
        let facts_of = rows
            .into_iter()
            .map(|values| Fact::Results(keys.iter().cloned().zip(values).collect()))
            .collect();
        let mut facts = self.facts.borrow_mut();
        facts.calls += 1;
        facts.rows = Some(facts_of);
    }

    pub(crate) fn record_objects(&self, keys: &Arc<[Arc<str>]>, values: &Rc<[Variable]>) {
        let mut facts = self.facts.borrow_mut();
        facts.calls += 1;
        facts.objects = Some((keys.clone(), values.clone()));
    }

    pub(crate) fn finish(&self) -> KernelFacts {
        std::mem::take(&mut *self.facts.borrow_mut())
    }
}

impl KernelFacts {
    pub(crate) fn facts(
        self,
        template: &Template,
        slot: Slot,
        shape: NodeShape,
        count: usize,
    ) -> Result<Facts, &'static str> {
        if matches!(shape, NodeShape::Hosted) {
            return Err("hosted node");
        }
        if self.calls != 1 {
            return Err("evaluated outside the batch kernels");
        }
        match shape {
            NodeShape::Hosted => Err("hosted node"),
            NodeShape::Loop { spans, inner } => self.looped(spans, inner).ok_or("loop"),
            NodeShape::Expression { keyed: true } if self.rows.is_some() => self
                .rows
                .filter(|rows| rows.len() == count)
                .map(Facts::Rows)
                .ok_or("expression results not captured"),
            NodeShape::Expression { keyed: true } => {
                if let Some(results) = self
                    .results
                    .filter(|(_, leaves)| leaves.iter().all(|leaf| leaf.len() == count))
                {
                    return Ok(Facts::Results(results));
                }
                self.objects
                    .filter(|(_, values)| values.len() == count)
                    .map(|(keys, values)| Facts::Objects(keys, values))
                    .ok_or("expression results not captured")
            }
            NodeShape::Expression { keyed: false } => Err("expression without one result per key"),
            NodeShape::Table { first, derived } => {
                self.table(template, slot, first, derived, count)
            }
        }
    }

    fn table(
        self,
        template: &Template,
        slot: Slot,
        first: bool,
        derived: bool,
        count: usize,
    ) -> Result<Facts, &'static str> {
        if first {
            let first = self
                .first
                .filter(|first| first.len() == count)
                .ok_or("table rules not captured")?;
            if derived {
                return Ok(Facts::First(first));
            }
            let fields = self.fields.as_deref().ok_or("table fields not captured")?;
            let facts = first
                .iter()
                .enumerate()
                .map(|(position, rule)| match rule {
                    None => Some(Fact::None),
                    Some(rule) => {
                        Self::captured_hit(template, slot, *rule, fields, position).map(Fact::Hit)
                    }
                })
                .collect::<Option<Vec<Fact>>>()
                .ok_or("table field references")?;
            return Ok(Facts::Rows(facts));
        }
        let hits = self
            .hits
            .filter(|hits| hits.len() == count)
            .ok_or("collected rules not captured")?;
        let fields = self.fields.as_deref().ok_or("table fields not captured")?;
        let facts = hits
            .iter()
            .enumerate()
            .map(|(position, rules)| {
                rules
                    .iter()
                    .map(|rule| Self::captured_hit(template, slot, *rule, fields, position))
                    .collect::<Option<Vec<Hit>>>()
                    .map(Fact::Hits)
            })
            .collect::<Option<Vec<Fact>>>()
            .ok_or("table field references")?;
        Ok(Facts::Rows(facts))
    }

    fn captured_hit(
        template: &Template,
        slot: Slot,
        rule: u32,
        fields: &[FieldColumn],
        position: usize,
    ) -> Option<Hit> {
        let refs = template.refs_with(slot, rule, |name| {
            let field = fields.iter().find(|f| &*f.name == name)?;
            Some(match field.failed.get(position).copied().unwrap_or(true) {
                true => None,
                false => Some(field.values.get(position)?.clone()),
            })
        })?;
        Some(Hit {
            rule,
            refs: Some(refs),
        })
    }

    fn looped(self, spans: Option<Rc<[(u32, u32)]>>, inner: LoopBody) -> Option<Facts> {
        let spans = spans?;
        let total = spans
            .iter()
            .map(|(_, count)| *count as usize)
            .sum::<usize>();
        if let (LoopBody::Results, Some(rows)) = (&inner, &self.rows) {
            if rows.len() != total {
                return None;
            }
            let mut rows = rows.iter();
            return Some(Facts::Rows(
                spans
                    .iter()
                    .map(|(_, count)| {
                        Fact::Items(rows.by_ref().take(*count as usize).cloned().collect())
                    })
                    .collect(),
            ));
        }
        let inner = match inner {
            LoopBody::Results => Facts::Results(
                self.results
                    .filter(|(_, leaves)| leaves.iter().all(|leaf| leaf.len() == total))?,
            ),
            LoopBody::FirstDerived => {
                Facts::First(self.first.filter(|first| first.len() == total)?)
            }
            LoopBody::Other => return None,
        };
        Some(Facts::Loop(spans, Box::new(inner)))
    }
}

pub(crate) type Recording = Level;

#[allow(clippy::large_enum_variant)]
pub(crate) enum TraceEvent {
    Node {
        node: Slot,
        parents: SmallVec<[Slot; 4]>,
        input: Variable,
        output: Variable,
        ns: u64,
        detail: Fact,
    },
    Switch {
        node: Slot,
        parents: SmallVec<[Slot; 4]>,
        input: Variable,
        ns: u64,
        branches: SmallVec<[u32; 4]>,
    },
}

pub(crate) struct Tracer {
    recording: Recording,
}

impl Tracer {
    pub(crate) fn new(template: &Arc<Template>, capacity: usize) -> Self {
        Self {
            recording: Level::with_capacity(template.clone(), capacity),
        }
    }

    pub(crate) fn record(&mut self, event: TraceEvent) {
        let record = match event {
            TraceEvent::Node {
                node,
                parents,
                input,
                output,
                ns,
                detail,
            } => Record {
                slot: node,
                parents,
                input,
                output,
                ns,
                fact: detail,
            },
            TraceEvent::Switch {
                node,
                parents,
                input,
                ns,
                branches,
            } => Record {
                slot: node,
                parents,
                output: input.clone(),
                input,
                ns,
                fact: Fact::Branches(branches),
            },
        };
        self.recording.push(record);
    }

    pub(crate) fn finish(self) -> Recording {
        self.recording
    }
}

#[derive(Default)]
pub(crate) struct NodeRecorder {
    fact: RefCell<Fact>,
}

impl NodeRecorder {
    pub(crate) fn expression(&self, results: Vec<(Arc<str>, Variable)>) {
        self.set(Fact::Results(results));
    }

    pub(crate) fn table_hit(&self, hit: Option<Hit>) {
        self.set(match hit {
            Some(hit) => Fact::Hit(hit),
            None => Fact::Hits(Vec::new()),
        });
    }

    pub(crate) fn table_hits(&self, hits: Vec<Hit>) {
        self.set(Fact::Hits(hits));
    }

    pub(crate) fn data(&self, data: Variable) {
        self.set(Fact::Data(data));
    }

    pub(crate) fn sub_decision(&self, recording: Recording) {
        self.set(Fact::Sub(Box::new(recording)));
    }

    pub(crate) fn item(&self, data: Option<Variable>) -> Fact {
        match (self.take(), data) {
            (Fact::None, Some(data)) => Fact::Data(data),
            (fact, _) => fact,
        }
    }

    pub(crate) fn items(&self, items: Vec<Fact>) {
        self.set(Fact::Items(items));
    }

    pub(crate) fn take(&self) -> Fact {
        std::mem::take(&mut *self.fact.borrow_mut())
    }

    pub(crate) fn finish(self, data: Option<&Variable>) -> Fact {
        match (self.fact.into_inner(), data) {
            (Fact::None, Some(data)) if !matches!(data, Variable::Null) => Fact::Data(data.clone()),
            (fact, _) => fact,
        }
    }

    fn set(&self, fact: Fact) {
        *self.fact.borrow_mut() = fact;
    }
}
