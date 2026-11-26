use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::variable::Variable;

use crate::compiled::host::input::dates::DeclaredDates;
use crate::compiled::leaf::Dating;
use crate::policy::ir::{DataModelIr, DictionaryIr, Property, PropertyTypeIr};
use crate::policy::refs::RefPoolIndex;
use crate::policy::MAX_RECURSION_DEPTH;
use crate::workspace::db::Db;
use crate::workspace::types::InputValidationError;
use zen_types::symbol::Symbol;

impl InputSchema {
    pub(crate) fn of(db: &Db, policy_path: &str) -> InputSchema {
        let entities = db.visible_entities(policy_path);
        let globals = Self::visible_globals(db, policy_path);
        let visible_dms = Self::visible_data_models(db, policy_path);
        let (roots, ref_targets) =
            DataModelIr::classify_roots(visible_dms.iter().map(|dm| dm.as_ref()));
        let dated = InputSchema::dated(&entities);
        InputSchema {
            entities,
            globals,
            roots,
            ref_targets,
            dated: Arc::new(dated),
            dictionaries: db.unit(policy_path).dictionaries.clone(),
        }
    }

    fn visible_globals(db: &Db, policy_path: &str) -> HashMap<Arc<str>, Property> {
        let visible = db.visible_policies(policy_path);
        let mut sorted: Vec<Arc<str>> = visible.iter().cloned().collect();
        sorted.sort();
        let mut out: HashMap<Arc<str>, Property> = HashMap::new();
        for pp in &sorted {
            let Some(parsed) = db.parsed(pp) else {
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

    fn visible_data_models(db: &Db, policy_path: &str) -> Vec<Arc<DataModelIr>> {
        let entities = db.visible_entities(policy_path);
        let visible = db.visible_policies(policy_path);
        let mut sorted: Vec<Arc<str>> = visible.iter().cloned().collect();
        sorted.sort();
        let mut out: Vec<Arc<str>> = entities.values().map(|d| d.name.clone()).collect();
        out.sort();
        let mut result: Vec<Arc<DataModelIr>> = out
            .into_iter()
            .filter_map(|name| entities.get(&name).cloned())
            .collect();
        for pp in &sorted {
            let Some(parsed) = db.parsed(pp) else {
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
    dated: Arc<HashSet<Arc<str>>>,
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
            dates: None,
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

    pub(crate) fn convert_dates(
        &self,
        input: &Variable,
        tz: zen_expression::context::Timezone,
    ) -> Option<Variable> {
        self.convert_input(input, &DateTexts::plain(tz))
    }

    fn dates(&self) -> Dates<'_> {
        Dates {
            entities: &self.entities,
            dated: &self.dated,
        }
    }

    fn convert_input(&self, input: &Variable, texts: &DateTexts) -> Option<Variable> {
        let dates = self.dates();
        DeclaredDates::rewrite_fields(input, |key, value| {
            if self.ref_targets.contains(key) {
                DeclaredDates::rewrite_items(value, |item| dates.convert_entity(item, key, 0, texts))
            } else if self.roots.contains(key) {
                dates.convert_entity(value, key, 0, texts)
            } else {
                dates.convert_property(value, self.globals.get(key)?, 0, texts)
            }
        })
    }

    pub(crate) fn root(&self, name: &str) -> bool {
        self.roots.contains(name)
    }

    pub(crate) fn has_dated(&self, entity: &str) -> bool {
        self.dated.contains(entity)
    }

    pub(crate) fn has_dates(&self) -> bool {
        !self.dated.is_empty()
            || self
                .globals
                .values()
                .any(|property| matches!(property.kind, PropertyTypeIr::Date))
    }

    pub(crate) fn dates_at(&self, path: &str) -> Option<DatesAt> {
        let mut segments = path.split('.');
        let first = segments.next()?;
        let mut at = if self.ref_targets.contains(first) {
            DatesAt::Items(Arc::from(first), 0)
        } else if self.roots.contains(first) {
            DatesAt::Entity(Arc::from(first), 0)
        } else {
            DatesAt::Property(self.globals.get(first)?.clone(), 0)
        };
        let dates = self.dates();
        for segment in segments {
            at = dates.step(&at, segment)?;
        }
        dates.holds(&at).then_some(at)
    }

    pub(crate) fn dating(&self, at: DatesAt, texts: &Rc<DateTexts>) -> Rc<dyn Dating> {
        Rc::new(DatingAt {
            entities: self.entities.clone(),
            dated: self.dated.clone(),
            at,
            texts: texts.clone(),
        })
    }
}

struct Dates<'s> {
    entities: &'s HashMap<Arc<str>, Arc<DataModelIr>>,
    dated: &'s HashSet<Arc<str>>,
}

impl Dates<'_> {
    fn convert_entity(
        &self,
        value: &Variable,
        entity: &str,
        depth: usize,
        texts: &DateTexts,
    ) -> Option<Variable> {
        if depth >= MAX_RECURSION_DEPTH || !self.dated.contains(entity) {
            return None;
        }
        let model = self.entities.get(entity)?;
        DeclaredDates::rewrite_fields(value, |key, child| {
            let property = model.properties.iter().find(|p| *p.name == *key)?;
            self.convert_property(child, property, depth + 1, texts)
        })
    }

    fn convert_property(
        &self,
        value: &Variable,
        property: &Property,
        depth: usize,
        texts: &DateTexts,
    ) -> Option<Variable> {
        let convert_one = |item: &Variable| match &property.kind {
            PropertyTypeIr::Date => match item {
                Variable::String(text) => texts.read(text),
                _ => None,
            },
            PropertyTypeIr::Relationship { target } if self.entities.contains_key(target) => {
                self.convert_entity(item, target, depth, texts)
            }
            _ => None,
        };
        match property.array {
            true => DeclaredDates::rewrite_items(value, convert_one),
            false => convert_one(value),
        }
    }

    fn convert_at(&self, at: &DatesAt, value: &Variable, texts: &DateTexts) -> Option<Variable> {
        match at {
            DatesAt::Entity(name, depth) => self.convert_entity(value, name, *depth, texts),
            DatesAt::Items(name, depth) => DeclaredDates::rewrite_items(value, |item| {
                self.convert_entity(item, name, *depth, texts)
            }),
            DatesAt::Property(property, depth) => {
                self.convert_property(value, property, *depth, texts)
            }
        }
    }

    fn step(&self, at: &DatesAt, segment: &str) -> Option<DatesAt> {
        let (entity, depth) = match at {
            DatesAt::Entity(name, depth) | DatesAt::Items(name, depth) => (name.clone(), *depth),
            DatesAt::Property(property, depth) => match &property.kind {
                PropertyTypeIr::Relationship { target } => (target.clone(), *depth),
                _ => return None,
            },
        };
        if depth >= MAX_RECURSION_DEPTH || !self.dated.contains(&entity) {
            return None;
        }
        let model = self.entities.get(&entity)?;
        let property = model.properties.iter().find(|p| *p.name == *segment)?;
        Some(DatesAt::Property(property.clone(), depth + 1))
    }

    fn holds(&self, at: &DatesAt) -> bool {
        match at {
            DatesAt::Entity(name, depth) | DatesAt::Items(name, depth) => {
                *depth < MAX_RECURSION_DEPTH && self.dated.contains(name)
            }
            DatesAt::Property(property, _) => match &property.kind {
                PropertyTypeIr::Date => true,
                PropertyTypeIr::Relationship { target } => {
                    self.entities.contains_key(target) && self.dated.contains(target)
                }
                _ => false,
            },
        }
    }
}

struct DatingAt {
    entities: Arc<HashMap<Arc<str>, Arc<DataModelIr>>>,
    dated: Arc<HashSet<Arc<str>>>,
    at: DatesAt,
    texts: Rc<DateTexts>,
}

impl DatingAt {
    fn dates(&self) -> Dates<'_> {
        Dates {
            entities: &self.entities,
            dated: &self.dated,
        }
    }
}

impl Dating for DatingAt {
    fn convert(&self, value: &Variable) -> Option<Variable> {
        self.dates().convert_at(&self.at, value, &self.texts)
    }

    fn single(&self) -> bool {
        match &self.at {
            DatesAt::Property(property, _) => {
                !property.array && matches!(property.kind, PropertyTypeIr::Date)
            }
            _ => false,
        }
    }

    fn listed(&self) -> bool {
        match &self.at {
            DatesAt::Items(..) => true,
            DatesAt::Property(property, _) => {
                property.array && matches!(property.kind, PropertyTypeIr::Relationship { .. })
            }
            DatesAt::Entity(..) => false,
        }
    }

    fn field(&self, name: &str) -> Option<Rc<dyn Dating>> {
        let dates = self.dates();
        let at = dates.step(&self.at, name).filter(|at| dates.holds(at))?;
        Some(Rc::new(DatingAt {
            entities: self.entities.clone(),
            dated: self.dated.clone(),
            at,
            texts: self.texts.clone(),
        }))
    }
}

pub(crate) struct DateTexts {
    tz: zen_expression::context::Timezone,
    seen: Option<RefCell<HashMap<Box<str>, Option<Variable>>>>,
}

impl DateTexts {
    const LIMIT: usize = 8192;

    fn plain(tz: zen_expression::context::Timezone) -> Self {
        Self { tz, seen: None }
    }

    pub(crate) fn remembered(tz: zen_expression::context::Timezone) -> Self {
        Self {
            tz,
            seen: Some(RefCell::new(HashMap::new())),
        }
    }

    pub(crate) fn read(&self, text: &str) -> Option<Variable> {
        let Some(seen) = &self.seen else {
            return zen_expression::DateValue::from_text_in(text, self.tz);
        };
        let mut seen = seen.borrow_mut();
        if let Some(known) = seen.get(text) {
            return known.clone();
        }
        let value = zen_expression::DateValue::from_text_in(text, self.tz);
        if seen.len() < Self::LIMIT {
            seen.insert(Box::from(text), value.clone());
        }
        value
    }
}

pub(crate) enum DatesAt {
    Entity(Arc<str>, usize),
    Items(Arc<str>, usize),
    Property(Property, usize),
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
    dates: Option<&'a RefCell<HashMap<Box<str>, bool>>>,
}

impl InputValidator<'_> {
    const REMEMBERED: usize = 8192;

    fn date(&self, text: &str) -> bool {
        let Some(seen) = self.dates else {
            return zen_expression::DateValue::is_text(text);
        };
        let mut seen = seen.borrow_mut();
        if let Some(known) = seen.get(text) {
            return *known;
        }
        let known = zen_expression::DateValue::is_text(text);
        if seen.len() < Self::REMEMBERED {
            seen.insert(Box::from(text), known);
        }
        known
    }

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
        self.errors.push(InputValidationError {
            path,
            expected,
            got,
        });
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
                Variable::String(text) => text.is_empty() || self.date(text),
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

struct Sureness<'s> {
    schema: &'s InputSchema,
    rows: usize,
    sure: Vec<bool>,
    resolved: bool,
    dates: RefCell<HashMap<Box<str>, bool>>,
}

impl Sureness<'_> {
    fn number(column: &zen_expression::lane::Column) -> bool {
        use zen_expression::lane::{Dictionary, Values};
        matches!(
            column.values,
            Values::Dec(_)
                | Values::Scaled { .. }
                | Values::I64(_)
                | Values::F64(_)
                | Values::Dict {
                    values: Dictionary::Scaled { .. },
                    ..
                }
        )
    }

