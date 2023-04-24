#[derive(Debug, PartialEq)]
pub(crate) enum Associativity {
    Left,
    Right,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Arity {
    Single,
    Closure,
    Dual,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Operator {
    pub precedence: u8,
    pub associativity: Associativity,
}

#[derive(Debug, PartialEq)]
pub(crate) struct BuiltIn {
    pub arity: Arity,
}

#[macro_export]
macro_rules! hashmap {
    (@single $($x:tt)*) => (());
    (@count $($rest:expr),*) => (<[()]>::len(&[$(hashmap!(@single $rest)),*]));

    ($($key:expr => $value:expr,)+) => { hashmap!($($key => $value),+) };
    ($($key:expr => $value:expr),*) => {
        {
            let _cap = hashmap!(@count $($key),*);
            let mut _map = ::std::collections::HashMap::default();
            _map.reserve(_cap);
            $(
                let _ = _map.insert($key, $value);
            )*
            _map
        }
    };
}
