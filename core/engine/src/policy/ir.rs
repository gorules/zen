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
    /// What the entity's records are, for the feature store.
    pub records: Records,
}

/// What an entity's records are: plain request data, events (`events`) or
/// reference data (`reference`). Windowed features aggregate the latter two.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Records {
    #[default]
    Plain,
    Events,
    Reference,
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
    /// The type as written when it is checked as another (`integer` as
    /// `number`, `timestamp` as `date`).
    pub exact: Option<Arc<str>>,
    /// Supplied by the host rather than the request: a feature, a value
    /// computed per event, or a model's output.
    pub supply: Option<Arc<Supply>>,
    /// Filled in when the value is missing or null (a property's or a
    /// feature's `default`), so never null.
    pub default: Option<Arc<serde_json::Value>>,
}

/// How the host supplies a property.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Supply {
    /// A feature: `base` is the property as written, `window` the one this
    /// property is (`txn_count` over `1h` is `txn_count_1h`).
    Feature {
        base: Arc<str>,
        expr: Arc<str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        window: Option<Arc<str>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        default: Option<serde_json::Value>,
    },
    /// Computed per event from the event's other properties.
    Compute { expr: Arc<str> },
    /// A model's output.
    Model {
        #[serde(skip_serializing_if = "Option::is_none")]
        datasource: Option<Arc<str>>,
    },
    /// A stored relationship's members, found by the host: the instances of
    /// the target sharing this entity's key (`on`), or its counterparties in
    /// an events entity (`through`). Never in the request; features on this
    /// entity aggregate them.
    Members { by: MembersBy },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MembersBy {
    Key,
    Events,
}

impl Supply {
    /// Shown after the type: `count(transaction) · 1h`.
    pub fn summary(&self) -> String {
        match self {
            Supply::Feature { expr, window, .. } => match window {
                Some(window) => format!("{expr} · {window}"),
                None => expr.to_string(),
            },
            Supply::Compute { expr } => expr.to_string(),
            Supply::Model { datasource } => match datasource {
                Some(datasource) => format!("model {datasource}"),
                None => "model".to_string(),
            },
            Supply::Members { by: MembersBy::Key } => "members by key".to_string(),
            Supply::Members { by: MembersBy::Events } => "counterparties in events".to_string(),
        }
    }
}

impl Property {
    /// A stored relationship: members the host finds (`on` / `through`).
    pub fn is_stored(&self) -> bool {
        matches!(self.supply.as_deref(), Some(Supply::Members { .. }))
    }

    /// The type as written, with `[]` and `?`: `integer?`.
    pub fn type_label(&self) -> String {
        let base = match &self.exact {
            Some(exact) => exact.to_string(),
            None => match &self.kind {
                PropertyTypeIr::Relationship { target } | PropertyTypeIr::Reference { target } => {
                    target.to_string()
                }
                PropertyTypeIr::Enum(_) => "enum".to_string(),
                kind => kind.to_string(),
            },
        };
        let array = if self.array { "[]" } else { "" };
        let optional = if self.optional { "?" } else { "" };
        format!("{base}{array}{optional}")
    }

