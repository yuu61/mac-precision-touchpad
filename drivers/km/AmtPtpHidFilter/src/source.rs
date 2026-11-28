//! The data source: the trackpad's vendor collection (`Col03`), opened as a remote I/O target (not
//! in C).
//!
//! `AmtPtpBthFilter`, below `HidBth`, declares the multitouch report `0x31` and the mode-switch
//! feature `0xF1` in a vendor collection that HIDCLASS exposes as `Col03`, a sibling of this filter's
//! `Col01`. This module finds that collection and opens it like any HID client; the core then reads
//! the padded reports from it, sends the mode switch to it and answers `IOCTL_HID_GET_STRING` with
//! its strings. Nothing here reads undocumented structures (C patched HIDCLASS's dispatch table).
//!
//! - [`start`] (`EvtDeviceSelfManagedIoInit`, `PASSIVE_LEVEL`): creates the work item and registers
//!   for `GUID_DEVINTERFACE_HID` arrivals, existing interfaces included.
//! - The notification only remembers the symbolic link of a matching collection and queues the work
//!   item: opening a device from a PnP notification callback can deadlock the PnP manager.
//! - The work item opens the link (read/write, shared), checks that the collection belongs to the same
//!   trackpad (same parent device node), reads its strings, publishes the target and calls the core's
//!   [`source_opened`].
//! - Query-remove closes the target (the core's [`source_closed`] first), so the Bluetooth device can
//!   be disabled or updated; a cancelled removal reopens it; a completed one deletes it.
//! - [`stop`] (`EvtDeviceSelfManagedIoCleanup`): unregisters, flushes the work item and closes the
//!   target.

use core::ffi::c_void;
use core::ptr::null_mut;

use crate::io::{source_closed, source_opened};

use ptp_hid::ioctl::{
    IOCTL_HID_GET_MANUFACTURER_STRING, IOCTL_HID_GET_PRODUCT_STRING,
    IOCTL_HID_GET_SERIALNUMBER_STRING,
};
use tracelogging as tlg;
use wdf_support::init::{
    memory_descriptor_init_buffer, object_attributes_init, sync_send_options, wdf_size,
    workitem_config_init,
};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_SUCCESS};
use wdk_sys::ntddk::{
    IoGetDevicePropertyData, IoRegisterPlugPlayNotification, IoUnregisterPlugPlayNotificationEx,
};
use wdk_sys::{
    _IO_NOTIFICATION_EVENT_CATEGORY, _WDF_IO_TARGET_OPEN_TYPE,
    DEVICE_INTERFACE_CHANGE_NOTIFICATION, DEVPROPKEY, DEVPROPTYPE, FILE_ATTRIBUTE_NORMAL,
    FILE_OPEN, FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE, GUID, NTSTATUS,
    PDEVICE_OBJECT, PNPNOTIFY_DEVICE_INTERFACE_INCLUDE_EXISTING_INTERFACES, PVOID, ULONG,
    ULONG_PTR, UNICODE_STRING, WDF_IO_TARGET_OPEN_PARAMS, WDFDEVICE, WDFIOTARGET, WDFWORKITEM,
    call_unsafe_wdf_function_binding,
};

use crate::context::{
    DeviceContext, MAX_LINK_UNITS, MAX_STRING_UNITS, SourceStrings, Utf16, device_context,
};
use crate::io::Filter;
use crate::{KW_DEVICE, PROVIDER};

/// `GUID_DEVINTERFACE_HID` {4D1E55B2-F16F-11CF-88CB-001111000030} (hidclass.h; defined here because
/// wdk-sys only declares it as an external symbol).
static GUID_DEVINTERFACE_HID: GUID = GUID {
    Data1: 0x4D1E_55B2,
    Data2: 0xF16F,
    Data3: 0x11CF,
    Data4: [0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30],
};

/// `GUID_DEVICE_INTERFACE_ARRIVAL` {CB3A4004-46F0-11D0-B08F-00609713053F} (wdmguid.h).
const GUID_DEVICE_INTERFACE_ARRIVAL: GUID = GUID {
    Data1: 0xCB3A_4004,
    Data2: 0x46F0,
    Data3: 0x11D0,
    Data4: [0xB0, 0x8F, 0x00, 0x60, 0x97, 0x13, 0x05, 0x3F],
};

