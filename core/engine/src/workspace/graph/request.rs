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

use crate::policy::ir::{DataModelIr, DictionaryIr, PropertyTypeIr};

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

impl RequestTypes<'_> {
    /// The graph's input for a request of `target`: the entity's fields,
    /// its references and relationships typed as the entities they reach.
    pub(crate) fn input(&self, target: &Arc<str>) -> (VariableType, Vec<Arc<str>>) {
        let roots = self
            .entities
            .get(target)
            .map(|dm| dm.properties.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default();
        (self.hydrated(target, &mut Vec::new()), roots)
    }

    /// An entity as expressions read it: links to entities opened, once on a
    /// path (a cycle reads the link as on the wire: an id, or the object).
    fn hydrated(&self, name: &Arc<str>, path: &mut Vec<Arc<str>>) -> VariableType {
        let Some(dm) = self.entities.get(name) else {
            return VariableType::Any;
        };
        path.push(name.clone());
        let mut fields: HashMap<Rc<str>, VariableType> = HashMap::default();
        for prop in &dm.properties {
            let linked = match &prop.kind {
                PropertyTypeIr::Reference { target } | PropertyTypeIr::Relationship { target }
                    if self.entities.contains_key(target) && !path.contains(target) =>
                {
                    Some(self.hydrated(target, path))
                }
                _ => None,
            };
            let ty = match linked {
                Some(inner) if prop.array => inner.array(),
                Some(inner) => inner,
                None => DataModelIr::wire_property_type(
                    prop,
                    self.entities,
                    self.dictionaries,
                    &mut HashSet::default(),
                ),
            };
            fields.insert(Rc::from(prop.name.as_ref()), ty);
        }
        path.pop();
        VariableType::Object(Rc::new(RefCell::new(fields)))
    }
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
