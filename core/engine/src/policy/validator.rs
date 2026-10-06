use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::variable::Variable;

use crate::nodes::input::dates::DeclaredDates;
use crate::policy::ir::{DataModelIr, DictionaryIr, Property, PropertyTypeIr};
use crate::policy::refs::RefPoolIndex;
use crate::policy::MAX_RECURSION_DEPTH;
use crate::workspace::db::Db;
use crate::workspace::types::InputValidationError;
use zen_types::symbol::Symbol;

impl Db {
    pub(crate) fn input_schema(&self, policy_path: &str) -> InputSchema {
        let entities = self.visible_entities(policy_path);
        let globals = self.visible_globals(policy_path);
        let visible_dms = self.visible_data_models(policy_path);
        let (roots, ref_targets) =
            DataModelIr::classify_roots(visible_dms.iter().map(|dm| dm.as_ref()));
        let dated = InputSchema::dated(&entities);
        InputSchema {
            entities,
            globals,
            roots,
            ref_targets,
            dated,
            dictionaries: self.unit(policy_path).dictionaries.clone(),
        }
    }

    fn visible_globals(&self, policy_path: &str) -> HashMap<Arc<str>, Property> {
        let visible = self.visible_policies(policy_path);
        let mut sorted: Vec<Arc<str>> = visible.iter().cloned().collect();
        sorted.sort();
        let mut out: HashMap<Arc<str>, Property> = HashMap::new();
        for pp in &sorted {
            let Some(parsed) = self.parsed(pp) else {
                continue;
            };
            for (_, dm) in parsed.policy.global_data_models() {
                for prop in &dm.properties {
                    out.entry(prop.name.clone()).or_insert_with(|| prop.clone());
                }
            }
        }
        out
    }

    fn visible_data_models(&self, policy_path: &str) -> Vec<Arc<DataModelIr>> {
        let entities = self.visible_entities(policy_path);
        let visible = self.visible_policies(policy_path);
        let mut sorted: Vec<Arc<str>> = visible.iter().cloned().collect();
        sorted.sort();
        let mut out: Vec<Arc<str>> = entities.values().map(|d| d.name.clone()).collect();
        out.sort();
        let mut result: Vec<Arc<DataModelIr>> = out
            .into_iter()
            .filter_map(|name| entities.get(&name).cloned())
            .collect();
        for pp in &sorted {
            let Some(parsed) = self.parsed(pp) else {
                continue;
            };
            for (_, dm) in parsed.policy.global_data_models() {
                result.push(Arc::new(dm.clone()));
            }
        }
        result
    }
}

pub(crate) struct InputSchema {
    entities: Arc<HashMap<Arc<str>, Arc<DataModelIr>>>,
    globals: HashMap<Arc<str>, Property>,
    roots: HashSet<Arc<str>>,
    ref_targets: HashSet<Arc<str>>,
    dated: HashSet<Arc<str>>,
    dictionaries: HashMap<Arc<str>, Arc<DictionaryIr>>,
}

impl InputSchema {
    pub(crate) fn validate(&self, input: &Variable) -> Vec<InputValidationError> {
        let ref_pools = RefPoolIndex::from_input(input, self.ref_targets.iter().cloned());
        let mut validator = InputValidator {
            entities: &self.entities,
            dictionaries: &self.dictionaries,
            ref_pools: &ref_pools,
            errors: Vec::new(),
            depth: 0,
            path: Vec::new(),
        };

        let Some(input_obj) = input.as_object() else {
            if !matches!(input, Variable::Null) {
                validator.errors.push(InputValidationError {
                    path: String::new(),
                    expected: "object".into(),
                    got: input.type_name().into(),
                });
            }
            return validator.errors;
        };

        for (key, val) in input_obj.borrow().iter() {
            if matches!(val, Variable::Null) {
                continue;
            }
            let key_str: &str = key.as_ref();
            validator.path.push(Segment::Key(key.clone()));
            if self.ref_targets.contains(key_str) {
                validator.validate_array_of_entity(val, key_str);
            } else if self.roots.contains(key_str) {
                validator.validate_entity(val, key_str);
            } else if let Some(prop) = self.globals.get(key_str) {
                validator.validate_global(val, prop);
            }
            validator.path.pop();
        }

        validator.errors
    }
}