/// `DEVPKEY_Device_Parent` {4340a6c5-93fa-4706-972c-7b648008a5a7}, 8 (devpkey.h).
static DEVPKEY_DEVICE_PARENT: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID {
        Data1: 0x4340_A6C5,
        Data2: 0x93FA,
        Data3: 0x4706,
        Data4: [0x97, 0x2C, 0x7B, 0x64, 0x80, 0x08, 0xA5, 0xA7],
    },
    pid: 8,
};

/// `LOCALE_NEUTRAL`.
const LOCALE_NEUTRAL: ULONG = 0;

/// The parts of the vendor collection's symbolic link that identify it (compared ASCII
/// case-insensitively): the Bluetooth HID service, the Magic Trackpad 2, collection 3.
const LINK_MARKERS: [&[u8]; 3] = [b"{00001124-", b"_pid&0265&col03#", b"vid&0001004c"];

fn guid_eq(a: &GUID, b: &GUID) -> bool {
    a.Data1 == b.Data1 && a.Data2 == b.Data2 && a.Data3 == b.Data3 && a.Data4 == b.Data4
}

/// Whether `link` (UTF-16) contains every [`LINK_MARKERS`] entry, ignoring ASCII case.
fn is_vendor_collection(link: &[u16]) -> bool {
    LINK_MARKERS.iter().all(|marker| {
        link.windows(marker.len()).any(|window| {
            window.iter().zip(marker.iter()).all(|(&unit, &byte)| {
                u8::try_from(unit).is_ok_and(|unit| unit.eq_ignore_ascii_case(&byte))
            })
        })
    })
}

fn trace_failure(step: &str, status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "SourceFailed",
        level(Error),
        keyword(KW_DEVICE),
        str8("Step", step),
        win_ntstatus("Status", &status)
    );
}

/// Creates the source work item and registers for HID interface arrivals.
///
/// # Safety
/// `PASSIVE_LEVEL` (`EvtDeviceSelfManagedIoInit`); `device` is this driver's live device and `ctx`
/// its context.
pub(crate) unsafe fn start(device: WDFDEVICE, ctx: &DeviceContext) -> NTSTATUS {
    if ctx.source_work_item().is_null() {
        let mut config = workitem_config_init(Some(evt_open_source));
        config.AutomaticSerialization = 0;
        let mut attributes = object_attributes_init();
        attributes.ParentObject = device.cast();
        let mut work_item: WDFWORKITEM = null_mut();
        // SAFETY: initialized config and attributes (parent = the live device); valid out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfWorkItemCreate,
                &raw mut config,
                &raw mut attributes,
                &raw mut work_item
            )
        };
        if !NT_SUCCESS(status) {
            trace_failure("WdfWorkItemCreate", status);
            return status;
        }
        ctx.set_source_work_item(work_item);
    }
    if !ctx.notification_entry().is_null() {
        return STATUS_SUCCESS;
    }
    let mut entry: PVOID = null_mut();
    let device_object = ctx.wdm_device_object();
    if device_object.is_null() {
        return wdf_support::ntstatus::STATUS_INVALID_DEVICE_STATE;
    }
    // SAFETY: PASSIVE_LEVEL; our driver object (from our own WDM device object, stored in
    // AddDevice); the callback's context is the device, which outlives the registration (`stop`
    // unregisters it in EvtDeviceSelfManagedIoCleanup and waits for running callbacks).
    let status = unsafe {
        let driver_object = (*device_object).DriverObject;
        IoRegisterPlugPlayNotification(
            _IO_NOTIFICATION_EVENT_CATEGORY::EventCategoryDeviceInterfaceChange,
            PNPNOTIFY_DEVICE_INTERFACE_INCLUDE_EXISTING_INTERFACES,
            (&raw const GUID_DEVINTERFACE_HID)
                .cast_mut()
                .cast::<c_void>(),
            driver_object,
            Some(on_interface_change),
            device.cast::<c_void>(),
            &raw mut entry,
        )
    };
    if !NT_SUCCESS(status) {
        trace_failure("IoRegisterPlugPlayNotification", status);
        return status;
    }
    ctx.set_notification_entry(entry);
    STATUS_SUCCESS
}

