pub(super) struct ByBlock<T>(Vec<(usize, T)>);

impl<T> Default for ByBlock<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> ByBlock<T> {
    pub fn get(&self, block: usize) -> Option<&T> {
        self.0
            .iter()
            .find(|(b, _)| *b == block)
            .map(|(_, value)| value)
    }

    pub fn get_mut(&mut self, block: usize) -> Option<&mut T> {
        self.0
            .iter_mut()
            .find(|(b, _)| *b == block)
            .map(|(_, value)| value)
    }

    pub fn has(&self, block: usize) -> bool {
        self.0.iter().any(|(b, _)| *b == block)
    }

    pub fn insert(&mut self, block: usize, value: T) {
        self.0.push((block, value));
    }

    pub fn entry(&mut self, block: usize, make: impl FnOnce() -> T) -> &mut T {
        let at = match self.0.iter().position(|(b, _)| *b == block) {
            Some(at) => at,
            None => {
                self.0.push((block, make()));
                self.0.len() - 1
            }
        };
        &mut self.0[at].1
    }
}

pub(super) struct Chosen {
    pub firsts: Vec<u32>,
    pub matched: Vec<u64>,
}

pub(super) struct Fallback {
    rows: Vec<bool>,
    fell: usize,
}

impl Fallback {
    pub fn new(rows: Vec<bool>) -> Self {
        Self { rows, fell: 0 }
    }

    pub fn fall(&mut self, row: usize) {
        if !self.rows[row] {
            self.rows[row] = true;
            self.fell += 1;
        }
    }

    pub fn fallen(&self, row: usize) -> bool {
        self.rows[row]
    }

    pub fn fell(&self) -> usize {
        self.fell
    }

    pub fn rows(self) -> Vec<bool> {
        self.rows
    }
}
