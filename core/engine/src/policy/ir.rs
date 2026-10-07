use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use zen_expression::variable::VariableType;

use crate::policy::blocks::{AssertionIr, Block, DecisionTableIr, ExpressionIr, MatchIr};
use crate::policy::raw::{
    BlockDoc, DataModelDoc, DictionaryDoc, PolicyDocument, PropertyTypeDoc, ScopeDoc,
};
use crate::policy::ArcStrTrim;
use crate::workspace::types::{Diagnostic, DiagnosticCode, DiagnosticLocation, SchemaFieldKind};

pub type PropertyPath = Arc<str>;

#[derive(Debug, Clone)]
pub struct Policy {
    pub rules: Vec<Block>,
    pub data_models: Vec<DataModelBlock>,
    pub dictionaries: Vec<DictionaryBlock>,
    pub imports: Vec<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct DataModelBlock {
    pub id: Arc<str>,
    pub ir: Arc<DataModelIr>,
}

#[derive(Debug, Clone)]
pub struct DictionaryBlock {
    pub id: Arc<str>,
    pub ir: Arc<DictionaryIr>,
}

#[derive(Debug, Clone)]
pub struct ParsedPolicy {
    pub policy: Arc<Policy>,
    pub diagnostics: Arc<Vec<Diagnostic>>,
}

impl Policy {
    pub fn parse(path: &Arc<str>, doc: &PolicyDocument) -> ParsedPolicy {
        let mut diagnostics = Vec::new();
        let mut rules = Vec::new();
        let mut data_models = Vec::new();
        let mut dictionaries = Vec::new();

        for env in &doc.blocks {
            match env {
                BlockDoc::Assertion { id, data } => {
                    rules.push(AssertionIr::parse(id, data, path, &mut diagnostics))
                }
                BlockDoc::DecisionTable { id, data } => {
                    rules.push(DecisionTableIr::parse(id, data, path, &mut diagnostics))
                }
                BlockDoc::Expression { id, data } => {
                    rules.push(ExpressionIr::parse(id, data, path, &mut diagnostics))
                }
                BlockDoc::Match { id, data } => {
                    rules.push(MatchIr::parse(id, data, path, &mut diagnostics))
                }
                BlockDoc::DataModel { id, data } => {
                    if let Some(ir) = DataModelIr::parse(id, data, path, &mut diagnostics) {
                        data_models.push(DataModelBlock {
                            id: id.clone(),
                            ir: Arc::new(ir),
                        });
                    }
                }
                BlockDoc::Dictionary { id, data } => {
                    if let Some(ir) = DictionaryIr::parse(id, data, path, &mut diagnostics) {
                        dictionaries.push(DictionaryBlock {
                            id: id.clone(),
                            ir: Arc::new(ir),
                        });
                    }
                }
                BlockDoc::Ignored(_) => {}
            }
        }

        let imports: Vec<Arc<str>> = doc
            .imports
            .iter()
            .map(|p| p.trimmed())
            .filter(|p| !p.is_empty())
            .collect();

        ParsedPolicy {
            policy: Arc::new(Policy {
                rules,
                data_models,
                dictionaries,
                imports,
            }),
            diagnostics: Arc::new(diagnostics),
        }
    }

    pub fn rules(&self) -> impl Iterator<Item = &Block> {
        self.rules.iter()
    }

    pub fn data_models(&self) -> impl Iterator<Item = (&Arc<str>, &DataModelIr)> {
        self.data_models.iter().map(|b| (&b.id, b.ir.as_ref()))
    }

    pub fn entity_data_models(&self) -> impl Iterator<Item = (&Arc<str>, &DataModelIr)> {
        self.data_models().filter(|(_, dm)| !dm.scope.is_global())
    }

    pub fn global_data_models(&self) -> impl Iterator<Item = (&Arc<str>, &DataModelIr)> {
        self.data_models().filter(|(_, dm)| dm.scope.is_global())
    }

    pub fn dictionaries(&self) -> impl Iterator<Item = (&Arc<str>, &Arc<DictionaryIr>)> {
        self.dictionaries.iter().map(|b| (&b.id, &b.ir))
    }