/// Unregisters, flushes the work item and closes the source.
///
/// # Safety
/// `PASSIVE_LEVEL` (`EvtDeviceSelfManagedIoCleanup`); `ctx` is the live device's context.
pub(crate) unsafe fn stop(ctx: &DeviceContext) {
    let entry = ctx.notification_entry();
    if !entry.is_null() {
        ctx.set_notification_entry(null_mut());
        // SAFETY: PASSIVE_LEVEL; the entry returned by IoRegisterPlugPlayNotification. The Ex form
        // waits for callbacks in progress, after which none uses the device.
        let _status = unsafe { IoUnregisterPlugPlayNotificationEx(entry) };
    }
    let work_item = ctx.source_work_item();
    if !work_item.is_null() {
        // SAFETY: our live work item (parent: the device); PASSIVE_LEVEL, not called from it.
        unsafe { call_unsafe_wdf_function_binding!(WdfWorkItemFlush, work_item) };
    }
    let target = ctx.hid_io_target();
    if !target.is_null() {
        source_closed(&mut Filter::new(ctx), &ctx.state);
        ctx.set_hid_io_target(null_mut());
        // SAFETY: our open remote target; nothing sends to it any more (no source).
        unsafe {
            call_unsafe_wdf_function_binding!(WdfIoTargetClose, target);
            call_unsafe_wdf_function_binding!(WdfObjectDelete, target.cast::<c_void>());
        }
        tlg::write_event!(
            PROVIDER,
            "SourceClosed",
            level(Informational),
            keyword(KW_DEVICE)
        );
    }
}

/// `GUID_DEVINTERFACE_HID` arrival (`PASSIVE_LEVEL`): remembers the link of the vendor collection and
/// queues the work item that opens it.
unsafe extern "C" fn on_interface_change(notification: PVOID, context: PVOID) -> NTSTATUS {
    let notification = notification.cast::<DEVICE_INTERFACE_CHANGE_NOTIFICATION>();
    // SAFETY: the PnP manager passes a valid interface-change notification for this category.
    let (event, link) = unsafe { ((*notification).Event, (*notification).SymbolicLinkName) };
    if !guid_eq(&event, &GUID_DEVICE_INTERFACE_ARRIVAL) || link.is_null() {
        return STATUS_SUCCESS;
    }
    // SAFETY: a valid UNICODE_STRING for the duration of the callback.
    let (buffer, bytes) = unsafe { ((*link).Buffer, usize::from((*link).Length)) };
    if buffer.is_null() {
        return STATUS_SUCCESS;
    }
    // SAFETY: `Length` bytes of UTF-16 at `Buffer`, valid during the callback.
    let units = unsafe { core::slice::from_raw_parts(buffer, bytes / 2) };
    if !is_vendor_collection(units) || units.len() > MAX_LINK_UNITS {
        return STATUS_SUCCESS;
    }
    // SAFETY: the registration's context is our device, alive until `stop` unregisters.
    let ctx = unsafe { device_context(context.cast()) };
    // SAFETY: the spin lock was created in AddDevice; the closure does not call back into WDF.
    unsafe {
        ctx.pending_link.with(|pending| {
            if let Some(dst) = pending.units.get_mut(..units.len()) {
                dst.copy_from_slice(units);
                pending.len = units.len();
            }
        });
    }
    let work_item = ctx.source_work_item();
    if !work_item.is_null() {
        // SAFETY: our live work item; enqueueing an already queued item is a no-op.
        unsafe { call_unsafe_wdf_function_binding!(WdfWorkItemEnqueue, work_item) };
    }
    STATUS_SUCCESS
}

