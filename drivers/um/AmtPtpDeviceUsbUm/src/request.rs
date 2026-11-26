//! The core's [`HidRequest`] over a WDFREQUEST: one WDF DDI per trait method, as in Hid.c /
//! InputInterrupt.c.
//!
//! | trait method | WDF calls (C) |
//! |---|---|
//! | `input_memory` | `WdfRequestRetrieveInputMemory` + `WdfMemoryGetBuffer` |
//! | `output_memory_len` | `WdfRequestRetrieveOutputMemory` + `WdfMemoryGetBuffer(.., &length)` (no access to the bytes) |
//! | `output_memory` | `WdfRequestRetrieveOutputMemory` + `WdfMemoryGetBuffer` |
//! | `copy_to_output_memory` | `WdfRequestRetrieveOutputMemory` + `WdfMemoryCopyFromBuffer(memory, 0, src, len)` |
//! | `output_buffer` | `WdfRequestRetrieveOutputBuffer(request, minimum, &buffer, &length)` |
//!
//! The memory objects are retrieved once per request and cached, so each C code path makes the same
//! `WdfRequestRetrieve*Memory` calls as in C. Every framework status (e.g. `STATUS_BUFFER_TOO_SMALL` for
//! a zero-length buffer, `STATUS_INVALID_BUFFER_SIZE` from `WdfMemoryCopyFromBuffer` into a short buffer)
//! comes from WDF itself: the glue never checks lengths in place of the framework.
//!
//! With `UmdfMethodNeitherAction=Copy` (INF) the reflector copies the METHOD_NEITHER HID IOCTL buffers,
//! which is what makes `WdfRequestRetrieve*Memory` work for them; input and output are separate buffers.

use core::borrow::Borrow;
use core::ptr::null_mut;

use amtptp_usbum_core::io::HidRequest;
use amtptp_usbum_core::status::NtStatus;
use wdf_support::ntstatus::NT_SUCCESS;
use wdf_support::request::Request;
use wdk_sys::{PVOID, WDFMEMORY, call_unsafe_wdf_function_binding};

/// `WdfMemoryGetBuffer(memory, &length)`: the buffer pointer and its length.
///
/// # Safety
/// `memory` must be a valid, live WDFMEMORY.
pub(crate) unsafe fn memory_buffer(memory: WDFMEMORY) -> (*mut u8, usize) {
    let mut length: usize = 0;
    // SAFETY: valid WDFMEMORY (caller contract); `length` is an out pointer.
    let buffer =
        unsafe { call_unsafe_wdf_function_binding!(WdfMemoryGetBuffer, memory, &raw mut length) };
    (buffer.cast(), length)
}

/// A shared byte slice over a framework buffer; NULL or a zero length gives `&[]`
/// (`slice::from_raw_parts` must never see a NULL pointer, even for length 0).
///
/// # Safety
/// If `bytes` is non-null and `len` > 0, `bytes..bytes + len` must be readable, initialized memory
/// that nobody writes for the lifetime `'a`.
pub(crate) unsafe fn slice_or_empty<'a>(bytes: *const u8, len: usize) -> &'a [u8] {
    if bytes.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: non-null, `len` readable bytes valid for 'a (caller contract); u8 has alignment 1.
        unsafe { core::slice::from_raw_parts(bytes, len) }
    }
}

/// A mutable byte slice over a framework buffer; NULL or a zero length gives `&mut []`.
///
/// # Safety
/// If `bytes` is non-null and `len` > 0, `bytes..bytes + len` must be writable memory that nothing
/// else accesses for the lifetime `'a`.
pub(crate) unsafe fn slice_mut_or_empty<'a>(bytes: *mut u8, len: usize) -> &'a mut [u8] {
    if bytes.is_null() || len == 0 {
        &mut []
    } else {
        // SAFETY: non-null, `len` writable bytes exclusively ours for 'a (caller contract).
        unsafe { core::slice::from_raw_parts_mut(bytes, len) }
    }
}

/// [`HidRequest`] over a framework-delivered request: borrowed (`R = &Request`, the default queue's
/// dispatcher completes it) or owned (`R = Request`, a READ_REPORT retrieved from the InputQueue).
pub(crate) struct RequestIo<R> {
    request: R,
    /// Cached result of `WdfRequestRetrieveInputMemory` (NULL until retrieved successfully).
    input_memory: WDFMEMORY,
    /// Cached result of `WdfRequestRetrieveOutputMemory` (NULL until retrieved successfully).
    output_memory: WDFMEMORY,
}

