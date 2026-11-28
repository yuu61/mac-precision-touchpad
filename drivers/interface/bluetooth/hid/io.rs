//! Framework I/O for the Bluetooth HID collection.

use core::ffi::c_void;
use core::mem::size_of;
use core::ptr::null_mut;

use amtptp_core::hid::ioctl::IOCTL_HID_SET_FEATURE;
use amtptp_usb_bt::bluetooth::{NtStatus, STATUS_INVALID_DEVICE_STATE};
use tracelogging as tlg;
use wdf_support::init::sync_send_options;
use wdf_support::ntstatus::NT_SUCCESS;
use wdk_sys::{
    _WDF_DEVICE_FAILED_ACTION, HID_XFER_PACKET, ULONG, ULONG_PTR, WDFIOTARGET, WDFMEMORY,
    WDFREQUEST, call_unsafe_wdf_function_binding,
};

use crate::context::{DeviceContext, LowerRead, QueuedCompletion};
use crate::{KW_DEVICE, KW_INPUT, PROVIDER};

mod dispatch;
mod lower_read;
mod pipeline;
mod pnp;
mod upper_request;
pub(crate) use dispatch::dispatch_internal_ioctl;
pub(crate) use pipeline::{LowerReadOutcome, complete_lower_read, work_item_callback};
pub(crate) use pnp::{
    d0_entry, d0_exit, prepare_hardware, recovery_timer_callback, self_managed_io_init,
    self_managed_io_restart, source_closed, source_opened,
};

type UpperRequest = WDFREQUEST;
type ReadMemory = WDFMEMORY;
/// `WDF_DEVICE_FAILED_ACTION` for `WdfDeviceSetFailed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailedAction {
    /// `WdfDeviceFailedAttemptRestart`.
    AttemptRestart,
    /// `WdfDeviceFailedNoRestart`.
    NoRestart,
}

const _: () =
    assert!(size_of::<HID_XFER_PACKET>() == amtptp_usb_bt::bluetooth::HID_XFER_PACKET_LEN);
const _: () = assert!(size_of::<ULONG_PTR>() == size_of::<usize>());

/// The device's framework operations, for one callback invocation. Holds only a shared reference
/// to the device context (its state is atomic or behind the read lock) and a tracing flag, so
/// independent instances can coexist, e.g. when a lower read completes synchronously inside
/// `WdfRequestSend` on the dispatch path.
pub(crate) struct Filter<'a> {
    pub(crate) ctx: &'a DeviceContext,
    /// Trace a failed `WdfIoQueueRetrieveNextRequest` as a dropped input packet (Input.c:176-180).
    /// Only the lower-read completion sets it: `D0Exit` drains `HidReadQueue` until that call fails,
    /// which is not an error there.
    trace_input_drop: bool,
}

impl<'a> Filter<'a> {
    pub(crate) const fn new(ctx: &'a DeviceContext) -> Self {
        Self {
            ctx,
            trace_input_drop: false,
        }
    }

    /// The instance of the lower-read completion routine (see `trace_input_drop`).
    pub(crate) const fn for_read_completion(ctx: &'a DeviceContext) -> Self {
        Self {
            ctx,
            trace_input_drop: true,
        }
    }

    /// The data source's target, or `None` while it is not open. The core only issues requests while
    /// the source is open, so `None` only appears in a race with its removal; it keeps a NULL handle
    /// away from WDF (which would bugcheck).
    fn io_target(&self) -> Option<WDFIOTARGET> {
        let target = self.ctx.hid_io_target();
        if target.is_null() {
            tlg::write_event!(PROVIDER, "NoIoTarget", level(Error), keyword(KW_DEVICE));
            None
        } else {
            Some(target)
        }
    }
}

/// `WdfObjectDelete(object)`.
///
/// # Safety
/// `object` is a live WDF object this driver may delete (a driver-created request or memory).
unsafe fn object_delete<T>(object: *mut T) {
    // SAFETY: caller contract; WdfObjectDelete is callable at <= DISPATCH_LEVEL.
    unsafe { call_unsafe_wdf_function_binding!(WdfObjectDelete, object.cast::<c_void>()) };
}