impl InputSchema {
    fn dated(entities: &HashMap<Arc<str>, Arc<DataModelIr>>) -> HashSet<Arc<str>> {
        let mut dated: HashSet<Arc<str>> = HashSet::default();
        loop {
            let before = dated.len();
            for (name, model) in entities.iter() {
                if dated.contains(name) {
                    continue;
                }
                let any = model.properties.iter().any(|p| match &p.kind {
                    PropertyTypeIr::Date => true,
                    PropertyTypeIr::Relationship { target } => dated.contains(target),
                    _ => false,
                });
                if any {
                    dated.insert(name.clone());
                }
            }
            if dated.len() == before {
                return dated;
            }
        }
    }

    pub(crate) fn convert_dates(&self, input: &Variable) -> Option<Variable> {
        DeclaredDates::rewrite_fields(input, |key, value| {
            if self.ref_targets.contains(key) {
                DeclaredDates::rewrite_items(value, |item| self.convert_entity(item, key, 0))
            } else if self.roots.contains(key) {
                self.convert_entity(value, key, 0)
            } else {
                self.convert_property(value, self.globals.get(key)?, 0)
            }
        })
    }

    fn convert_entity(&self, value: &Variable, entity: &str, depth: usize) -> Option<Variable> {
        if depth >= MAX_RECURSION_DEPTH || !self.dated.contains(entity) {
            return None;
        }
        let model = self.entities.get(entity)?;
        DeclaredDates::rewrite_fields(value, |key, child| {
            let property = model.properties.iter().find(|p| *p.name == *key)?;
            self.convert_property(child, property, depth + 1)
        })
    }

    fn convert_property(
        &self,
        value: &Variable,
        property: &Property,
        depth: usize,
    ) -> Option<Variable> {
        let convert_one = |item: &Variable| match &property.kind {
            PropertyTypeIr::Date => match item {
                Variable::String(text) => zen_expression::DateValue::from_text(text),
                _ => None,
            },
            PropertyTypeIr::Relationship { target } if self.entities.contains_key(target) => {
                self.convert_entity(item, target, depth)
            }
            _ => None,
        };
        match property.array {
            true => DeclaredDates::rewrite_items(value, convert_one),
            false => convert_one(value),
        }
    }
}

enum Segment {
    Key(Symbol),
    Name(Arc<str>),
    Index(usize),
}

struct InputValidator<'a> {
    entities: &'a HashMap<Arc<str>, Arc<DataModelIr>>,
    dictionaries: &'a HashMap<Arc<str>, Arc<DictionaryIr>>,
    ref_pools: &'a RefPoolIndex,
    errors: Vec<InputValidationError>,
    depth: usize,
    path: Vec<Segment>,
}

