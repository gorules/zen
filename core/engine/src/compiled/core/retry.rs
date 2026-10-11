use crate::compiled::leaf::Bits;
use std::rc::Rc;

pub(crate) struct Take {
    bits: Vec<u64>,
    rows: usize,
}

impl Take {
    pub fn new(rows: usize) -> Self {
        Self {
            bits: vec![0u64; rows.div_ceil(64)],
            rows,
        }
    }

    pub fn among(faults: &[u64], positions: &[usize]) -> (Vec<usize>, Vec<usize>) {
        positions.iter().partition(|&&row| Bits::get(faults, row))
    }

    pub fn seed(&mut self, prefault: Option<&[u64]>, positions: Option<&[usize]>) {
        let Some(prefault) = prefault else {
            return;
        };
        match positions {
            Some(positions) => positions.iter().filter(|&&row| Bits::get(prefault, row)).for_each(|&row| Bits::set(&mut self.bits, row, true)),
            None => self.bits.iter_mut().zip(prefault).for_each(|(t, f)| *t |= f),
        }
    }

    pub fn absorb(&mut self, failed: &[u64], ids: Option<&[u32]>, positions: Option<&[usize]>, local: usize) {
        match ids {
            Some(ids) => (0..local)
                .filter(|&at| Bits::get(failed, ids[at] as usize))
                .for_each(|at| Bits::set(&mut self.bits, positions.map_or(at, |p| p[at]), true)),
            None => self.bits.iter_mut().zip(failed).for_each(|(t, f)| *t |= f),
        }
        Bits::trim(&mut self.bits, self.rows);
    }

    pub fn any(&self) -> bool {
        !Bits::none(&self.bits)
    }

    pub fn bits(&self) -> &[u64] {
        &self.bits
    }

    pub fn drain(&mut self, keep: impl Fn(usize) -> bool) -> Vec<usize> {
        let rows = Bits::rows(&self.bits).filter(|&row| keep(row)).collect();
        self.bits.iter_mut().for_each(|word| *word = 0);
        rows
    }

    pub fn set(&mut self, row: usize) {
        Bits::set(&mut self.bits, row, true);
    }

    pub fn located(&self) -> Rc<[(usize, usize)]> {
        (0..self.rows).map(|row| (usize::from(Bits::get(&self.bits, row)), row)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Take;

    #[test]
    fn seeds_absorbs_and_drains_in_row_order() {
        let mut take = Take::new(70);
        take.seed(Some(&[0b1010, 0]), Some(&[1, 2, 3]));
        assert_eq!(take.bits(), &[0b1010, 0]);
        take.absorb(&[0b11], Some(&[1, 0, 1]), Some(&[5, 66, 69]), 3);
        assert_eq!(take.bits(), &[0b10_1010, 0b100100]);
        let rows = take.drain(|row| row != 3);
        assert_eq!(rows, vec![1, 5, 66, 69]);
        assert!(!take.any());
        take.set(2);
        assert_eq!(take.located()[2], (1, 2));
        assert_eq!(take.located()[1], (0, 1));
    }

    #[test]
    fn absorb_without_ids_trims_past_rows() {
        let mut take = Take::new(3);
        take.absorb(&[u64::MAX], None, None, 3);
        assert_eq!(take.bits(), &[0b111]);
    }

    #[test]
    fn among_keeps_position_order() {
        let (faulted, kept) = Take::among(&[0b100100], &[5, 1, 2, 3]);
        assert_eq!(faulted, vec![5, 2]);
        assert_eq!(kept, vec![1, 3]);
    }
}
