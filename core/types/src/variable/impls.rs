use crate::variable::Variable;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde_json::Value;
use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;

pub trait ToVariable {
    fn to_variable(&self) -> Variable;
}

impl ToVariable for String {
    fn to_variable(&self) -> Variable {
        Variable::String(Rc::from(self.as_str()))
    }
}

impl ToVariable for str {
    fn to_variable(&self) -> Variable {
        Variable::String(Rc::from(self))
    }
}

impl ToVariable for bool {
    fn to_variable(&self) -> Variable {
        Variable::Bool(*self)
    }
}

impl ToVariable for Decimal {
    fn to_variable(&self) -> Variable {
        Variable::Number(*self)
    }
}

impl ToVariable for Variable {
    fn to_variable(&self) -> Variable {
        self.clone()
    }
}

impl ToVariable for Value {
    fn to_variable(&self) -> Variable {
        Variable::from(self)
    }
}

macro_rules! impl_to_variable_numeric {
    ($($t:ty),* $(,)?) => {
        $(
            impl ToVariable for $t {
                fn to_variable(&self) -> Variable {
                    Variable::Number(Decimal::from(*self))
                }
            }
        )*
    };
}

impl_to_variable_numeric!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize
);

impl ToVariable for f32 {
    fn to_variable(&self) -> Variable {
        Variable::Number(Decimal::from_f32(*self).unwrap_or_default())
    }
}

impl ToVariable for f64 {
    fn to_variable(&self) -> Variable {
        Variable::Number(Decimal::from_f64(*self).unwrap_or_default())
    }
}

impl<T> ToVariable for Vec<T>
where
    T: ToVariable,
{
    fn to_variable(&self) -> Variable {
        Variable::from_array(self.iter().map(|v| v.to_variable()).collect())
    }
}

impl<V, S> ToVariable for HashMap<Rc<str>, V, S>
where
    V: ToVariable,
    S: std::hash::BuildHasher,
{
    fn to_variable(&self) -> Variable {
        Variable::from_object(
            self.iter()
                .map(|(k, v)| (k.clone(), v.to_variable()))
                .collect(),
        )
    }
}

impl<V, S> ToVariable for HashMap<Arc<str>, V, S>
where
    V: ToVariable,
    S: std::hash::BuildHasher,
{
    fn to_variable(&self) -> Variable {
        Variable::from_object(
            self.iter()
                .map(|(k, v)| (Rc::<str>::from(k.deref()), v.to_variable()))
                .collect(),
        )
    }
}

impl<V, S> ToVariable for HashMap<String, V, S>
where
    V: ToVariable,
    S: std::hash::BuildHasher,
{
    fn to_variable(&self) -> Variable {
        Variable::from_object(
            self.iter()
                .map(|(k, v)| (Rc::from(k.as_str()), v.to_variable()))
                .collect(),
        )
    }
}

macro_rules! tuple_impls {
    ( $( ($($T:ident),+) ),+ ) => {
        $(
            impl<$($T),+> ToVariable for ($($T,)+)
            where
                $($T: ToVariable,)+
            {
                #[allow(non_snake_case)]
                fn to_variable(&self) -> Variable {
                    let ($($T,)+) = self;
                    Variable::from_array(vec![
                        $($T.to_variable(),)+
                    ])
                }
            }
        )+
    };
}

tuple_impls! {
    (T1),
    (T1, T2),
    (T1, T2, T3),
    (T1, T2, T3, T4),
    (T1, T2, T3, T4, T5)
}

impl<T> ToVariable for &T
where
    T: ?Sized + ToVariable,
{
    fn to_variable(&self) -> Variable {
        (**self).to_variable()
    }
}

impl<T> ToVariable for &mut T
where
    T: ?Sized + ToVariable,
{
    fn to_variable(&self) -> Variable {
        (**self).to_variable()
    }
}

impl<T> ToVariable for Option<T>
where
    T: ToVariable,
{
    fn to_variable(&self) -> Variable {
        match self {
            Some(value) => value.to_variable(),
            None => Variable::Null,
        }
    }
}

impl<T> ToVariable for Box<T>
where
    T: ?Sized + ToVariable,
{
    fn to_variable(&self) -> Variable {
        (**self).to_variable()
    }
}

impl<T> ToVariable for Rc<T>
where
    T: ?Sized + ToVariable,
{
    fn to_variable(&self) -> Variable {
        (**self).to_variable()
    }
}

impl<T> ToVariable for Arc<T>
where
    T: ?Sized + ToVariable,
{
    fn to_variable(&self) -> Variable {
        (**self).to_variable()
    }
}
