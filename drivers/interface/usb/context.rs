//! The device context: the port of the C `DEVICE_CONTEXT` (Device.h:25-47).
//!
//! The hardware-independent fields live in [`DeviceState`] (core). The WDF handles stay here. The C
//! context has no locking although the reader completion (≤ `DISPATCH_LEVEL`), the parallel I/O queue and
//! the PnP/power callbacks use it concurrently, so every field is atomic (port policy P2 "atomics with the same
//! semantics", port record KM-X1): handles are `AtomicPtr` with `Relaxed` ordering, which is the plain
//! pointer load/store the C compiler emits. The context is written once with `ptr.write` right after
//! `WdfDeviceCreate`; afterwards only shared references (`&DeviceContext`) exist.
//!
//! | C field | here |
//! |---|---|
//! | `UsbDevice` | [`DeviceContext::usb_device`] |
//! | `InterruptPipe` | [`DeviceContext::interrupt_pipe`] (handle) + `DeviceState::interrupt_pipe` (pipe index) |
//! | `UsbInterface` | [`DeviceContext::usb_interface`] |
//! | `InputQueue` | [`DeviceContext::input_queue`] |
//! | `DeviceDescriptor`, `UsbDeviceTraits`, `DeviceInfo`, `IsWellspringModeOn`, `PtpReportTouch`, `PtpReportButton`, `LastReportTime` | [`DeviceContext::state`] |
//! | `PtpInputOn` | dropped (never read or written) |

use amtptp_usb_bt::usb::wellspring::WELLSPRING_DATA_LEN;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use amtptp_usb_bt::usb::input::ContactTracker;
use amtptp_usb_bt::usb::state::DeviceState;
use wdf_support::declare_context_type;
use wdf_support::sync::{SpinLocked, WaitLock};
use wdk_sys::{
    WDFQUEUE, WDFQUEUE__, WDFUSBDEVICE, WDFUSBDEVICE__, WDFUSBINTERFACE, WDFUSBINTERFACE__,
    WDFUSBPIPE, WDFUSBPIPE__,
};

const RELAXED: Ordering = Ordering::Relaxed;

/// Per-device context (`DEVICE_CONTEXT`).
pub(crate) struct DeviceContext {
    /// `UsbDevice`: created by the first `PrepareHardware`, reused on restart.
    usb_device: AtomicPtr<WDFUSBDEVICE__>,
    /// `InterruptPipe`: the continuous-reader pipe (NULL until selected; reset at every reselection).
    interrupt_pipe: AtomicPtr<WDFUSBPIPE__>,
    /// `UsbInterface`: the selected interface.
    usb_interface: AtomicPtr<WDFUSBINTERFACE__>,
    /// `InputQueue`: the manual queue of pended `READ_REPORTs`.
    input_queue: AtomicPtr<WDFQUEUE__>,
    pub(crate) recovery_pending: AtomicBool,
    pub(crate) mode_recovery: AtomicPtr<wdk_sys::WDFWORKITEM__>,
    /// Serializes the wellspring mode switches (not in C): created in `create_device`, held by
    /// `UsbIo::serialized` around each outermost `GET_REPORT`/`SET_REPORT` sequence.
    pub(crate) mode_switch_lock: WaitLock<[u8; WELLSPRING_DATA_LEN]>,
    /// Persistent `ContactID` and liftoff state, protected by a `WDFSPINLOCK` created in
    /// `create_device`. Never held across synchronous USB control transfers.
    pub(crate) contacts: SpinLocked<ContactTracker>,
    /// Everything else (core).
    pub(crate) state: DeviceState,
}

// The C name, so `!wdfkd.wdfobject` shows `DEVICE_CONTEXT` like the C driver.
declare_context_type!(pub(crate) DeviceContext, DEVICE_CONTEXT_TYPE_INFO, device_get_context, name = "DEVICE_CONTEXT");

impl DeviceContext {
    /// The zero state `WdfDeviceCreate` gives the C context (all handles NULL, all flags FALSE).
    pub(crate) const fn new() -> Self {
        Self {
            usb_device: AtomicPtr::new(ptr::null_mut()),
            interrupt_pipe: AtomicPtr::new(ptr::null_mut()),
            usb_interface: AtomicPtr::new(ptr::null_mut()),
            input_queue: AtomicPtr::new(ptr::null_mut()),
            recovery_pending: AtomicBool::new(false),
            mode_recovery: AtomicPtr::new(ptr::null_mut()),
            mode_switch_lock: WaitLock::new([0; WELLSPRING_DATA_LEN]),
            contacts: SpinLocked::new(ContactTracker::new()),
            state: DeviceState::new(),
        }
    }

    /// The context of `device` (the C `DeviceGetContext`), or `None` if the object has none (impossible
    /// for the devices this driver creates; the C would dereference NULL).
    ///
    /// # Safety
    /// `device` is a live `WDFDEVICE` created by [`crate::device`] (its context was initialized with
    /// `ptr.write` before any callback that calls this can run); IRQL <= `DISPATCH_LEVEL`. The returned
    /// reference must not outlive the device.
    pub(crate) unsafe fn of<'a, H>(device: *mut H) -> Option<&'a Self> {
        // SAFETY: live WDF handle and IRQL per the caller contract.
        let context = unsafe { device_get_context(device) };
        // SAFETY: a non-null result points at this device's context memory, which was initialized right
        // after WdfDeviceCreate (caller contract) and is only accessed through shared references (all
        // fields are atomics); it lives as long as the device.
        unsafe { context.as_ref() }
    }

    pub(crate) fn usb_device(&self) -> WDFUSBDEVICE {
        self.usb_device.load(RELAXED)
    }

    pub(crate) fn set_usb_device(&self, usb_device: WDFUSBDEVICE) {
        self.usb_device.store(usb_device, RELAXED);
    }

    pub(crate) fn interrupt_pipe(&self) -> WDFUSBPIPE {
        self.interrupt_pipe.load(RELAXED)
    }

    pub(crate) fn set_interrupt_pipe(&self, pipe: WDFUSBPIPE) {
        self.interrupt_pipe.store(pipe, RELAXED);
    }

    pub(crate) fn usb_interface(&self) -> WDFUSBINTERFACE {
        self.usb_interface.load(RELAXED)
    }

    pub(crate) fn set_usb_interface(&self, interface: WDFUSBINTERFACE) {
        self.usb_interface.store(interface, RELAXED);
    }

    pub(crate) fn input_queue(&self) -> WDFQUEUE {
        self.input_queue.load(RELAXED)
    }

    pub(crate) fn set_input_queue(&self, queue: WDFQUEUE) {
        self.input_queue.store(queue, RELAXED);
    }
}
