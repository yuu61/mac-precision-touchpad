//! SPI power transitions, multitouch mode, and read recovery.

use core::ptr::null_mut;

use amtptp_core::spi::runtime::{
    D0_EXIT_DRAIN_STATUS, DeviceRuntime, ReadTracker, RecoveryAction, SuspendedReads,
};
use amtptp_core::spi::wire::{SET_STATE_IOCTL, SET_STATE_PACKET_LEN, with_set_state_packet};
use tracelogging as tlg;
use wdf_support::init::{memory_descriptor_init_buffer, sync_send_options};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE, STATUS_SUCCESS};
use wdf_support::queue::{cancel_referenced_reads, collect_reads_to_cancel, drain_manual_queue};
use wdf_support::time::query_performance_frequency;
use wdk_sys::{
    _WDF_DEVICE_FAILED_ACTION, NTSTATUS, ULONG, WDF_POWER_DEVICE_STATE, WDFDEVICE, WDFTIMER,
    call_unsafe_wdf_function_binding,
};

use super::{DeviceContext, LowerRead, device_context};
use crate::{PROVIDER, input, keyword};

/// `AmtPtpEvtDeviceD0Entry` (Device.c:319-351): `DeviceStatus = D0ActiveAndUnconfigured` and
/// initializes the reporting clock with the cached QPC frequency. No device I/O.
pub(super) unsafe extern "C" fn evt_device_d0_entry(
    device: WDFDEVICE,
    previous_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    // SAFETY: WDF supplies the live device to this D0 entry callback.
    let system_power_action =
        unsafe { call_unsafe_wdf_function_binding!(WdfDeviceGetSystemPowerAction, device) };
    tlg::write_event!(
        PROVIDER,
        "D0Entry",
        level(Informational),
        keyword(keyword::DRIVER),
        i32("PreviousState", &previous_state),
        i32("SystemPowerAction", &system_power_action)
    );
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    // SAFETY: live device context at PASSIVE_LEVEL.
    unsafe { context.with_contacts(|_, contacts| contacts.reset()) };
    context.runtime.on_d0_entry(query_performance_frequency())
}

/// `EvtDeviceSelfManagedIoSuspend` (not in C): runs before every `D0Exit` (power-down, resource
/// rebalance, removal and surprise removal).
///
/// 1. `DeviceStatus = D3` and a new epoch, under the read lock: no lower read is sent from now on, and
///    the completion of every read sent before is stale (it leaves `HidQueue` alone).
/// 2. `WdfTimerStop(Wait = FALSE)`: a pending recovery timer does not fire. A callback that is already
///    running finds the device suspended and does nothing. It is not waited for, because its mode switch
///    has no timeout.
/// 3. `WdfRequestCancelSentRequest` on every read that was in flight. Their completions are not waited
///    for either; they are dropped whenever they arrive.
///
/// Together with the epoch check in the completion routine this fixes SPI-B12/B14 (P3).
pub(super) unsafe extern "C" fn evt_device_self_managed_io_suspend(device: WDFDEVICE) -> NTSTATUS {
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    // SAFETY: the context belongs to a live device; PASSIVE_LEVEL.
    let cancelled = unsafe { cancel_in_flight_reads(context, DeviceRuntime::suspend) };
    // SAFETY: the timers were created in AddDevice and live as long as the device; Wait = FALSE is
    // allowed at any IRQL <= DISPATCH_LEVEL.
    let was_queued = unsafe {
        let _retry_stopped =
            call_unsafe_wdf_function_binding!(WdfTimerStop, context.read_retry_timer.get(), 0);
        call_unsafe_wdf_function_binding!(WdfTimerStop, context.power_on_recovery_timer.get(), 0)
    };
    tlg::write_event!(
        PROVIDER,
        "SelfManagedIoSuspend",
        level(Informational),
        keyword(keyword::DRIVER),
        u32("CancelledLowerReads", &cancelled),
        bool8("TimerWasQueued", &(was_queued != 0))
    );
    STATUS_SUCCESS
}