    fn string(column: &zen_expression::lane::Column) -> bool {
        use zen_expression::lane::{Dictionary, Values};
        matches!(
            column.values,
            Values::Utf8 { .. }
                | Values::Text { .. }
                | Values::LargeUtf8 { .. }
                | Values::Strs(_)
                | Values::Dict {
                    values: Dictionary::Text { .. },
                    ..
                }
        )
    }

    fn boolean(column: &zen_expression::lane::Column) -> bool {
        use zen_expression::lane::{Dictionary, Values};
        matches!(
            column.values,
            Values::Bool { .. }
                | Values::Dict {
                    values: Dictionary::Bool { .. },
                    ..
                }
        )
    }

    fn scalar(
        &self,
        kind: &PropertyTypeIr,
        column: &zen_expression::lane::Column,
        at: usize,
    ) -> bool {
        if !column.valid(at) {
            return true;
        }
        match kind {
            PropertyTypeIr::Number => {
                Self::number(column)
                    || matches!(column.variable(at), Variable::Number(_) | Variable::Null)
            }
            PropertyTypeIr::String => {
                Self::string(column)
                    || matches!(column.variable(at), Variable::String(_) | Variable::Null)
            }
            PropertyTypeIr::Boolean => {
                Self::boolean(column)
                    || matches!(column.variable(at), Variable::Bool(_) | Variable::Null)
            }
            PropertyTypeIr::Enum(values) if Self::string(column) => column
                .text(at)
                .is_none_or(|text| values.iter().any(|v| v.as_ref() == text)),
            PropertyTypeIr::Enum(values) => match column.variable(at) {
                Variable::Null => true,
                Variable::String(s) => values.iter().any(|v| v.as_ref() == s.as_str()),
                _ => false,
            },
            PropertyTypeIr::Relationship { target } => self.objects(target, column, at),
            PropertyTypeIr::Date if Self::string(column) => column
                .text(at)
                .is_none_or(|text| text.is_empty() || zen_expression::DateValue::reads(text)),
            PropertyTypeIr::Date => match column.variable(at) {
                Variable::Null => true,
                Variable::String(text) => {
                    text.is_empty() || zen_expression::DateValue::reads(&text)
                }
                other => zen_expression::DateValue::is(&other),
            },
            PropertyTypeIr::Reference { .. } => {
                self.resolved
                    && matches!(column.variable(at), Variable::String(_) | Variable::Null)
            }
        }
    }

