//! A value guarded by a framework spin lock (`WDFSPINLOCK`), for state that callbacks running
//! concurrently at up to `DISPATCH_LEVEL` must change together.

use core::cell::UnsafeCell;
use core::ptr::null_mut;

use wdk_sys::{NTSTATUS, WDFOBJECT, WDFSPINLOCK, call_unsafe_wdf_function_binding};

use crate::init::object_attributes_init;

/// A `T` that is only reached while holding a `WDFSPINLOCK`.
///
/// Built in two steps because it lives in a WDF object context: [`SpinLocked::new`] in the context
/// initializer, then [`SpinLocked::create`] once, while the object is being set up. [`SpinLocked::with`]
/// may be called only after `create` succeeded.
#[derive(Debug)]
pub struct SpinLocked<T> {
    lock: UnsafeCell<WDFSPINLOCK>,
    value: UnsafeCell<T>,
}

impl<T> SpinLocked<T> {
    /// The value, with no lock yet.
    pub const fn new(value: T) -> Self {
        Self {
            lock: UnsafeCell::new(null_mut()),
            value: UnsafeCell::new(value),
        }
    }

    /// `WdfSpinLockCreate` with `ParentObject = parent`. Returns its status.
    ///
    /// # Safety
    /// Called once, at `PASSIVE_LEVEL`, before any [`SpinLocked::with`] and while nothing else can reach
    /// `self`; `parent` is a live framework object that outlives every use of the lock.
    pub unsafe fn create(&self, parent: WDFOBJECT) -> NTSTATUS {
        let mut attributes = object_attributes_init();
        attributes.ParentObject = parent;
        // SAFETY: initialized attributes; the out-parameter is the lock cell, which nothing else reads or
        // writes until this returns (caller contract).
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfSpinLockCreate,
                &raw mut attributes,
                self.lock.get()
            )
        }
    }

    /// Runs `f` with exclusive access to the value: `WdfSpinLockAcquire`, `f`, `WdfSpinLockRelease`.
    /// `f` runs at `DISPATCH_LEVEL`.
    ///
    /// # Safety
    /// [`SpinLocked::create`] succeeded; the caller runs at IRQL <= `DISPATCH_LEVEL`; `f` does not call
    /// `with` on the same value (the lock is not recursive) and calls nothing that can run a callback of
    /// this driver synchronously (such as sending or cancelling a request).
    pub unsafe fn with<R, F: FnOnce(&mut T) -> R>(&self, f: F) -> R {
        // SAFETY: written once by `create` before any call of `with` (caller contract), read-only since.
        let lock = unsafe { *self.lock.get() };
        // SAFETY: a valid spin lock (caller contract), IRQL <= DISPATCH_LEVEL.
        unsafe { call_unsafe_wdf_function_binding!(WdfSpinLockAcquire, lock) };
        // SAFETY: every access to the value goes through `with`, which holds the lock for the lifetime of
        // this exclusive borrow, and `f` does not re-enter (caller contract).
        let result = f(unsafe { &mut *self.value.get() });
        // SAFETY: the lock acquired above, released once, at the same IRQL.
        unsafe { call_unsafe_wdf_function_binding!(WdfSpinLockRelease, lock) };
        result
    }
}
