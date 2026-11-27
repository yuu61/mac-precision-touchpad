//! Framework locks kept in object contexts:
//! - [`SpinLocked`]: a value guarded by a `WDFSPINLOCK`, for state that callbacks running
//!   concurrently at up to `DISPATCH_LEVEL` must change together;
//! - [`WaitLock`]: a `WDFWAITLOCK` that serializes `PASSIVE_LEVEL` sequences, such as a
//!   read-modify-write of a device register made of synchronous transfers.

use core::cell::UnsafeCell;
use core::ptr::null_mut;

use wdk_sys::{NTSTATUS, WDFOBJECT, WDFSPINLOCK, WDFWAITLOCK, call_unsafe_wdf_function_binding};

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

// SAFETY: the value is only reached through `with`, which holds the spin lock for the whole exclusive
// borrow, so at most one thread uses it at a time (like `Mutex<T>`: `Sync` when `T: Send`). The lock
// handle is written once by `create` before any concurrent use and read-only afterwards.
unsafe impl<T: Send> Sync for SpinLocked<T> {}
// SAFETY: moving the guard moves the value and the lock handle, an opaque framework handle that is
// valid on any thread.
unsafe impl<T: Send> Send for SpinLocked<T> {}

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

/// A `WDFWAITLOCK` that serializes sequences running at `PASSIVE_LEVEL`, which may block (synchronous
/// I/O) while holding it.
///
/// Built in two steps, like [`SpinLocked`]: [`WaitLock::new`] in the context initializer, then
/// [`WaitLock::create`] once while the object is being set up.
#[derive(Debug)]
pub struct WaitLock {
    lock: UnsafeCell<WDFWAITLOCK>,
}

// SAFETY: the lock handle is written once by `create` before any concurrent use and read-only
// afterwards; acquiring and releasing a WDFWAITLOCK is thread safe.
unsafe impl Sync for WaitLock {}
// SAFETY: the handle is an opaque framework handle, valid on any thread.
unsafe impl Send for WaitLock {}

impl Default for WaitLock {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitLock {
    /// No lock yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lock: UnsafeCell::new(null_mut()),
        }
    }

    /// `WdfWaitLockCreate` with `ParentObject = parent`. Returns its status.
    ///
    /// # Safety
    /// Called once, at `PASSIVE_LEVEL`, before any [`WaitLock::with`] and while nothing else can reach
    /// `self`; `parent` is a live framework object that outlives every use of the lock.
    pub unsafe fn create(&self, parent: WDFOBJECT) -> NTSTATUS {
        let mut attributes = object_attributes_init();
        attributes.ParentObject = parent;
        // SAFETY: initialized attributes; the out-parameter is the lock cell, which nothing else reads
        // or writes until this returns (caller contract).
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfWaitLockCreate,
                &raw mut attributes,
                self.lock.get()
            )
        }
    }

    /// Runs `f` while holding the lock: `WdfWaitLockAcquire` (no timeout), `f`, `WdfWaitLockRelease`.
    /// `f` may block, e.g. in a synchronous request.
    ///
    /// # Safety
    /// [`WaitLock::create`] succeeded; the caller runs at `PASSIVE_LEVEL`; `f` does not call `with` on
    /// the same lock (it is not recursive), and nothing that `f` waits for takes this lock.
    pub unsafe fn with<R, F: FnOnce() -> R>(&self, f: F) -> R {
        // SAFETY: written once by `create` before any call of `with` (caller contract), read-only since.
        let lock = unsafe { *self.lock.get() };
        // SAFETY: a valid wait lock (caller contract) at PASSIVE_LEVEL; a NULL timeout waits until the
        // lock is acquired, so the status is always STATUS_SUCCESS.
        let _acquired = unsafe {
            call_unsafe_wdf_function_binding!(WdfWaitLockAcquire, lock, null_mut())
        };
        let result = f();
        // SAFETY: the lock acquired above, released once by the same thread.
        unsafe { call_unsafe_wdf_function_binding!(WdfWaitLockRelease, lock) };
        result
    }
}
