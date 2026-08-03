// Safety: `Variable` is `Rc`-based, so reading through a `RefCell` without a
// guard is the only way to hand out `'a` borrows. Scoped to this module.
#![allow(unsafe_code)]

use std::borrow::Cow;

use jsonschema::json::{Array, Json, JsonNumber, Node, NodeIdentity, Object};
use jsonschema::JsonType;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use zen_types::variable::{Variable, VariableMap};

pub struct VariableJson;

impl Json for VariableJson {
    type Node<'a> = &'a Variable;
    type PreparedKey = Box<str>;
    type StringBuffer = Variable;

    fn prepare_key(key: &str) -> Box<str> {
        Box::from(key)
    }

    fn with_string_node<T>(
        buffer: &mut Variable,
        string: &str,
        f: impl FnOnce(&Variable) -> T,
    ) -> T {
        *buffer = Variable::String((string).into());
        f(buffer)
    }
}

pub struct VariableNumber(Decimal);

impl JsonNumber for VariableNumber {
    fn as_u64(&self) -> Option<u64> {
        self.0.to_u64()
    }

    fn as_i64(&self) -> Option<i64> {
        self.0.to_i64()
    }

    fn as_f64(&self) -> Option<f64> {
        self.0.to_f64()
    }

    fn as_str(&self) -> Cow<'_, str> {
        Cow::Owned(self.0.normalize().to_string())
    }

    fn to_number(&self) -> Cow<'_, serde_json::Number> {
        let number = self
            .0
            .to_f64()
            .and_then(serde_json::Number::from_f64)
            .unwrap_or_else(|| serde_json::Number::from(0));
        Cow::Owned(number)
    }
}

/// # Safety
///
/// Validation is a pure read: nothing in `jsonschema` holds a `Variable`, and no
/// node handler mutates the input while its schema is being checked. The `Rc`
/// keeps the cell alive for at least `'a`, so reading through the cell's pointer
/// cannot outlive the allocation and cannot race a `borrow_mut`.
#[inline]
unsafe fn cell_ref<'a, T: zen_types::rccell::Recycle>(
    cell: &zen_types::rccell::RcCell<T>,
) -> &'a T {
    unsafe { cell.get_ref() }
}

impl<'a> Node<'a, VariableJson> for &'a Variable {
    type Object = &'a VariableMap;
    type Array = &'a [Variable];
    type Number = VariableNumber;

    fn as_object(&self) -> Option<&'a VariableMap> {
        match self {
            Variable::Object(map) => Some(unsafe { cell_ref(map) }),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&'a [Variable]> {
        match self {
            Variable::Array(items) => Some(unsafe { cell_ref(items) }.as_slice()),
            _ => None,
        }
    }

    fn as_string(&self) -> Option<Cow<'a, str>> {
        match self {
            Variable::String(string) => Some(Cow::Borrowed(string)),
            Variable::Dynamic(dynamic) => Some(Cow::Owned(dynamic.to_string())),
            _ => None,
        }
    }

    fn as_number(&self) -> Option<VariableNumber> {
        match self {
            Variable::Number(number) => Some(VariableNumber(*number)),
            _ => None,
        }
    }

    fn as_boolean(&self) -> Option<bool> {
        match self {
            Variable::Bool(boolean) => Some(*boolean),
            _ => None,
        }
    }

    fn is_null(&self) -> bool {
        matches!(self, Variable::Null)
    }

    fn json_type(&self) -> JsonType {
        match self {
            Variable::Null => JsonType::Null,
            Variable::Bool(_) => JsonType::Boolean,
            Variable::Number(_) => JsonType::Number,
            Variable::String(_) => JsonType::String,
            Variable::Array(_) => JsonType::Array,
            Variable::Object(_) => JsonType::Object,
            Variable::Dynamic(_) => JsonType::String,
        }
    }

    fn to_value(&self) -> Cow<'a, Value> {
        Cow::Owned(Variable::to_value(self))
    }

    fn identity(&self) -> Option<NodeIdentity> {
        Some(NodeIdentity::new(
            std::ptr::from_ref::<Variable>(*self) as usize
        ))
    }
}

impl<'a> Object<'a, VariableJson> for &'a VariableMap {
    type Node = &'a Variable;
    type MemberName = &'a str;
    type MembersIter = VariableMembersIter<'a>;

    fn len(&self) -> usize {
        VariableMap::len(self)
    }

    fn get(&self, key: &Box<str>) -> Option<&'a Variable> {
        VariableMap::get_str(self, key)
    }

    fn members(&self) -> VariableMembersIter<'a> {
        VariableMembersIter(self.iter())
    }
}

pub struct VariableMembersIter<'a>(zen_types::variable::MapIter<'a>);

impl<'a> Iterator for VariableMembersIter<'a> {
    type Item = (&'a str, &'a Variable);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(name, value)| (name.as_str(), value))
    }
}

impl<'a> Array<'a, VariableJson> for &'a [Variable] {
    type Node = &'a Variable;
    type ElementsIter = std::slice::Iter<'a, Variable>;

    fn len(&self) -> usize {
        <[Variable]>::len(self)
    }

    fn elements(&self) -> std::slice::Iter<'a, Variable> {
        self.iter()
    }
}
