//! KMDF HID minidriver helpers for request buffers and queue setup.

use core::ptr::null_mut;

use wdk_sys::{
    _WDF_IO_QUEUE_DISPATCH_TYPE, _WDF_TRI_STATE, HID_XFER_PACKET, NTSTATUS,
    PFN_WDF_IO_QUEUE_IO_INTERNAL_DEVICE_CONTROL, PFN_WDF_IO_QUEUE_IO_STOP, WDF_IO_QUEUE_CONFIG,
    WDF_OBJECT_ATTRIBUTES, WDFDEVICE, WDFMEMORY, WDFQUEUE, WDFREQUEST,
    call_unsafe_wdf_function_binding,
};

use crate::init::{io_queue_config_init, io_queue_config_init_default_queue};
use crate::ntstatus::NT_SUCCESS;
use crate::wdm::request_user_buffer;

/// Retrieves the WDF output memory for a descriptor or report.
///
/// # Safety
/// The caller must own the live request and use the memory only while the request remains pending,
/// at an IRQL allowed by `WdfRequestRetrieveOutputMemory`.
///
/// # Errors
/// Returns the failure status from WDF.
pub unsafe fn retrieve_output_memory(request: WDFREQUEST) -> Result<WDFMEMORY, NTSTATUS> {
    let mut memory: WDFMEMORY = null_mut();
    // SAFETY: the caller owns the request and memory is a writable out parameter.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestRetrieveOutputMemory, request, &raw mut memory)
    };
    if NT_SUCCESS(status) {
        Ok(memory)
    } else {
        Err(status)
    }
}

/// Copies a descriptor or report to offset zero, with WDF's buffer-size check.
///
/// # Safety
/// The caller must hold a valid, writable memory object at an IRQL allowed by
/// `WdfMemoryCopyFromBuffer`. Its destination must not overlap `source`.
#[must_use]
pub unsafe fn copy_to_output_memory(memory: WDFMEMORY, source: &[u8]) -> NTSTATUS {
    // SAFETY: the caller guarantees the destination. WDF reads the valid source slice.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfMemoryCopyFromBuffer,
            memory,
            0,
            source.as_ptr().cast_mut().cast(),
            source.len()
        )
    }
}

/// Retrieves a request's output buffer and its length, typically for `HID_DEVICE_ATTRIBUTES`.
///
/// # Safety
/// The caller must own the live request and access the buffer only while the request is pending,
/// at an IRQL allowed by `WdfRequestRetrieveOutputBuffer`.
///
/// # Errors
/// Returns the failure status from WDF.
pub unsafe fn retrieve_output_buffer(
    request: WDFREQUEST,
    minimum_length: usize,
) -> Result<(*mut u8, usize), NTSTATUS> {
    let mut buffer = null_mut();
    let mut length = 0;
    // SAFETY: the caller owns the request and both out parameters are writable.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestRetrieveOutputBuffer,
            request,
            minimum_length,
            &raw mut buffer,
            &raw mut length
        )
    };
    if NT_SUCCESS(status) {
        Ok((buffer.cast(), length))
    } else {
        Err(status)
    }
}

/// Copies the transfer packet from `Irp->UserBuffer`, tolerating a null or unaligned pointer.
/// The caller must validate the buffer length before calling.
///
/// # Safety
/// `request` is a live HIDCLASS feature request and a non-null `UserBuffer` points to at least
/// `size_of::<HID_XFER_PACKET>()` readable bytes. The embedded report pointer is returned by value.
#[must_use]
pub unsafe fn read_xfer_packet(request: WDFREQUEST) -> Option<HID_XFER_PACKET> {
    // SAFETY: the caller holds a live KMDF request with the specified buffer contract.
    let packet = unsafe { request_user_buffer(request) }.cast::<HID_XFER_PACKET>();
    if packet.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a readable packet and the read requires no alignment.
    Some(unsafe { packet.read_unaligned() })
}

/// Configures the parallel default queue for a HID minidriver.
#[must_use]
pub fn default_queue_config(
    internal_device_control: PFN_WDF_IO_QUEUE_IO_INTERNAL_DEVICE_CONTROL,
    stop: PFN_WDF_IO_QUEUE_IO_STOP,
) -> WDF_IO_QUEUE_CONFIG {
    let mut config =
        io_queue_config_init_default_queue(_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel);
    config.EvtIoInternalDeviceControl = internal_device_control;
    config.EvtIoStop = stop;
    config
}

/// Configures the non-power-managed manual queue that holds pending HID reads.
#[must_use]
pub fn read_queue_config() -> WDF_IO_QUEUE_CONFIG {
    let mut config = io_queue_config_init(_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchManual);
    config.PowerManaged = _WDF_TRI_STATE::WdfFalse;
    config
}

/// Creates a HID minidriver queue with the given configuration and optional object attributes.
///
/// # Safety
/// `device` is the live device being initialized, `attributes` is null or points to valid WDF object
/// attributes, and `queue` points to writable storage. Call at `PASSIVE_LEVEL`.
#[must_use]
pub unsafe fn create_queue(
    device: WDFDEVICE,
    config: &mut WDF_IO_QUEUE_CONFIG,
    attributes: *mut WDF_OBJECT_ATTRIBUTES,
    queue: *mut WDFQUEUE,
) -> NTSTATUS {
    // SAFETY: the caller guarantees the device, IRQL and pointer validity.
    unsafe {
        call_unsafe_wdf_function_binding!(WdfIoQueueCreate, device, config, attributes, queue)
    }
}
