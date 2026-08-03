#![allow(unsafe_code)]

use std::cell::{Cell, RefCell, UnsafeCell};
use std::fmt::{Debug, Formatter};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

const POOL_CAP: usize = 512;
const RETAINED_CAPACITY: usize = 64;

pub struct PoolStore {
    slots: Vec<NonNull<u8>>,
    free: unsafe fn(NonNull<u8>),
}

impl PoolStore {
    pub fn new(free: unsafe fn(NonNull<u8>)) -> Self {
        Self {
            slots: Vec::new(),
            free,
        }
    }
}

impl Drop for PoolStore {
    fn drop(&mut self) {
        for slot in self.slots.drain(..) {
            unsafe { (self.free)(slot) }
        }
    }
}

pub trait Recycle: Sized + Default + 'static {
    fn with_pool<R>(f: impl FnOnce(&mut PoolStore) -> R) -> Option<R>;

    fn recycle(&mut self);
}

struct Inner<T> {
    strong: Cell<usize>,
    borrow: Cell<isize>,
    value: UnsafeCell<T>,
}

unsafe fn free_slot<T>(ptr: NonNull<u8>) {
    unsafe { drop(Box::from_raw(ptr.cast::<Inner<T>>().as_ptr())) }
}

pub struct RcCell<T: Recycle> {
    ptr: NonNull<Inner<T>>,
}

impl<T: Recycle> RcCell<T> {
    pub fn new(value: T) -> Self {
        match Self::take_slot() {
            Some(ptr) => {
                unsafe { *ptr.as_ref().value.get() = value }
                Self { ptr }
            }
            None => Self::boxed(value),
        }
    }

    pub fn recycled() -> Self {
        match Self::take_slot() {
            Some(ptr) => Self { ptr },
            None => Self::boxed(T::default()),
        }
    }

    fn boxed(value: T) -> Self {
        let inner = Box::new(Inner {
            strong: Cell::new(1),
            borrow: Cell::new(0),
            value: UnsafeCell::new(value),
        });

        Self {
            ptr: unsafe { NonNull::new_unchecked(Box::into_raw(inner)) },
        }
    }

    fn take_slot() -> Option<NonNull<Inner<T>>> {
        let slot = T::with_pool(|pool| pool.slots.pop())??;
        let ptr = slot.cast::<Inner<T>>();
        unsafe {
            ptr.as_ref().strong.set(1);
            ptr.as_ref().borrow.set(0);
        }

        Some(ptr)
    }

    #[inline]
    fn inner(&self) -> &Inner<T> {
        unsafe { self.ptr.as_ref() }
    }

    #[inline]
    pub fn borrow(&self) -> Ref<'_, T> {
        let inner = self.inner();
        let state = inner.borrow.get();
        if state < 0 {
            panic!("RcCell is already mutably borrowed");
        }

        inner.borrow.set(state + 1);
        Ref { inner }
    }

    #[inline]
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        let inner = self.inner();
        if inner.borrow.get() != 0 {
            panic!("RcCell is already borrowed");
        }

        inner.borrow.set(-1);
        RefMut { inner }
    }

    pub fn try_borrow(&self) -> Option<Ref<'_, T>> {
        let inner = self.inner();
        let state = inner.borrow.get();
        if state < 0 {
            return None;
        }

        inner.borrow.set(state + 1);
        Some(Ref { inner })
    }

    pub fn try_borrow_mut(&self) -> Option<RefMut<'_, T>> {
        let inner = self.inner();
        if inner.borrow.get() != 0 {
            return None;
        }

        inner.borrow.set(-1);
        Some(RefMut { inner })
    }

    /// Reads through the cell without tracking a borrow. The caller guarantees
    /// no `borrow_mut` overlaps the returned reference, and that the `RcCell`
    /// outlives it.
    pub unsafe fn get_ref<'b>(&self) -> &'b T {
        unsafe { &*self.inner().value.get() }
    }

    #[inline]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        a.ptr == b.ptr
    }

    #[inline]
    pub fn as_ptr(this: &Self) -> *const T {
        this.inner().value.get()
    }

    pub fn strong_count(this: &Self) -> usize {
        this.inner().strong.get()
    }

    pub fn try_unwrap(this: Self) -> Result<T, Self> {
        if this.inner().strong.get() != 1 {
            return Err(this);
        }

        Ok(unsafe { std::mem::take(&mut *this.inner().value.get()) })
    }
}

