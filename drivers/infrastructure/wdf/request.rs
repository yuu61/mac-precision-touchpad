//! An owned `WDFREQUEST` that is completed, forwarded or explicitly released
//! exactly once.
//!
//! port policy rule: every request a driver obtains (presented to an `EvtIo*`
//! callback, or retrieved from a manual queue with
//! `WdfIoQueueRetrieveNextRequest`) must be completed or forwarded exactly once
//! on every path. [`Request`] encodes that in the type system:
//!
//! - all terminal operations take `self` by value, so a second completion does
//!   not compile;
//! - operations that can fail without consuming the request
//!   ([`Request::forward_to_io_queue`], [`Request::send_and_forget`]) hand it
//!   back in [`Rejected`], so the error path still owns it and must complete it
//!   (the C code completes with the returned status);
//! - [`Request::into_raw`] is the explicit escape hatch for hand-offs the guard
//!   does not model (e.g. `WdfRequestSend` with a completion routine, a
//!   synchronous send, `WdfRequestStopAcknowledge`); re-wrap with
//!   [`Request::from_raw`] when ownership comes back;
//! - dropping a `Request` without a terminal operation is a bug. In that case
//!   `Drop` completes it with the fallback status the caller chose at
//!   construction ([`Request::from_raw`]), so the request can never leak or
//!   hang the queue. The guard never invents a status of its own, and it does
//!   not assert/break (a panic or breakpoint in kernel mode would bugcheck
//!   without a debugger).
//!
//! Only for requests delivered by the framework. Driver-created requests
//! (`WdfRequestCreate`) are never completed; they are deleted with
//! `WdfObjectDelete` and must not be wrapped in [`Request`].
//!
//! All methods are callable at IRQL <= `DISPATCH_LEVEL` (KMDF) and in UMDF.

use core::mem::ManuallyDrop;

use wdk_sys::{
    _WDF_REQUEST_SEND_OPTIONS_FLAGS, NT_SUCCESS, NTSTATUS, ULONG, ULONG_PTR,
    WDF_REQUEST_PARAMETERS, WDFIOTARGET, WDFQUEUE, WDFREQUEST, WDFREQUEST__,
    call_unsafe_wdf_function_binding,
};

use crate::handle::WdfHandle;
use crate::init::{request_parameters_init, request_send_options_init};

/// A framework-delivered `WDFREQUEST` owned by the driver (see the module docs).
#[must_use = "a WDFREQUEST must be completed, forwarded or released exactly once"]
#[derive(Debug)]
pub struct Request {
    handle: WdfHandle<WDFREQUEST__>,
    drop_status: NTSTATUS,
}

/// A request that could not be forwarded or sent; ownership stays with the
/// driver, which must complete it (normally with [`Rejected::status`]).
#[must_use = "the rejected request is still owned and must be completed"]
#[derive(Debug)]
pub struct Rejected {
    /// The still-owned request.
    pub request: Request,
    /// The failure status (`WdfRequestForwardToIoQueue` result, or
    /// `WdfRequestGetStatus` after a failed `WdfRequestSend`).
    pub status: NTSTATUS,
}

impl Rejected {
    /// Completes the rejected request with the failure status, which is what all
    /// four C drivers do on these paths.
    pub fn complete(self) {
        self.request.complete(self.status);
    }
}

impl Request {
    /// Takes ownership of a framework-delivered request.
    ///
    /// `drop_status` is used only if the `Request` is dropped without a terminal
    /// operation (a driver bug); choose the status of the surrounding error
    /// path, e.g. `STATUS_UNSUCCESSFUL` or `STATUS_INVALID_DEVICE_REQUEST`.
    ///
    /// # Safety
    /// - `handle` is a valid `WDFREQUEST` that the driver currently owns: it was
    ///   presented to an `EvtIo*` callback, retrieved from a queue, or returned
    ///   by [`Request::into_raw`], and it has not been completed, forwarded or
    ///   sent since.
    /// - It was not created by the driver with `WdfRequestCreate`.
    /// - No other code completes, forwards or sends it while this `Request`
    ///   exists (i.e. at most one `Request` per handle).
    pub const unsafe fn from_raw(handle: WDFREQUEST, drop_status: NTSTATUS) -> Self {
        Self {
            handle: WdfHandle::new(handle),
            drop_status,
        }
    }

    /// The raw handle, for WDF calls that do not transfer ownership
    /// (`WdfRequestRetrieveOutputBuffer`, `WdfRequestGetParameters`,
    /// `WdfRequestWdmGetIrp`, ...). Never complete, forward or send it directly.
    #[must_use]
    pub const fn as_raw(&self) -> WDFREQUEST {
        self.handle.get()
    }

    /// Gives up ownership without completing the request, returning the raw
    /// handle. The caller becomes responsible for its completion (e.g. a
    /// completion routine after `WdfRequestSend`, or `WdfRequestStopAcknowledge`).
    #[must_use = "the returned handle must still be completed by someone"]
    pub fn into_raw(self) -> WDFREQUEST {
        ManuallyDrop::new(self).handle.get()
    }