    fn objects(&self, target: &str, column: &zen_expression::lane::Column, at: usize) -> bool {
        use zen_expression::lane::Values;
        if !self.schema.entities.contains_key(target) {
            return match (self.schema.dictionaries.get(target), column.variable(at)) {
                (Some(_), Variable::Null) => true,
                (Some(dict), Variable::String(text)) => dict.values().any(|value| **value == *text),
                _ => false,
            };
        }
        match column.values {
            Values::Struct { fields, .. } => self.structs(target, fields, at),
            _ => self.loose(target, &column.variable(at)),
        }
    }

    fn loose_items(&self, prop: &Property, value: &Variable) -> bool {
        match value {
            Variable::Null => return true,
            Variable::Array(items)
                if items
                    .borrow()
                    .iter()
                    .all(|item| Self::shallow(item, MAX_RECURSION_DEPTH / 2)) => {}
            _ => return false,
        }
        if matches!(prop.kind, PropertyTypeIr::Reference { .. }) {
            return false;
        }
        let pools = RefPoolIndex::from_input(&Variable::Null, std::iter::empty());
        let mut validator = InputValidator {
            entities: &self.schema.entities,
            dictionaries: &self.schema.dictionaries,
            ref_pools: &pools,
            errors: Vec::new(),
            depth: 0,
            path: Vec::new(),
            dates: Some(&self.dates),
        };
        validator.validate_array_of_property(value, prop);
        validator.errors.is_empty()
    }

