use std::marker::PhantomData;

use bumpalo::Bump;

/// Structure used for self-referential arena (Bump).
/// Compared to regular reference, this prevents issues with retag caused by misaligned lifetimes
/// when trying to transmute. Bump is safely dropped once the owner struct is dropped.
#[derive(Debug)]
pub(crate) struct UnsafeArena<'arena> {
    arena: *mut Bump,
    _marker: PhantomData<&'arena Bump>,
}

impl<'arena> UnsafeArena<'arena> {
    pub fn new() -> Self {
        let boxed = Box::new(Bump::new());
        let leaked = Box::leak(boxed);

        Self {
            arena: leaked,
            _marker: Default::default(),
        }
    }

    pub fn get(&self) -> &'arena Bump {
        unsafe { &*self.arena }
    }

    pub fn with_mut<F>(&mut self, callback: F)
    where
        F: FnOnce(&mut Bump),
    {
        let reference = unsafe { &mut *self.arena };
        callback(reference);
    }
}

impl<'arena> Drop for UnsafeArena<'arena> {
    fn drop(&mut self) {
        unsafe {
            let _ = Box::from_raw(self.arena);
        }
    }
}
