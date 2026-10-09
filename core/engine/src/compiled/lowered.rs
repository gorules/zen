use crate::compiled::typed::{Array, Bits, Dict, Leaf, Store};
use std::rc::Rc;
use std::sync::OnceLock;
use zen_expression::typed::{Data, Hir, Outputs, Slot};
use zen_expression::Variable;

pub(crate) struct Exports {
    dicts: Vec<(Vec<i32>, String)>,
}

impl Exports {
    const TYPED: bool = true;

    pub fn enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var("ZEN_TYPED").map_or(Self::TYPED, |value| value.trim() != "0"))
    }

    pub fn of(hir: &Hir) -> Self {
        let dicts = hir
            .schema
            .dicts
            .iter()
            .map(|dict| {
                let mut offsets = vec![0i32];
                let mut data = String::new();
                for value in &dict.values {
                    data.push_str(value);
                    offsets.push(i32::try_from(data.len()).unwrap_or(i32::MAX));
                }
                (offsets, data)
            })
            .collect();
        Self { dicts }
    }

    pub fn leaves<'a>(&self, out: &mut Outputs) -> Vec<Leaf<'a>> {
        let mut dicts = self.dictionaries();
        let mut leaves = Vec::with_capacity(out.slots().len());
        for at in 0..out.slots().len() {
            if let Some(slot) = out.slot_mut(at) {
                leaves.push(self.leaf(slot, &mut dicts));
            }
        }
        leaves
    }

    pub fn leaf<'a>(&self, slot: &mut Slot, dicts: &mut [Option<Rc<Dict>>]) -> Leaf<'a> {
        if let Some(offsets) = slot.offsets.take() {
            return self.items(slot, &offsets);
        }
        let len = slot.len;
        let dict = slot.dict();
        let array = match &slot.data {
            Data::Num { .. } if !slot.wide.is_empty() => {
                return Leaf::Any((0..len).map(|at| slot.decimal(at).map_or(Variable::Null, Variable::Number)).collect());
            }
            Data::Num { .. } => match slot.take_num() {
                Some((mut mant, mut scale, valid)) => {
                    let valid = Self::validity(valid, len);
                    if let Some(bits) = &valid {
                        for at in (0..len).filter(|&at| !Bits::get(bits, at)) {
                            mant[at] = 0;
                            scale[at] = 0;
                        }
                    }
                    Array::parts(Store::Scaled { mant, scale }, valid, len)
                }
                None => return Leaf::nulls(len),
            },
            Data::Bool(_) => match slot.take_bool() {
                Some((mut bits, valid)) => {
                    Bits::trim(&mut bits, len);
                    Array::parts(Store::Bool(bits), Self::validity(valid, len), len)
                }
                None => return Leaf::nulls(len),
            },
            Data::Code(_) => {
                let (Some((mut codes, valid)), Some(d)) = (slot.take_code(), dict) else {
                    return Leaf::nulls(len);
                };
                let Some(dict) = self.dictionary(dicts, usize::from(d)) else {
                    return Leaf::nulls(len);
                };
                match Self::validity(valid, len) {
                    None => Array::coded(codes.into(), dict),
                    Some(bits) => {
                        for at in (0..len).filter(|&at| !Bits::get(&bits, at)) {
                            codes[at] = -1;
                        }
                        Array::coded_with(codes.into(), dict, bits)
                    }
                }
            }
            Data::Text { .. } => match slot.take_text() {
                Some((offsets, data, valid)) => Array::parts(Store::Text { offsets, data }, Self::validity(valid, len), len),
                None => return Leaf::nulls(len),
            },
            Data::Date(_) => Array::from_values((0..len).map(|at| slot.variable(at)).collect()),
        };
        Leaf::typed(array)
    }

    fn items<'a>(&self, slot: &Slot, offsets: &[i32]) -> Leaf<'a> {
        let text = |code: i32| -> Option<&str> {
            let (offsets, data) = self.dicts.get(usize::from(slot.dict()?))?;
            let at = usize::try_from(code).ok()?;
            data.get(usize::try_from(*offsets.get(at)?).ok()?..usize::try_from(*offsets.get(at + 1)?).ok()?)
        };
        let value = |at: usize| -> Variable {
            match &slot.data {
                Data::Num { .. } => slot.decimal(at).map_or(Variable::Null, Variable::Number),
                Data::Bool(_) => slot.boolean(at).map_or(Variable::Null, Variable::Bool),
                Data::Code(_) => slot.code(at).and_then(text).map_or(Variable::Null, |t| Variable::String(t.into())),
                Data::Text { .. } => slot.text(at).map_or(Variable::Null, |t| Variable::String(t.into())),
                Data::Date(_) => slot.variable(at),
            }
        };
        Leaf::Any(
            offsets
                .windows(2)
                .map(|w| {
                    let (lo, hi) = (usize::try_from(w[0]).unwrap_or(0), usize::try_from(w[1]).unwrap_or(0));
                    Variable::from_array((lo..hi).map(value).collect())
                })
                .collect(),
        )
    }

    pub fn dictionaries(&self) -> Vec<Option<Rc<Dict>>> {
        vec![None; self.dicts.len()]
    }

    fn dictionary(&self, dicts: &mut [Option<Rc<Dict>>], at: usize) -> Option<Rc<Dict>> {
        let slot = dicts.get_mut(at)?;
        if slot.is_none() {
            let (offsets, data) = self.dicts.get(at)?;
            *slot = Some(Rc::new(Dict::Text {
                offsets: offsets.clone(),
                data: data.clone(),
            }));
        }
        slot.clone()
    }

    fn validity(mut valid: Vec<u64>, len: usize) -> Option<Vec<u64>> {
        valid.resize(len.div_ceil(64), 0);
        Bits::trim(&mut valid, len);
        let full = valid.iter().map(|w| w.count_ones() as usize).sum::<usize>() == len;
        (!full).then_some(valid)
    }
}
