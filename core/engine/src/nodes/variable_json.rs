use std::borrow::Cow;
use std::cell::Ref;

use jsonschema::json::{Array, Json, JsonNumber, Node, NodeIdentity, Object};
use jsonschema::JsonType;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use self_cell::self_cell;
use serde_json::Value;
use typed_arena::Arena;
use zen_types::variable::{MapIter, RcCell, Variable, VariableMap};

pub struct VariableJson;

type MapRef<'a> = Ref<'a, VariableMap>;
type VecRef<'a> = Ref<'a, Vec<Variable>>;

self_cell!(
    struct MapGuard {
        owner: RcCell<VariableMap>,

        #[covariant]
        dependent: MapRef,
    }
);

self_cell!(
    struct VecGuard {
        owner: RcCell<Vec<Variable>>,

        #[covariant]
        dependent: VecRef,
    }
);

pub struct Guards {
    objects: Arena<MapGuard>,
    arrays: Arena<VecGuard>,
}

impl Default for Guards {
    fn default() -> Self {
        Self {
            objects: Arena::new(),
            arrays: Arena::new(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct VariableNode<'a> {
    var: &'a Variable,
    guards: &'a Guards,
}

impl<'a> VariableNode<'a> {
    pub fn new(var: &'a Variable, guards: &'a Guards) -> Self {
        Self { var, guards }
    }
}

impl Json for VariableJson {
    type Node<'a> = VariableNode<'a>;
    type PreparedKey = Box<str>;
    type StringBuffer = Variable;

    fn prepare_key(key: &str) -> Box<str> {
        Box::from(key)
    }

    fn with_string_node<T>(
        buffer: &mut Variable,
        string: &str,
        f: impl FnOnce(VariableNode<'_>) -> T,
    ) -> T {
        *buffer = Variable::String((string).into());
        let guards = Guards::default();
        f(VariableNode::new(buffer, &guards))
    }
}

pub struct VariableNumber(Decimal);

impl JsonNumber for VariableNumber {
    fn as_u64(&self) -> Option<u64> {
        self.0.is_integer().then(|| self.0.to_u64()).flatten()
    }

    fn as_i64(&self) -> Option<i64> {
        self.0.is_integer().then(|| self.0.to_i64()).flatten()
    }

    fn as_f64(&self) -> Option<f64> {
        self.0.to_f64()
    }

    fn as_str(&self) -> Cow<'_, str> {
        Cow::Owned(self.0.normalize().to_string())
    }

    fn to_number(&self) -> Cow<'_, serde_json::Number> {
        let normalized = self.0.normalize().to_string();
        #[cfg(feature = "arbitrary_precision")]
        let number = serde_json::Number::from_string_unchecked(normalized);
        #[cfg(not(feature = "arbitrary_precision"))]
        let number = normalized
            .parse()
            .ok()
            .or_else(|| self.0.to_f64().and_then(serde_json::Number::from_f64))
            .unwrap_or_else(|| serde_json::Number::from(0));
        Cow::Owned(number)
    }
}

impl<'a> Node<'a, VariableJson> for VariableNode<'a> {
    type Object = ObjectNode<'a>;
    type Array = ArrayNode<'a>;
    type Number = VariableNumber;

    fn as_object(&self) -> Option<ObjectNode<'a>> {
        let Variable::Object(cell) = self.var else {
            return None;
        };

        let guard = self
            .guards
            .objects
            .alloc(MapGuard::new(cell.clone(), |cell| cell.borrow()));
        Some(ObjectNode {
            map: guard.borrow_dependent(),
            guards: self.guards,
        })
    }

    fn as_array(&self) -> Option<ArrayNode<'a>> {
        let Variable::Array(cell) = self.var else {
            return None;
        };

        let guard = self
            .guards
            .arrays
            .alloc(VecGuard::new(cell.clone(), |cell| cell.borrow()));
        Some(ArrayNode {
            items: guard.borrow_dependent().as_slice(),
            guards: self.guards,
        })
    }

    fn as_string(&self) -> Option<Cow<'a, str>> {
        match self.var {
            Variable::String(string) => Some(Cow::Borrowed(string.as_str())),
            Variable::Dynamic(dynamic) => Some(Cow::Owned(dynamic.to_string())),
            _ => None,
        }
    }

    fn as_number(&self) -> Option<VariableNumber> {
        match self.var {
            Variable::Number(number) => Some(VariableNumber(*number)),
            _ => None,
        }
    }

    fn as_boolean(&self) -> Option<bool> {
        match self.var {
            Variable::Bool(boolean) => Some(*boolean),
            _ => None,
        }
    }

    fn is_null(&self) -> bool {
        matches!(self.var, Variable::Null)
    }

    fn json_type(&self) -> JsonType {
        match self.var {
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
        Cow::Owned(Variable::to_value(self.var))
    }

    fn identity(&self) -> Option<NodeIdentity> {
        Some(NodeIdentity::new(
            std::ptr::from_ref::<Variable>(self.var) as usize
        ))
    }
}

pub struct ObjectNode<'a> {
    map: &'a VariableMap,
    guards: &'a Guards,
}

impl<'a> Object<'a, VariableJson> for ObjectNode<'a> {
    type Node = VariableNode<'a>;
    type MemberName = &'a str;
    type MembersIter = VariableMembersIter<'a>;

    fn len(&self) -> usize {
        self.map.len()
    }

    fn get(&self, key: &Box<str>) -> Option<VariableNode<'a>> {
        self.map
            .get_str(key)
            .map(|var| VariableNode::new(var, self.guards))
    }

    fn members(&self) -> VariableMembersIter<'a> {
        VariableMembersIter {
            iter: self.map.iter(),
            guards: self.guards,
        }
    }
}

pub struct VariableMembersIter<'a> {
    iter: MapIter<'a>,
    guards: &'a Guards,
}

impl<'a> Iterator for VariableMembersIter<'a> {
    type Item = (&'a str, VariableNode<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        self.iter
            .next()
            .map(|(name, value)| (name.as_str(), VariableNode::new(value, self.guards)))
    }
}

pub struct ArrayNode<'a> {
    items: &'a [Variable],
    guards: &'a Guards,
}

impl<'a> Array<'a, VariableJson> for ArrayNode<'a> {
    type Node = VariableNode<'a>;
    type ElementsIter = VariableElementsIter<'a>;

    fn len(&self) -> usize {
        self.items.len()
    }

    fn elements(&self) -> VariableElementsIter<'a> {
        VariableElementsIter {
            iter: self.items.iter(),
            guards: self.guards,
        }
    }
}

pub struct VariableElementsIter<'a> {
    iter: std::slice::Iter<'a, Variable>,
    guards: &'a Guards,
}

impl<'a> Iterator for VariableElementsIter<'a> {
    type Item = VariableNode<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter
            .next()
            .map(|var| VariableNode::new(var, self.guards))
    }
}
