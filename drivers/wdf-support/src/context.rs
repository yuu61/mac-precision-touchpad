//! WDF object contexts: the Rust equivalent of
//! `WDF_DECLARE_CONTEXT_TYPE_WITH_NAME` (see [`crate::declare_context_type!`]).
//!
//! In C (KMDF 1.23 / UMDF 2.15 `wdfobject.h`) the macro emits
//!
//! ```c
//! __declspec(allocate(".data")) __declspec(selectany)
//! extern const WDF_OBJECT_CONTEXT_TYPE_INFO _WDF_DEVICE_CONTEXT_TYPE_INFO = {
//!     sizeof(WDF_OBJECT_CONTEXT_TYPE_INFO), "DEVICE_CONTEXT", sizeof(DEVICE_CONTEXT),
//!     &_WDF_DEVICE_CONTEXT_TYPE_INFO,   // UniqueType
//!     NULL,                             // EvtDriverGetUniqueContextType
//! };
//! DEVICE_CONTEXT* DeviceGetContext(WDFOBJECT Handle) {
//!     return WdfObjectGetTypedContextWorker(Handle, _WDF_DEVICE_CONTEXT_TYPE_INFO.UniqueType);
//! }
//! ```
//!
//! and `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE` stores `...TYPE_INFO.UniqueType`
//! in `WDF_OBJECT_ATTRIBUTES::ContextTypeInfo`. The macro here produces the same
//! static (in `.data`, self-referential `UniqueType`) and an `unsafe fn` getter
//! that calls [`get_typed_context`]; [`crate::init::object_attributes_init_context_type`]
//! takes the static.
//!
//! The framework zero-fills context memory; it never runs Rust constructors or
//! destructors. Initialize a context with `ptr.write(value)` right after the
//! object is created, and release resources in `EvtCleanupCallback` /
//! `EvtDestroyCallback` (Drop never runs).

use core::ffi::c_void;

use wdk_sys::{PCWDF_OBJECT_CONTEXT_TYPE_INFO, WDF_OBJECT_CONTEXT_TYPE_INFO, WDFOBJECT, call_unsafe_wdf_function_binding};

/// A `WDF_OBJECT_CONTEXT_TYPE_INFO` that can live in a `static`.
///
/// Create it only through [`crate::declare_context_type!`].
#[repr(transparent)]
pub struct ContextTypeInfo(WDF_OBJECT_CONTEXT_TYPE_INFO);

// SAFETY: the wrapped structure only holds pointers to immutable 'static data
// (the NUL-terminated name and the structure itself) and is never mutated after
// const initialization; WDF only reads it. Sharing `&ContextTypeInfo` between
// threads is therefore sound.
unsafe impl Sync for ContextTypeInfo {}

impl ContextTypeInfo {
    /// Wraps a fully initialized type-info structure. Used by
    /// [`crate::declare_context_type!`]; not meant to be called directly.
    #[doc(hidden)]
    #[must_use]
    pub const fn __new(info: WDF_OBJECT_CONTEXT_TYPE_INFO) -> Self {
        Self(info)
    }

    /// Pointer to the wrapped `WDF_OBJECT_CONTEXT_TYPE_INFO`
    /// (`WDF_GET_CONTEXT_TYPE_INFO(T)`).
    #[must_use]
    pub const fn as_ptr(&'static self) -> PCWDF_OBJECT_CONTEXT_TYPE_INFO {
        &raw const self.0
    }

    /// `WDF_GET_CONTEXT_TYPE_INFO(T)->UniqueType`: the value C passes to
    /// `WdfObjectGetTypedContextWorker` and stores in
    /// `WDF_OBJECT_ATTRIBUTES::ContextTypeInfo`. For types declared with
    /// [`crate::declare_context_type!`] this points at the structure itself.
    #[must_use]
    pub const fn unique_type(&'static self) -> PCWDF_OBJECT_CONTEXT_TYPE_INFO {
        self.0.UniqueType
    }

    /// `sizeof(T)` of the context type.
    #[must_use]
    pub const fn context_size(&'static self) -> usize {
        self.0.ContextSize
    }
}

/// `WdfObjectGetTypedContextWorker(handle, context_type->UniqueType)`.
///
/// Returns the context of `context_type` attached to `handle`, or null if the
/// object has no such context.
///
/// # Safety
/// - `handle` must be a valid, live WDF object handle (any `WDFxxx` type).
/// - Must be called at IRQL <= DISPATCH_LEVEL (KMDF).
/// - The returned pointer is valid until the object is deleted; the caller is
///   responsible for not creating aliasing `&mut` references to the context.
#[must_use]
pub unsafe fn get_typed_context(handle: WDFOBJECT, context_type: &'static ContextTypeInfo) -> *mut c_void {
    // SAFETY: `handle` is a valid WDF object (caller contract) and
    // `unique_type()` points at a 'static, correctly initialized
    // WDF_OBJECT_CONTEXT_TYPE_INFO; WdfObjectGetTypedContextWorker is in the
    // KMDF 1.23 and UMDF 2.15 function tables.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfObjectGetTypedContextWorker,
            handle,
            context_type.unique_type()
        )
    }
}