    /// `integer? · count(transaction) · 1h` for a host-supplied property.
    pub fn supply_detail(&self) -> Option<String> {
        let supply = self.supply.as_ref()?;
        Some(format!("{} · {}", self.type_label(), supply.summary()))
    }
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
                    // A stored relationship's target stays a root: its members are found, not nested.
                    PropertyTypeIr::Relationship { .. } if prop.is_stored() => {}
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
            // A stored relationship is never on the wire: the host finds its members.
            for prop in dm.properties.iter().filter(|p| !p.is_stored()) {
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
                PropertyTypeDoc::Number | PropertyTypeDoc::Decimal | PropertyTypeDoc::Integer => {
                    PropertyTypeIr::Number
                }
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

            // The type as written when it says more than the engine's type:
            // `object` and `any` are both any here. (`decimal`, `integer` and
            // `timestamp` are old names for number and date.)
            let exact: Option<Arc<str>> = match &prop.property_type {
                PropertyTypeDoc::Object => Some(Arc::from("object")),
                PropertyTypeDoc::Any => Some(Arc::from("any")),
                _ => None,
            };

            // A feature: the host supplies it, one property per window
            // (`txn_count` over 1h, 7d: `txn_count_1h`, `txn_count_7d`).
            // Without a `default` (or `required`) it is absent when unknown
            // (not covered), so optional: rules handle null. With one, the
            // host always supplies a value (or rejects the request).
            if let Some(feature) = &prop.feature {
                let optional = feature
                    .default
                    .as_ref()
                    .is_none_or(serde_json::Value::is_null)
                    && prop.required != Some(true);
                let windows = feature.window.as_ref().map(|w| w.list()).unwrap_or_default();
                let names: Vec<(Arc<str>, Option<Arc<str>>)> = if windows.is_empty() {
                    vec![(prop_name.clone(), None)]
                } else {
                    let listed = windows.len() > 1;
                    windows
                        .iter()
                        .filter_map(|window| {
                            if !is_window(window) {
                                diagnostics.push(Diagnostic::error(
                                    DiagnosticCode::InvalidDuration,
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
                            Some((window_name(&prop_name, window, listed), Some(window.clone())))
                        })
                        .collect()
                };
                let default = feature.default.clone();
                for (feature_name, window) in names {
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
                        exact: exact.clone(),
                        default: default.clone().filter(|d| !d.is_null()).map(Arc::new),
                        supply: Some(Arc::new(Supply::Feature {
                            base: prop_name.clone(),
                            expr: feature.expr.clone(),
                            window,
                            default: default.clone(),
                        })),
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
                // Filled with its `default` when missing: always there. (A
                // compute is null when what it reads may be: see below.)
                optional: (prop.optional || host_optional)
                    && prop.required != Some(true)
                    && prop.rest.get("default").is_none_or(serde_json::Value::is_null),
                exact,
                default: prop
                    .rest
                    .get("default")
                    .filter(|d| !d.is_null())
                    .map(|d| Arc::new(d.clone())),
                supply: match (&prop.compute, &prop.model) {
                    (Some(expr), _) => Some(Arc::new(Supply::Compute { expr: expr.clone() })),
                    (None, Some(model)) => Some(Arc::new(Supply::Model {
                        datasource: model
                            .get("datasource")
                            .and_then(serde_json::Value::as_str)
                            .filter(|d| !d.is_empty())
                            .map(Arc::from),
                    })),
                    (None, None) => match (&prop.property_type, prop.rest.get("on"), prop.rest.get("through")) {
                        (PropertyTypeDoc::Relationship { .. }, Some(_), _) => {
                            Some(Arc::new(Supply::Members { by: MembersBy::Key }))
                        }
                        (PropertyTypeDoc::Relationship { .. }, None, Some(_)) => {
                            Some(Arc::new(Supply::Members { by: MembersBy::Events }))
                        }
                        _ => None,
                    },
                },
            });
        }

        Self::computes_propagate_null(doc, &mut properties);

        let records = if doc.events.is_some() {
            Records::Events
        } else if doc.reference.is_some() {
            Records::Reference
        } else {
            Records::Plain
        };
        Some(DataModelIr {
            name,
            scope,
            properties,
            records,
        })
    }
}

impl DataModelIr {
    /// A compute is null when anything it reads may be (null in, null out)
    /// or when it can give null itself (`x > 0 ? y : null`), unless
    /// `required` or a `default` says it is always there. What it reads
    /// through a relationship, a reference or `$root` counts as nullable.
    fn computes_propagate_null(doc: &DataModelDoc, properties: &mut [Property]) {
        use zen_expression::intellisense::{IntelliSense, ReadDependency};
        let decides = |prop: &Property| {
            matches!(prop.supply.as_deref(), Some(Supply::Compute { .. }))
                && prop.default.is_none()
                && doc
                    .properties
                    .iter()
                    .find(|p| p.id == prop.id)
                    .is_none_or(|p| p.required != Some(true))
        };
        let computes: Vec<usize> = (0..properties.len())
            .filter(|&i| decides(&properties[i]))
            .collect();
        if computes.is_empty() {
            return;
        }
        let mut intellisense = IntelliSense::new();
        // Each compute's own-field reads, or `None` when it may be null whatever they are.
        let reads: Vec<Option<Vec<Arc<str>>>> = computes
            .iter()
            .map(|&i| {
                let Some(Supply::Compute { expr }) = properties[i].supply.as_deref() else {
                    return None;
                };
                let own = |path: &[Rc<str>]| -> Option<Arc<str>> {
                    let [name] = path else { return None };
                    let prop = properties.iter().find(|p| *p.name == **name)?;
                    (!matches!(prop.kind, PropertyTypeIr::Relationship { .. }))
                        .then(|| prop.name.clone())
                };
                let mut names = Vec::new();
                for read in intellisense.reads(expr) {
                    match read {
                        ReadDependency::Direct { path, .. } => names.push(own(&path)?),
                        ReadDependency::Iteration { collection, .. } => {
                            names.push(own(&collection)?)
                        }
                        ReadDependency::Unresolved { .. } => return None,
                    }
                }
                let fields: HashMap<Rc<str>, VariableType> = properties
                    .iter()
                    .map(|p| {
                        let kind = match &p.kind {
                            PropertyTypeIr::String | PropertyTypeIr::Reference { .. } => {
                                VariableType::String
                            }
                            PropertyTypeIr::Enum(values) => {
                                VariableType::Enum(None, enum_values_to_rc(values))
                            }
                            PropertyTypeIr::Number => VariableType::Number,
                            PropertyTypeIr::Boolean => VariableType::Bool,
                            PropertyTypeIr::Date => VariableType::Date,
                            PropertyTypeIr::Relationship { .. } | PropertyTypeIr::Any => {
                                VariableType::Any
                            }
                        };
                        let kind = if p.array { kind.array() } else { kind };
                        (Rc::from(p.name.as_ref()), kind)
                    })
                    .collect();
                let data = VariableType::Object(Rc::new(RefCell::new(fields)));
                let returns = intellisense.analyze(expr, &data).return_type.clone();
                (!matches!(returns, VariableType::Null | VariableType::Nullable(_)))
                    .then_some(names)
            })
            .collect();
        // Until nothing changes: a compute may read another.
        let mut changed = true;
        while changed {
            changed = false;
            for (&i, reads) in computes.iter().zip(&reads) {
                if properties[i].optional {
                    continue;
                }
                let nullable = reads.as_ref().is_none_or(|names| {
                    names
                        .iter()
                        .any(|name| properties.iter().any(|p| p.name == *name && p.optional))
                });
                if nullable {
                    properties[i].optional = true;
                    changed = true;
                }
            }
        }
    }
}

/// A feature window as written: a positive whole number (no leading zero)
/// of minutes, hours or days (`10m`, `1h`, `7d`), or `all` (all time).
fn is_window(window: &str) -> bool {
    // `all`: every day there is data for.
    window == "all" || is_duration(window, &["m", "h", "d"])
}

/// A positive whole number (no leading zero, no spaces) followed by one of
/// `units`: `30s`, `10m`, `250ms`.
pub(crate) fn is_duration(text: &str, units: &[&str]) -> bool {
    units.iter().any(|unit| {
        text.strip_suffix(unit).is_some_and(|digits| {
            !digits.is_empty()
                && !digits.starts_with('0')
                && digits.bytes().all(|b| b.is_ascii_digit())
        })
    })
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
