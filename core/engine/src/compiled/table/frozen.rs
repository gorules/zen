use crate::compiled::leaf::Dict;
use std::rc::Rc;
use super::Table;

pub(super) enum Frozen {
    Text { offsets: Vec<i32>, data: String },
    Scaled { mant: Vec<i64>, scale: Vec<u8> },
    Bool(Vec<u64>),
}

impl Frozen {
    pub(super) fn of(dict: Dict) -> Option<Frozen> {
        match dict {
            Dict::Text { offsets, data } => Some(Frozen::Text { offsets, data }),
            Dict::Scaled { mant, scale } => Some(Frozen::Scaled { mant, scale }),
            Dict::Bool(bits) => Some(Frozen::Bool(bits)),
            Dict::Any(_) => None,
        }
    }

    fn thaw(&self) -> Dict {
        match self {
            Frozen::Text { offsets, data } => Dict::Text {
                offsets: offsets.clone(),
                data: data.clone(),
            },
            Frozen::Scaled { mant, scale } => Dict::Scaled {
                mant: mant.clone(),
                scale: scale.clone(),
            },
            Frozen::Bool(bits) => Dict::Bool(bits.clone()),
        }
    }
}

pub(super) type Static = Option<(Frozen, Vec<u64>, Vec<u64>)>;

impl Table {
    pub(super) fn thawed(&self, slot: usize, frozen: &Frozen) -> Rc<Dict> {
        Self::THAWED.with_borrow_mut(|cache| {
            if cache.len() >= Self::THAWS {
                cache.clear();
            }
            cache.entry((self.id, slot)).or_insert_with(|| Rc::new(frozen.thaw())).clone()
        })
    }
}
