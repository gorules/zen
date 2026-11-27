use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(crate) struct Switches {
    typed: AtomicBool,
    census: AtomicBool,
    compilations: AtomicUsize,
}

impl Switches {
    pub fn typed(&self) -> bool {
        self.typed.load(Ordering::Relaxed)
    }

    pub fn census(&self) -> bool {
        self.census.load(Ordering::Relaxed)
    }

    pub fn compiled(&self) {
        self.compilations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn compilations(&self) -> usize {
        self.compilations.load(Ordering::Relaxed)
    }

    pub fn set_typed(&self, on: bool) {
        self.typed.store(on, Ordering::Relaxed);
    }

    pub fn set_census(&self, on: bool) {
        self.census.store(on, Ordering::Relaxed);
    }
}

pub(crate) static SWITCHES: Switches = Switches {
    typed: AtomicBool::new(true),
    census: AtomicBool::new(false),
    compilations: AtomicUsize::new(0),
};
