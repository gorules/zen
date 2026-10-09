//! A Request node typed by an entity (`inputNode.content.target`): the
//! graph's input is that entity itself, flat (`{ txn_id, amount, customer }`),
//! as a schema-typed request is. A reference holds the record it names
//! (`customer: { … }`): the host places it; there are no pools. A request of
//! several entities is an entity of them.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashSet};
use zen_expression::variable::VariableType;

use crate::policy::ir::{DataModelIr, DictionaryIr, Property, PropertyTypeIr, Records, Scope};

/// What a graph's Request node takes, as far as its target goes.
#[derive(Clone)]
pub(crate) enum RequestTarget {
    /// A schema, or nothing declared.
    None,
    /// An entity no import makes visible.
    Missing(Arc<str>),
    Entity {
        /// The graph's input: the entity, references opened.
        input: VariableType,
        /// The names at the input's root: the entity's fields.
        roots: Vec<Arc<str>>,
    },
}

impl RequestTarget {
    pub(crate) fn input(&self) -> Option<&VariableType> {
        match self {
            RequestTarget::Entity { input, .. } => Some(input),
            _ => None,
        }
    }
}

pub(crate) struct RequestTypes<'a> {
    pub entities: &'a HashMap<Arc<str>, Arc<DataModelIr>>,
    pub dictionaries: &'a HashMap<Arc<str>, Arc<DictionaryIr>>,
}

/// How many links (references, relationships) a request's type opens from
/// its root: a host fills in the records a few levels deep, so past that a
/// link reads as unknown (`any`). It also bounds the type when entities
/// reference each other in a cycle (each level opens every link again).
pub(crate) const REQUEST_LINK_DEPTH: usize = 4;

impl RequestTypes<'_> {
    /// The graph's input for a request of `target`: the entity's fields,
    /// its references and relationships typed as the entities they reach.
    pub(crate) fn input(&self, target: &Arc<str>) -> (VariableType, Vec<Arc<str>>) {
        let roots = self
            .entities
            .get(target)
            .map(|dm| dm.properties.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default();
        (self.hydrated(target, 0), roots)
    }

    /// An entity `depth` links from the request's root, as expressions read
    /// it: each field as [`Self::field`] types it.
    fn hydrated(&self, name: &Arc<str>, depth: usize) -> VariableType {
        let Some(dm) = self.entities.get(name) else {
            return VariableType::Any;
        };
        let fields: HashMap<Rc<str>, VariableType> = dm
            .properties
            .iter()
            .map(|prop| (Rc::from(prop.name.as_ref()), self.field(prop, depth)))
            .collect();
        VariableType::Object(Rc::new(RefCell::new(fields)))
    }

    /// A field of an entity `depth` links from the request's root: a link
    /// to an entity holds its record (or records), opened up to
    /// [`REQUEST_LINK_DEPTH`]; an optional field is nullable.
    pub(crate) fn field(&self, prop: &Property, depth: usize) -> VariableType {
        let ty = match &prop.kind {
            PropertyTypeIr::Reference { target } | PropertyTypeIr::Relationship { target }
                if self.entities.contains_key(target) =>
            {
                let inner = if depth < REQUEST_LINK_DEPTH {
                    self.hydrated(target, depth + 1)
                } else {
                    VariableType::Any
                };
                if prop.array {
                    inner.array()
                } else {
                    inner
                }
            }
            _ => DataModelIr::wire_property_type(
                prop,
                self.entities,
                self.dictionaries,
                &mut HashSet::default(),
            ),
        };
        if prop.optional {
            super::wrap_optional(ty)
        } else {
            ty
        }
    }
}

/// Entities of one name declared in several policies are one entity: each
/// property once (the first declared wins) and the first kind of records
/// that isn't plain. `models` come in the order their policies are walked.
pub(crate) fn merge_entities<'a>(
    models: impl IntoIterator<Item = &'a DataModelIr>,
) -> HashMap<Arc<str>, Arc<DataModelIr>> {
    let mut merged: HashMap<Arc<str>, DataModelIr> = HashMap::default();
    for dm in models {
        let entity = merged
            .entry(dm.name.clone())
            .or_insert_with(|| DataModelIr {
                name: dm.name.clone(),
                scope: Scope::Entity,
                properties: Vec::new(),
                records: Records::Plain,
            });
        if entity.records == Records::Plain {
            entity.records = dm.records;
        }
        for prop in &dm.properties {
            if !entity.properties.iter().any(|p| p.name == prop.name) {
                entity.properties.push(prop.clone());
            }
        }
    }
    merged
        .into_iter()
        .map(|(name, dm)| (name, Arc::new(dm)))
        .collect()
}

/// The entities with each reference read as the record it names (a
/// relationship), as a flat request carries it: what its checks and date
/// conversion go by.
pub(crate) fn with_inline_references(
    entities: &HashMap<Arc<str>, Arc<DataModelIr>>,
) -> HashMap<Arc<str>, Arc<DataModelIr>> {
    entities
        .iter()
        .map(|(name, dm)| {
            let mut inline = dm.as_ref().clone();
            for prop in &mut inline.properties {
                if let PropertyTypeIr::Reference { target } = &prop.kind {
                    if entities.contains_key(target) {
                        prop.kind = PropertyTypeIr::Relationship {
                            target: target.clone(),
                        };
                    }
                }
            }
            (name.clone(), Arc::new(inline))
        })
        .collect()
}