    /// `WDF_REQUEST_PARAMETERS_INIT` + `WdfRequestGetParameters`.
    #[must_use]
    pub fn parameters(&self) -> WDF_REQUEST_PARAMETERS {
        let mut parameters = request_parameters_init();
        // SAFETY: `self.handle` is a valid owned request (from_raw contract);
        // `parameters` is an initialized WDF_REQUEST_PARAMETERS with Size set.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestGetParameters,
                self.handle.get(),
                &raw mut parameters
            );
        }
        parameters
    }

    /// `WdfRequestSetInformation(Request, Information)` (e.g. the number of
    /// bytes returned); usually followed by [`Request::complete`].
    pub fn set_information(&self, information: ULONG_PTR) {
        // SAFETY: valid owned request (from_raw contract).
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestSetInformation,
                self.handle.get(),
                information
            );
        }
    }

    /// `WdfRequestGetInformation(Request)`.
    #[must_use]
    pub fn information(&self) -> ULONG_PTR {
        // SAFETY: valid owned request (from_raw contract).
        unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetInformation, self.handle.get()) }
    }

    /// `WdfRequestComplete(Request, Status)`; the request's information value
    /// is whatever was set before (see [`Request::set_information`]).
    pub fn complete(self, status: NTSTATUS) {
        let handle = self.into_raw();
        // SAFETY: we owned the request (from_raw contract) and give up the
        // ownership here; it is completed exactly once.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestComplete, handle, status);
        }
    }

    /// `WdfRequestCompleteWithInformation(Request, Status, Information)`.
    pub fn complete_with_information(self, status: NTSTATUS, information: ULONG_PTR) {
        let handle = self.into_raw();
        // SAFETY: as in `complete`.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestCompleteWithInformation,
                handle,
                status,
                information
            );
        }
    }

    /// `WdfRequestForwardToIoQueue(Request, Queue)`.
    ///
    /// On success the request belongs to `queue` (it is delivered again or
    /// retrieved later). On failure the driver still owns it.
    ///
    /// # Errors
    /// [`Rejected`] with the status returned by `WdfRequestForwardToIoQueue`.
    ///
    /// # Safety
    /// `queue` must be a valid, live `WDFQUEUE` of the device the request
    /// belongs to (an invalid handle is a bugcheck / host crash in WDF).
    pub unsafe fn forward_to_io_queue(self, queue: WDFQUEUE) -> Result<(), Rejected> {
        // SAFETY: valid owned request (from_raw contract); `queue` is a valid
        // queue handle (caller contract).
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestForwardToIoQueue,
                self.handle.get(),
                queue
            )
        };
        if NT_SUCCESS(status) {
            let _transferred = self.into_raw();
            Ok(())
        } else {
            Err(Rejected {
                request: self,
                status,
            })
        }
    }

    /// Passes the request down unchanged:
    /// `WdfRequestFormatRequestUsingCurrentType(Request)`, then `WdfRequestSend`
    /// with `WDF_REQUEST_SEND_OPTION_SEND_AND_FORGET` (the filter-forwarding
    /// pattern of `AmtPtpHidFilter` and `AmtPtpDeviceSpiKm`).
    ///
    /// On success the target completes the request; the driver must not touch
    /// it again. On failure the driver still owns it.
    ///
    /// # Errors
    /// [`Rejected`] with `WdfRequestGetStatus(Request)` when `WdfRequestSend`
    /// returns `FALSE`.
    ///
    /// # Safety
    /// `target` must be a valid, live `WDFIOTARGET` (e.g. `WdfDeviceGetIoTarget`
    /// of the request's device).
    pub unsafe fn send_and_forget(self, target: WDFIOTARGET) -> Result<(), Rejected> {
        let raw = self.handle.get();
        // SAFETY: valid owned request (from_raw contract).
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestFormatRequestUsingCurrentType, raw);
        };
        let mut options = request_send_options_init(
            // Small non-negative enum constant (0x8), as in the C macro use.
            _WDF_REQUEST_SEND_OPTIONS_FLAGS::WDF_REQUEST_SEND_OPTION_SEND_AND_FORGET as ULONG,
        );
        // SAFETY: valid owned request, formatted above; `target` is a valid
        // I/O target (caller contract); `options` is initialized.
        let sent =
            unsafe { call_unsafe_wdf_function_binding!(WdfRequestSend, raw, target, &raw mut options) };
        if sent != 0 {
            let _transferred = self.into_raw();
            Ok(())
        } else {
            // SAFETY: the send failed, so the request is still ours and valid.
            let status = unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetStatus, raw) };
            Err(Rejected {
                request: self,
                status,
            })
        }
    }
}

impl Drop for Request {
    /// Last-resort completion of a request that reached no terminal operation
    /// (a driver bug): completes it with the `drop_status` given to
    /// [`Request::from_raw`] so it can never be leaked.
    fn drop(&mut self) {
        // SAFETY: Drop only runs if no consuming method ran (they all go
        // through `into_raw`, which suppresses Drop), so the request is still
        // owned and has not been completed.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestComplete,
                self.handle.get(),
                self.drop_status
            );
        }
    }
}
