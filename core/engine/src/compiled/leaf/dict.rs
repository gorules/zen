use std::rc::Rc;
use zen_expression::lane::Dictionary;
use zen_expression::Variable;

#[derive(Debug)]
pub(crate) enum Dict {
    Text { offsets: Vec<i32>, data: String },
    Scaled { mant: Vec<i64>, scale: Vec<u8> },
    Bool(Vec<u64>),
    Any(Vec<Variable>),
}

impl Dict {
    fn len(&self) -> Option<usize> {
        match self {
            Dict::Text { offsets, .. } => Some(offsets.len().saturating_sub(1)),
            Dict::Scaled { mant, .. } => Some(mant.len()),
            Dict::Any(values) => Some(values.len()),
            Dict::Bool(_) => None,
        }
    }

    pub fn concat(dicts: &[&Rc<Dict>]) -> Option<(Rc<Dict>, Vec<i32>)> {
        if let [single] = dicts {
            return Some(((*single).clone(), vec![0]));
        }
        let mut bases = Vec::with_capacity(dicts.len());
        let mut total = 0usize;
        for dict in dicts {
            bases.push(i32::try_from(total).ok()?);
            total += dict.len()?;
        }
        i32::try_from(total).ok()?;
        let merged = match dicts.first().map(|d| &***d)? {
            Dict::Text { .. } => {
                let mut offsets = vec![0i32];
                let mut data = String::new();
                for dict in dicts {
                    let Dict::Text { offsets: o, data: d } = &***dict else {
                        return None;
                    };
                    let base = i32::try_from(data.len()).ok()?;
                    offsets.extend(o.iter().skip(1).map(|x| x + base));
                    data.push_str(d);
                }
                i32::try_from(data.len()).ok()?;
                Dict::Text { offsets, data }
            }
            Dict::Scaled { .. } => {
                let (mut mant, mut scale) = (Vec::with_capacity(total), Vec::with_capacity(total));
                for dict in dicts {
                    let Dict::Scaled { mant: m, scale: s } = &***dict else {
                        return None;
                    };
                    mant.extend_from_slice(m);
                    scale.extend_from_slice(s);
                }
                Dict::Scaled { mant, scale }
            }
            Dict::Any(_) => {
                let mut values = Vec::with_capacity(total);
                for dict in dicts {
                    let Dict::Any(v) = &***dict else {
                        return None;
                    };
                    values.extend(v.iter().cloned());
                }
                Dict::Any(values)
            }
            Dict::Bool(_) => return None,
        };
        Some((Rc::new(merged), bases))
    }

    pub(super) fn dictionary(&self) -> Dictionary<'_> {
        match self {
            Dict::Text { offsets, data } => Dictionary::Text { offsets, data },
            Dict::Scaled { mant, scale } => Dictionary::Scaled { mant, scale },
            Dict::Bool(bits) => Dictionary::Bool { bits },
            Dict::Any(values) => Dictionary::Any(values),
        }
    }
}
