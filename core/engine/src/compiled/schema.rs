use crate::compiled::data::Data;
use crate::compiled::typed::{Bits, Leaf};
use serde_json::Value;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::Values;
use zen_expression::Variable;
use zen_types::variable::VariableMap;

#[derive(Debug, Clone, Copy, PartialEq)]
enum SchemaType {
    Number,
    Integer,
    Boolean,
    String,
    Array,
    Object,
}

#[derive(Debug)]
enum Check {
    Any,
    Type(SchemaType),
    Strings(Vec<String>),
    Object(ObjectSchema),
    Array(Box<Check>),
}

#[derive(Debug)]
struct Property {
    name: String,
    required: bool,
    check: Check,
}

#[derive(Debug)]
pub(crate) struct ObjectSchema {
    properties: Vec<Property>,
    additional: bool,
}

enum Target<'s> {
    Free,
    Check(&'s Check),
    Forbidden,
}

impl Check {
    fn annotation(key: &str) -> bool {
        matches!(
            key,
            "title" | "description" | "$comment" | "default" | "examples" | "readOnly" | "writeOnly"
        )
    }

    fn compile(schema: &Value) -> Option<Self> {
        if schema == &Value::Bool(true) {
            return Some(Self::Any);
        }
        let object = schema.as_object()?;
        let keys: Vec<&str> = object
            .keys()
            .filter(|key| !Self::annotation(key))
            .map(String::as_str)
            .collect();
        if keys.is_empty() {
            return Some(Self::Any);
        }
        if keys == ["type"] {
            return match object["type"].as_str()? {
                "number" => Some(Self::Type(SchemaType::Number)),
                "integer" => Some(Self::Type(SchemaType::Integer)),
                "boolean" => Some(Self::Type(SchemaType::Boolean)),
                "string" => Some(Self::Type(SchemaType::String)),
                "array" => Some(Self::Type(SchemaType::Array)),
                "object" => Some(Self::Type(SchemaType::Object)),
                _ => None,
            };
        }
        if keys.iter().all(|key| matches!(*key, "type" | "enum" | "const"))
            && object.get("type").is_none_or(|ty| ty.as_str() == Some("string"))
        {
            let strings: Option<Vec<String>> = match (object.get("enum"), object.get("const")) {
                (Some(Value::Array(values)), None) => values.iter().map(|v| v.as_str().map(str::to_owned)).collect(),
                (None, Some(Value::String(value))) => Some(vec![value.clone()]),
                _ => None,
            };
            if let Some(mut strings) = strings {
                strings.sort_unstable();
                strings.dedup();
                return Some(Self::Strings(strings));
            }
            return None;
        }
        if let Some(nested) = ObjectSchema::compile(schema) {
            return Some(Self::Object(nested));
        }
        if keys.iter().all(|key| matches!(*key, "type" | "items")) && object.get("type").and_then(Value::as_str) == Some("array") {
            return object.get("items").and_then(Self::compile).map(|items| Self::Array(Box::new(items)));
        }
        None
    }

    fn sure(&self, value: &Variable) -> bool {
        match self {
            Self::Any => true,
            Self::Type(kind) => match (kind, value) {
                (SchemaType::Number, Variable::Number(_)) => true,
                (SchemaType::Integer, Variable::Number(n)) => n.scale() == 0 && i64::try_from(*n).is_ok(),
                (SchemaType::Boolean, Variable::Bool(_)) => true,
                (SchemaType::String, Variable::String(_)) => true,
                (SchemaType::Array, Variable::Array(_)) => true,
                (SchemaType::Object, Variable::Object(_)) => true,
                _ => false,
            },
            Self::Strings(strings) => match value {
                Variable::String(text) => strings.binary_search_by(|s| s.as_str().cmp(text.as_ref())).is_ok(),
                _ => false,
            },
            Self::Object(schema) => match value {
                Variable::Object(map) => schema.sure_map(&map.borrow()),
                _ => false,
            },
            Self::Array(items) => match value {
                Variable::Array(values) => values.borrow().iter().all(|item| items.sure(item)),
                _ => false,
            },
        }
    }

    fn sure_bits(&self, leaf: &Leaf, rows: usize) -> Vec<u64> {
        let column = leaf.column();
        match (self, column.values) {
            (Self::Any, _)
            | (Self::Type(SchemaType::Number), Values::Scaled { .. } | Values::Dec(_) | Values::I64(_))
            | (Self::Type(SchemaType::Integer), Values::I64(_))
            | (Self::Type(SchemaType::Boolean), Values::Bool { .. }) => leaf.validity(rows),
            _ => Bits::of(rows, |row| self.sure_column(&column, row)),
        }
    }

