use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::variable::Variable;

use crate::policy::ir::{DataModelIr, DictionaryIr, Property, PropertyTypeIr};
use crate::policy::refs::RefPoolIndex;
use crate::policy::MAX_RECURSION_DEPTH;
use crate::workspace::db::Db;
use crate::workspace::types::InputValidationError;

impl Db {
    pub(crate) fn input_schema(&self, policy_path: &str) -> InputSchema {
        let entities = self.visible_entities(policy_path);
        let globals = self.visible_globals(policy_path);
        let visible_dms = self.visible_data_models(policy_path);
        let (roots, ref_targets) =
            DataModelIr::classify_roots(visible_dms.iter().map(|dm| dm.as_ref()));
        InputSchema {
            entities,
            globals,
            roots,
            ref_targets,
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
            if self.ref_targets.contains(key_str) {
                validator.validate_array_of_entity(val, key_str, key_str.to_string());
            } else if self.roots.contains(key_str) {
                validator.validate_entity(val, key_str, key_str.to_string());
            } else if let Some(prop) = self.globals.get(key_str) {
                validator.validate_global(val, prop, key_str.to_string());
            }
        }

        validator.errors
    }
}

struct InputValidator<'a> {
    entities: &'a HashMap<Arc<str>, Arc<DataModelIr>>,
    dictionaries: &'a HashMap<Arc<str>, Arc<DictionaryIr>>,
    ref_pools: &'a RefPoolIndex,
    errors: Vec<InputValidationError>,
    depth: usize,
}

impl InputValidator<'_> {
    fn validate_entity(&mut self, value: &Variable, entity_name: &str, path: String) {
        if self.depth >= MAX_RECURSION_DEPTH {
            self.errors.push(InputValidationError {
                path,
                expected: format!("entity nesting within {MAX_RECURSION_DEPTH} levels"),
                got: "deeper".into(),
            });
            return;
        }
        let Some(obj) = value.as_object() else {
            self.errors.push(InputValidationError {
                path,
                expected: format!("object ({entity_name})"),
                got: value.type_name().into(),
            });
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
                .find(|p| p.name.as_ref() == key.as_ref())
            else {
                continue;
            };
            let child_path = format!("{path}.{}", prop.name);
            if prop.array {
                self.validate_array_of_property(val, prop, child_path);
            } else {
                self.validate_kind(val, &prop.kind, child_path);
            }
        }
        self.depth -= 1;
    }

    fn validate_global(&mut self, value: &Variable, prop: &Property, path: String) {
        if prop.array {
            self.validate_array_of_property(value, prop, path);
        } else {
            self.validate_kind(value, &prop.kind, path);
        }
    }

    fn validate_array_of_property(&mut self, value: &Variable, prop: &Property, path: String) {
        let Some(arr) = value.as_array() else {
            self.errors.push(InputValidationError {
                path,
                expected: format!("array of {}", prop.kind),
                got: value.type_name().into(),
            });
            return;
        };
        for (i, item) in arr.borrow().iter().enumerate() {
            if matches!(item, Variable::Null) {
                continue;
            }
            self.validate_kind(item, &prop.kind, format!("{path}[{i}]"));
        }
    }

    fn validate_array_of_entity(&mut self, value: &Variable, entity_name: &str, path: String) {
        let Some(arr) = value.as_array() else {
            self.errors.push(InputValidationError {
                path,
                expected: format!("array of {entity_name}"),
                got: value.type_name().into(),
            });
            return;
        };
        for (i, item) in arr.borrow().iter().enumerate() {
            if matches!(item, Variable::Null) {
                continue;
            }
            self.validate_entity(item, entity_name, format!("{path}[{i}]"));
        }
    }

    fn validate_kind(&mut self, value: &Variable, kind: &PropertyTypeIr, path: String) {
        let ok = match kind {
            PropertyTypeIr::String => matches!(value, Variable::String(_)),
            PropertyTypeIr::Enum(values) => {
                self.validate_enum(value, values, path);
                return;
            }
            PropertyTypeIr::Number => matches!(value, Variable::Number(_)),
            PropertyTypeIr::Boolean => matches!(value, Variable::Bool(_)),
            PropertyTypeIr::Date => matches!(value, Variable::String(_)),
            PropertyTypeIr::Reference { target } => {
                self.validate_reference(value, target, path);
                return;
            }
            PropertyTypeIr::Relationship { target } => {
                if !self.entities.contains_key(target) {
                    if let Some(dict) = self.dictionaries.get(target) {
                        let values: Vec<Arc<str>> = dict.values().cloned().collect();
                        self.validate_enum(value, &values, path);
                        return;
                    }
                }
                self.validate_entity(value, target, path);
                return;
            }
        };
        if !ok {
            self.errors.push(InputValidationError {
                path,
                expected: kind.to_string(),
                got: value.type_name().into(),
            });
        }
    }

    fn validate_enum(&mut self, value: &Variable, values: &[Arc<str>], path: String) {
        let Some(s) = value.as_rc_str() else {
            self.errors.push(InputValidationError {
                path,
                expected: format!(
                    "one of {}",
                    values
                        .iter()
                        .map(|v| format!("'{v}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                got: value.type_name().into(),
            });
            return;
        };
        if !values.iter().any(|v| v.as_ref() == s.as_ref()) {
            self.errors.push(InputValidationError {
                path,
                expected: format!(
                    "one of {}",
                    values
                        .iter()
                        .map(|v| format!("'{v}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                got: format!("'{s}'"),
            });
        }
    }

    fn validate_reference(&mut self, value: &Variable, target: &Arc<str>, path: String) {
        let Some(id) = value.as_rc_str() else {
            self.errors.push(InputValidationError {
                path,
                expected: format!("reference id (string → {target})"),
                got: value.type_name().into(),
            });
            return;
        };
        if !self.ref_pools.contains(target, &id) {
            self.errors.push(InputValidationError {
                path,
                expected: format!("reference id present in '{target}' pool"),
                got: format!("'{id}' (not found)"),
            });
        }
    }
}
