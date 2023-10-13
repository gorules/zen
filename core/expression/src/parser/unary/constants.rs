use phf::{phf_map, Map};

use crate::parser::definitions::Associativity::{Left, Right};
use crate::parser::definitions::{Arity, BuiltIn, Operator};

pub(crate) const BUILT_INS: Map<&'static str, BuiltIn> = phf_map! {
    "date" => BuiltIn { arity: Arity::Single },
    "time" => BuiltIn { arity: Arity::Single },
    "duration" => BuiltIn { arity: Arity::Single },

    "string" => BuiltIn { arity: Arity::Single },
    "number" => BuiltIn { arity: Arity::Single },
    "isNumeric" => BuiltIn { arity: Arity::Single },

    "upper" => BuiltIn { arity: Arity::Single },
    "lower" => BuiltIn { arity: Arity::Single },

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

    "flatten" => BuiltIn { arity: Arity::Single },

    "year" => BuiltIn { arity: Arity::Single },
    "dayOfWeek" => BuiltIn { arity: Arity::Single },
    "dayOfMonth" => BuiltIn { arity: Arity::Single },
    "dayOfYear" => BuiltIn { arity: Arity::Single },
    "weekOfYear" => BuiltIn { arity: Arity::Single },
    "monthOfYear" => BuiltIn { arity: Arity::Single },
    "monthString" => BuiltIn { arity: Arity::Single },
    "dateString" => BuiltIn { arity: Arity::Single },
    "weekdayString" => BuiltIn { arity: Arity::Single },
};

pub(crate) const STANDARD_OPERATORS: Map<&'static str, Operator> = phf_map! {
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
};
