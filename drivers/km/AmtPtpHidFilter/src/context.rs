//! WDF object contexts (`include/Device.h`, `include/Queue.h`).
//!
//! The context type names given to WDF are the C ones (`DEVICE_CONTEXT`, `QUEUE_CONTEXT`,
//! `WORKER_REQUEST_CONTEXT`), so `!wdfkd.wdfobject` shows the same names as for the C driver. The
//! layouts are private and differ from C (spec 3.1 says so explicitly).

use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

use amtptp_hidfilter_core::DeviceState;
use wdf_support::declare_context_type;
use wdk_sys::{
    _DEVICE_OBJECT, PDEVICE_OBJECT, WDFDEVICE, WDFDEVICE__, WDFIOTARGET, WDFIOTARGET__,
    WDFLOOKASIDE, WDFLOOKASIDE__, WDFMEMORY, WDFQUEUE, WDFQUEUE__, WDFTIMER, WDFTIMER__,
    WDFWORKITEM, WDFWORKITEM__,
};

/// `DEVICE_CONTEXT` (Device.h:26-60).
///
/// The non-handle fields (IDs, transport parameters, `DeviceConfigured`, `LastReportTime`, the
/// feature-report flags, `IsHidIoDetourCompleted`) are the core's [`DeviceState`] (relaxed atomics:
/// the C code reads and writes them without locks from up to five concurrent contexts, port policy P2).
///
/// The handles are atomics too: the context becomes visible to WDF as soon as it is written in the
/// create path, and `AddDevice` and the detour store handles into it afterwards. No `&mut` to the
/// context is ever formed after its initializing `ptr.write`. Every handle except the I/O target is
/// stored once during `AddDevice`, before the device can receive any callback, so `Relaxed` suffices;
/// the I/O target is stored in `EvtDeviceSelfManagedIoInit` and published with `Release`/`Acquire`.
pub(crate) struct DeviceContext {
    /// `WdmDeviceObject`: our own WDM device object (`WdfDeviceWdmGetDeviceObject`).
    wdm_device_object: AtomicPtr<_DEVICE_OBJECT>,
    /// `Device`: the WDFDEVICE itself.
    device: AtomicPtr<WDFDEVICE__>,
    /// `HidReadQueue`: manual queue of parked upper `IOCTL_HID_READ_REPORT` requests.
    hid_read_queue: AtomicPtr<WDFQUEUE__>,
    /// `HidReadBufferLookaside`: 1024-byte `NonPagedPoolNx` buffers for lower reads.
    hid_read_buffer_lookaside: AtomicPtr<WDFLOOKASIDE__>,
    /// `HidIoTarget`: the default (local) I/O target, set by the detour; NULL before it.
    hid_io_target: AtomicPtr<WDFIOTARGET__>,
    /// `HidTransportRecoveryTimer`: one-shot passive-level retry timer.
    hid_transport_recovery_timer: AtomicPtr<WDFTIMER__>,
    /// `HidTransportRecoveryWorkItem`: re-issues a lower read at `PASSIVE_LEVEL`.
    hid_transport_recovery_work_item: AtomicPtr<WDFWORKITEM__>,
    /// Every other `DEVICE_CONTEXT` member.
    pub(crate) state: DeviceState,
}

declare_context_type!(pub(crate) DeviceContext, DEVICE_CONTEXT_TYPE_INFO, device_get_context, name = "DEVICE_CONTEXT");

impl DeviceContext {
    /// The context right after `WdfDeviceCreate` + `ctx->Device = device` (KMDF zero-fills the C
    /// context; the C code then sets the IDs to 0 and `DeviceConfigured = FALSE` again).
    pub(crate) const fn new(device: WDFDEVICE) -> Self {
        Self {
            wdm_device_object: AtomicPtr::new(null_mut()),
            device: AtomicPtr::new(device),
            hid_read_queue: AtomicPtr::new(null_mut()),
            hid_read_buffer_lookaside: AtomicPtr::new(null_mut()),
            hid_io_target: AtomicPtr::new(null_mut()),
            hid_transport_recovery_timer: AtomicPtr::new(null_mut()),
            hid_transport_recovery_work_item: AtomicPtr::new(null_mut()),
            state: DeviceState::new(),
        }
    }