    fn shallow(value: &Variable, limit: usize) -> bool {
        if limit == 0 {
            return false;
        }
        match value {
            Variable::Object(map) => map.borrow().values().all(|v| Self::shallow(v, limit - 1)),
            Variable::Array(items) => items.borrow().iter().all(|v| Self::shallow(v, limit - 1)),
            _ => true,
        }
    }

    fn loose(&self, target: &str, value: &Variable) -> bool {
        const SHALLOW: usize = MAX_RECURSION_DEPTH / 2;
        fn nesting(value: &Variable, depth: usize) -> usize {
            if depth > SHALLOW {
                return depth;
            }
            match value {
                Variable::Object(map) => map
                    .borrow()
                    .values()
                    .map(|v| nesting(v, depth + 1))
                    .max()
                    .unwrap_or(depth + 1),
                Variable::Array(items) => items
                    .borrow()
                    .iter()
                    .map(|v| nesting(v, depth + 1))
                    .max()
                    .unwrap_or(depth + 1),
                _ => depth,
            }
        }
        match value {
            Variable::Null => return true,
            Variable::Object(_) if nesting(value, 0) <= SHALLOW => {}
            _ => return false,
        }
        let pools = RefPoolIndex::from_input(&Variable::Null, std::iter::empty());
        let mut validator = InputValidator {
            entities: &self.schema.entities,
            dictionaries: &self.schema.dictionaries,
            ref_pools: &pools,
            errors: Vec::new(),
            depth: 0,
            path: Vec::new(),
            dates: Some(&self.dates),
        };
        validator.validate_entity(value, target);
        validator.errors.is_empty()
    }

    fn structs(
        &self,
        target: &str,
        fields: &[(&str, zen_expression::lane::Column)],
        at: usize,
    ) -> bool {
        let Some(model) = self.schema.entities.get(target) else {
            return false;
        };
        fields.iter().all(|(name, column)| {
            match model.properties.iter().find(|p| p.name.as_ref() == *name) {
                None => true,
                Some(prop) => self.property(prop, column, at),
            }
        })
    }