    fn sure_column(&self, column: &zen_expression::lane::Column, row: usize) -> bool {
        if !column.valid(row) {
            return false;
        }
        match (self, column.values) {
            (Self::Any, _) => true,
            (Self::Type(SchemaType::Number), Values::Scaled { .. } | Values::Dec(_) | Values::I64(_)) => true,
            (Self::Type(SchemaType::Number), Values::F64(_)) => column.number(row).is_some(),
            (Self::Type(SchemaType::Integer), Values::Scaled { scale, .. }) => scale.get(row) == Some(&0),
            (Self::Type(SchemaType::Integer), Values::I64(_)) => true,
            (Self::Type(SchemaType::Integer), Values::Dec(_) | Values::F64(_)) => column
                .number(row)
                .is_some_and(|n| n.scale() == 0 && i64::try_from(n).is_ok()),
            (Self::Type(SchemaType::Boolean), Values::Bool { .. }) => true,
            (Self::Type(SchemaType::String), Values::Text { .. } | Values::Utf8 { .. } | Values::Dict { .. }) => {
                column.text(row).is_some()
            }
            (Self::Strings(strings), Values::Text { .. } | Values::Utf8 { .. } | Values::LargeUtf8 { .. } | Values::Dict { .. }) => column
                .bytes(row)
                .is_some_and(|bytes| strings.iter().any(|s| s.len() == bytes.len() && s.as_bytes() == bytes)),
            (Self::Type(SchemaType::Array), Values::List { .. }) => true,
            (Self::Array(items), Values::List { offsets, child }) => {
                let child = child.column();
                match (offsets.get(row), offsets.get(row + 1)) {
                    (Some(a), Some(b)) => (*a as usize..*b as usize).all(|i| items.sure_column(&child, i)),
                    _ => false,
                }
            }
            (_, Values::Any(_)) => column.borrowed(row).is_some_and(|value| self.sure(value)),
            _ => false,
        }
    }
}

type Compiled = (Arc<Value>, Option<Rc<ObjectSchema>>);

impl ObjectSchema {
    thread_local! {
        static CACHE: RefCell<Vec<Compiled>> = const { RefCell::new(Vec::new()) };
    }

    pub fn cached(schema: &Arc<Value>) -> Option<Rc<ObjectSchema>> {
        Self::CACHE.with_borrow_mut(|cache| {
            if let Some((_, compiled)) = cache.iter().find(|(s, _)| Arc::ptr_eq(s, schema)) {
                return compiled.clone();
            }
            let compiled = Self::compile(schema).map(Rc::new);
            if cache.len() >= 64 {
                cache.remove(0);
            }
            cache.push((schema.clone(), compiled.clone()));
            compiled
        })
    }

    fn compile(schema: &Value) -> Option<Self> {
        let object = schema.as_object()?;
        if object.get("type")?.as_str()? != "object"
            || object.keys().any(|key| {
                !Check::annotation(key)
                    && !matches!(key.as_str(), "type" | "properties" | "required" | "additionalProperties" | "$schema")
            })
            || Self::scoped(schema)
        {
            return None;
        }
        let additional = match object.get("additionalProperties") {
            None | Some(Value::Bool(true)) => true,
            Some(Value::Bool(false)) => false,
            _ => return None,
        };
        let mut properties = Vec::new();
        if let Some(fields) = object.get("properties") {
            for (name, schema) in fields.as_object()? {
                properties.push(Property {
                    name: name.clone(),
                    required: false,
                    check: Check::compile(schema)?,
                });
            }
        }
        if let Some(required) = object.get("required") {
            for name in required.as_array()? {
                let name = name.as_str()?;
                match properties.iter_mut().find(|field| field.name == name) {
                    Some(field) => field.required = true,
                    None if !additional => return None,
                    None => properties.push(Property {
                        name: name.to_owned(),
                        required: true,
                        check: Check::Any,
                    }),
                }
            }
        }
        if properties.iter().any(|field| field.name.is_empty() || field.name.contains('.')) {
            return None;
        }
        properties.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Some(Self { properties, additional })
    }

    fn scoped(schema: &Value) -> bool {
        match schema {
            Value::Object(map) => map.iter().any(|(key, value)| {
                matches!(key.as_str(), "$ref" | "$id" | "$anchor" | "$dynamicRef" | "$recursiveRef") || Self::scoped(value)
            }),
            Value::Array(values) => values.iter().any(Self::scoped),
            _ => false,
        }
    }

