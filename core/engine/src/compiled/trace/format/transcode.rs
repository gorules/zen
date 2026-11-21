use super::value::{parse_number, ParseError, Parser};
use serde::ser::{Error, SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use std::cell::RefCell;
use zen_types::variable::Variable;

pub struct JsonText<'a>(pub &'a str);

impl Serialize for JsonText<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let cursor = RefCell::new(Parser::new(self.0));
        let out = emit(&cursor, serializer, 0)?;
        let mut p = cursor.borrow_mut();
        p.ws();
        match p.done() {
            true => Ok(out),
            false => Err(S::Error::custom(p.err("trailing characters"))),
        }
    }
}

struct Next<'c, 'a> {
    cursor: &'c RefCell<Parser<'a>>,
    depth: usize,
}

impl Serialize for Next<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        emit(self.cursor, serializer, self.depth)
    }
}

fn fail<E: Error>(e: ParseError) -> E {
    E::custom(e)
}

fn emit<S: Serializer>(
    cursor: &RefCell<Parser<'_>>,
    serializer: S,
    depth: usize,
) -> Result<S::Ok, S::Error> {
    let mut p = cursor.borrow_mut();
    if depth > 4096 {
        return Err(fail(p.err("nesting too deep")));
    }
    p.ws();
    match p.peek() {
        None => Err(fail(p.err("unexpected end"))),
        Some(b'n') => {
            p.word("null").map_err(fail)?;
            serializer.serialize_unit()
        }
        Some(b't') => {
            p.word("true").map_err(fail)?;
            serializer.serialize_bool(true)
        }
        Some(b'f') => {
            p.word("false").map_err(fail)?;
            serializer.serialize_bool(false)
        }
        Some(b'"') => {
            let s = p.string().map_err(fail)?;
            drop(p);
            serializer.serialize_str(&s)
        }
        Some(b'[') => {
            p.bump();
            p.ws();
            if p.peek() == Some(b']') {
                p.bump();
                drop(p);
                return serializer.serialize_seq(Some(0))?.end();
            }
            drop(p);
            let mut seq = serializer.serialize_seq(None)?;
            loop {
                seq.serialize_element(&Next {
                    cursor,
                    depth: depth + 1,
                })?;
                let mut p = cursor.borrow_mut();
                p.ws();
                match p.peek() {
                    Some(b',') => p.bump(),
                    Some(b']') => {
                        p.bump();
                        break;
                    }
                    _ => return Err(fail(p.err("expected , or ]"))),
                }
            }
            seq.end()
        }
        Some(b'{') => {
            p.bump();
            p.ws();
            if p.peek() == Some(b'}') {
                p.bump();
                drop(p);
                return serializer.serialize_map(Some(0))?.end();
            }
            drop(p);
            let mut map = serializer.serialize_map(None)?;
            loop {
                let key = {
                    let mut p = cursor.borrow_mut();
                    p.ws();
                    if p.peek() != Some(b'"') {
                        return Err(fail(p.err("expected key")));
                    }
                    let key = p.string().map_err(fail)?;
                    p.ws();
                    if p.peek() != Some(b':') {
                        return Err(fail(p.err("expected :")));
                    }
                    p.bump();
                    key
                };
                map.serialize_entry(
                    &key,
                    &Next {
                        cursor,
                        depth: depth + 1,
                    },
                )?;
                let mut p = cursor.borrow_mut();
                p.ws();
                match p.peek() {
                    Some(b',') => p.bump(),
                    Some(b'}') => {
                        p.bump();
                        break;
                    }
                    _ => return Err(fail(p.err("expected , or }"))),
                }
            }
            map.end()
        }
        Some(_) => {
            let text = p.number_text().map_err(fail)?;
            let n = parse_number(text).ok_or_else(|| fail(p.err("number out of range")))?;
            drop(p);
            Variable::Number(n).serialize(serializer)
        }
    }
}
