use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use ahash::AHasher;
use once_cell::sync::Lazy;

use crate::hashmap;
use crate::parser::definitions::Associativity::{Left, Right};
use crate::parser::definitions::{Arity, BuiltIn, Operator};

type ADefHasher = BuildHasherDefault<AHasher>;

pub(crate) static BUILT_INS: Lazy<HashMap<&'static str, BuiltIn, ADefHasher>> = Lazy::new(|| {
    hashmap! {
        "len" => BuiltIn { arity: Arity::Single },
        "date" => BuiltIn { arity: Arity::Single },
        "time" => BuiltIn { arity: Arity::Single },
        "duration" => BuiltIn { arity: Arity::Single },
        "upper" => BuiltIn { arity: Arity::Single },
        "lower" => BuiltIn { arity: Arity::Single },
        "flatten" => BuiltIn { arity: Arity::Single },

        "string" => BuiltIn { arity: Arity::Single },
        "number" => BuiltIn { arity: Arity::Single },
        "isNumeric" => BuiltIn { arity: Arity::Single },

        "abs" => BuiltIn { arity: Arity::Single },
        "sum" => BuiltIn { arity: Arity::Single },
        "avg" => BuiltIn { arity: Arity::Single },
        "min" => BuiltIn { arity: Arity::Single },
        "max" => BuiltIn { arity: Arity::Single },
        "rand" => BuiltIn { arity: Arity::Single },
        "median" => BuiltIn { arity: Arity::Single },
        "mode" => BuiltIn { arity: Arity::Single },

        "floor" => BuiltIn { arity: Arity::Single },
        "ceil" => BuiltIn { arity: Arity::Single },
        "round" => BuiltIn { arity: Arity::Single },

        "year" => BuiltIn { arity: Arity::Single },
        "dayOfWeek" => BuiltIn { arity: Arity::Single },
        "dayOfMonth" => BuiltIn { arity: Arity::Single },
        "dayOfYear" => BuiltIn { arity: Arity::Single },
        "weekOfYear" => BuiltIn { arity: Arity::Single },
        "monthOfYear" => BuiltIn { arity: Arity::Single },
        "monthString" => BuiltIn { arity: Arity::Single },
        "dateString" => BuiltIn { arity: Arity::Single },
        "weekdayString" => BuiltIn { arity: Arity::Single },
        "startOf" => BuiltIn { arity: Arity::Dual },
        "endOf" => BuiltIn { arity: Arity::Dual },

        "startsWith" => BuiltIn { arity: Arity::Dual },
        "endsWith" => BuiltIn { arity: Arity::Dual },
        "contains" => BuiltIn { arity: Arity::Dual },
        "matches" => BuiltIn { arity: Arity::Dual },
        "extract" => BuiltIn { arity: Arity::Dual },

        "all" => BuiltIn { arity: Arity::Closure },
        "some" => BuiltIn { arity: Arity::Closure },
        "none" => BuiltIn { arity: Arity::Closure },
        "filter" => BuiltIn { arity: Arity::Closure },
        "map" => BuiltIn { arity: Arity::Closure },
        "count" => BuiltIn { arity: Arity::Closure },
        "one" => BuiltIn { arity: Arity::Closure },
        "flatMap" => BuiltIn { arity: Arity::Closure },
    }
});

pub(crate) static BINARY_OPERATORS: Lazy<HashMap<&'static str, Operator, ADefHasher>> =
    Lazy::new(|| {
        hashmap! {
            "or" => Operator { precedence: 10, associativity: Left },
            "and" => Operator { precedence: 15, associativity: Left },
            "==" => Operator { precedence: 20, associativity: Left },
            "!=" => Operator { precedence: 20, associativity: Left },
            "<" => Operator { precedence: 20, associativity: Left },
            ">" => Operator { precedence: 20, associativity: Left },
            "<=" => Operator { precedence: 20, associativity: Left },
            ">=" => Operator { precedence: 20, associativity: Left },
            "not in" => Operator { precedence: 20, associativity: Left },
            "in" => Operator { precedence: 20, associativity: Left },
            "+" => Operator { precedence: 30, associativity: Left },
            "-" => Operator { precedence: 30, associativity: Left },
            "*" => Operator { precedence: 60, associativity: Left },
            "/" => Operator { precedence: 60, associativity: Left },
            "%" => Operator { precedence: 60, associativity: Left },
            "^" => Operator { precedence: 70, associativity: Right },
        }
    });

pub(crate) static UNARY_OPERATORS: Lazy<HashMap<&'static str, Operator, ADefHasher>> =
    Lazy::new(|| {
        hashmap! {
            "not" => Operator { precedence: 50, associativity: Left },
            "!" => Operator { precedence: 50, associativity: Left },
            "+" => Operator { precedence: 200, associativity: Left },
            "-" => Operator { precedence: 200, associativity: Left },
        }
    });
