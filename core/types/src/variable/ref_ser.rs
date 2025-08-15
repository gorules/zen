use crate::rcvalue::RcValue;
use crate::variable::Variable;
use ahash::AHashMap;
use nohash_hasher::BuildNoHashHasher;
use std::collections::HashMap;
use std::rc::Rc;

pub struct RefSerializer {
    ref_counts: HashMap<usize, usize, BuildNoHashHasher<usize>>,
    refs: HashMap<usize, (usize, Rc<str>), BuildNoHashHasher<usize>>,
    string_intern: AHashMap<Rc<str>, Rc<str>>,
    ref_data: Vec<RcValue>,
    min_ref_count: usize,
    min_str_len: usize,
}

impl RefSerializer {
    pub fn new() -> Self {
        Self {
            ref_counts: HashMap::default(),
            refs: HashMap::default(),
            string_intern: AHashMap::default(),
            ref_data: Vec::new(),
            min_ref_count: 2,
            min_str_len: 5,
        }
    }

    fn escape_at_string(s: &Rc<str>) -> Rc<str> {
        if s.starts_with('@') {
            let string = format!("@{s}");
            Rc::from(string.as_str())
        } else {
            s.clone()
        }
    }

    fn intern_string_addr(&mut self, s: &Rc<str>) -> usize {
        let reference = match self.string_intern.get(s) {
            Some(interned) => interned,
            None => {
                self.string_intern.insert(s.clone(), s.clone());
                s
            }
        };

        Rc::as_ptr(&reference) as *const () as usize
    }

    pub fn serialize(mut self, var: &Variable) -> serde_json::Result<RcValue> {
        self.count_refs(var);
        self.assign_ref_ids();

        let data = self.serialize_with_refs(var)?;

        let mut result = HashMap::default();
        if !self.ref_data.is_empty() {
            result.insert(Rc::from("$refs"), RcValue::Array(self.ref_data));
        }

        result.insert(Rc::from("$root"), data);
        Ok(RcValue::Object(result))
    }

    fn count_refs(&mut self, var: &Variable) {
        match var {
            Variable::String(s) => {
                if s.len() < self.min_str_len {
                    return;
                }

                let addr = self.intern_string_addr(s);
                *self.ref_counts.entry(addr).or_insert(0) += 1;
            }
            Variable::Array(arr) => {
                let addr = Rc::as_ptr(arr) as *const () as usize;
                *self.ref_counts.entry(addr).or_insert(0) += 1;

                let borrowed = arr.borrow();
                for item in borrowed.iter() {
                    self.count_refs(item);
                }
            }
            Variable::Object(obj) => {
                let addr = Rc::as_ptr(obj) as *const () as usize;
                *self.ref_counts.entry(addr).or_insert(0) += 1;

                let borrowed = obj.borrow();
                for (key, value) in borrowed.iter() {
                    let key_addr = self.intern_string_addr(key);
                    *self.ref_counts.entry(key_addr).or_insert(0) += 1;
                    self.count_refs(value);
                }
            }
            Variable::Dynamic(_) => {}
            _ => {} // Null, Bool, Number don't need ref counting
        }
    }

    fn assign_ref_ids(&mut self) {
        let mut sorted_refs: Vec<_> = self
            .ref_counts
            .iter()
            .filter(|&(_, &count)| count >= self.min_ref_count)
            .collect();

        sorted_refs.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));

        self.refs.reserve(sorted_refs.len());
        self.ref_data.reserve(sorted_refs.len());

        for (&addr, _) in sorted_refs {
            let id = self.ref_data.len();
            let id_string = format!("@{id}");

            self.refs.insert(addr, (id, Rc::from(id_string.as_str())));
            self.ref_data.push(RcValue::Null);
        }
    }

    fn serialize_with_refs(&mut self, var: &Variable) -> serde_json::Result<RcValue> {
        match var {
            Variable::String(s) => {
                let addr = self.intern_string_addr(s);
                let Some((id, id_str)) = self.refs.get(&addr) else {
                    return Ok(RcValue::String(Self::escape_at_string(s)));
                };

                if self.ref_data[*id] == RcValue::Null {
                    self.ref_data[*id] = RcValue::String(Self::escape_at_string(s));
                }

                Ok(RcValue::String(id_str.clone()))
            }

            Variable::Array(arr) => {
                let addr = Rc::as_ptr(arr) as *const () as usize;
                let data = {
                    let borrowed = arr.borrow();
                    let items: Result<Vec<_>, _> = borrowed
                        .iter()
                        .map(|item| self.serialize_with_refs(item))
                        .collect();

                    RcValue::Array(items?)
                };

                let Some((id, id_str)) = self.refs.get(&addr) else {
                    return Ok(data);
                };

                if self.ref_data[*id] == RcValue::Null {
                    self.ref_data[*id] = data;
                }

                Ok(RcValue::String(id_str.clone()))
            }

            Variable::Object(obj) => {
                let addr = Rc::as_ptr(obj) as *const () as usize;
                let data = {
                    let borrowed = obj.borrow();
                    let mut map = HashMap::with_capacity_and_hasher(
                        borrowed.len(),
                        ahash::RandomState::new(),
                    );

                    for (key, value) in borrowed.iter() {
                        let key_addr = self.intern_string_addr(key);
                        let key_str = if let Some((key_id, key_id_str)) = self.refs.get(&key_addr) {
                            if self.ref_data[*key_id] == RcValue::Null {
                                self.ref_data[*key_id] =
                                    RcValue::String(Self::escape_at_string(key));
                            }

                            key_id_str.clone()
                        } else {
                            Self::escape_at_string(key)
                        };

                        map.insert(key_str, self.serialize_with_refs(value)?);
                    }

                    RcValue::Object(map)
                };

                let Some((id, id_str)) = self.refs.get(&addr) else {
                    return Ok(data);
                };

                if self.ref_data[*id] == RcValue::Null {
                    self.ref_data[*id] = data;
                }

                Ok(RcValue::String(id_str.clone()))
            }

            _ => Ok(RcValue::from(var)),
        }
    }
}

impl Default for RefSerializer {
    fn default() -> Self {
        Self::new()
    }
}
