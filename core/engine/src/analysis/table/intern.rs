pub(super) struct Interner<T> {
    stride: usize,
    data: Vec<T>,
    slots: Vec<u32>,
    len: usize,
}

impl<T: Copy + Eq + Into<u64>> Default for Interner<T> {
    fn default() -> Self {
        Self::new(0)
    }
}

impl<T: Copy + Eq + Into<u64>> Interner<T> {
    const INITIAL_SLOTS: usize = 16;

    pub(super) fn new(stride: usize) -> Self {
        Self {
            stride,
            data: Vec::new(),
            slots: vec![0; Self::INITIAL_SLOTS],
            len: 0,
        }
    }

    pub(super) fn reset(&mut self, stride: usize) {
        self.stride = stride;
        self.data.clear();
        self.slots.iter_mut().for_each(|slot| *slot = 0);
        self.len = 0;
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn stride(&self) -> usize {
        self.stride
    }

    pub(super) fn get(&self, id: u32) -> &[T] {
        let start = id as usize * self.stride;
        &self.data[start..start + self.stride]
    }

    pub(super) fn intern(&mut self, key: &[T]) -> u32 {
        debug_assert_eq!(key.len(), self.stride);
        if (self.len + 1) * 2 > self.slots.len() {
            self.grow();
        }
        let mask = self.slots.len() - 1;
        let mut slot = Self::hash(key) as usize & mask;
        loop {
            match self.slots[slot] {
                0 => {
                    let id = self.len as u32;
                    self.slots[slot] = id + 1;
                    self.data.extend_from_slice(key);
                    self.len += 1;
                    return id;
                }
                taken if self.get(taken - 1) == key => return taken - 1,
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    fn grow(&mut self) {
        let mask = self.slots.len() * 2 - 1;
        let mut slots = vec![0u32; mask + 1];
        for id in 0..self.len as u32 {
            let mut slot = Self::hash(self.get(id)) as usize & mask;
            while slots[slot] != 0 {
                slot = (slot + 1) & mask;
            }
            slots[slot] = id + 1;
        }
        self.slots = slots;
    }

    fn hash(key: &[T]) -> u64 {
        let mut hash: u64 = 0x9E37_79B9_7F4A_7C15;
        for &part in key {
            hash = (hash ^ part.into()).wrapping_mul(0xA24B_AED4_963E_E407);
            hash ^= hash >> 29;
        }
        hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_equal_keys_once_and_keeps_insertion_order() {
        let mut interner: Interner<u64> = Interner::new(2);
        let ids: Vec<u32> = (0..500u64)
            .map(|i| interner.intern(&[i % 97, i % 3]))
            .collect();
        assert_eq!(interner.len(), 97 * 3);
        assert_eq!(ids[0], 0);
        assert_eq!(ids[1], 1);
        for (i, id) in ids.iter().enumerate() {
            let i = i as u64;
            assert_eq!(interner.get(*id), &[i % 97, i % 3]);
        }
        interner.reset(1);
        assert_eq!(interner.len(), 0);
        assert_eq!(interner.intern(&[7]), 0);
        assert_eq!(interner.intern(&[7]), 0);
        assert_eq!(interner.intern(&[8]), 1);
    }

    #[test]
    fn empty_keys_share_one_id() {
        let mut interner: Interner<u32> = Interner::new(0);
        assert_eq!(interner.intern(&[]), 0);
        assert_eq!(interner.intern(&[]), 0);
        assert_eq!(interner.len(), 1);
    }
}
