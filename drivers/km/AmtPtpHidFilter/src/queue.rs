//! Default-queue callbacks (`Queue.c:56-129`).
//!
//! The default queue is parallel and not power-managed (filter device); its callbacks run at
//! IRQL <= `DISPATCH_LEVEL` and are non-paged. Only `IRP_MJ_INTERNAL_DEVICE_CONTROL` is handled here;
//! KMDF forwards every other major function of a filter device to the lower driver.

use amtptp_hidfilter_core::{InternalIoctl, dispatch_internal_ioctl};
use ptp_hid::ioctl::ioctl_name;
use tracelogging as tlg;
use wdk_sys::{ULONG, WDFQUEUE, WDFREQUEST};

use crate::context::{device_context, queue_get_context};
use crate::io::Filter;
use crate::{KW_QUEUE, PROVIDER};

/// `FilterEvtIoIntDeviceControl` (Queue.c:56-117): `EvtIoInternalDeviceControl` of the default
/// queue. The core completes, parks or forwards the request exactly once:
///
/// - `GET_DEVICE_DESCRIPTOR` / `GET_REPORT_DESCRIPTOR` / `GET_DEVICE_ATTRIBUTES`: synchronous (descriptor
///   only for PID 0x0265, attributes with VID 0x8910);
/// - `GET_STRING`: `WdfRequestFormatRequestUsingCurrentType` + send-and-forget to the PDO;
/// - `READ_REPORT`: forwarded to `HidReadQueue`, plus one lower read when configured;
/// - `GET_FEATURE` / `SET_FEATURE`: handled in software, `Information` untouched (HF-B21);
/// - every other code, including the listed `WRITE_REPORT`, ACTIVATE/DEACTIVATE, idle notification
///   and UMDF codes: completed with `STATUS_NOT_SUPPORTED`, never passed down.
pub(crate) unsafe extern "C" fn evt_io_internal_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    output_buffer_length: usize,
    input_buffer_length: usize,
    io_control_code: ULONG,
) {
    // SAFETY: `queue` is our default queue; its QUEUE_CONTEXT was written right after creation.
    let device = unsafe { (*queue_get_context(queue)).device };
    // SAFETY: the queue's device is alive while the queue dispatches requests.
    let ctx = unsafe { device_context(device) };
    if InternalIoctl::from_code(io_control_code) == InternalIoctl::NotSupported {
        tlg::write_event!(
            PROVIDER,
            "IoctlNotSupported",
            level(Warning),
            keyword(KW_QUEUE),
            u32_hex("IoControlCode", &io_control_code),
            str8("Name", ioctl_name(io_control_code))
        );
    }
    dispatch_internal_ioctl(
        &mut Filter::new(ctx),
        &ctx.state,
        request,
        output_buffer_length,
        input_buffer_length,
        io_control_code,
    );
}

/// `FilterEvtIoStop` (Queue.c:119-129): an empty body, registered as in C. It neither acknowledges
/// nor completes; the queue is not power-managed, so KMDF only calls it during removal, for requests
/// the driver still owns from this queue, and there are none (every request is completed
/// synchronously, forwarded with send-and-forget or moved to `HidReadQueue`).
pub(crate) const unsafe extern "C" fn evt_io_stop(
    _queue: WDFQUEUE,
    _request: WDFREQUEST,
    _action_flags: ULONG,
) {
}
