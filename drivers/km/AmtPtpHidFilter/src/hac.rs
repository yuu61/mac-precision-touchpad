//! The HIDCLASS detour (`Detour.c`, `include/Hac.h`): the **only** module that reads undocumented
//! kernel structures and writes into another driver's `DRIVER_OBJECT`.
//!
//! The checks, their order, the reference balance and the `strncmp` semantics are in
//! `amtptp_hidfilter_core::detour_hid_stack`; this module implements its [`DetourIo`] pointer
//! reads and the single pointer store with raw pointers into hand-declared `#[repr(C)]` mirrors of
//! `Hac.h`, whose offsets are checked at compile time against the core's constants (and those
//! against the LP64 values the spec and the C oracle recorded: 0x28, 8, 16, 32, slot 0xA8, 304).
//!
//! What the patch does (spec 6.2): the transport minidriver's original
//! `IRP_MJ_INTERNAL_DEVICE_CONTROL` handler, which HIDCLASS saved in its client driver extension, is
//! written back into the minidriver's `DRIVER_OBJECT.MajorFunction[0x0F]`, so internal HID IOCTLs
//! sent to the HID collection PDO reach HidBth/HidUsb directly. The patch is global (every device
//! of that minidriver), never reverted, version-fragile and unsynchronized with other CPUs reading
//! the slot; all of that is preserved C behaviour (HF-B26). The stored value is the minidriver's own
//! routine, so nothing dangles after this driver unloads.
//!
//! `IoGetLowerDeviceObject` (C: hand-declared in `Hac.h` because it lives in `ntifs.h`) and
//! `ObfDereferenceObject` (behind the `ObDereferenceObject` macro) come from wdk-sys's ntddk
//! bindings, which include `ntifs.h`.
//!
//! Runs only from `EvtDeviceSelfManagedIoInit`, at `PASSIVE_LEVEL`.

use core::ffi::c_void;
use core::mem::{offset_of, size_of};
use core::ptr::addr_of;

use amtptp_hidfilter_core::DetourIo;
use amtptp_hidfilter_core::detour::{
    DRIVER_EXTENSION_IO_CLIENT_EXTENSION_OFFSET, HIDCLASS_DRIVER_EXTENSION_LEN,
    HIDCLASS_EXTENSION_MAJOR_FUNCTION_OFFSET, IO_CLIENT_EXTENSION_CLIENT_ID_OFFSET,
    IO_CLIENT_EXTENSION_LEN, IRP_MJ_INTERNAL_DEVICE_CONTROL, IRP_MJ_MAXIMUM_FUNCTION,
    MINIDRIVER_INTERNAL_IOCTL_SLOT_OFFSET,
};
use tracelogging as tlg;
use wdk_sys::{
    BOOLEAN, DRIVER_EXTENSION, DRIVER_OBJECT, LIST_ENTRY, LONG, PDEVICE_OBJECT, PDRIVER_ADD_DEVICE,
    PDRIVER_DISPATCH, PDRIVER_OBJECT, PDRIVER_UNLOAD, PVOID, ULONG, UNICODE_STRING,
    call_unsafe_wdf_function_binding,
};

use crate::io::Filter;
use crate::{KW_DEVICE, PROVIDER};

/// `HIDCLASS_DRIVER_EXTENSION` (Hac.h:7-17): HIDCLASS's private per-minidriver data, which follows
/// the `IO_CLIENT_EXTENSION` header of its driver-object extension. Only `MajorFunction[0x0F]` is
/// read; the other members exist for the layout.
#[repr(C)]
pub(crate) struct HidclassDriverExtension {
    _minidriver_object: PDRIVER_OBJECT,
    _registry_path: UNICODE_STRING,
    _device_extension_size: ULONG,
    major_function: [PDRIVER_DISPATCH; IRP_MJ_MAXIMUM_FUNCTION + 1],
    _add_device: PDRIVER_ADD_DEVICE,
    _driver_unload: PDRIVER_UNLOAD,
    _reference_count: LONG,
    _list_entry: LIST_ENTRY,
    _devices_are_polled: BOOLEAN,
}

/// `IO_CLIENT_EXTENSION` (Hac.h:19-23): the I/O manager's header of a driver-object extension
/// allocated with `IoAllocateDriverObjectExtension`.
#[repr(C)]
pub(crate) struct IoClientExtension {
    _next_extension: *mut Self,
    client_identification_address: PVOID,
}

/// `DRIVER_EXTENSION_EXT` (Hac.h:25-31): the documented `DRIVER_EXTENSION` plus the undocumented
/// `IoClientExtension` that follows it.
#[repr(C)]
pub(crate) struct DriverExtensionExt {
    _driver_object: *mut DRIVER_OBJECT,
    _add_device: PDRIVER_ADD_DEVICE,
    count: ULONG,
    service_key_name: UNICODE_STRING,
    io_client_extension: *mut IoClientExtension,
}