/// Declares a WDF object context type: the Rust equivalent of
/// `WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(Type, Getter)`.
///
/// ```ignore
/// pub struct DeviceContext { /* ... */ }
///
/// wdf_support::declare_context_type!(
///     pub(crate) DeviceContext,       // context type (an identifier in scope)
///     DEVICE_CONTEXT_TYPE_INFO,       // name of the generated static
///     device_get_context              // name of the generated getter
/// );
/// // Optional 4th argument: the ContextName WDF shows in the debugger
/// // (`!wdfkd.wdfobject`), e.g. `name = "DEVICE_CONTEXT"` to keep the C name.
///
/// let mut attributes =
///     wdf_support::init::object_attributes_init_context_type(&DEVICE_CONTEXT_TYPE_INFO);
/// // ... WdfDeviceCreate(&mut device_init, &mut attributes, &mut device) ...
/// // SAFETY: `device` was just created with this context type.
/// unsafe { device_get_context(device).write(DeviceContext { /* ... */ }) };
/// ```
///
/// Generates (with the given visibility):
/// - `static $info: ContextTypeInfo` in section `.data` with `Size`,
///   `ContextName` (NUL-terminated), `ContextSize = size_of::<$ctx>()`,
///   `UniqueType = &$info`, `EvtDriverGetUniqueContextType = None`;
/// - `unsafe fn $getter<H>(handle: *mut H) -> *mut $ctx`, accepting any WDF
///   handle type (`WDFDEVICE`, `WDFQUEUE`, `WDFOBJECT`, ...).
///
/// The context type must be `Sized`; its alignment must not exceed what WDF
/// guarantees for context memory (`MEMORY_ALLOCATION_ALIGNMENT`, 16 bytes on
/// x64/ARM64), which is checked at compile time.
#[macro_export]
macro_rules! declare_context_type {
    ($vis:vis $ctx:ident, $info:ident, $getter:ident $(,)?) => {
        $crate::declare_context_type!(
            @emit $vis $ctx, $info, $getter,
            ::core::concat!(::core::stringify!($ctx), "\0")
        );
    };
    ($vis:vis $ctx:ident, $info:ident, $getter:ident, name = $name:literal $(,)?) => {
        $crate::declare_context_type!(@emit $vis $ctx, $info, $getter, ::core::concat!($name, "\0"));
    };
    (@emit $vis:vis $ctx:ident, $info:ident, $getter:ident, $c_name:expr) => {
        /// WDF context type information for
        #[doc = ::core::concat!("[`", ::core::stringify!($ctx), "`]")]
        /// (`WDF_DECLARE_CONTEXT_TYPE_WITH_NAME`).
        // C places these in ".data" (WDF_TYPE_DEFAULT_SECTION_NAME).
        #[unsafe(link_section = ".data")]
        $vis static $info: $crate::context::ContextTypeInfo = $crate::context::ContextTypeInfo::__new(
            $crate::__private::wdk_sys::WDF_OBJECT_CONTEXT_TYPE_INFO {
                Size: $crate::init::wdf_size::<$crate::__private::wdk_sys::WDF_OBJECT_CONTEXT_TYPE_INFO>(),
                // NUL-terminated; WDF only reads it (the C field is PCHAR).
                ContextName: $c_name.as_ptr().cast::<$crate::__private::wdk_sys::CHAR>().cast_mut(),
                ContextSize: ::core::mem::size_of::<$ctx>(),
                // UniqueType points at the canonical instance: this static.
                // `ContextTypeInfo` is `repr(transparent)`, so the cast is exact.
                UniqueType: (&raw const $info).cast::<$crate::__private::wdk_sys::WDF_OBJECT_CONTEXT_TYPE_INFO>(),
                EvtDriverGetUniqueContextType: ::core::option::Option::None,
            },
        );

        const _: () = ::core::assert!(
            ::core::mem::align_of::<$ctx>() <= 16,
            "WDF context memory is only MEMORY_ALLOCATION_ALIGNMENT (16) aligned"
        );

        /// Returns the
        #[doc = ::core::concat!("[`", ::core::stringify!($ctx), "`]")]
        /// attached to a WDF object (the C casting function generated by
        /// `WDF_DECLARE_CONTEXT_TYPE_WITH_NAME`), or null if the object has no
        /// context of this type.
        ///
        /// # Safety
        /// `handle` must be a valid, live WDF object handle, called at
        /// IRQL <= DISPATCH_LEVEL in kernel mode. The context memory is zeroed
        /// by WDF and must be initialized with `ptr.write(..)` before it is read
        /// as the Rust type; the caller must not create aliasing `&mut`
        /// references to it.
        #[allow(dead_code)]
        $vis unsafe fn $getter<H>(handle: *mut H) -> *mut $ctx {
            // SAFETY: forwarded to the caller (valid live handle, IRQL);
            // `$info` is the 'static type info generated above.
            unsafe { $crate::context::get_typed_context(handle.cast(), &$info) }.cast::<$ctx>()
        }
    };
}
