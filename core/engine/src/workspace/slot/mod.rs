mod facts;
mod graph;
mod policy;
pub(crate) mod utf16;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt};
use serde::Serialize;
use zen_expression::intellisense::IntelliSense;
use zen_expression::slot::{EnumTable, LabelResolver, LiteralFact, Slot, SlotResult};
use zen_expression::variable::VariableType;

use crate::policy::ir::DictionaryIr;
use crate::workspace::db::{Db, Unit};
use crate::workspace::graph::GraphAnalysis;
use crate::workspace::types::{Cursor, CursorTarget, ExpressionKind};

pub use facts::ExpressionFacts;
pub use zen_expression::slot::SlotRole;

/// Slot at the caret plus literal facts for the live text; `pos` and spans are UTF-16 code units.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotResponse {
    pub kind: ExpressionKind,
    pub role: SlotRole,
    pub subject_type: Option<VariableType>,
    pub expected_type: Option<VariableType>,
    pub slot: Slot,
    pub literals: Vec<LiteralFact>,
    pub enums: Vec<EnumTable>,
}

impl SlotResponse {
    pub fn compute(is: &mut IntelliSense, scope: &CursorScope, text: &str, pos: u32) -> Self {
        let unary = matches!(scope.kind, ExpressionKind::Unary);
        let pos = utf16::utf16_to_byte(text, pos) as u32;
        let SlotResult {
            mut slot,
            literals,
            enums,
            ..
        } = is.slot(
            text,
            pos,
            unary,
            scope.role,
            &scope.scope,
            scope.expected.as_ref(),
        );
        slot.replace_span = utf16::utf16_span(text, slot.replace_span);
        Self {
            kind: scope.kind,
            role: scope.role,
            subject_type: scope.subject_type(),
            expected_type: scope.expected.as_ref().map(VariableType::shallow_clone),
            slot,
            literals: literals.into_iter().map(|f| fact_utf16(text, f)).collect(),
            enums,
        }
    }
}

pub(crate) fn fact_utf16(text: &str, fact: LiteralFact) -> LiteralFact {
    match fact {
        LiteralFact::Enum {
            span,
            value,
            name,
            label,
            valid,
            enum_index,
        } => LiteralFact::Enum {
            span: utf16::utf16_span(text, span),
            value,
            name,
            label,
            valid,
            enum_index,
        },
        LiteralFact::Date { span, arg } => LiteralFact::Date {
            span: utf16::utf16_span(text, span),
            arg,
        },
        LiteralFact::Bool { span, value } => LiteralFact::Bool {
            span: utf16::utf16_span(text, span),
            value,
        },
    }
}

type LabelMap = HashMap<Arc<str>, HashMap<Arc<str>, Arc<str>>>;

enum LabelKey {
    Unit(Arc<Unit>),
    Graph(Arc<GraphAnalysis>),
}