impl InputValidator<'_> {
    fn rendered(&self) -> String {
        let mut out = String::new();
        for (i, segment) in self.path.iter().enumerate() {
            match segment {
                Segment::Key(key) => {
                    if i > 0 {
                        out.push('.');
                    }
                    out.push_str(key.as_ref());
                }
                Segment::Name(name) => {
                    out.push('.');
                    out.push_str(name);
                }
                Segment::Index(index) => {
                    out.push('[');
                    out.push_str(&index.to_string());
                    out.push(']');
                }
            }
        }
        out
    }

    fn fail(&mut self, expected: String, got: String) {
        let path = self.rendered();
        self.errors.push(InputValidationError { path, expected, got });
    }

    fn validate_entity(&mut self, value: &Variable, entity_name: &str) {
        if self.depth >= MAX_RECURSION_DEPTH {
            self.fail(
                format!("entity nesting within {MAX_RECURSION_DEPTH} levels"),
                "deeper".into(),
            );
            return;
        }
        let Some(obj) = value.as_object() else {
            self.fail(format!("object ({entity_name})"), value.type_name().into());
            return;
        };
        let Some(dm) = self.entities.get(entity_name) else {
            return;
        };

        self.depth += 1;
        let dm_props = dm.clone();
        for (key, val) in obj.borrow().iter() {
            if matches!(val, Variable::Null) {
                continue;
            }
            let Some(prop) = dm_props
                .properties
                .iter()
                .find(|p| *p.name == *key.as_str())
            else {
                continue;
            };
            self.path.push(Segment::Name(prop.name.clone()));
            if prop.array {
                self.validate_array_of_property(val, prop);
            } else {
                self.validate_kind(val, &prop.kind);
            }
            self.path.pop();
        }
        self.depth -= 1;
    }

    fn validate_global(&mut self, value: &Variable, prop: &Property) {
        if prop.array {
            self.validate_array_of_property(value, prop);
        } else {
            self.validate_kind(value, &prop.kind);
        }
    }

    fn validate_array_of_property(&mut self, value: &Variable, prop: &Property) {
        let Some(arr) = value.as_array() else {
            self.fail(format!("array of {}", prop.kind), value.type_name().into());
            return;
        };
        for (i, item) in arr.borrow().iter().enumerate() {
            if matches!(item, Variable::Null) {
                continue;
            }
            self.path.push(Segment::Index(i));
            self.validate_kind(item, &prop.kind);
            self.path.pop();
        }
    }

    fn validate_array_of_entity(&mut self, value: &Variable, entity_name: &str) {
        let Some(arr) = value.as_array() else {
            self.fail(format!("array of {entity_name}"), value.type_name().into());
            return;
        };
        for (i, item) in arr.borrow().iter().enumerate() {
            if matches!(item, Variable::Null) {
                continue;
            }
            self.path.push(Segment::Index(i));
            self.validate_entity(item, entity_name);
            self.path.pop();
        }
    }

    fn validate_kind(&mut self, value: &Variable, kind: &PropertyTypeIr) {
        let ok = match kind {
            PropertyTypeIr::String => matches!(value, Variable::String(_)),
            PropertyTypeIr::Enum(values) => {
                self.validate_enum(value, values);
                return;
            }
            PropertyTypeIr::Number => matches!(value, Variable::Number(_)),
            PropertyTypeIr::Boolean => matches!(value, Variable::Bool(_)),
            PropertyTypeIr::Date => match value {
                Variable::String(text) => {
                    text.is_empty() || zen_expression::DateValue::is_text(text)
                }
                other => zen_expression::DateValue::is(other),
            },
            PropertyTypeIr::Reference { target } => {
                self.validate_reference(value, target);
                return;
            }
            PropertyTypeIr::Relationship { target } => {
                if !self.entities.contains_key(target) {
                    if let Some(dict) = self.dictionaries.get(target) {
                        let values: Vec<Arc<str>> = dict.values().cloned().collect();
                        self.validate_enum(value, &values);
                        return;
                    }
                }
                self.validate_entity(value, target);
                return;
            }
        };
        if !ok {
            self.fail(kind.to_string(), value.type_name().into());
        }
    }

    fn validate_enum(&mut self, value: &Variable, values: &[Arc<str>]) {
        let Some(s) = value.as_rc_str() else {
            self.fail(
                format!(
                    "one of {}",
                    values
                        .iter()
                        .map(|v| format!("'{v}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                value.type_name().into(),
            );
            return;
        };
        if !values.iter().any(|v| v.as_ref() == s.as_ref()) {
            self.fail(
                format!(
                    "one of {}",
                    values
                        .iter()
                        .map(|v| format!("'{v}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                format!("'{s}'"),
            );
        }
    }

    fn validate_reference(&mut self, value: &Variable, target: &Arc<str>) {
        let Some(id) = value.as_rc_str() else {
            self.fail(
                format!("reference id (string → {target})"),
                value.type_name().into(),
            );
            return;
        };
        if !self.ref_pools.contains(target, &id) {
            self.fail(
                format!("reference id present in '{target}' pool"),
                format!("'{id}' (not found)"),
            );
        }
    }
}