    fn property(&self, prop: &Property, column: &zen_expression::lane::Column, at: usize) -> bool {
        use zen_expression::lane::{Dictionary, Values};
        if !column.valid(at) {
            return true;
        }
        match prop.array {
            false => self.scalar(&prop.kind, column, at),
            true => {
                let Values::List { child, .. } = column.values else {
                    return self.loose_items(prop, &column.variable(at));
                };
                let Some((a, b)) = column.range(at) else {
                    return true;
                };
                let child = match child {
                    Dictionary::Column(child) => *child,
                    other => other.column(),
                };
                (a..b).all(|item| self.scalar(&prop.kind, &child, item))
            }
        }
    }

    fn merged(into: &mut Option<Vec<bool>>, found: Option<Vec<bool>>) {
        match (into.as_mut(), found) {
            (_, None) => {}
            (None, found) => *into = found,
            (Some(bad), Some(found)) => bad
                .iter_mut()
                .zip(found)
                .for_each(|(bad, found)| *bad |= found),
        }
    }

    fn kept(bad: Vec<bool>) -> Option<Vec<bool>> {
        bad.contains(&true).then_some(bad)
    }

    fn unsure(
        &self,
        kind: &PropertyTypeIr,
        column: &zen_expression::lane::Column,
        len: usize,
    ) -> Option<Vec<bool>> {
        use zen_expression::lane::Values;
        if self.kinded(kind, column) {
            return None;
        }
        match (kind, column.values) {
            (PropertyTypeIr::Relationship { target }, Values::Struct { fields, .. })
                if self.schema.entities.contains_key(target) =>
            {
                let model = self.schema.entities.get(target)?;
                let mut bad: Option<Vec<bool>> = None;
                for (name, field) in fields {
                    if let Some(prop) = model.properties.iter().find(|p| p.name.as_ref() == *name)
                    {
                        Self::merged(&mut bad, self.doubts(prop, field, len));
                    }
                }
                let mut bad = bad?;
                if column.validity.is_some() {
                    bad.iter_mut()
                        .enumerate()
                        .for_each(|(at, bad)| *bad &= column.valid(at));
                }
                Self::kept(bad)
            }
            (PropertyTypeIr::Reference { .. }, _) => {
                Self::kept((0..len).map(|at| !self.scalar(kind, column, at)).collect())
            }
            (_, Values::Dict { keys, values }) => {
                let coded = values.column();
                let mut known: Vec<Option<bool>> = vec![None; coded.len()];
                Self::kept(
                    (0..len)
                        .map(|at| {
                            if !column.valid(at) {
                                return false;
                            }
                            let code = keys.get(at).and_then(|key| usize::try_from(*key).ok());
                            match code.and_then(|code| known.get_mut(code).map(|slot| (code, slot)))
                            {
                                Some((code, slot)) => {
                                    *slot.get_or_insert_with(|| !self.scalar(kind, &coded, code))
                                }
                                None => !self.scalar(kind, column, at),
                            }
                        })
                        .collect(),
                )
            }
            _ => Self::kept((0..len).map(|at| !self.scalar(kind, column, at)).collect()),
        }
    }

    fn doubts(
        &self,
        prop: &Property,
        column: &zen_expression::lane::Column,
        len: usize,
    ) -> Option<Vec<bool>> {
        use zen_expression::lane::{Dictionary, Values};
        match (prop.array, column.values) {
            (false, _) => self.unsure(&prop.kind, column, len),
            (true, Values::List { child, .. }) => {
                let child = match child {
                    Dictionary::Column(child) => *child,
                    other => other.column(),
                };
                let inner = self.unsure(&prop.kind, &child, child.len())?;
                Self::kept(
                    (0..len)
                        .map(|at| {
                            column.range(at).is_some_and(|(a, b)| {
                                inner.get(a..b).map_or_else(
                                    || (a..b).any(|item| !self.scalar(&prop.kind, &child, item)),
                                    |items| items.contains(&true),
                                )
                            })
                        })
                        .collect(),
                )
            }
            (true, _) => {
                Self::kept((0..len).map(|at| !self.property(prop, column, at)).collect())
            }
        }
    }

    fn typed(&self, prop: &Property, column: &zen_expression::lane::Column) -> bool {
        use zen_expression::lane::{Dictionary, Values};
        match prop.array {
            false => self.kinded(&prop.kind, column),
            true => match column.values {
                Values::List { child, .. } => {
                    let child = match child {
                        Dictionary::Column(child) => *child,
                        other => other.column(),
                    };
                    self.kinded(&prop.kind, &child)
                }
                _ => false,
            },
        }
    }

