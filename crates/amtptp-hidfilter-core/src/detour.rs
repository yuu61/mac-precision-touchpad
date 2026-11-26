//! `PtpFilterDetourWindowsHIDStack` (Detour.c:6-86): restores the transport minidriver's own
//! `IRP_MJ_INTERNAL_DEVICE_CONTROL` handler so internal IOCTLs sent to the HID collection PDO reach
//! HidBth/HidUsb directly.
//!
//! The pointer chasing into undocumented structures (`Hac.h`) is the glue's job ([`DetourIo`], with
//! the offsets below). The checks, their order, the reference balance and the `strncmp` semantics are
//! here. The patch is global and never reverted (HF-B26, preserved).

use crate::device::DeviceState;
use crate::status::{NtStatus, STATUS_SUCCESS, STATUS_UNSUCCESSFUL};

/// `IRP_MJ_INTERNAL_DEVICE_CONTROL` (0x0F): the only `MajorFunction` slot patched.
pub const IRP_MJ_INTERNAL_DEVICE_CONTROL: usize = 0x0F;
/// `IRP_MJ_MAXIMUM_FUNCTION` (0x1B): `MajorFunction` arrays have 28 entries.
pub const IRP_MJ_MAXIMUM_FUNCTION: usize = 0x1B;
/// `offsetof(DRIVER_EXTENSION_EXT, IoClientExtension)` on LP64 (Hac.h:25-31): 0x28.
pub const DRIVER_EXTENSION_IO_CLIENT_EXTENSION_OFFSET: usize = 0x28;
/// `offsetof(IO_CLIENT_EXTENSION, ClientIdentificationAddress)` (Hac.h:19-23): 8.
pub const IO_CLIENT_EXTENSION_CLIENT_ID_OFFSET: usize = 8;
/// `sizeof(IO_CLIENT_EXTENSION)`: 16. The `HIDCLASS_DRIVER_EXTENSION` follows it
/// (`(PHIDCLASS_DRIVER_EXTENSION)(cli + 1)`).
pub const IO_CLIENT_EXTENSION_LEN: usize = 16;
/// `offsetof(HIDCLASS_DRIVER_EXTENSION, MajorFunction)` (Hac.h:7-17): 32.
pub const HIDCLASS_EXTENSION_MAJOR_FUNCTION_OFFSET: usize = 32;
/// `sizeof(HIDCLASS_DRIVER_EXTENSION)` on LP64: 304.
pub const HIDCLASS_DRIVER_EXTENSION_LEN: usize = 304;
/// Offset of the saved minidriver `MajorFunction[0x0F]` from the `IO_CLIENT_EXTENSION`:
/// 16 + 32 + 15 * 8 = 0xA8. The glue reads the function pointer there.
pub const MINIDRIVER_INTERNAL_IOCTL_SLOT_OFFSET: usize = IO_CLIENT_EXTENSION_LEN
    + HIDCLASS_EXTENSION_MAJOR_FUNCTION_OFFSET
    + IRP_MJ_INTERNAL_DEVICE_CONTROL * POINTER_LEN;
const POINTER_LEN: usize = 8;
const _: () = assert!(MINIDRIVER_INTERNAL_IOCTL_SLOT_OFFSET == 0xA8);

/// `HID_CLASS_EXTENSION_LITERAL_ID` `"HIDCLASS"` with its NUL (`sizeof` = 9 bytes are compared).
pub const HIDCLASS_CLIENT_ID: [u8; 9] = *b"HIDCLASS\0";

/// `strncmp("HIDCLASS", client_id, 9) == 0`, reading the client ID one byte at a time through `read`
/// and stopping at the first mismatch or after the matching NUL, so it touches exactly the bytes
/// `strncmp` touches (`HIDCLASS\0junk` matches; `HIDCLASSX` does not).
pub fn client_id_matches<F: FnMut(usize) -> u8>(mut read: F) -> bool {
    for (index, expected) in HIDCLASS_CLIENT_ID.iter().enumerate() {
        let actual = read(index);
        if actual != *expected {
            return false;
        }
        if actual == 0 {
            return true;
        }
    }
    true
}

/// The pointer reads and writes of the detour, implemented by the glue with raw pointers. Each method
/// documents the exact C expression. Pointer types are the glue's (`PDEVICE_OBJECT` etc.).
pub trait DetourIo {
    /// The lower `PDEVICE_OBJECT` (referenced by `IoGetLowerDeviceObject`).
    type LowerDevice: Copy;
    /// Its `PDRIVER_OBJECT`.
    type DriverObject: Copy;
    /// `DriverObject->DriverExtension`.
    type DriverExtension: Copy;
    /// The `PIO_CLIENT_EXTENSION`.
    type ClientExtension: Copy;