    pub fn imports(&self) -> &[Arc<str>] {
        &self.imports
    }
}

#[derive(Debug, Clone)]
pub struct DataModelIr {
    pub name: Arc<str>,
    pub scope: Scope,
    pub properties: Vec<Property>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Entity,
    Global,
}

impl Scope {
    pub fn is_global(self) -> bool {
        matches!(self, Scope::Global)
    }
}

impl From<ScopeDoc> for Scope {
    fn from(value: ScopeDoc) -> Self {
        match value {
            ScopeDoc::Entity => Scope::Entity,
            ScopeDoc::Global => Scope::Global,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Property {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub kind: PropertyTypeIr,
    pub array: bool,
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub enum PropertyTypeIr {
    String,
    Enum(Vec<Arc<str>>),
    Number,
    Boolean,
    Date,
    Relationship { target: Arc<str> },
    Reference { target: Arc<str> },
    /// Any value (`object`, `any`): not checked.
    Any,
}

impl DataModelIr {
    pub(crate) fn classify_roots<'a>(
        models: impl IntoIterator<Item = &'a DataModelIr>,
    ) -> (HashSet<Arc<str>>, HashSet<Arc<str>>) {
        let mut entities: HashSet<Arc<str>> = HashSet::new();
        let mut relationship_targets: HashSet<Arc<str>> = HashSet::new();
        let mut ref_targets: HashSet<Arc<str>> = HashSet::new();
        let mut global_relationship_targets: HashSet<Arc<str>> = HashSet::new();
        for dm in models {
            if !dm.scope.is_global() {
                entities.insert(dm.name.clone());
            }
            for prop in &dm.properties {
                match &prop.kind {
                    PropertyTypeIr::Relationship { target } => {
                        if dm.scope.is_global() {
                            global_relationship_targets.insert(target.clone());
                        } else {
                            relationship_targets.insert(target.clone());
                        }
                    }
                    PropertyTypeIr::Reference { target } => {
                        ref_targets.insert(target.clone());
                    }
                    _ => {}
                }
            }
        }
        let global_only_relationship: HashSet<Arc<str>> = global_relationship_targets
            .difference(&relationship_targets)
            .filter(|t| !ref_targets.contains(*t))
            .cloned()
            .collect();
        let nested: HashSet<Arc<str>> = relationship_targets
            .union(&ref_targets)
            .cloned()
            .chain(global_only_relationship.iter().cloned())
            .collect();
        let roots: HashSet<Arc<str>> = entities.difference(&nested).cloned().collect();
        (roots, ref_targets)
    }

    pub(crate) fn wire_property_type(
        prop: &Property,
        entities: &HashMap<Arc<str>, Arc<DataModelIr>>,
        dictionaries: &HashMap<Arc<str>, Arc<DictionaryIr>>,
        visited: &mut HashSet<Arc<str>>,
    ) -> VariableType {
        let inner = match &prop.kind {
            PropertyTypeIr::String => VariableType::String,
            PropertyTypeIr::Date => VariableType::Date,
            PropertyTypeIr::Enum(values) => VariableType::Enum(None, enum_values_to_rc(values)),
            PropertyTypeIr::Number => VariableType::Number,
            PropertyTypeIr::Boolean => VariableType::Bool,
            PropertyTypeIr::Any => VariableType::Any,
            PropertyTypeIr::Reference { .. } => VariableType::String,
            PropertyTypeIr::Relationship { target } => match dictionaries.get(target.as_ref()) {
                Some(dict) if !entities.contains_key(target.as_ref()) => dict.enum_type(),
                _ => Self::wire_object(target, entities, dictionaries, visited),
            },
        };
        if prop.array {
            inner.array()
        } else {
            inner
        }
    }

    pub(crate) fn wire_object(
        name: &Arc<str>,
        entities: &HashMap<Arc<str>, Arc<DataModelIr>>,
        dictionaries: &HashMap<Arc<str>, Arc<DictionaryIr>>,
        visited: &mut HashSet<Arc<str>>,
    ) -> VariableType {
        if !visited.insert(name.clone()) {
            return VariableType::Any;
        }
        let mut fields: HashMap<Rc<str>, VariableType> = HashMap::new();
        if let Some(dm) = entities.get(name) {
            for prop in &dm.properties {
                fields.insert(
                    Rc::from(prop.name.as_ref()),
                    Self::wire_property_type(prop, entities, dictionaries, visited),
                );
            }
        }
        visited.remove(name);
        VariableType::Object(Rc::new(RefCell::new(fields)))
    }

    pub(crate) fn validate_identifier(name: &str) -> Result<(), &'static str> {
        if name.is_empty() {
            return Err("is empty");
        }
        if name.starts_with(|c: char| c.is_ascii_digit()) {
            return Err("starts with a digit");
        }
        for c in name.chars() {
            if c == '.' {
                return Err("contains '.'");
            }
            if c == '[' || c == ']' {
                return Err("contains a bracket");
            }
            if c.is_whitespace() {
                return Err("contains whitespace");
            }
        }
        Ok(())
    }

    pub fn parse(
        id: &Arc<str>,
        doc: &DataModelDoc,
        policy_path: &Arc<str>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Self> {
        let name = doc.name.trimmed();
        let scope = Scope::from(doc.scope);
        if name.is_empty() && !scope.is_global() {
            diagnostics.push(Diagnostic::error(
                DiagnosticCode::ParseError,
                DiagnosticLocation::block(policy_path.clone(), id.clone()),
                "data model is missing a name",
            ));
            return None;
        }
        if !name.is_empty() {
            if let Err(reason) = Self::validate_identifier(&name) {
                diagnostics.push(Diagnostic::error(
                    DiagnosticCode::InvalidName,
                    DiagnosticLocation::block(policy_path.clone(), id.clone()),
                    format!("entity name '{name}' {reason}"),
                ));
                return None;
            }
        }

        let mut seen: ahash::HashMap<Arc<str>, Arc<str>> = ahash::HashMap::default();
        let mut properties = Vec::with_capacity(doc.properties.len());

        for prop in &doc.properties {
            let prop_name = prop.name.trimmed();
            if prop_name.is_empty() {
                diagnostics.push(Diagnostic::error(
                    DiagnosticCode::ParseError,
                    DiagnosticLocation::expression(
                        policy_path.clone(),
                        id.clone(),
                        prop.id.clone(),
                        None,
                    ),
                    format!("property in entity '{name}' is missing a name"),
                ));
                continue;
            }
            if let Err(reason) = Self::validate_identifier(&prop_name) {
                diagnostics.push(Diagnostic::error(
                    DiagnosticCode::InvalidName,
                    DiagnosticLocation::expression(
                        policy_path.clone(),
                        id.clone(),
                        prop.id.clone(),
                        None,
                    ),
                    format!("property name '{prop_name}' in entity '{name}' {reason}"),
                ));
                continue;
            }
            if let Some(prev_id) = seen.get(&prop_name) {
                diagnostics.push(Diagnostic::error(
                    DiagnosticCode::DuplicateProperty,
                    DiagnosticLocation::expression(
                        policy_path.clone(),
                        id.clone(),
                        prev_id.clone(),
                        None,
                    ),
                    format!("duplicate property '{prop_name}' in entity '{name}'"),
                ));
                continue;
            }
            seen.insert(prop_name.clone(), prop.id.clone());

            let kind = match &prop.property_type {
                PropertyTypeDoc::String { values } => {
                    let mut trimmed: Vec<Arc<str>> = Vec::new();
                    let mut duplicates: Vec<Arc<str>> = Vec::new();
                    if let Some(vs) = values.as_ref() {
                        for v in vs {
                            let v = v.trimmed();
                            if v.is_empty() {
                                continue;
                            }
                            if trimmed.iter().any(|prev| *prev == v) {
                                if !duplicates.iter().any(|d| *d == v) {
                                    duplicates.push(v);
                                }
                                continue;
                            }
                            trimmed.push(v);
                        }
                    }
                    for dup in &duplicates {
                        diagnostics.push(Diagnostic::error(
                            DiagnosticCode::DuplicateEnumValue,
                            DiagnosticLocation::expression(
                                policy_path.clone(),
                                id.clone(),
                                prop.id.clone(),
                                None,
                            ),
                            format!(
                                "duplicate enum value '{dup}' in property '{prop_name}' of entity '{name}'"
                            ),
                        ));
                    }
                    if trimmed.is_empty() {
                        PropertyTypeIr::String
                    } else {
                        PropertyTypeIr::Enum(trimmed)
                    }
                }
                PropertyTypeDoc::Number
                | PropertyTypeDoc::Decimal { .. }
                | PropertyTypeDoc::Integer => PropertyTypeIr::Number,
                PropertyTypeDoc::Boolean => PropertyTypeIr::Boolean,
                PropertyTypeDoc::Date | PropertyTypeDoc::Timestamp => PropertyTypeIr::Date,
                PropertyTypeDoc::Object | PropertyTypeDoc::Any => PropertyTypeIr::Any,
                PropertyTypeDoc::Relationship { target } => PropertyTypeIr::Relationship {
                    target: target.trimmed(),
                },
                PropertyTypeDoc::Reference { target } => PropertyTypeIr::Reference {
                    target: target.trimmed(),
                },
            };

            // A feature: the host supplies it, one property per window
            // (`txn_count` over 1h, 7d: `txn_count_1h`, `txn_count_7d`).
            // Without a `default` (or `required`) it is absent when unknown
            // (not covered), so optional: rules handle null. With one, the
            // host always supplies a value (or rejects the request).
            if let Some(feature) = &prop.feature {
                let optional = !feature.rest.contains_key("default") && prop.required != Some(true);
                let windows = feature.window.as_ref().map(|w| w.list()).unwrap_or_default();
                let names: Vec<Arc<str>> = if windows.is_empty() {
                    vec![prop_name.clone()]
                } else {
                    let listed = windows.len() > 1;
                    windows
                        .iter()
                        .filter_map(|window| {
                            if !is_window(window) {
                                diagnostics.push(Diagnostic::error(
                                    DiagnosticCode::ParseError,
                                    DiagnosticLocation::expression(
                                        policy_path.clone(),
                                        id.clone(),
                                        prop.id.clone(),
                                        None,
                                    ),
                                    format!(
                                        "window '{window}' of feature '{prop_name}' in entity '{name}' is not a duration such as 10m, 1h or 7d"
                                    ),
                                ));
                                return None;
                            }
                            Some(window_name(&prop_name, window, listed))
                        })
                        .collect()
                };
                for feature_name in names {
                    if feature_name != prop_name {
                        if let Some(prev_id) = seen.get(&feature_name) {
                            diagnostics.push(Diagnostic::error(
                                DiagnosticCode::DuplicateProperty,
                                DiagnosticLocation::expression(
                                    policy_path.clone(),
                                    id.clone(),
                                    prev_id.clone(),
                                    None,
                                ),
                                format!("duplicate property '{feature_name}' in entity '{name}'"),
                            ));
                            continue;
                        }
                        seen.insert(feature_name.clone(), prop.id.clone());
                    }
                    properties.push(Property {
                        id: prop.id.clone(),
                        name: feature_name,
                        kind: kind.clone(),
                        array: prop.array,
                        optional,
                    });
                }
                continue;
            }

            // A model's output: supplied by the host; null when the model was
            // not called (`when`) or fell back without a value, unless
            // `required` or a fallback value makes it always there.
            let host_optional = prop.model.as_ref().is_some_and(|m| {
                prop.required != Some(true) && m.get("fallback").is_none_or(serde_json::Value::is_null)
            });
            properties.push(Property {
                id: prop.id.clone(),
                name: prop_name,
                kind,
                array: prop.array,
                // Computed by the host: always there.
                optional: (prop.optional || host_optional) && prop.compute.is_none() && prop.required != Some(true),
            });
        }

        Some(DataModelIr {
            name,
            scope,
            properties,
        })
    }
}

/// A feature window as written: a positive whole number (no leading zero)
/// of minutes, hours or days (`10m`, `1h`, `7d`).
fn is_window(window: &str) -> bool {
    let Some((digits, unit)) = window.split_at_checked(window.len().saturating_sub(1)) else {
        return false;
    };
    matches!(unit, "m" | "h" | "d")
        && !digits.is_empty()
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit())
}

/// The property a feature window becomes: `<name>_<window>`; a single
/// window already at the end of the name (`txn_count_7d`) is not doubled.
/// The feature store names its features the same way.
pub fn window_name(name: &Arc<str>, window: &str, listed: bool) -> Arc<str> {
    let suffix = format!("_{window}");
    if !listed && name.ends_with(&suffix) {
        name.clone()
    } else {
        Arc::from(format!("{name}{suffix}"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictionaryIr {
    pub name: Arc<str>,
    pub entries: Vec<DictionaryEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictionaryEntry {
    pub value: Arc<str>,
    pub label: Arc<str>,
}

impl DictionaryIr {
    pub fn parse(
        id: &Arc<str>,
        doc: &DictionaryDoc,
        policy_path: &Arc<str>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Self> {
        let name = doc.name.trimmed();
        if name.is_empty() {
            diagnostics.push(Diagnostic::error(
                DiagnosticCode::ParseError,
                DiagnosticLocation::block(policy_path.clone(), id.clone()),
                "dictionary is missing a name",
            ));
            return None;
        }
        if let Err(reason) = DataModelIr::validate_identifier(&name) {
            diagnostics.push(Diagnostic::error(
                DiagnosticCode::InvalidName,
                DiagnosticLocation::block(policy_path.clone(), id.clone()),
                format!("dictionary name '{name}' {reason}"),
            ));
            return None;
        }

        let mut entries: Vec<DictionaryEntry> = Vec::with_capacity(doc.entries.len());
        for entry in &doc.entries {
            let value = entry.value.trimmed();
            if value.is_empty() {
                continue;
            }
            if entries.iter().any(|e| e.value == value) {
                diagnostics.push(Diagnostic::error(
                    DiagnosticCode::DuplicateEnumValue,
                    DiagnosticLocation::expression(
                        policy_path.clone(),
                        id.clone(),
                        entry.id.clone(),
                        None,
                    ),
                    format!("duplicate value '{value}' in dictionary '{name}'"),
                ));
                continue;
            }
            entries.push(DictionaryEntry {
                value,
                label: entry.label.trimmed(),
            });
        }

        Some(DictionaryIr { name, entries })
    }

    pub fn values(&self) -> impl Iterator<Item = &Arc<str>> {
        self.entries.iter().map(|e| &e.value)
    }

    pub(crate) fn enum_type(&self) -> VariableType {
        VariableType::Enum(
            Some(Rc::from(self.name.as_ref())),
            self.entries
                .iter()
                .map(|e| Rc::from(e.value.as_ref()))
                .collect(),
        )
    }
}

impl PropertyTypeIr {
    pub(crate) fn to_schema_field_kind(&self, array: bool) -> SchemaFieldKind {
        match self {
            PropertyTypeIr::Relationship { target } => SchemaFieldKind::Relationship {
                target: target.clone(),
                array,
            },
            PropertyTypeIr::Reference { target } => SchemaFieldKind::Reference {
                target: target.clone(),
                array,
            },
            PropertyTypeIr::Enum(values) => SchemaFieldKind::Enum {
                values: values.clone(),
                array,
            },
            _ => SchemaFieldKind::Scalar,
        }
    }

    pub(crate) fn same_shape_as(&self, other: &PropertyTypeIr) -> bool {
        use PropertyTypeIr::*;
        match (self, other) {
            (String, String) | (Number, Number) | (Boolean, Boolean) | (Date, Date) => true,
            (Enum(a), Enum(b)) => a == b,
            (Relationship { target: a }, Relationship { target: b })
            | (Reference { target: a }, Reference { target: b }) => a == b,
            _ => false,
        }
    }
}

pub(crate) fn enum_values_to_rc(values: &[Arc<str>]) -> Vec<Rc<str>> {
    values.iter().map(|v| Rc::from(v.as_ref())).collect()
}

impl std::fmt::Display for PropertyTypeIr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PropertyTypeIr::String => f.write_str("string"),
            PropertyTypeIr::Enum(values) => {
                let rendered = values
                    .iter()
                    .map(|v| format!("'{v}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "enum ({rendered})")
            }
            PropertyTypeIr::Number => f.write_str("number"),
            PropertyTypeIr::Boolean => f.write_str("bool"),
            PropertyTypeIr::Any => f.write_str("any"),
            PropertyTypeIr::Date => f.write_str("date"),
            PropertyTypeIr::Reference { target } => {
                write!(f, "reference id (string → {target})")
            }
            PropertyTypeIr::Relationship { target } => write!(f, "object ({target})"),
        }
    }
}
