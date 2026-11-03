use super::{Bits, Leaf};
use std::rc::Rc;

#[derive(Clone, Debug)]
pub(crate) enum Mask<'a> {
    Bits(Rc<[u64]>),
    Valid(Leaf<'a>),
    Picked(Rc<[u64]>, Rc<[usize]>),
}

impl<'a> Mask<'a> {
    pub fn get(&self, row: usize) -> bool {
        match self {
            Mask::Bits(bits) => Bits::get(bits, row),
            Mask::Valid(leaf) => leaf.valid_at(row),
            Mask::Picked(bits, positions) => {
                positions.get(row).is_some_and(|&at| Bits::get(bits, at))
            }
        }
    }

    pub fn pick(&self, positions: &Rc<[usize]>) -> Mask<'a> {
        match self {
            Mask::Bits(bits) => Mask::Picked(bits.clone(), positions.clone()),
            Mask::Valid(leaf) => Mask::Valid(leaf.pick(positions)),
            Mask::Picked(bits, inner) => {
                Mask::Picked(bits.clone(), positions.iter().map(|&i| inner[i]).collect())
            }
        }
    }

    pub fn dense(&self, rows: usize) -> Rc<[u64]> {
        match self {
            Mask::Bits(bits) => bits.clone(),
            Mask::Valid(leaf) => leaf.validity(rows).into(),
            Mask::Picked(..) => Bits::of(rows, |row| self.get(row)).into(),
        }
    }
}
