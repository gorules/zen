mod facts;
mod scope;
mod siblings;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt};
use serde::Serialize;
use zen_expression::intellisense::IntelliSense;
use zen_expression::slot::{LabelResolver, Literals, Slot, SlotResult};
use zen_expression::variable::VariableType;

use crate::policy::ir::DictionaryIr;
use crate::workspace::db::{Db, Unit};
use crate::workspace::graph::GraphAnalysis;
use crate::workspace::types::{Cursor, CursorTarget, ExpressionKind, SpanOps};

pub use facts::ExpressionFacts;
pub use zen_expression::slot::SlotRole;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotResponse {
    pub kind: ExpressionKind,
    pub role: SlotRole,
    pub subject_type: Option<VariableType>,
    pub expected_type: Option<VariableType>,
    pub slot: Slot,
    #[serde(flatten)]
    pub literals: Literals,
}

impl SlotResponse {
    pub fn compute(is: &mut IntelliSense, scope: &CursorScope, text: &str, pos: u32) -> Self {
        let pos = SpanOps::byte_offset(text, pos) as u32;
        let SlotResult { mut slot, literals } = is.slot(
            text,
            pos,
            scope.is_unary(),
            scope.role,
            &scope.scope,
            scope.expected.as_ref(),
        );
        slot.replace_span = SpanOps::char_span(text, slot.replace_span);
        Self {
            kind: scope.kind,
            role: scope.role,
            subject_type: scope.subject_type(),
            expected_type: scope.expected.as_ref().map(VariableType::shallow_clone),
            slot,
            literals: literals.map_spans(|span| SpanOps::char_span(text, span)),
        }
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

type LabelEntry = (LabelKey, Option<Arc<LabelMap>>);

#[derive(Default)]
pub(crate) struct LabelCache {
    entries: RefCell<HashMap<Arc<str>, LabelEntry>>,
}

impl LabelCache {
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
                    LabelKey::Graph(_) => LabelCache::label_map(
                        self.graph_dictionary_blocks(&self.graph_imports(policy))
                            .into_iter()
                            .map(|entry| (entry.ir.name.clone(), entry.ir)),
                    ),
                    LabelKey::Unit(unit) => LabelCache::label_map(
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

    pub fn is_unary(&self) -> bool {
        matches!(self.kind, ExpressionKind::Unary)
    }

    pub fn subject_type(&self) -> Option<VariableType> {
        match self.kind {
            ExpressionKind::Unary => Some(self.scope.get("$")),
            ExpressionKind::Standard => self.expected.as_ref().map(VariableType::shallow_clone),
        }
    }

    pub(super) fn known_type(resolved: VariableType) -> Option<VariableType> {
        let (base, _) = resolved.unwrap_nullable();
        match base {
            VariableType::Any | VariableType::Null => None,
            _ => Some(resolved),
        }
    }

    pub(super) fn date_hint(kind: VariableType) -> VariableType {
        match kind {
            VariableType::String => VariableType::Date,
            VariableType::Array(inner) => Self::date_hint(inner.shallow_clone()).array(),
            VariableType::Nullable(inner) => {
                VariableType::Nullable(Rc::new(Self::date_hint(inner.shallow_clone())))
            }
            other => other,
        }
    }

    pub(super) fn literal_union(merged: VariableType) -> Option<VariableType> {
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

    pub(super) fn collected_type(value: VariableType, collect: bool) -> VariableType {
        if collect {
            value
                .iterator()
                .map(|t| t.as_ref().shallow_clone())
                .unwrap_or(value)
        } else {
            value
        }
    }
}

impl Db {
    pub fn cursor_scope(&self, cursor: &Cursor) -> Option<CursorScope> {
        self.cursor_scope_cached(cursor, &mut siblings::SiblingCache::default())
    }

    fn cursor_scope_cached(
        &self,
        cursor: &Cursor,
        cache: &mut siblings::SiblingCache,
    ) -> Option<CursorScope> {
        if matches!(
            cursor.target,
            CursorTarget::DataModelName | CursorTarget::DataModelProperty { .. }
        ) {
            return None;
        }
        if self.is_graph(&cursor.policy_path) {
            self.graph_cursor_scope(cursor, cache)
        } else {
            self.policy_cursor_scope(cursor, cache)
        }
    }
}