    fn kinded(&self, kind: &PropertyTypeIr, column: &zen_expression::lane::Column) -> bool {
        use zen_expression::lane::Values;
        match kind {
            PropertyTypeIr::Number => Self::number(column),
            PropertyTypeIr::String => Self::string(column),
            PropertyTypeIr::Boolean => Self::boolean(column),
            PropertyTypeIr::Relationship { target } => {
                match (column.values, self.schema.entities.get(target)) {
                    (Values::Struct { fields, .. }, Some(model)) => {
                        fields.iter().all(|(name, field)| {
                            match model.properties.iter().find(|p| p.name.as_ref() == *name) {
                                None => true,
                                Some(prop) => self.typed(prop, field),
                            }
                        })
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn pool(&self, target: &str, column: &zen_expression::lane::Column, row: usize) -> bool {
        use zen_expression::lane::{Dictionary, Values};
        let Values::List { child, .. } = column.values else {
            return false;
        };
        let Some((a, b)) = column.range(row) else {
            return true;
        };
        let child = match child {
            Dictionary::Column(child) => *child,
            other => other.column(),
        };
        (a..b).all(|item| !child.valid(item) || self.objects(target, &child, item))
    }

    fn mark(&mut self, column: &zen_expression::lane::Column, test: impl Fn(&Self, usize) -> bool) {
        for row in 0..self.rows {
            if self.sure[row] && column.valid(row) && !test(self, row) {
                self.sure[row] = false;
            }
        }
    }

    fn path(&mut self, path: &str, column: &zen_expression::lane::Column) -> Option<()> {
        let segments: Vec<&str> = path.split('.').collect();
        let (first, rest) = segments.split_first()?;
        if self.schema.ref_targets.contains(*first) {
            if !self.resolved || !rest.is_empty() {
                return None;
            }
            let target: Arc<str> = Arc::from(*first);
            self.mark(column, |s, row| s.pool(&target, column, row));
            return Some(());
        }
        if self.schema.roots.contains(*first) {
            let mut entity: Arc<str> = Arc::from(*first);
            for (depth, segment) in rest.iter().enumerate() {
                let model = self.schema.entities.get(&entity)?.clone();
                let Some(prop) = model
                    .properties
                    .iter()
                    .find(|p| p.name.as_ref() == *segment)
                else {
                    return Some(());
                };
                if depth + 1 == rest.len() {
                    if let Some(bad) = self.doubts(prop, column, self.rows) {
                        self.sure
                            .iter_mut()
                            .zip(bad)
                            .for_each(|(sure, bad)| *sure &= !bad);
                    }
                    return Some(());
                }
                match (&prop.kind, prop.array) {
                    (PropertyTypeIr::Relationship { target }, false)
                        if self.schema.entities.contains_key(target) =>
                    {
                        entity = target.clone()
                    }
                    _ => {
                        self.mark(column, |_, _| false);
                        return Some(());
                    }
                }
            }
            let target = entity.clone();
            self.mark(column, |s, row| s.objects(&target, column, row));
            return Some(());
        }
        if let Some(prop) = self.schema.globals.get(*first) {
            match rest.is_empty() {
                true if self.typed(prop, column) => {}
                true => self.mark(column, |s, row| s.property(prop, column, row)),
                false => self.mark(column, |_, _| false),
            }
        }
        Some(())
    }
}

impl InputSchema {
    pub(crate) fn sure(
        &self,
        columns: &zen_expression::lane::Columns,
        converted: bool,
        resolved: bool,
    ) -> Option<Vec<bool>> {
        let dated_global = self.globals.values().any(|p| match &p.kind {
            PropertyTypeIr::Date => true,
            PropertyTypeIr::Relationship { target } => self.dated.contains(target),
            _ => false,
        });
        if (!resolved && !self.ref_targets.is_empty())
            || (!converted && (!self.dated.is_empty() || dated_global))
        {
            return None;
        }
        let mut sureness = Sureness {
            schema: self,
            rows: columns.rows,
            sure: vec![true; columns.rows],
            resolved,
            dates: RefCell::new(HashMap::new()),
        };
        for (path, column) in &columns.columns {
            sureness.path(path, column)?;
        }
        Some(sureness.sure)
    }
}
