use crate::functions::{FunctionKind, InternalFunction};
use crate::lane::builtins::{Apply, Arg, Array, Builtin, Fail, Out, Outcome, Params};
use crate::variable::Variable;
#[cfg(not(feature = "regex-lite"))]
use regex::Regex;
#[cfg(feature = "regex-lite")]
use regex_lite::Regex;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;

pub(crate) struct Text;

#[derive(Clone, Copy)]
pub(crate) enum TextKernel {
    Test(TextTest),
    Map(TextMap),
    Size,
}

#[derive(Clone, Copy)]
pub(crate) enum TextTest {
    Contains,
    Starts,
    Ends,
}

impl TextTest {
    #[inline(always)]
    pub(crate) fn apply(self, a: &str, b: &str) -> bool {
        match self {
            TextTest::Contains => match (b.as_bytes(), a.len() <= 64) {
                ([byte], true) => a.as_bytes().iter().any(|c| c == byte),
                _ => a.contains(b),
            },
            TextTest::Starts => a.starts_with(b),
            TextTest::Ends => a.ends_with(b),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum TextMap {
    Lower,
    Upper,
    Trim,
}

impl TextMap {
    #[inline]
    pub(crate) fn apply(self, a: &str, out: &mut String) -> Option<(usize, usize)> {
        match self {
            TextMap::Lower => Text::lower(a, out),
            TextMap::Upper => Text::upper(a, out),
            TextMap::Trim => Text::trim(a, out),
        }
    }

    #[inline]
    pub(crate) fn unchanged(self, a: &str) -> bool {
        match self {
            TextMap::Trim => {
                !a.starts_with(char::is_whitespace) && !a.ends_with(char::is_whitespace)
            }
            TextMap::Lower | TextMap::Upper => false,
        }
    }

    pub(crate) fn bytewise(self) -> Option<fn(&mut str)> {
        match self {
            TextMap::Lower => Some(str::make_ascii_lowercase),
            TextMap::Upper => Some(str::make_ascii_uppercase),
            TextMap::Trim => None,
        }
    }
}

struct Patterns;

impl Patterns {
    const CAPACITY: usize = 256;

    thread_local! {
        static CACHE: std::cell::RefCell<ahash::HashMap<String, Regex>> = Default::default();
    }

    fn with<T>(pattern: &str, f: impl FnOnce(&Regex) -> T) -> Result<T, Fail> {
        Self::CACHE.with_borrow_mut(|cache| {
            if let Some(regex) = cache.get(pattern) {
                return Ok(f(regex));
            }
            let regex =
                Regex::new(pattern).map_err(|_| "Invalid regular expression".to_string())?;
            if cache.len() >= Self::CAPACITY {
                cache.clear();
            }
            let out = f(&regex);
            cache.insert(pattern.to_string(), regex);
            Ok(out)
        })
    }
}

impl Text {
    pub(crate) fn kernel(kind: &FunctionKind) -> Option<TextKernel> {
        let FunctionKind::Internal(function) = kind else {
            return None;
        };
        Some(match function {
            InternalFunction::Contains => TextKernel::Test(TextTest::Contains),
            InternalFunction::StartsWith => TextKernel::Test(TextTest::Starts),
            InternalFunction::EndsWith => TextKernel::Test(TextTest::Ends),
            InternalFunction::Lower => TextKernel::Map(TextMap::Lower),
            InternalFunction::Upper => TextKernel::Map(TextMap::Upper),
            InternalFunction::Trim => TextKernel::Map(TextMap::Trim),
            InternalFunction::Len => TextKernel::Size,
            _ => return None,
        })
    }

    fn lower(a: &str, out: &mut String) -> Option<(usize, usize)> {
        match a.is_ascii() {
            true if !a.bytes().any(|b| b.is_ascii_uppercase()) => Some((0, a.len())),
            true => {
                let start = out.len();
                out.push_str(a);
                if let Some(tail) = out.get_mut(start..) {
                    tail.make_ascii_lowercase();
                }
                None
            }
            false => {
                out.push_str(&a.to_lowercase());
                None
            }
        }
    }

    fn upper(a: &str, out: &mut String) -> Option<(usize, usize)> {
        match a.is_ascii() {
            true if !a.bytes().any(|b| b.is_ascii_lowercase()) => Some((0, a.len())),
            true => {
                let start = out.len();
                out.push_str(a);
                if let Some(tail) = out.get_mut(start..) {
                    tail.make_ascii_uppercase();
                }
                None
            }
            false => {
                out.push_str(&a.to_uppercase());
                None
            }
        }
    }

    fn trim(a: &str, _: &mut String) -> Option<(usize, usize)> {
        let start = a.len() - a.trim_start().len();
        Some((start, start + a[start..].trim_end().len()))
    }

    const TWO: &'static [(&'static str, bool)] = &[("string", false), ("string", false)];
    const ONE: &'static [(&'static str, bool)] = &[("string", false)];

    pub(crate) const LEN: Builtin = Builtin {
        overloads: &[Self::len_str, Self::len_array],
        fail: Self::len_fail,
    };

    fn len_str(args: &[Arg]) -> Outcome {
        Apply::one(args, |a: &str| Ok(a.len()))
    }

    fn len_array(args: &[Arg]) -> Outcome {
        Apply::one(args, |a: Array| Ok(a.len()))
    }

    fn len_fail(args: &[Arg]) -> Fail {
        match args.first() {
            Some(a) => format!("Cannot determine len of type {}", a.type_name()),
            None => "Argument on 0 position out of bounds".to_string(),
        }
    }

    pub(crate) const CONTAINS: Builtin = Builtin {
        overloads: &[Self::contains_str, Self::contains_array],
        fail: Self::contains_fail,
    };

    fn contains_str(args: &[Arg]) -> Outcome {
        Apply::two(args, |a: &str, b: &str| Ok(a.contains(b)))
    }

    fn contains_array(args: &[Arg]) -> Outcome {
        Apply::two(args, |a: Array, b: Arg| {
            Ok(a.any(|x| match (x, b) {
                (Arg::Num(x), Arg::Num(y)) => x == y,
                (Arg::Str(x), Arg::Str(y)) => x == y,
                (Arg::Bool(x), Arg::Bool(y)) => x == y,
                (Arg::Null, Arg::Null) => true,
                (x, y) => match (x.date(), y.date()) {
                    (Some(d), _) => d.is_valid() && y.coerce_date() == Some(d),
                    (None, Some(d)) => d.is_valid() && x.coerce_date() == Some(d),
                    (None, None) => false,
                },
            }))
        })
    }

    fn contains_fail(args: &[Arg]) -> Fail {
        match (args.first(), args.get(1)) {
            (Some(a), Some(b)) => format!(
                "Cannot determine contains for type {} and {}",
                a.type_name(),
                b.type_name()
            ),
            (None, _) => "Argument on 0 position out of bounds".to_string(),
            (_, None) => "Argument on 1 position out of bounds".to_string(),
        }
    }

    pub(crate) const STARTS_WITH: Builtin = Builtin {
        overloads: &[|args| Apply::two(args, |a: &str, b: &str| Ok(a.starts_with(b)))],
        fail: |args| Params::fail(args, Self::TWO),
    };

    pub(crate) const ENDS_WITH: Builtin = Builtin {
        overloads: &[|args| Apply::two(args, |a: &str, b: &str| Ok(a.ends_with(b)))],
        fail: |args| Params::fail(args, Self::TWO),
    };

    pub(crate) const MATCHES: Builtin = Builtin {
        overloads: &[|args| {
            Apply::two(args, |a: &str, b: &str| {
                Patterns::with(b, |regex| regex.is_match(a))
            })
        }],
        fail: |args| Params::fail(args, Self::TWO),
    };

    pub(crate) const UPPER: Builtin = Builtin {
        overloads: &[|args| Apply::one(args, |a: &str| Ok(a.to_uppercase()))],
        fail: |args| Params::fail(args, Self::ONE),
    };

    pub(crate) const LOWER: Builtin = Builtin {
        overloads: &[|args| Apply::one(args, |a: &str| Ok(a.to_lowercase()))],
        fail: |args| Params::fail(args, Self::ONE),
    };

    pub(crate) const TRIM: Builtin = Builtin {
        overloads: &[|args| Apply::one(args, |a: &str| Ok(Out::from(a.trim())))],
        fail: |args| Params::fail(args, Self::ONE),
    };

    pub(crate) const EXTRACT: Builtin = Builtin {
        overloads: &[Self::extract],
        fail: |args| Params::fail(args, Self::TWO),
    };

    fn extract(args: &[Arg]) -> Outcome {
        Apply::two(args, |a: &str, b: &str| {
            let captures: Vec<Variable> = Patterns::with(b, |regex| {
                regex
                    .captures(a)
                    .map(|capture| {
                        capture
                            .iter()
                            .flatten()
                            .map(|c| Variable::String(c.as_str().into()))
                            .collect()
                    })
                    .unwrap_or_default()
            })?;
            Ok(Variable::from_array(captures))
        })
    }

    pub(crate) const SPLIT: Builtin = Builtin {
        overloads: &[Self::split],
        fail: |args| Params::fail(args, Self::TWO),
    };

    fn split(args: &[Arg]) -> Outcome {
        Apply::two(args, |a: &str, b: &str| {
            Ok(Variable::from_array(
                a.split(b).map(|s| Variable::String(s.into())).collect(),
            ))
        })
    }

    pub(crate) const FUZZY_MATCH: Builtin = Builtin {
        overloads: &[Self::fuzzy],
        fail: |args| Params::fail(args, &[("value", false), ("string", false)]),
    };

    fn similarity(a: &str, b: &str) -> Decimal {
        Decimal::from_f64(strsim::normalized_damerau_levenshtein(a, b)).unwrap_or(Decimal::ZERO)
    }

    fn fuzzy(args: &[Arg]) -> Outcome {
        Apply::two(args, |a: Arg, b: &str| match a {
            Arg::Str(a) => Ok(Variable::Number(Self::similarity(a, b))),
            Arg::Date(_) | Arg::Var(Variable::Dynamic(_)) => match a.text() {
                Some(a) => Ok(Variable::Number(Self::similarity(&a, b))),
                None => Err("Fuzzy match not available for type".to_string()),
            },
            Arg::Var(Variable::Array(_)) | Arg::List(_) => {
                let items = match a {
                    Arg::List(l) => Array::List(l),
                    Arg::Var(Variable::Array(v)) => Array::Var(v),
                    _ => return Err("Fuzzy match not available for type".to_string()),
                };
                items.with(|values| {
                    values
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .map(|s| Variable::Number(Self::similarity(s, b)))
                                .ok_or_else(|| "Expected string array".to_string())
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(Variable::from_array)
                })
            }
            _ => Err("Fuzzy match not available for type".to_string()),
        })
    }
}