// Layout checks: the mirrors against the core's offsets, and the documented prefix against the WDK.
const _: () = {
    assert!(
        offset_of!(DriverExtensionExt, io_client_extension)
            == DRIVER_EXTENSION_IO_CLIENT_EXTENSION_OFFSET
    );
    assert!(DRIVER_EXTENSION_IO_CLIENT_EXTENSION_OFFSET == 0x28);
    assert!(size_of::<DRIVER_EXTENSION>() == DRIVER_EXTENSION_IO_CLIENT_EXTENSION_OFFSET);
    assert!(
        offset_of!(DriverExtensionExt, service_key_name)
            == offset_of!(DRIVER_EXTENSION, ServiceKeyName)
    );
    assert!(offset_of!(DriverExtensionExt, count) == offset_of!(DRIVER_EXTENSION, Count));
    assert!(
        offset_of!(IoClientExtension, client_identification_address)
            == IO_CLIENT_EXTENSION_CLIENT_ID_OFFSET
    );
    assert!(size_of::<IoClientExtension>() == IO_CLIENT_EXTENSION_LEN);
    assert!(
        offset_of!(HidclassDriverExtension, major_function)
            == HIDCLASS_EXTENSION_MAJOR_FUNCTION_OFFSET
    );
    assert!(size_of::<HidclassDriverExtension>() == HIDCLASS_DRIVER_EXTENSION_LEN);
    assert!(HIDCLASS_DRIVER_EXTENSION_LEN == 304);
    assert!(
        size_of::<IoClientExtension>()
            + offset_of!(HidclassDriverExtension, major_function)
            + IRP_MJ_INTERNAL_DEVICE_CONTROL * size_of::<PDRIVER_DISPATCH>()
            == MINIDRIVER_INTERNAL_IOCTL_SLOT_OFFSET
    );
    assert!(MINIDRIVER_INTERNAL_IOCTL_SLOT_OFFSET == 0xA8);
    assert!(size_of::<PDRIVER_DISPATCH>() == size_of::<*mut c_void>());
    assert!(IRP_MJ_INTERNAL_DEVICE_CONTROL == wdk_sys::IRP_MJ_INTERNAL_DEVICE_CONTROL as usize);
    assert!(IRP_MJ_MAXIMUM_FUNCTION == wdk_sys::IRP_MJ_MAXIMUM_FUNCTION as usize);
    assert!(
        offset_of!(DRIVER_OBJECT, MajorFunction)
            + size_of::<[PDRIVER_DISPATCH; IRP_MJ_MAXIMUM_FUNCTION + 1]>()
            <= size_of::<DRIVER_OBJECT>()
    );
};