/// Suspends the read pipeline with `suspend` (under the read lock), then cancels every `Sent` read
/// it handed out outside the lock.
///
/// # Safety
/// `context` belongs to a live device set up by [`super::create_device`]; IRQL <= `DISPATCH_LEVEL`.
unsafe fn cancel_in_flight_reads<F>(context: &DeviceContext, suspend: F) -> u32
where
    F: FnOnce(&DeviceRuntime, &mut ReadTracker<LowerRead>) -> SuspendedReads<LowerRead>,
{
    // SAFETY: caller contract; `collect_reads_to_cancel` only increments WDF refcounts under lock.
    let (held, count) = unsafe {
        context.reads.with(|(reads, contacts, _)| {
            contacts.reset();
            let suspended = suspend(&context.runtime, reads);
            collect_reads_to_cancel(&suspended)
        })
    };
    // SAFETY: outside `context.reads` lock; `(held, count)` came from `collect_reads_to_cancel`.
    unsafe { cancel_referenced_reads(&held, count) };
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// `AmtPtpEvtDeviceD0Exit`: suspends remaining in-flight reads, drains `HidQueue` with
/// `STATUS_CANCELLED`, and sends best-effort `spi_set_state(false)` before returning `STATUS_SUCCESS`.
pub(super) unsafe extern "C" fn evt_device_d0_exit(
    device: WDFDEVICE,
    target_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    // SAFETY: WDF supplies the live device to this D0 exit callback.
    let system_power_action =
        unsafe { call_unsafe_wdf_function_binding!(WdfDeviceGetSystemPowerAction, device) };
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    let mut status = STATUS_SUCCESS;
    // SAFETY: the context belongs to a live device; PASSIVE_LEVEL.
    let _cancelled = unsafe {
        cancel_in_flight_reads(context, |runtime, reads| {
            let (d0_exit_status, suspended) = runtime.on_d0_exit(reads);
            status = d0_exit_status;
            suspended
        })
    };

    // SAFETY: `hid_queue` is the device's manual queue; PASSIVE_LEVEL.
    let drained = unsafe { drain_manual_queue(context.hid_queue.get(), D0_EXIT_DRAIN_STATUS) };

    // Best-effort switch back to mouse mode (failures on surprise removal are ignored).
    // SAFETY: PASSIVE_LEVEL (`D0Exit`); `context` belongs to the live device.
    let _off_status = unsafe { spi_set_state(context, false) };

    tlg::write_event!(
        PROVIDER,
        "D0Exit",
        level(Informational),
        keyword(keyword::DRIVER),
        i32("TargetState", &target_state),
        i32("SystemPowerAction", &system_power_action),
        u32("CancelledReads", &drained)
    );
    status
}

/// `AmtPtpEvtDeviceSelfManagedIoInitOrRestart` (Device.c:397-427), registered for both
/// `SelfManagedIoInit` and `SelfManagedIoRestart`; `PASSIVE_LEVEL`.
///
/// Runs the mode switch. Success: `DeviceStatus = D0ActiveAndConfigured`, a new reporting session, and
/// one lower read for every upstream read already waiting in `HidQueue` (P3 fix of SPI-B13). Failure:
/// start the 5 s recovery timer. Always `STATUS_SUCCESS`.
pub(super) unsafe extern "C" fn evt_device_self_managed_io_init_or_restart(
    device: WDFDEVICE,
) -> NTSTATUS {
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    // SAFETY: PASSIVE_LEVEL (SelfManagedIoInit/Restart).
    let set_state = unsafe { spi_set_state(context, true) };
    // SAFETY: the context belongs to a live device; PASSIVE_LEVEL.
    let outcome = unsafe {
        context.with_reads(|runtime, reads| {
            runtime.on_self_managed_io_init_or_restart(
                reads,
                set_state,
                query_performance_frequency,
            )
        })
    };
    if outcome.configured {
        // SAFETY: `device` is live and `context` its context; PASSIVE_LEVEL.
        let issued = unsafe { input::issue_backlog_reads(device, context) };
        trace_configured(b"SelfManagedIo", issued);
    }
    if let Some(due_time) = outcome.start_recovery_timer {
        tlg::write_event!(
            PROVIDER,
            "SetStateFailedRetryIn5s",
            level(Error),
            keyword(keyword::DRIVER),
            win_ntstatus("Status", &set_state)
        );
        // SAFETY: the timer was created in AddDevice and lives as long as the
        // device. The "already queued" result is ignored, as in C.
        let _was_pending = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfTimerStart,
                context.power_on_recovery_timer.get(),
                due_time
            )
        };
    }
    outcome.status
}

/// `AmtPtpSpiSetState(Device, DesiredState)` (Device.c:457-539): Apple's multitouch mode switch.
///
/// Sends HID `SET_FEATURE` report 2 (`02 01` to enable) as a synchronous internal
/// `IOCTL_HID_SET_FEATURE` with the 255-byte `HID_XFER_PACKET` buffer as **input**, no output, no send
/// options and no timeout (SPI-B23, preserved). The buffer holds a pointer to its own byte 16, so it is
/// filled in place here and stays at this stack address until the synchronous send has returned.
///
/// # Safety
/// `PASSIVE_LEVEL` (synchronous send). `context` belongs to a live device.
unsafe fn spi_set_state(context: &DeviceContext, desired_state: bool) -> NTSTATUS {
    let status = with_set_state_packet(desired_state, |packet| {
        let mut input = memory_descriptor_init_buffer(
            packet.as_ptr().cast_mut().cast(),
            SET_STATE_PACKET_DESCRIPTOR_LEN,
        );
        // Not in C (no options): bound the wait for a lower driver that never completes (B-3).
        let mut options = sync_send_options(0);
        // SAFETY: PASSIVE_LEVEL (caller contract); valid local I/O target; `input`
        // describes `packet`, which is neither moved nor dropped before this
        // synchronous call returns (its self-pointer stays valid; on a timeout the
        // framework waits for the cancelled request); request, output and
        // bytes-returned NULL, as in C.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoTargetSendInternalIoctlSynchronously,
                context.spi_trackpad_io_target.get(),
                null_mut(),
                SET_STATE_IOCTL,
                &raw mut input,
                null_mut(),
                &raw mut options,
                null_mut()
            )
        }
    })
    .unwrap_or(wdf_support::ntstatus::STATUS_INVALID_DEVICE_STATE);
    if NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "SetState",
            level(Informational),
            keyword(keyword::DRIVER),
            bool8("Enabled", &desired_state)
        );
    } else {
        tlg::write_event!(
            PROVIDER,
            "SetStateFailed",
            level(Error),
            keyword(keyword::DRIVER),
            bool8("Enabled", &desired_state),
            win_ntstatus("Status", &status)
        );
    }
    status
}