/// The source work item (`PASSIVE_LEVEL`): opens the pending link unless a source is already open.
unsafe extern "C" fn evt_open_source(work_item: WDFWORKITEM) {
    // SAFETY: our live work item, parented to the device.
    let device: WDFDEVICE =
        unsafe { call_unsafe_wdf_function_binding!(WdfWorkItemGetParentObject, work_item) }.cast();
    // SAFETY: the work item's parent is our device, alive while its child runs.
    let ctx = unsafe { device_context(device) };
    if !ctx.hid_io_target().is_null() {
        return;
    }
    let mut link = [0_u16; MAX_LINK_UNITS];
    // SAFETY: the spin lock was created in AddDevice; the closure does not call back into WDF.
    let len = unsafe {
        ctx.pending_link.with(|pending| {
            let len = pending.len;
            if let (Some(dst), Some(src)) = (link.get_mut(..len), pending.units.get(..len)) {
                dst.copy_from_slice(src);
            }
            pending.len = 0;
            len
        })
    };
    let Some(link) = link.get(..len).filter(|l| !l.is_empty()) else {
        return;
    };
    // SAFETY: PASSIVE_LEVEL; our live device and its context.
    unsafe { open(device, ctx, link) };
}

/// Opens `link`, checks that it is this trackpad's collection, reads its strings and publishes it.
///
/// # Safety
/// `PASSIVE_LEVEL`; `device` is this driver's live device and `ctx` its context.
unsafe fn open(device: WDFDEVICE, ctx: &DeviceContext, link: &[u16]) {
    let mut attributes = object_attributes_init();
    attributes.ParentObject = device.cast();
    let mut target: WDFIOTARGET = null_mut();
    // SAFETY: our live device; initialized attributes (parent = device); valid out pointer.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoTargetCreate,
            device,
            &raw mut attributes,
            &raw mut target
        )
    };
    if !NT_SUCCESS(status) {
        trace_failure("WdfIoTargetCreate", status);
        return;
    }
    let Ok(bytes) = u16::try_from(link.len() * 2) else {
        // SAFETY: the target we just created, not opened.
        unsafe { call_unsafe_wdf_function_binding!(WdfObjectDelete, target.cast::<c_void>()) };
        return;
    };
    let name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: link.as_ptr().cast_mut(),
    };
    let mut params = open_by_name(name);
    // SAFETY: PASSIVE_LEVEL; our target; `params` is initialized and `link` outlives the call.
    let status =
        unsafe { call_unsafe_wdf_function_binding!(WdfIoTargetOpen, target, &raw mut params) };
    if !NT_SUCCESS(status) {
        trace_failure("WdfIoTargetOpen", status);
        // SAFETY: our target, not open.
        unsafe { call_unsafe_wdf_function_binding!(WdfObjectDelete, target.cast::<c_void>()) };
        return;
    }
    // SAFETY: PASSIVE_LEVEL; our live device and the open target.
    if !unsafe { same_trackpad(device, target) } {
        tlg::write_event!(
            PROVIDER,
            "SourceOfAnotherDevice",
            level(Warning),
            keyword(KW_DEVICE)
        );
        // SAFETY: our open target, nothing sent to it.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfIoTargetClose, target);
            call_unsafe_wdf_function_binding!(WdfObjectDelete, target.cast::<c_void>());
        }
        return;
    }
    // SAFETY: PASSIVE_LEVEL; the open target.
    let strings = unsafe { read_strings(target) };
    // SAFETY: the spin lock was created in AddDevice; the closure does not call back into WDF.
    unsafe { ctx.strings.with(|s| *s = strings) };
    ctx.set_hid_io_target(target);
    tlg::write_event!(
        PROVIDER,
        "SourceOpened",
        level(Informational),
        keyword(KW_DEVICE)
    );
    source_opened(&mut Filter::new(ctx), &ctx.state);
}

/// `WDF_IO_TARGET_OPEN_PARAMS_INIT_OPEN_BY_NAME(&params, name, GENERIC_READ | GENERIC_WRITE)`, shared
/// for reading and writing, with the removal callbacks.
fn open_by_name(name: UNICODE_STRING) -> WDF_IO_TARGET_OPEN_PARAMS {
    WDF_IO_TARGET_OPEN_PARAMS {
        Size: wdf_size::<WDF_IO_TARGET_OPEN_PARAMS>(),
        Type: _WDF_IO_TARGET_OPEN_TYPE::WdfIoTargetOpenByName,
        EvtIoTargetQueryRemove: Some(evt_query_remove),
        EvtIoTargetRemoveCanceled: Some(evt_remove_canceled),
        EvtIoTargetRemoveComplete: Some(evt_remove_complete),
        TargetDeviceName: name,
        DesiredAccess: GENERIC_READ | GENERIC_WRITE,
        ShareAccess: FILE_SHARE_READ | FILE_SHARE_WRITE,
        FileAttributes: FILE_ATTRIBUTE_NORMAL,
        CreateDisposition: FILE_OPEN,
        ..Default::default()
    }
}