    /// `ctx->WdmDeviceObject != NULL && ctx->WdmDeviceObject->DriverObject != NULL`.
    fn own_device_object_valid(&mut self) -> bool;
    /// `IoGetLowerDeviceObject(ctx->WdmDeviceObject)`; `None` if NULL. A returned object holds a
    /// reference that [`DetourIo::dereference_lower_device`] releases.
    fn get_lower_device_object(&mut self) -> Option<Self::LowerDevice>;
    /// `lower->DriverObject`; `None` if NULL.
    fn lower_driver_object(&mut self, lower: Self::LowerDevice) -> Option<Self::DriverObject>;
    /// `driver->DriverExtension`; `None` if NULL.
    fn driver_extension(&mut self, driver: Self::DriverObject) -> Option<Self::DriverExtension>;
    /// `((PDRIVER_EXTENSION_EXT)extension)->IoClientExtension` (at
    /// [`DRIVER_EXTENSION_IO_CLIENT_EXTENSION_OFFSET`]); `None` if NULL. Only the first client extension
    /// is examined; `NextExtension` is never walked.
    fn io_client_extension(
        &mut self,
        extension: Self::DriverExtension,
    ) -> Option<Self::ClientExtension>;
    /// Byte `index` of the string at `client->ClientIdentificationAddress` (unvalidated pointer, as in
    /// C). Called for consecutive indexes from 0 until [`client_id_matches`] decides.
    fn client_id_byte(&mut self, client: Self::ClientExtension, index: usize) -> u8;
    /// `driver->MajorFunction[IRP_MJ_INTERNAL_DEVICE_CONTROL] = *(PDRIVER_DISPATCH*)((PUCHAR)client +
    /// MINIDRIVER_INTERNAL_IOCTL_SLOT_OFFSET)` (one pointer-sized store; nothing saves the old value).
    fn patch_internal_device_control(
        &mut self,
        driver: Self::DriverObject,
        client: Self::ClientExtension,
    );
    /// `ctx->HidIoTarget = WdfDeviceGetIoTarget(Device)`.
    fn acquire_default_io_target(&mut self);
    /// `ObDereferenceObject(lower)`.
    fn dereference_lower_device(&mut self, lower: Self::LowerDevice);
}

/// Applies the detour. Every check failure returns `STATUS_UNSUCCESSFUL`; once
/// `IoGetLowerDeviceObject` succeeded its reference is released on every path. On success sets
/// `IsHidIoDetourCompleted` and the I/O target. Idempotent.
pub fn detour_hid_stack<P: DetourIo>(io: &mut P, state: &DeviceState) -> NtStatus {
    if !io.own_device_object_valid() {
        return STATUS_UNSUCCESSFUL;
    }
    let Some(lower) = io.get_lower_device_object() else {
        return STATUS_UNSUCCESSFUL;
    };
    let status = patch(io, state, lower);
    io.dereference_lower_device(lower);
    status
}

fn patch<P: DetourIo>(io: &mut P, state: &DeviceState, lower: P::LowerDevice) -> NtStatus {
    let Some(driver) = io.lower_driver_object(lower) else {
        return STATUS_UNSUCCESSFUL;
    };
    let Some(extension) = io.driver_extension(driver) else {
        return STATUS_UNSUCCESSFUL;
    };
    let Some(client) = io.io_client_extension(extension) else {
        return STATUS_UNSUCCESSFUL;
    };
    if !client_id_matches(|index| io.client_id_byte(client, index)) {
        return STATUS_UNSUCCESSFUL;
    }
    io.patch_internal_device_control(driver, client);
    state.set_detour_completed();
    io.acquire_default_io_target();
    state.set_io_target_set();
    STATUS_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(id: &[u8]) -> (bool, usize) {
        let mut reads = 0;
        let ok = client_id_matches(|i| {
            reads += 1;
            id.get(i).copied().unwrap_or(0)
        });
        (ok, reads)
    }

    #[test]
    fn strncmp_semantics() {
        assert_eq!(matches(b"HIDCLASS\0"), (true, 9));
        assert_eq!(matches(b"HIDCLASS\0junk"), (true, 9));
        assert_eq!(matches(b"HIDCLASSX"), (false, 9));
        assert_eq!(matches(b"XIDCLASS\0"), (false, 1));
        assert_eq!(matches(b"hidclass\0"), (false, 1));
        assert_eq!(matches(b"HIDCLAS\0"), (false, 8));
        assert_eq!(matches(b"\0"), (false, 1));
    }
}