impl<T: Recycle> Clone for RcCell<T> {
    #[inline]
    fn clone(&self) -> Self {
        let inner = self.inner();
        inner.strong.set(inner.strong.get() + 1);

        Self { ptr: self.ptr }
    }
}

impl<T: Recycle> Drop for RcCell<T> {
    fn drop(&mut self) {
        let inner = self.inner();
        let strong = inner.strong.get() - 1;
        inner.strong.set(strong);
        if strong != 0 {
            return;
        }

        // Runs before the pool is touched: recycling drops nested cells, which
        // re-enter the pool for their own type.
        unsafe { (*inner.value.get()).recycle() }
        inner.borrow.set(0);

        let ptr = self.ptr;
        let pooled = T::with_pool(|pool| {
            if pool.slots.len() >= POOL_CAP {
                return false;
            }

            pool.slots.push(ptr.cast::<u8>());
            true
        });

        if pooled != Some(true) {
            unsafe { drop(Box::from_raw(ptr.as_ptr())) }
        }
    }
}

impl<T: Recycle> Default for RcCell<T> {
    fn default() -> Self {
        Self::recycled()
    }
}

impl<T: Recycle + PartialEq> PartialEq for RcCell<T> {
    fn eq(&self, other: &Self) -> bool {
        Self::ptr_eq(self, other) || *self.borrow() == *other.borrow()
    }
}

impl<T: Recycle + Eq> Eq for RcCell<T> {}

impl<T: Recycle + serde::Serialize> serde::Serialize for RcCell<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.borrow().serialize(serializer)
    }
}

impl<T: Recycle + Debug> Debug for RcCell<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&*self.borrow(), f)
    }
}

pub struct Ref<'a, T: Recycle> {
    inner: &'a Inner<T>,
}

impl<'a, T: Recycle> Deref for Ref<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        unsafe { &*self.inner.value.get() }
    }
}

impl<'a, T: Recycle> Drop for Ref<'a, T> {
    #[inline]
    fn drop(&mut self) {
        self.inner.borrow.set(self.inner.borrow.get() - 1);
    }
}

pub struct RefMut<'a, T: Recycle> {
    inner: &'a Inner<T>,
}

impl<'a, T: Recycle> Deref for RefMut<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        unsafe { &*self.inner.value.get() }
    }
}

impl<'a, T: Recycle> DerefMut for RefMut<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.inner.value.get() }
    }
}

impl<'a, T: Recycle> Drop for RefMut<'a, T> {
    #[inline]
    fn drop(&mut self) {
        self.inner.borrow.set(0);
    }
}

macro_rules! recyclable {
    ($t:ty, $tls:ident, $reset:expr) => {
        thread_local! {
            static $tls: RefCell<PoolStore> = RefCell::new(PoolStore::new(free_slot::<$t>));
        }

        impl Recycle for $t {
            #[inline]
            fn with_pool<R>(f: impl FnOnce(&mut PoolStore) -> R) -> Option<R> {
                $tls.try_with(|pool| f(&mut pool.borrow_mut())).ok()
            }

            #[inline]
            fn recycle(&mut self) {
                let reset: fn(&mut $t) = $reset;
                reset(self)
            }
        }
    };
}

recyclable!(Vec<crate::variable::Variable>, ARRAY_POOL, |vec| {
    vec.clear();
    if vec.capacity() > RETAINED_CAPACITY {
        *vec = Vec::new();
    }
});

recyclable!(crate::variable::VariableMap, OBJECT_POOL, |map| map.reset());
