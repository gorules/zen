use std::cell::OnceCell;
use std::rc::Rc;
use zen_expression::lane::Column;
use zen_expression::Variable;
use super::{Bits, Leaf, Literal};

#[derive(Debug)]
pub(crate) struct Picked<'a> {
    pub(super) source: Leaf<'a>,
    pub(super) positions: Rc<[usize]>,
    pub(super) dense: OnceCell<Leaf<'a>>,
}

#[derive(Debug)]
pub(crate) struct Stitched<'a> {
    pub(super) sources: Vec<Leaf<'a>>,
    pub(super) located: Rc<[(usize, usize)]>,
    pub(super) dense: OnceCell<Leaf<'a>>,
}

#[derive(Debug)]
pub(crate) struct Input<'a> {
    pub column: Column<'a>,
    pub rows: usize,
    pub(super) valued: OnceCell<Vec<u64>>,
}

#[derive(Debug)]
pub(crate) struct Masked<'a> {
    pub(super) inner: Leaf<'a>,
    pub(super) mask: Rc<[u64]>,
    pub(super) bits: OnceCell<Rc<[u64]>>,
}

impl Masked<'_> {
    pub(super) fn bits(&self) -> &Rc<[u64]> {
        self.bits.get_or_init(|| {
            let rows = self.inner.len();
            let mut bits = self.inner.validity(rows);
            bits.iter_mut().zip(self.mask.iter()).for_each(|(b, m)| *b &= m);
            bits.iter_mut().skip(self.mask.len()).for_each(|b| *b = 0);
            bits.into()
        })
    }
}

#[derive(Debug)]
pub(crate) struct Scattered<'a> {
    pub(super) inner: Leaf<'a>,
    pub(super) positions: Rc<[usize]>,
    pub(super) rows: usize,
    pub(super) dense: OnceCell<Leaf<'a>>,
}

#[derive(Debug)]
pub(crate) struct Templated {
    pub(super) codes: Rc<[i32]>,
    pub(super) templates: Rc<[Literal]>,
    pub(super) valid: Vec<u64>,
    dense: OnceCell<Rc<[Variable]>>,
}

impl Templated {
    pub fn leaf<'a>(codes: Rc<[i32]>, templates: Rc<[Literal]>) -> Leaf<'a> {
        let valid = Bits::of(codes.len(), |row| {
            usize::try_from(codes[row]).ok().and_then(|code| templates.get(code)).is_some_and(|literal| !matches!(literal, Literal::Null))
        });
        Leaf::Templated(Rc::new(Templated {
            codes,
            templates,
            valid,
            dense: OnceCell::new(),
        }))
    }

    pub(super) fn template(&self, row: usize) -> Option<&Literal> {
        let code = usize::try_from(*self.codes.get(row)?).ok()?;
        self.templates.get(code).filter(|literal| !matches!(literal, Literal::Null))
    }

    pub(super) fn get(&self, row: usize) -> Variable {
        self.template(row).map_or(Variable::Null, Literal::variable)
    }

    pub(super) fn mapped<'a>(&self, rows: impl Iterator<Item = usize>) -> Leaf<'a> {
        Templated::leaf(rows.map(|row| self.codes.get(row).copied().unwrap_or(-1)).collect(), self.templates.clone())
    }

    pub(super) fn dense(&self) -> &Rc<[Variable]> {
        self.dense.get_or_init(|| (0..self.codes.len()).map(|row| self.get(row)).collect())
    }
}
