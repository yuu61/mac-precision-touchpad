//! Handles and context pointers that object contexts share between callbacks.
//!
//! [`crate::declare_context_type!`] requires every context type to be `Sync`: callbacks of one device
//! run concurrently on several processors and all of them reach the context through a shared
//! reference. Raw pointers are neither `Send` nor `Sync`, so a context stores its WDF handles as
//! [`WdfHandle`] (or `AtomicPtr` when a handle changes after the context is published) and pointers
//! to other contexts as [`ContextPtr`]. Any other field that is not `Sync` (a `Cell`, a plain
//! mutable counter) still fails to compile.

use core::fmt;
use core::ptr::{null, null_mut};

/// A WDF object handle (`WDFDEVICE`, `WDFQUEUE`, `WDFREQUEST`, ...) kept in an object context.
///
/// A WDF handle is an opaque value the framework validates on every call, and a WDF method may be
/// called with it from any thread (each method states its IRQL). Sharing or sending the value is
/// therefore sound; whether the object is still alive is part of the safety contract of each WDF call
/// made with it, as it was for the raw handle.
///
/// `repr(transparent)`, so `&raw mut field` of a `WdfHandle<T>` can be cast to the `*mut *mut T`
/// out-parameter of a `Wdf*Create` call.
#[repr(transparent)]
pub struct WdfHandle<T>(*mut T);

// SAFETY: see the type docs: the handle is an opaque value, valid from any thread.
unsafe impl<T> Send for WdfHandle<T> {}
// SAFETY: as above; `&WdfHandle` only gives out copies of the value.
unsafe impl<T> Sync for WdfHandle<T> {}

impl<T> WdfHandle<T> {
    /// No object (`NULL`, `WDF_NO_HANDLE`).
    pub const NULL: Self = Self(null_mut());

    /// Wraps a raw handle.
    #[must_use]
    pub const fn new(raw: *mut T) -> Self {
        Self(raw)
    }

    /// The raw handle, for a WDF call.
    #[must_use]
    pub const fn get(self) -> *mut T {
        self.0
    }

    /// Whether this is `NULL`.
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0.is_null()
    }
}

impl<T> Clone for WdfHandle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for WdfHandle<T> {}

impl<T> PartialEq for WdfHandle<T> {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.0, other.0)
    }
}

impl<T> Eq for WdfHandle<T> {}

impl<T> fmt::Debug for WdfHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WdfHandle({:#x})", self.0.addr())
    }
}

/// A pointer to an object context that is itself `Sync`, e.g. a request context's back pointer to the
/// context of the device that issued the request.
///
/// It may be shared and sent because the pointee is `Sync`: everything reachable through it is safe
/// to use from several threads at once. Dereferencing it is still `unsafe` (the context must be
/// alive), exactly as for the raw pointer.
#[repr(transparent)]
pub struct ContextPtr<T>(*const T);

// SAFETY: the pointee is `Sync`, so a shared reference to it may be used on any thread; the pointer
// itself is a plain value.
unsafe impl<T: Sync> Send for ContextPtr<T> {}
// SAFETY: as above.
unsafe impl<T: Sync> Sync for ContextPtr<T> {}

impl<T> ContextPtr<T> {
    /// No context.
    pub const NULL: Self = Self(null());

    /// Wraps a pointer to a context.
    #[must_use]
    pub const fn new(context: *const T) -> Self {
        Self(context)
    }

    /// The raw pointer.
    #[must_use]
    pub const fn get(self) -> *const T {
        self.0
    }
}

impl<T> Clone for ContextPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for ContextPtr<T> {}

impl<T> fmt::Debug for ContextPtr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContextPtr({:#x})", self.0.addr())
    }
}