impl DetourIo for Filter<'_> {
    type LowerDevice = PDEVICE_OBJECT;
    type DriverObject = PDRIVER_OBJECT;
    type DriverExtension = *mut DriverExtensionExt;
    type ClientExtension = *mut IoClientExtension;

    fn own_device_object_valid(&mut self) -> bool {
        let own = self.ctx.wdm_device_object();
        // SAFETY: `own` is our own WDM device object (from WdfDeviceWdmGetDeviceObject), alive as
        // long as the device; plain field read after the NULL check.
        let valid = !own.is_null() && !unsafe { addr_of!((*own).DriverObject).read() }.is_null();
        if !valid {
            tlg::write_event!(
                PROVIDER,
                "DetourOwnDeviceObjectNull",
                level(Error),
                keyword(KW_DEVICE)
            );
        }
        valid
    }

    fn get_lower_device_object(&mut self) -> Option<PDEVICE_OBJECT> {
        // SAFETY: PASSIVE_LEVEL; our own device object is valid (checked first). A non-NULL result
        // is referenced and released by `dereference_lower_device` on every path (core).
        let lower = unsafe { wdk_sys::ntddk::IoGetLowerDeviceObject(self.ctx.wdm_device_object()) };
        if lower.is_null() {
            tlg::write_event!(
                PROVIDER,
                "DetourLowerDeviceNull",
                level(Error),
                keyword(KW_DEVICE)
            );
            None
        } else {
            Some(lower)
        }
    }

    fn lower_driver_object(&mut self, lower: PDEVICE_OBJECT) -> Option<PDRIVER_OBJECT> {
        // SAFETY: `lower` is a referenced, live DEVICE_OBJECT; plain field read.
        let driver = unsafe { addr_of!((*lower).DriverObject).read() };
        if driver.is_null() {
            tlg::write_event!(
                PROVIDER,
                "DetourLowerDriverNull",
                level(Error),
                keyword(KW_DEVICE)
            );
            None
        } else {
            Some(driver)
        }
    }

    fn driver_extension(&mut self, driver: PDRIVER_OBJECT) -> Option<*mut DriverExtensionExt> {
        // SAFETY: `driver` is the live DRIVER_OBJECT of a referenced device; plain field read.
        let extension = unsafe { addr_of!((*driver).DriverExtension).read() };
        if extension.is_null() {
            tlg::write_event!(
                PROVIDER,
                "DetourDriverExtensionNull",
                level(Error),
                keyword(KW_DEVICE)
            );
            None
        } else {
            Some(extension.cast::<DriverExtensionExt>())
        }
    }

    fn io_client_extension(
        &mut self,
        extension: *mut DriverExtensionExt,
    ) -> Option<*mut IoClientExtension> {
        // SAFETY: `extension` is the driver's DRIVER_EXTENSION; reading the pointer that follows
        // its documented part (offset 0x28) is the undocumented access the C code makes
        // (Detour.c:51), unvalidated as in C [RISK].
        let client = unsafe { addr_of!((*extension).io_client_extension).read() };
        if client.is_null() {
            tlg::write_event!(
                PROVIDER,
                "DetourClientExtensionNull",
                level(Error),
                keyword(KW_DEVICE)
            );
            None
        } else {
            Some(client)
        }
    }

    fn client_id_byte(&mut self, client: *mut IoClientExtension, index: usize) -> u8 {
        // SAFETY: `client` is the first (non-NULL) client extension of the lower driver; plain
        // pointer read at offset 8.
        let id = unsafe { addr_of!((*client).client_identification_address).read() }.cast::<u8>();
        if id.is_null() {
            // P2: C's strncmp would dereference NULL; a NULL client ID is treated as a mismatch
            // (0 never equals 'H', so the comparison stops at index 0).
            return 0;
        }
        // SAFETY: the client ID address is unvalidated, exactly as in C; the core reads it one
        // byte at a time and stops at the first mismatch or after the matching NUL, so this
        // touches exactly the bytes `strncmp(.., 9)` touches.
        unsafe { id.add(index).read_volatile() }
    }

    fn patch_internal_device_control(
        &mut self,
        driver: PDRIVER_OBJECT,
        client: *mut IoClientExtension,
    ) {
        // `(PHIDCLASS_DRIVER_EXTENSION)(cli + 1)` and its `MajorFunction[0x0F]`, i.e. cli + 0xA8.
        // SAFETY: HIDCLASS's client extension is followed by its HIDCLASS_DRIVER_EXTENSION (the
        // client ID matched "HIDCLASS"); the offsets (16 + 32 + 15 * 8 = 0xA8) stay inside it.
        let source = unsafe {
            let extension = client.add(1).cast::<HidclassDriverExtension>();
            (&raw const (*extension).major_function)
                .cast::<*mut c_void>()
                .add(IRP_MJ_INTERNAL_DEVICE_CONTROL)
        };
        // SAFETY: `source` is the pointer-aligned saved dispatch routine (read as a raw pointer,
        // so no function-pointer validity is assumed).
        let routine = unsafe { source.read_volatile() };
        // SAFETY: `driver` is the live DRIVER_OBJECT of the referenced lower device;
        // `MajorFunction` has IRP_MJ_MAXIMUM_FUNCTION + 1 entries, so slot 0x0F is in bounds. One
        // aligned pointer-sized store, nothing saved, never reverted (Detour.c:72, HF-B26).
        unsafe {
            let slot = (&raw mut (*driver).MajorFunction)
                .cast::<*mut c_void>()
                .add(IRP_MJ_INTERNAL_DEVICE_CONTROL);
            slot.write_volatile(routine);
        }
        tlg::write_event!(
            PROVIDER,
            "DetourPatched",
            level(Informational),
            keyword(KW_DEVICE),
            pointer("Driver", &(driver as usize)),
            pointer("Routine", &(routine as usize))
        );
    }

    fn acquire_default_io_target(&mut self) {
        // SAFETY: live device; WdfDeviceGetIoTarget returns its default local I/O target.
        let target =
            unsafe { call_unsafe_wdf_function_binding!(WdfDeviceGetIoTarget, self.ctx.device()) };
        self.ctx.set_hid_io_target(target);
    }

    fn dereference_lower_device(&mut self, lower: PDEVICE_OBJECT) {
        // SAFETY: releases the reference IoGetLowerDeviceObject took (exactly once, core).
        let _reference_count = unsafe { wdk_sys::ntddk::ObfDereferenceObject(lower.cast()) };
    }
}