/// `SET_STATE_PACKET_LEN` (255) as the `ULONG` length of a memory descriptor.
const SET_STATE_PACKET_DESCRIPTOR_LEN: ULONG = {
    assert!(SET_STATE_PACKET_LEN <= ULONG::MAX as usize);
    SET_STATE_PACKET_LEN as ULONG
};

/// `AmtPtpPowerRecoveryTimerCallback` (Device.c:541-562): the passive-level timer callback (a framework
/// work item), 5 s after a failed SelfManagedIoInit/Restart.
///
/// Only while the device is in D0 and still waiting for the mode switch (P3 fix of SPI-B12: the C
/// callback also ran in D3 and after a successful restart): run the mode switch again. Success, unless
/// the device was suspended meanwhile: `DeviceStatus = D0ActiveAndConfigured`, a new reporting session,
/// and one lower read for every queued upstream read. Failure: nothing, no reschedule.
pub(super) unsafe extern "C" fn power_recovery_timer_callback(timer: WDFTIMER) {
    // SAFETY: `timer` is the framework's timer handle for this callback.
    let device: WDFDEVICE =
        unsafe { call_unsafe_wdf_function_binding!(WdfTimerGetParentObject, timer) }.cast();
    // SAFETY: the timer's parent is the device it was created for (AddDevice);
    // the callback runs at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return;
    };
    // SAFETY: the context belongs to a live device; PASSIVE_LEVEL.
    let Some(ticket) =
        (unsafe { context.with_reads(|runtime, reads| runtime.begin_recovery(reads)) })
    else {
        tlg::write_event!(
            PROVIDER,
            "PowerRecoveryTimerSkipped",
            level(Informational),
            keyword(keyword::DRIVER),
            u32("DeviceStatus", &(context.runtime.device_status() as u32))
        );
        return;
    };
    // SAFETY: passive-level timer callback.
    let status = unsafe { spi_set_state(context, true) };
    // SAFETY: as above.
    let action = unsafe {
        context.with_reads(|runtime, reads| {
            runtime.finish_recovery(reads, ticket, status, query_performance_frequency)
        })
    };
    let mut configured = false;
    let mut issued = 0;
    match action {
        RecoveryAction::Configured => {
            configured = true;
            // SAFETY: `device` is the live parent of the timer and `context` its context; PASSIVE.
            issued = unsafe { input::issue_backlog_reads(device, context) };
        }
        RecoveryAction::Retry(due_time) => {
            // SAFETY: `power_on_recovery_timer` is live.
            let _was_pending = unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfTimerStart,
                    context.power_on_recovery_timer.get(),
                    due_time
                )
            };
        }
        RecoveryAction::FailDevice => {
            // SAFETY: `device` is live; `WdfDeviceSetFailed` is callable at PASSIVE_LEVEL.
            unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfDeviceSetFailed,
                    device,
                    _WDF_DEVICE_FAILED_ACTION::WdfDeviceFailedAttemptRestart
                );
            }
        }
        RecoveryAction::Ignored => {}
    }
    tlg::write_event!(
        PROVIDER,
        "PowerRecoveryTimer",
        level(Informational),
        keyword(keyword::DRIVER),
        win_ntstatus("Status", &status),
        bool8("Configured", &configured),
        u32("ReadsIssued", &issued)
    );
}

/// Passive-level replenishment callback that issues missing lower reads after a transient issue or
/// lower-completion failure.
pub(super) unsafe extern "C" fn read_retry_timer_callback(timer: WDFTIMER) {
    // SAFETY: `timer` was created with `device` as parent in `initialize_device`.
    let device: WDFDEVICE =
        unsafe { call_unsafe_wdf_function_binding!(WdfTimerGetParentObject, timer) }.cast();
    // SAFETY: live device at `PASSIVE_LEVEL`.
    let Some(context) = (unsafe { device_context(device) }) else {
        return;
    };
    // SAFETY: live device and context at `PASSIVE_LEVEL`.
    let _issued = unsafe { input::issue_backlog_reads(device, context) };
}

/// Trace for a transition to configured and the backlog reads issued for it.
fn trace_configured(source: &[u8], issued: u32) {
    tlg::write_event!(
        PROVIDER,
        "Configured",
        level(Informational),
        keyword(keyword::DRIVER),
        str8("Source", source),
        u32("BacklogReadsIssued", &issued)
    );
}
