use super::{Bits, Leaf, Literal};
use std::cell::OnceCell;
use std::rc::Rc;
use zen_expression::lane::Column;
use zen_expression::Variable;

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
            bits.iter_mut()
                .zip(self.mask.iter())
                .for_each(|(b, m)| *b &= m);
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
            usize::try_from(codes[row])
                .ok()
                .and_then(|code| templates.get(code))
                .is_some_and(|literal| !matches!(literal, Literal::Null))
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
        self.templates
            .get(code)
            .filter(|literal| !matches!(literal, Literal::Null))
    }

    pub(super) fn get(&self, row: usize) -> Variable {
        self.template(row).map_or(Variable::Null, Literal::variable)
    }

    pub(super) fn mapped<'a>(&self, rows: impl Iterator<Item = usize>) -> Leaf<'a> {
        Templated::leaf(
            rows.map(|row| self.codes.get(row).copied().unwrap_or(-1))
                .collect(),
            self.templates.clone(),
        )
    }

    pub(super) fn dense(&self) -> &Rc<[Variable]> {
        self.dense
            .get_or_init(|| (0..self.codes.len()).map(|row| self.get(row)).collect())
    }
}

pub(crate) trait Dating {
    fn convert(&self, value: &Variable) -> Option<Variable>;
    fn single(&self) -> bool;
    fn listed(&self) -> bool;
    fn field(&self, name: &str) -> Option<Rc<dyn Dating>>;
}

pub(crate) struct Dated<'a> {
    raw: Leaf<'a>,
    len: usize,
    dating: Rc<dyn Dating>,
    dense: OnceCell<Leaf<'a>>,
    marks: OnceCell<Vec<u64>>,
    changed: OnceCell<bool>,
}

impl std::fmt::Debug for Dated<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dated")
            .field("raw", &self.raw)
            .field("len", &self.len)
            .finish()
    }
}

impl<'a> Dated<'a> {
    pub fn leaf(raw: Leaf<'a>, len: usize, dating: Rc<dyn Dating>) -> Leaf<'a> {
        Leaf::Dated(Rc::new(Dated {
            raw,
            len,
            dating,
            dense: OnceCell::new(),
            marks: OnceCell::new(),
            changed: OnceCell::new(),
        }))
    }

    pub fn raw(&self) -> &Leaf<'a> {
        &self.raw
    }

    pub fn dating(&self) -> &Rc<dyn Dating> {
        &self.dating
    }

    pub fn settled(&self) -> bool {
        self.dense.get().is_some()
    }

    pub fn get(&self, row: usize) -> Variable {
        if let Some(dense) = self.dense.get() {
            return dense.get(row);
        }
        let value = self.raw.get(row);
        self.dating.convert(&value).unwrap_or(value)
    }

    pub fn changed(&self) -> bool {
        *self.changed.get_or_init(|| match self.marks.get() {
            Some(marks) => marks.iter().any(|word| *word != 0),
            None => (0..self.len).any(|row| self.dating.convert(&self.raw.get(row)).is_some()),
        })
    }

    pub fn declared(&self) -> bool {
        self.dating.single()
            && matches!(
                crate::compiled::shred::Normalizer::class(&self.raw.column().values),
                Some(zen_expression::typed::Class::Text)
            )
            && self.changed()
    }

    pub fn marks(&self) -> &[u64] {
        self.dense();
        self.marks.get().map_or(&[], Vec::as_slice)
    }

    pub fn dense(&self) -> &Leaf<'a> {
        self.dense.get_or_init(|| {
            let mut marks = vec![0u64; self.len.div_ceil(64)];
            let mut changed = false;
            let values: Vec<Variable> = (0..self.len)
                .map(|row| {
                    let value = self.raw.get(row);
                    match self.dating.convert(&value) {
                        Some(next) => {
                            changed = true;
                            Bits::on(&mut marks, row);
                            next
                        }
                        None => value,
                    }
                })
                .collect();
            let _ = self.marks.set(marks);
            if !changed {
                return self.raw.clone();
            }
            let any = Leaf::Any(values.into());
            let mut validity = self.raw.validity(self.len);
            Bits::trim(&mut validity, self.len);
            let valid = validity
                .iter()
                .map(|w| w.count_ones() as usize)
                .sum::<usize>();
            match valid == self.len {
                true => any,
                false => Leaf::masked(any, &validity.into()),
            }
        })
    }
}