    pub(crate) fn device(&self) -> WDFDEVICE {
        self.device.load(Ordering::Relaxed)
    }

    pub(crate) fn wdm_device_object(&self) -> PDEVICE_OBJECT {
        self.wdm_device_object.load(Ordering::Relaxed)
    }

    pub(crate) fn set_wdm_device_object(&self, object: PDEVICE_OBJECT) {
        self.wdm_device_object.store(object, Ordering::Relaxed);
    }

    pub(crate) fn hid_read_queue(&self) -> WDFQUEUE {
        self.hid_read_queue.load(Ordering::Relaxed)
    }

    pub(crate) fn set_hid_read_queue(&self, queue: WDFQUEUE) {
        self.hid_read_queue.store(queue, Ordering::Relaxed);
    }

    pub(crate) fn hid_read_buffer_lookaside(&self) -> WDFLOOKASIDE {
        self.hid_read_buffer_lookaside.load(Ordering::Relaxed)
    }

    pub(crate) fn set_hid_read_buffer_lookaside(&self, lookaside: WDFLOOKASIDE) {
        self.hid_read_buffer_lookaside
            .store(lookaside, Ordering::Relaxed);
    }

    /// `HidIoTarget`; NULL until the detour succeeded.
    pub(crate) fn hid_io_target(&self) -> WDFIOTARGET {
        self.hid_io_target.load(Ordering::Acquire)
    }

    pub(crate) fn set_hid_io_target(&self, target: WDFIOTARGET) {
        self.hid_io_target.store(target, Ordering::Release);
    }

    pub(crate) fn recovery_timer(&self) -> WDFTIMER {
        self.hid_transport_recovery_timer.load(Ordering::Relaxed)
    }

    pub(crate) fn set_recovery_timer(&self, timer: WDFTIMER) {
        self.hid_transport_recovery_timer
            .store(timer, Ordering::Relaxed);
    }

    pub(crate) fn recovery_work_item(&self) -> WDFWORKITEM {
        self.hid_transport_recovery_work_item
            .load(Ordering::Relaxed)
    }

    pub(crate) fn set_recovery_work_item(&self, work_item: WDFWORKITEM) {
        self.hid_transport_recovery_work_item
            .store(work_item, Ordering::Relaxed);
    }
}

/// Returns the [`DeviceContext`] of `device`.
///
/// # Safety
/// `device` must be a live WDFDEVICE created by this driver's device-add (so it carries an
/// initialized `DEVICE_CONTEXT`), called at IRQL <= `DISPATCH_LEVEL`. The reference must not outlive
/// the device; every caller uses it only for the duration of one WDF callback on that device (or of
/// an object parented to it).
pub(crate) unsafe fn device_context<'a>(device: WDFDEVICE) -> &'a DeviceContext {
    // SAFETY: caller contract (live device of this driver, context initialized by the create path
    // with `ptr.write`, never null for such a device). Only shared references are formed; all
    // mutation goes through atomics.
    unsafe { &*device_get_context(device) }
}

/// `QUEUE_CONTEXT` (Queue.h:13-18) of the default queue. `DeviceIoTarget` is dropped: C copies
/// `HidIoTarget` into it while it is still NULL and never reads it (HF-B19).
pub(crate) struct QueueContext {
    /// `Device`: the queue's WDFDEVICE.
    pub(crate) device: WDFDEVICE,
}

declare_context_type!(pub(crate) QueueContext, QUEUE_CONTEXT_TYPE_INFO, queue_get_context, name = "QUEUE_CONTEXT");

/// `WORKER_REQUEST_CONTEXT` (Device.h:65-70), attached to every lower `IOCTL_HID_READ_REPORT`
/// request; its address is also the completion routine's context (Input.c:100).
pub(crate) struct WorkerRequestContext {
    /// `DeviceContext`: back pointer to the device context (the request is a child of the device).
    pub(crate) device_context: *const DeviceContext,
    /// `RequestMemory`: the lookaside buffer formatted as the request's output buffer.
    pub(crate) request_memory: WDFMEMORY,
}

declare_context_type!(
    pub(crate) WorkerRequestContext,
    WORKER_REQUEST_CONTEXT_TYPE_INFO,
    worker_request_get_context,
    name = "WORKER_REQUEST_CONTEXT"
);