impl LabelKey {
    fn same(&self, other: &LabelKey) -> bool {
        match (self, other) {
            (LabelKey::Unit(a), LabelKey::Unit(b)) => Arc::ptr_eq(a, b),
            (LabelKey::Graph(a), LabelKey::Graph(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// Dictionary labels per policy, keyed by the unit or graph analysis identity.
#[derive(Default)]
pub(crate) struct LabelCache {
    entries: RefCell<HashMap<Arc<str>, (LabelKey, Option<Arc<LabelMap>>)>>,
}

fn label_map(
    dictionaries: impl Iterator<Item = (Arc<str>, Arc<DictionaryIr>)>,
) -> Option<Arc<LabelMap>> {
    let mut labels = LabelMap::new();
    for (name, dict) in dictionaries {
        let entries: HashMap<Arc<str>, Arc<str>> = dict
            .entries
            .iter()
            .filter(|e| !e.label.is_empty())
            .map(|e| (e.value.clone(), e.label.clone()))
            .collect();
        if !entries.is_empty() {
            labels.insert(name, entries);
        }
    }
    (!labels.is_empty()).then(|| Arc::new(labels))
}

impl Db {
    pub fn slot(&self, cursor: &Cursor, text: &str) -> Option<SlotResponse> {
        let scope = self.cursor_scope(cursor)?;
        let labels = self.label_resolver(&cursor.policy_path);
        let intellisense = self.cursor_intellisense(cursor);
        let mut is = intellisense.borrow_mut();
        is.set_labels(labels);
        let response = SlotResponse::compute(&mut is, &scope, text, cursor.pos);
        is.set_labels(None);
        Some(response)
    }

    pub(crate) fn label_resolver(&self, policy: &str) -> Option<LabelResolver> {
        let key = if self.is_graph(policy) {
            let path: Arc<str> = Arc::from(policy);
            LabelKey::Graph(self.graph_analysis(&path)?)
        } else {
            LabelKey::Unit(self.unit(policy))
        };
        let cached = self
            .labels
            .entries
            .borrow()
            .get(policy)
            .filter(|(cached, _)| cached.same(&key))
            .map(|(_, map)| map.clone());
        let map = match cached {
            Some(map) => map,
            None => {
                let map = match &key {
                    LabelKey::Graph(_) => label_map(
                        self.graph_dictionary_blocks(&self.graph_imports(policy))
                            .into_iter()
                            .map(|entry| (entry.ir.name.clone(), entry.ir)),
                    ),
                    LabelKey::Unit(unit) => label_map(
                        unit.dictionaries
                            .iter()
                            .map(|(name, dict)| (name.clone(), dict.clone())),
                    ),
                };
                self.labels
                    .entries
                    .borrow_mut()
                    .insert(Arc::from(policy), (key, map.clone()));
                map
            }
        }?;
        Some(Rc::new(move |name: &str, value: &str| {
            map.get(name)?.get(value).map(|l| l.to_string())
        }))
    }
}

#[derive(Debug, Clone)]
pub struct CursorScope {
    pub kind: ExpressionKind,
    pub role: SlotRole,
    pub scope: VariableType,
    pub expected: Option<VariableType>,
}

impl CursorScope {
    fn unary(scope: VariableType) -> Self {
        Self {
            kind: ExpressionKind::Unary,
            role: SlotRole::Unary,
            scope,
            expected: None,
        }
    }

    fn condition(scope: VariableType) -> Self {
        Self {
            kind: ExpressionKind::Standard,
            role: SlotRole::Condition,
            scope,
            expected: Some(VariableType::Bool),
        }
    }

    fn value(scope: VariableType, expected: Option<VariableType>) -> Self {
        Self {
            kind: ExpressionKind::Standard,
            role: SlotRole::Value,
            scope,
            expected,
        }
    }

    fn path(scope: VariableType) -> Self {
        Self {
            kind: ExpressionKind::Standard,
            role: SlotRole::Path,
            scope,
            expected: None,
        }
    }

    pub fn subject_type(&self) -> Option<VariableType> {
        match self.kind {
            ExpressionKind::Unary => Some(self.scope.get("$")),
            ExpressionKind::Standard => self.expected.as_ref().map(VariableType::shallow_clone),
        }
    }
}

impl Db {
    pub fn cursor_scope(&self, cursor: &Cursor) -> Option<CursorScope> {
        if matches!(
            cursor.target,
            CursorTarget::DataModelName | CursorTarget::DataModelProperty { .. }
        ) {
            return None;
        }
        if self.is_graph(&cursor.policy_path) {
            graph::graph_scope(self, cursor)
        } else {
            policy::policy_scope(self, cursor)
        }
    }
}

fn known_type(resolved: VariableType) -> Option<VariableType> {
    let (base, _) = resolved.unwrap_nullable();
    match base {
        VariableType::Any | VariableType::Null => None,
        other => Some(other.shallow_clone()),
    }
}

fn literal_union(merged: VariableType) -> Option<VariableType> {
    let (base, _) = merged.unwrap_nullable();
    match base {
        VariableType::Enum(..)
        | VariableType::Const(_)
        | VariableType::Bool
        | VariableType::Number
        | VariableType::Date => Some(base.shallow_clone()),
        _ => None,
    }
}