    fn property(&self, name: &str) -> Option<&Property> {
        self.properties
            .binary_search_by(|field| field.name.as_str().cmp(name))
            .ok()
            .map(|at| &self.properties[at])
    }

    fn sure_map(&self, map: &VariableMap) -> bool {
        self.properties.iter().all(|field| match map.get_str(&field.name) {
            Some(value) => field.check.sure(value),
            None => !field.required,
        }) && (self.additional || map.iter().all(|(key, _)| self.property(key.as_ref()).is_some()))
    }

    fn target<'s>(&'s self, path: &str) -> (Target<'s>, Vec<(String, &'s ObjectSchema)>) {
        let mut levels = vec![(String::new(), self)];
        let mut current = self;
        let mut prefix = String::new();
        let mut segments = path.split('.').peekable();
        while let Some(segment) = segments.next() {
            let Some(property) = current.property(segment) else {
                return match current.additional {
                    true => (Target::Free, levels),
                    false => (Target::Forbidden, levels),
                };
            };
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(segment);
            if segments.peek().is_none() {
                return (Target::Check(&property.check), levels);
            }
            match &property.check {
                Check::Object(nested) => {
                    levels.push((prefix.clone(), nested));
                    current = nested;
                }
                Check::Any | Check::Type(SchemaType::Object) => return (Target::Free, levels),
                _ => return (Target::Forbidden, levels),
            }
        }
        (Target::Free, levels)
    }

    pub fn sure(&self, data: &Data) -> Option<Vec<bool>> {
        let leaves = data.leaves();
        let rows = data.len();
        if leaves.is_empty() || leaves.iter().any(|(path, _)| path.is_empty() || path.starts_with('$')) {
            return None;
        }
        let overlapping = leaves.iter().any(|(a, _)| {
            leaves
                .iter()
                .any(|(b, _)| b.strip_prefix(a.as_ref()).is_some_and(|rest| rest.starts_with('.')))
        });
        if overlapping {
            return None;
        }
        let mut sure = vec![true; rows];
        let mut objects: Vec<(String, &ObjectSchema)> = Vec::new();
        let presences: Vec<Vec<u64>> = (0..leaves.len()).map(|index| data.presence_bits(index)).collect::<Option<_>>()?;
        for ((path, leaf), present) in leaves.iter().zip(&presences) {
            let (target, levels) = self.target(path);
            for level in levels {
                if !objects.iter().any(|(p, _)| *p == level.0) {
                    objects.push(level);
                }
            }
            let passing = match &target {
                Target::Free => None,
                Target::Forbidden => Some(vec![0u64; rows.div_ceil(64)]),
                Target::Check(check) => Some(check.sure_bits(leaf, rows)),
            };
            if let Some(passing) = passing {
                for (row, sure) in sure.iter_mut().enumerate() {
                    *sure &= !Bits::get(present, row) || Bits::get(&passing, row);
                }
            }
        }
        for (prefix, schema) in objects {
            let under = |path: &str, name: &str| -> bool {
                let rest = match prefix.is_empty() {
                    true => Some(path),
                    false => path.strip_prefix(prefix.as_str()).and_then(|r| r.strip_prefix('.')),
                };
                rest.is_some_and(|rest| rest == name || rest.strip_prefix(name).is_some_and(|r| r.starts_with('.')))
            };
            let within: Vec<usize> = leaves
                .iter()
                .enumerate()
                .filter(|(_, (path, _))| prefix.is_empty() || path.strip_prefix(prefix.as_str()).is_some_and(|r| r.starts_with('.')))
                .map(|(index, _)| index)
                .collect();
            let words = rows.div_ceil(64);
            let union = |indices: &mut dyn Iterator<Item = usize>| {
                let mut bits = vec![0u64; words];
                for i in indices {
                    bits.iter_mut().zip(&presences[i]).for_each(|(b, p)| *b |= p);
                }
                bits
            };
            let exists = match prefix.is_empty() {
                true => vec![u64::MAX; words],
                false => union(&mut within.iter().copied()),
            };
            for field in schema.properties.iter().filter(|field| field.required) {
                let held = union(&mut within.iter().copied().filter(|&i| under(&leaves[i].0, &field.name)));
                for (row, sure) in sure.iter_mut().enumerate() {
                    if Bits::get(&exists, row) && !Bits::get(&held, row) {
                        *sure = false;
                    }
                }
            }
        }
        Some(sure)
    }
}
