use crate::compiled::leaf::Bits;
use std::cell::OnceCell;
use std::rc::Rc;
use zen_expression::lane::Column;
use zen_expression::typed::List;

pub(crate) type Family = (Rc<[usize]>, Rc<[usize]>);

#[derive(Clone)]
pub(crate) struct Frame {
    pub offsets: Rc<[i32]>,
    pub validity: Option<Rc<[u64]>>,
    pub children: usize,
}

pub(crate) struct Kin {
    pub at: usize,
    pub kids: Rc<[usize]>,
    pub parents: Rc<[usize]>,
    pub identity: bool,
    codes: OnceCell<Option<Rc<[i32]>>>,
}

impl Frame {
    pub fn range(&self, row: usize) -> Option<(usize, usize)> {
        if self.validity.as_ref().is_some_and(|bits| !Bits::get(bits, row)) {
            return None;
        }
        let a = usize::try_from(*self.offsets.get(row)?).unwrap_or(0);
        let b = usize::try_from(*self.offsets.get(row + 1)?).unwrap_or(0).max(a);
        Some((a.min(self.children), b.min(self.children)))
    }

    pub fn span(&self, rows: usize) -> Option<(usize, usize)> {
        let offsets = self.offsets.get(..=rows)?;
        let base = usize::try_from(*offsets.first()?).ok()?;
        let end = usize::try_from(*offsets.last()?).ok()?;
        let sorted = offsets.windows(2).all(|pair| pair[0] <= pair[1]);
        (sorted && base <= end && end <= self.children).then_some((base, end))
    }

    pub fn identity(&self, kids: &[usize]) -> bool {
        kids.len() == self.children && kids.iter().enumerate().all(|(i, &k)| i == k)
    }

    pub fn list<'f>(&'f self, validity: Option<&'f [u64]>, fields: Vec<Column<'f>>) -> List<'f> {
        List::new(&self.offsets, validity.or(self.validity.as_deref()).map(|bits| (bits, 0)), fields)
    }
}

impl Kin {
    pub fn new(at: usize, kids: Rc<[usize]>, parents: Rc<[usize]>, identity: bool) -> Self {
        Self {
            at,
            kids,
            parents,
            identity,
            codes: OnceCell::new(),
        }
    }

    pub fn spans(frame: &Frame, rows: usize) -> Option<Family> {
        if frame.validity.is_some() {
            return None;
        }
        let offsets = frame.offsets.get(..=rows)?;
        let ordered = offsets.first() == Some(&0)
            && offsets.last().and_then(|&end| usize::try_from(end).ok()) == Some(frame.children)
            && offsets.windows(2).all(|pair| pair[0] <= pair[1]);
        if !ordered {
            return None;
        }
        let mut parents = Vec::with_capacity(frame.children);
        for (row, pair) in offsets.windows(2).enumerate() {
            parents.extend(std::iter::repeat_n(row, (pair[1] - pair[0]) as usize));
        }
        Some(((0..frame.children).collect(), parents.into()))
    }

    pub fn gather(
        positions: &[usize],
        capacity: usize,
        range: impl Fn(usize) -> Option<(usize, usize)>,
        mut visit: impl FnMut(usize, Option<(usize, usize)>),
    ) -> (Vec<usize>, Vec<usize>) {
        let mut kids = Vec::with_capacity(capacity);
        let mut parents = Vec::with_capacity(capacity);
        for &row in positions {
            match range(row) {
                Some((a, b)) => {
                    let start = kids.len();
                    kids.extend(a..b);
                    parents.extend(std::iter::repeat_n(row, b.saturating_sub(a)));
                    visit(row, Some((start, kids.len())));
                }
                None => visit(row, None),
            }
        }
        (kids, parents)
    }

    pub fn codes(&self) -> Option<Rc<[i32]>> {
        self.codes
            .get_or_init(|| self.parents.iter().map(|&row| i32::try_from(row).ok()).collect())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, Kin};
    use std::rc::Rc;

    fn frame(offsets: &[i32], validity: Option<&[u64]>, children: usize) -> Frame {
        Frame {
            offsets: offsets.into(),
            validity: validity.map(Rc::from),
            children,
        }
    }

    #[test]
    fn invalid_rows_have_no_range_and_empty_lists_do() {
        let lists = frame(&[0, 2, 2, 5], Some(&[0b101]), 5);
        assert_eq!(lists.range(0), Some((0, 2)));
        assert_eq!(lists.range(1), None);
        assert_eq!(lists.range(2), Some((2, 5)));
        assert_eq!(lists.range(3), None);
        let empty = frame(&[0, 3, 3], None, 3);
        assert_eq!(empty.range(1), Some((3, 3)));
    }

    #[test]
    fn ranges_clamp_to_children() {
        let lists = frame(&[0, 4, 2, 9], None, 6);
        assert_eq!(lists.range(1), Some((4, 4)));
        assert_eq!(lists.range(2), Some((2, 6)));
    }

    #[test]
    fn spans_need_sorted_offsets_within_children() {
        assert_eq!(frame(&[1, 2, 4], None, 4).span(2), Some((1, 4)));
        assert_eq!(frame(&[0, 3, 2], None, 4).span(2), None);
        assert_eq!(frame(&[0, 2, 5], None, 4).span(2), None);
        assert_eq!(frame(&[0, 2], None, 4).span(2), None);
    }

    #[test]
    fn kin_spans_and_gather_agree() {
        let lists = frame(&[0, 2, 2, 5], None, 5);
        let (kids, parents) = Kin::spans(&lists, 3).unwrap_or_else(|| (Rc::from([]), Rc::from([])));
        let mut seen = Vec::new();
        let (gathered, owners) = Kin::gather(&[0, 1, 2], 5, |row| lists.range(row), |row, span| seen.push((row, span)));
        assert_eq!(&*kids, gathered.as_slice());
        assert_eq!(&*parents, owners.as_slice());
        assert_eq!(seen, vec![(0, Some((0, 2))), (1, Some((2, 2))), (2, Some((2, 5)))]);
        assert!(lists.identity(&gathered));
        assert!(Kin::spans(&frame(&[0, 2, 5], Some(&[0b11]), 5), 2).is_none());
    }

    #[test]
    fn kin_codes_are_parent_rows() {
        let kin = Kin::new(0, Rc::from([0, 1, 2]), Rc::from([0, 0, 1]), true);
        assert_eq!(kin.codes().as_deref(), Some(&[0, 0, 1][..]));
    }
}