/// Whether the target's collection and this device have the same parent device node (the same
/// Bluetooth HID device). A PDO that cannot be found counts as the same: the link already names the
/// Magic Trackpad 2's collection 3.
///
/// # Safety
/// `PASSIVE_LEVEL`; our live device and an open target.
unsafe fn same_trackpad(device: WDFDEVICE, target: WDFIOTARGET) -> bool {
    // SAFETY: our live device and open target.
    let (ours, theirs) = unsafe {
        (
            call_unsafe_wdf_function_binding!(WdfDeviceWdmGetPhysicalDevice, device),
            call_unsafe_wdf_function_binding!(WdfIoTargetWdmGetTargetPhysicalDevice, target),
        )
    };
    if ours.is_null() || theirs.is_null() {
        return true;
    }
    let mut a = [0_u16; MAX_LINK_UNITS];
    let mut b = [0_u16; MAX_LINK_UNITS];
    // SAFETY: PASSIVE_LEVEL; live PDOs; buffers of the given sizes.
    match unsafe { (parent_of(ours, &mut a), parent_of(theirs, &mut b)) } {
        (Some(x), Some(y)) => a.get(..x) == b.get(..y),
        _ => true,
    }
}

/// `DEVPKEY_Device_Parent` of `pdo` into `buffer`; the number of UTF-16 units written.
///
/// # Safety
/// `PASSIVE_LEVEL`; `pdo` is a live PDO.
unsafe fn parent_of(pdo: PDEVICE_OBJECT, buffer: &mut [u16; MAX_LINK_UNITS]) -> Option<usize> {
    let mut required: ULONG = 0;
    let mut kind: DEVPROPTYPE = 0;
    // SAFETY: PASSIVE_LEVEL; live PDO; the buffer holds MAX_LINK_UNITS * 2 bytes.
    let status = unsafe {
        IoGetDevicePropertyData(
            pdo,
            &raw const DEVPKEY_DEVICE_PARENT,
            LOCALE_NEUTRAL,
            0,
            (MAX_LINK_UNITS * 2) as ULONG,
            buffer.as_mut_ptr().cast::<c_void>(),
            &raw mut required,
            &raw mut kind,
        )
    };
    NT_SUCCESS(status).then(|| (required as usize / 2).min(MAX_LINK_UNITS))
}

/// Reads the manufacturer, product and serial-number strings of the open target.
///
/// # Safety
/// `PASSIVE_LEVEL`; an open target.
unsafe fn read_strings(target: WDFIOTARGET) -> SourceStrings {
    // SAFETY: forwarded.
    let read = |code| unsafe { read_string(target, code) };
    SourceStrings {
        known: true,
        manufacturer: read(IOCTL_HID_GET_MANUFACTURER_STRING),
        product: read(IOCTL_HID_GET_PRODUCT_STRING),
        serial_number: read(IOCTL_HID_GET_SERIALNUMBER_STRING),
    }
}