impl<R: Borrow<Request>> RequestIo<R> {
    /// Wraps `request`; nothing is retrieved yet.
    pub(crate) fn new(request: R) -> Self {
        Self {
            request,
            input_memory: null_mut(),
            output_memory: null_mut(),
        }
    }

    /// The wrapped request.
    pub(crate) fn into_inner(self) -> R {
        self.request
    }

    /// `WdfRequestRetrieveInputMemory` (once per request).
    fn retrieve_input_memory(&mut self) -> Result<WDFMEMORY, NtStatus> {
        if !self.input_memory.is_null() {
            return Ok(self.input_memory);
        }
        let mut memory: WDFMEMORY = null_mut();
        // SAFETY: the request is valid and owned by the driver until it is completed, which needs the
        // `Request` this adapter holds or borrows; `memory` is an out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestRetrieveInputMemory,
                self.request.borrow().as_raw(),
                &raw mut memory
            )
        };
        if !NT_SUCCESS(status) {
            return Err(status);
        }
        self.input_memory = memory;
        Ok(memory)
    }

    /// `WdfRequestRetrieveOutputMemory` (once per request).
    fn retrieve_output_memory(&mut self) -> Result<WDFMEMORY, NtStatus> {
        if !self.output_memory.is_null() {
            return Ok(self.output_memory);
        }
        let mut memory: WDFMEMORY = null_mut();
        // SAFETY: as in retrieve_input_memory.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestRetrieveOutputMemory,
                self.request.borrow().as_raw(),
                &raw mut memory
            )
        };
        if !NT_SUCCESS(status) {
            return Err(status);
        }
        self.output_memory = memory;
        Ok(memory)
    }
}

impl<R: Borrow<Request>> HidRequest for RequestIo<R> {
    fn input_memory(&mut self) -> Result<&[u8], NtStatus> {
        let memory = self.retrieve_input_memory()?;
        // SAFETY: the request's input memory, valid until the request is completed.
        let (bytes, len) = unsafe { memory_buffer(memory) };
        // SAFETY: the input buffer belongs to the request, which cannot be completed while the
        // returned borrow of `self` is alive; nothing writes it meanwhile.
        Ok(unsafe { slice_or_empty(bytes.cast_const(), len) })
    }

    fn output_memory_len(&mut self) -> Result<usize, NtStatus> {
        let memory = self.retrieve_output_memory()?;
        // Only the length: the SET_FEATURE output buffer is not readable (mshidumdf convention).
        // SAFETY: the request's output memory, valid until the request is completed.
        let (_, len) = unsafe { memory_buffer(memory) };
        Ok(len)
    }

    fn output_memory(&mut self) -> Result<&mut [u8], NtStatus> {
        let memory = self.retrieve_output_memory()?;
        // SAFETY: the request's output memory, valid until the request is completed.
        let (bytes, len) = unsafe { memory_buffer(memory) };
        // SAFETY: the output buffer belongs to the request, which cannot be completed while the
        // returned borrow of `self` is alive; the driver is its only writer meanwhile, and the input
        // buffer (a separate copy in UMDF) is not borrowed at the same time (both borrow `self`).
        Ok(unsafe { slice_mut_or_empty(bytes, len) })
    }

    fn copy_to_output_memory(&mut self, src: &[u8]) -> Result<(), NtStatus> {
        let memory = self.retrieve_output_memory()?;
        let source: PVOID = src.as_ptr().cast_mut().cast();
        // SAFETY: valid output memory; `source` points at `src.len()` readable bytes (WDF only reads
        // the source buffer); the framework checks the destination range and fails a short buffer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(WdfMemoryCopyFromBuffer, memory, 0, source, src.len())
        };
        if NT_SUCCESS(status) {
            Ok(())
        } else {
            Err(status)
        }
    }

    fn output_buffer(&mut self, minimum_len: usize) -> Result<&mut [u8], NtStatus> {
        let mut buffer: PVOID = null_mut();
        let mut length: usize = 0;
        // SAFETY: valid request (see retrieve_input_memory); out pointers to locals.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestRetrieveOutputBuffer,
                self.request.borrow().as_raw(),
                minimum_len,
                &raw mut buffer,
                &raw mut length
            )
        };
        if !NT_SUCCESS(status) {
            return Err(status);
        }
        // SAFETY: `length` writable bytes of the request's output buffer, exclusively the driver's
        // until the request is completed, which cannot happen while the borrow of `self` is alive.
        Ok(unsafe { slice_mut_or_empty(buffer.cast(), length) })
    }
}
