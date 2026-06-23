use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use zen_expression::variable::VariableType;

use crate::policy::blocks::{AssertionIr, Block, DecisionTableIr, ExpressionIr, MatchIr};
use crate::policy::raw::{BlockDoc, DataModelDoc, PolicyDocument, PropertyTypeDoc, ScopeDoc};
use crate::policy::types::{Diagnostic, DiagnosticCode, DiagnosticLocation, SchemaFieldKind};
use crate::policy::ArcStrTrim;

pub type PropertyPath = Arc<str>;

#[derive(Debug, Clone)]
pub struct Policy {
    pub rules: Vec<Block>,
    pub data_models: Vec<DataModelBlock>,
    pub imports: Vec<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct DataModelBlock {
    pub id: Arc<str>,
    pub ir: Arc<DataModelIr>,
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
        visited: &mut HashSet<Arc<str>>,
    ) -> VariableType {
        let inner = match &prop.kind {
            PropertyTypeIr::String | PropertyTypeIr::Date => VariableType::String,
            PropertyTypeIr::Enum(values) => VariableType::Enum(None, enum_values_to_rc(values)),
            PropertyTypeIr::Number => VariableType::Number,
            PropertyTypeIr::Boolean => VariableType::Bool,
            PropertyTypeIr::Reference { .. } => VariableType::String,
            PropertyTypeIr::Relationship { target } => Self::wire_object(target, entities, visited),
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
                    Self::wire_property_type(prop, entities, visited),
                );
            }
        }
        visited.remove(name);
        VariableType::Object(Rc::new(RefCell::new(fields)))
    }

    fn validate_identifier(name: &str) -> Result<(), &'static str> {
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
                PropertyTypeDoc::Number => PropertyTypeIr::Number,
                PropertyTypeDoc::Boolean => PropertyTypeIr::Boolean,
                PropertyTypeDoc::Date => PropertyTypeIr::Date,
                PropertyTypeDoc::Relationship { target } => PropertyTypeIr::Relationship {
                    target: target.trimmed(),
                },
                PropertyTypeDoc::Reference { target } => PropertyTypeIr::Reference {
                    target: target.trimmed(),
                },
            };

            properties.push(Property {
                id: prop.id.clone(),
                name: prop_name,
                kind,
                array: prop.array,
                optional: prop.optional,
            });
        }

        Some(DataModelIr {
            name,
            scope,
            properties,
        })
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
            PropertyTypeIr::Date => f.write_str("date (string)"),
            PropertyTypeIr::Reference { target } => {
                write!(f, "reference id (string → {target})")
            }
            PropertyTypeIr::Relationship { target } => write!(f, "object ({target})"),
        }
    }
}
