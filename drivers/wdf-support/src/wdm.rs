//! KMDF-only WDM escape hatches: the IRP behind a `WDFREQUEST` and the
//! `wdm.h` inline helpers bindgen does not emit.
//!
//! The C KMDF drivers read the `HID_XFER_PACKET` of `METHOD_NEITHER` HID IOCTLs
//! (`IOCTL_HID_GET_FEATURE`, `IOCTL_HID_SET_FEATURE`, ...) as
//! `WdfRequestWdmGetIrp(Request)->UserBuffer`, and `AmtPtpHidFilter` stores a
//! packet pointer into `Irp->UserBuffer` of a request it built itself. These
//! helpers reproduce exactly those field accesses.
//!
//! Field paths go through bindgen's generated `__bindgen_anon_N` members of the
//! KM bindings (`_IRP`, `_IO_STACK_LOCATION`); only this module names them.
//!
//! Not available under UMDF: `WdfRequestWdmGetIrp` is not in the UMDF function
//! table and UMDF has no IRPs (same as the C code).

use wdk_sys::{PIO_STACK_LOCATION, PIRP, PVOID, WDFREQUEST, call_unsafe_wdf_function_binding};

/// `WdfRequestWdmGetIrp(Request)`.
///
/// Returns null if the framework has no IRP for the request (e.g. a
/// driver-created request that was never formatted).
///
/// # Safety
/// - `request` must be a valid `WDFREQUEST` the driver currently owns or that is
///   otherwise guaranteed to stay alive (not completed) while the IRP is used.
/// - Call at IRQL <= `DISPATCH_LEVEL`.
/// - The returned IRP is only valid until the request is completed; do not keep
///   it beyond that point.
#[must_use]
pub unsafe fn request_wdm_get_irp(request: WDFREQUEST) -> PIRP {
    // SAFETY: valid request handle per the caller contract;
    // WdfRequestWdmGetIrp is in the KMDF 1.23 function table.
    unsafe { call_unsafe_wdf_function_binding!(WdfRequestWdmGetIrp, request) }
}

/// `IoGetCurrentIrpStackLocation(Irp)` (`wdm.h`):
/// `Irp->Tail.Overlay.CurrentStackLocation`.
///
/// The `NT_ASSERT(Irp->CurrentLocation <= Irp->StackCount + 1)` of the C
/// inline is intentionally omitted (an assertion failure would be a panic, i.e.
/// a bugcheck).
///
/// # Safety
/// `irp` must point to a valid IRP that is currently owned by this driver
/// (i.e. it is the IRP of a request the driver has not completed/forwarded).
#[must_use]
pub unsafe fn io_get_current_irp_stack_location(irp: PIRP) -> PIO_STACK_LOCATION {
    // SAFETY: `irp` is valid per the caller contract. `Tail.Overlay` is the
    // active member of the `Tail` union while the IRP is owned by a driver
    // (I/O manager invariant); the read copies a pointer, no reference is made.
    unsafe {
        (*irp)
            .Tail
            .Overlay
            .__bindgen_anon_2
            .__bindgen_anon_1
            .CurrentStackLocation
    }
}

/// `IoGetNextIrpStackLocation(Irp)` (`wdm.h`):
/// `Irp->Tail.Overlay.CurrentStackLocation - 1`.
///
/// # Safety
/// As [`io_get_current_irp_stack_location`]; additionally the IRP must have a
/// next stack location (`CurrentLocation > 1`), which holds for IRPs built by
/// `WdfRequestCreate`/formatted for an I/O target.
#[must_use]
pub unsafe fn io_get_next_irp_stack_location(irp: PIRP) -> PIO_STACK_LOCATION {
    // SAFETY: caller guarantees a next stack location exists, so the result is
    // still inside the IRP's stack-location array (it precedes the current one).
    unsafe { io_get_current_irp_stack_location(irp).wrapping_sub(1) }
}

/// `Irp->UserBuffer`.
///
/// # Safety
/// `irp` must point to a valid IRP (see [`io_get_current_irp_stack_location`]).
/// The returned pointer is whatever the sender stored; for `METHOD_NEITHER` HID
/// IOCTLs from hidclass it is a kernel `HID_XFER_PACKET*`, but its validity and
/// size are the caller's responsibility.
#[must_use]
pub unsafe fn irp_user_buffer(irp: PIRP) -> PVOID {
    // SAFETY: `irp` is valid per the caller contract; plain field read.
    unsafe { (*irp).UserBuffer }
}

/// `Irp->UserBuffer = buffer`.
///
/// # Safety
/// `irp` must point to a valid IRP owned by this driver (typically one created
/// with `WdfRequestCreate` and not yet sent), and `buffer` must stay valid until
/// the IRP is completed.
pub unsafe fn set_irp_user_buffer(irp: PIRP, buffer: PVOID) {
    // SAFETY: `irp` is valid and owned per the caller contract; plain field write.
    unsafe { (*irp).UserBuffer = buffer };
}

/// `IoGetCurrentIrpStackLocation(Irp)->Parameters.DeviceIoControl.Type3InputBuffer`.
///
/// # Safety
/// As [`io_get_current_irp_stack_location`]; additionally the current stack
/// location must belong to an `IRP_MJ_DEVICE_CONTROL` or
/// `IRP_MJ_INTERNAL_DEVICE_CONTROL` request (so `DeviceIoControl` is the active
/// union member).
#[must_use]
pub unsafe fn irp_type3_input_buffer(irp: PIRP) -> PVOID {
    // SAFETY: caller contract: valid owned IRP whose current stack location is
    // an (internal) device-control request, so `Parameters.DeviceIoControl` is
    // the active member; plain pointer read.
    unsafe {
        let stack = io_get_current_irp_stack_location(irp);
        (*stack).Parameters.DeviceIoControl.Type3InputBuffer
    }
}

/// `WdfRequestWdmGetIrp(Request)->UserBuffer`, the C idiom for the
/// `HID_XFER_PACKET` of a `METHOD_NEITHER` HID IOCTL.
///
/// Returns null if the request has no IRP (the C code would dereference NULL
/// there; treating it as a null buffer is the P2 fix).
///
/// # Safety
/// As [`request_wdm_get_irp`] and [`irp_user_buffer`].
#[must_use]
pub unsafe fn request_user_buffer(request: WDFREQUEST) -> PVOID {
    // SAFETY: forwarded caller contract.
    let irp = unsafe { request_wdm_get_irp(request) };
    if irp.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: non-null IRP of a live request (caller contract).
    unsafe { irp_user_buffer(irp) }
}

/// `IoGetCurrentIrpStackLocation(WdfRequestWdmGetIrp(Request))
/// ->Parameters.DeviceIoControl.Type3InputBuffer`.
///
/// Returns null if the request has no IRP.
///
/// # Safety
/// As [`request_wdm_get_irp`] and [`irp_type3_input_buffer`].
#[must_use]
pub unsafe fn request_type3_input_buffer(request: WDFREQUEST) -> PVOID {
    // SAFETY: forwarded caller contract.
    let irp = unsafe { request_wdm_get_irp(request) };
    if irp.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: non-null IRP of a live (internal) device-control request.
    unsafe { irp_type3_input_buffer(irp) }
}