/// One string IOCTL (`METHOD_OUT_DIRECT`, a NUL-terminated UTF-16 string); empty on failure.
///
/// # Safety
/// `PASSIVE_LEVEL`; an open target.
unsafe fn read_string(target: WDFIOTARGET, code: ULONG) -> Utf16 {
    let mut out = Utf16::EMPTY;
    let mut descriptor = memory_descriptor_init_buffer(
        out.units.as_mut_ptr().cast::<c_void>(),
        (MAX_STRING_UNITS * 2) as ULONG,
    );
    let mut options = sync_send_options(0);
    let mut returned: ULONG_PTR = 0;
    // SAFETY: PASSIVE_LEVEL; open target; the descriptor covers `out.units`, which outlives the
    // synchronous call (on a timeout the framework waits for the cancelled request).
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoTargetSendIoctlSynchronously,
            target,
            null_mut(),
            code,
            null_mut(),
            &raw mut descriptor,
            &raw mut options,
            &raw mut returned
        )
    };
    if NT_SUCCESS(status) {
        let units = (returned as usize / 2).min(MAX_STRING_UNITS);
        out.len = out
            .units
            .get(..units)
            .and_then(|u| u.iter().position(|&c| c == 0))
            .unwrap_or(units);
    } else {
        tlg::write_event!(
            PROVIDER,
            "SourceStringFailed",
            level(Warning),
            keyword(KW_DEVICE),
            u32_hex("IoControlCode", &code),
            win_ntstatus("Status", &status)
        );
    }
    out
}

/// The device of one of this driver's remote targets.
///
/// # Safety
/// `target` is a live target this driver created.
unsafe fn target_context<'a>(target: WDFIOTARGET) -> &'a DeviceContext {
    // SAFETY: our target was created for our device, which outlives it.
    unsafe {
        let device = call_unsafe_wdf_function_binding!(WdfIoTargetGetDevice, target);
        device_context(device)
    }
}

/// The collection is about to be removed: stop using it and close it, so the removal can proceed.
unsafe extern "C" fn evt_query_remove(target: WDFIOTARGET) -> NTSTATUS {
    // SAFETY: our live target.
    let ctx = unsafe { target_context(target) };
    source_closed(&mut Filter::new(ctx), &ctx.state);
    ctx.set_hid_io_target(null_mut());
    // SAFETY: our open target; reads were cancelled and no new request is sent (no source).
    unsafe { call_unsafe_wdf_function_binding!(WdfIoTargetCloseForQueryRemove, target) };
    tlg::write_event!(
        PROVIDER,
        "SourceQueryRemove",
        level(Informational),
        keyword(KW_DEVICE)
    );
    STATUS_SUCCESS
}

/// The removal was cancelled: reopen the collection.
unsafe extern "C" fn evt_remove_canceled(target: WDFIOTARGET) {
    // SAFETY: our live target.
    let ctx = unsafe { target_context(target) };
    let mut params = WDF_IO_TARGET_OPEN_PARAMS {
        Size: wdf_size::<WDF_IO_TARGET_OPEN_PARAMS>(),
        Type: _WDF_IO_TARGET_OPEN_TYPE::WdfIoTargetOpenReopen,
        ..Default::default()
    };
    // SAFETY: our target, closed for query-remove; initialized reopen parameters.
    let status =
        unsafe { call_unsafe_wdf_function_binding!(WdfIoTargetOpen, target, &raw mut params) };
    if NT_SUCCESS(status) {
        ctx.set_hid_io_target(target);
        tlg::write_event!(
            PROVIDER,
            "SourceReopened",
            level(Informational),
            keyword(KW_DEVICE)
        );
        source_opened(&mut Filter::new(ctx), &ctx.state);
    } else {
        trace_failure("WdfIoTargetOpen(reopen)", status);
        // SAFETY: our target, not open.
        unsafe { call_unsafe_wdf_function_binding!(WdfObjectDelete, target.cast::<c_void>()) };
    }
}

/// The collection is gone: close and delete the target. A new collection is found through the
/// interface notification again.
unsafe extern "C" fn evt_remove_complete(target: WDFIOTARGET) {
    // SAFETY: our live target.
    let ctx = unsafe { target_context(target) };
    if ctx.hid_io_target() == target {
        source_closed(&mut Filter::new(ctx), &ctx.state);
        ctx.set_hid_io_target(null_mut());
    }
    // SAFETY: our target; the collection is removed.
    unsafe {
        call_unsafe_wdf_function_binding!(WdfIoTargetClose, target);
        call_unsafe_wdf_function_binding!(WdfObjectDelete, target.cast::<c_void>());
    }
    tlg::write_event!(
        PROVIDER,
        "SourceRemoved",
        level(Informational),
        keyword(KW_DEVICE)
    );
}