#[expect(
    clippy::unused_self,
    reason = "Callbacks use the same operation receiver for live requests and for the recording backend."
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "The recording backend updates its clock and records mode switches through this receiver."
)]
#[expect(
    clippy::multiple_inherent_impl,
    reason = "Device-wide operations have their own module alongside upper requests and lower reads."
)]
impl Filter<'_> {
    fn set_failed(&mut self, action: FailedAction) {
        let failed_action = match action {
            FailedAction::AttemptRestart => {
                _WDF_DEVICE_FAILED_ACTION::WdfDeviceFailedAttemptRestart
            }
            FailedAction::NoRestart => _WDF_DEVICE_FAILED_ACTION::WdfDeviceFailedNoRestart,
        };
        tlg::write_event!(
            PROVIDER,
            "DeviceSetFailed",
            level(Error),
            keyword(KW_INPUT),
            i32("Action", &failed_action)
        );
        // SAFETY: live device; WdfDeviceSetFailed is callable at <= DISPATCH_LEVEL.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfDeviceSetFailed, self.ctx.device(), failed_action);
        };
    }

    fn start_recovery_timer(&mut self, due_time: i64) {
        tlg::write_event!(
            PROVIDER,
            "RecoveryTimerStarted",
            level(Informational),
            keyword(KW_DEVICE),
            i64("DueTime", &due_time)
        );
        // SAFETY: the timer was created in AddDevice (parent = device); WdfTimerStart is callable at
        // <= DISPATCH_LEVEL. Its BOOLEAN (already queued) is ignored, as in C.
        let _was_queued = unsafe {
            call_unsafe_wdf_function_binding!(WdfTimerStart, self.ctx.recovery_timer(), due_time)
        };
    }

    fn stop_recovery_timer(&mut self) {
        // SAFETY: the timer was created in AddDevice (parent = device); Wait = FALSE is allowed at
        // <= DISPATCH_LEVEL. A callback that is already running is not waited for (its mode switch
        // can block up to SYNC_IO_TIMEOUT); it finds the device out of D0 and stops.
        let was_queued = unsafe {
            call_unsafe_wdf_function_binding!(WdfTimerStop, self.ctx.recovery_timer(), 0)
        };
        tlg::write_event!(
            PROVIDER,
            "RecoveryTimerStopped",
            level(Informational),
            keyword(KW_DEVICE),
            bool8("WasQueued", &(was_queued != 0))
        );
    }

    fn enqueue_recovery_work_item(&mut self) {
        // SAFETY: the work item was created in AddDevice (parent = device); WdfWorkItemEnqueue is
        // callable at <= DISPATCH_LEVEL (a no-op if already queued).
        unsafe {
            call_unsafe_wdf_function_binding!(WdfWorkItemEnqueue, self.ctx.recovery_work_item());
        };
    }

    fn query_performance_counter(&mut self) -> i64 {
        wdf_support::time::query_performance_counter()
    }

    fn query_performance_frequency(&mut self) -> i64 {
        wdf_support::time::query_performance_frequency()
    }

    // ---- mode switch (Device.c:371-418) --------------------------------------------------------

    fn send_mode_report(&mut self, report: amtptp_usb_bt::bluetooth::ModeSwitch) -> NtStatus {
        let Some(target) = self.io_target() else {
            return STATUS_INVALID_DEVICE_STATE;
        };
        let mut input = wdf_support::init::memory_descriptor_init_buffer(
            report.payload.as_ptr().cast_mut().cast(),
            report.payload.len() as ULONG,
        );
        let mut options = sync_send_options(0);
        let started = wdf_support::time::query_performance_counter();
        // SAFETY: the class IOCTL reads the static report. The target is live,
        // and this synchronous call runs in a passive callback.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoTargetSendIoctlSynchronously,
                target,
                null_mut(),
                IOCTL_HID_SET_FEATURE,
                &raw mut input,
                null_mut(),
                &raw mut options,
                null_mut()
            )
        };
        let elapsed_ms = elapsed_ms_since(started);
        if !NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "MultitouchModeSendFailed",
                level(Error),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status),
                u64("ElapsedMs", &elapsed_ms)
            );
            return status;
        }
        tlg::write_event!(
            PROVIDER,
            "MultitouchModeSet",
            level(Informational),
            keyword(KW_DEVICE),
            win_ntstatus("Status", &status),
            u64("ElapsedMs", &elapsed_ms)
        );
        status
    }
}

/// Milliseconds since the performance-counter value `since` (0 if the counter is unusable).
fn elapsed_ms_since(since: i64) -> u64 {
    let frequency = wdf_support::time::query_performance_frequency();
    let now = wdf_support::time::query_performance_counter();
    match (u64::try_from(now - since), u64::try_from(frequency)) {
        (Ok(ticks), Ok(frequency)) if frequency > 0 => ticks.saturating_mul(1000) / frequency,
        _ => 0,
    }
}
