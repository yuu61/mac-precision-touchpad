//! SPI model discovery and device settings read during `PrepareHardware`.

use core::ptr::null_mut;

use amtptp_core::spi::device::{
    DESIRED_REPORT_TYPE_VALUE_LEN, DESIRED_REPORT_TYPE_VALUE_NAME, PARAMETERS_KEY_ACCESS,
    desired_report_type_value,
};
use amtptp_core::spi::wire::{LOWER_ATTRIBUTES_IOCTL, LOWER_ATTRIBUTES_LEN};
use tracelogging as tlg;
use wdf_support::init::{memory_descriptor_init_buffer, sync_send_options};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE, STATUS_SUCCESS};
use wdk_sys::{
    NTSTATUS, ULONG, UNICODE_STRING, WDF_NO_OBJECT_ATTRIBUTES, WDFCMRESLIST, WDFDEVICE, WDFKEY,
    call_unsafe_wdf_function_binding,
};

use super::device_context;
use crate::{PROVIDER, keyword};

/// The 32-byte `HID_DEVICE_ATTRIBUTES` output buffer of the identification request, aligned like the C
/// struct (its first field is a `ULONG`).
#[repr(C, align(8))]
struct LowerAttributesBuffer([u8; LOWER_ATTRIBUTES_LEN]);

/// `LOWER_ATTRIBUTES_LEN` (32) as the `ULONG` length of a memory descriptor.
const LOWER_ATTRIBUTES_DESCRIPTOR_LEN: ULONG = {
    assert!(LOWER_ATTRIBUTES_LEN <= ULONG::MAX as usize);
    LOWER_ATTRIBUTES_LEN as ULONG
};

/// `AmtPtpEvtDevicePrepareHardware` (Device.c:166-317), `PASSIVE_LEVEL` (non-paged, as in C).
///
/// 1. Synchronous lower `IOCTL_HID_GET_DEVICE_ATTRIBUTES` into a zeroed 32-byte buffer (no timeout:
///    SPI-B23, preserved). Failure returns that status.
/// 2. Store VID/PID/version, then look the device up in the config table: no match returns
///    `STATUS_NOT_FOUND` (the IDs stay stored, as in C).
/// 3. Turn both selective-reporting switches on (not in C, which never used them).
/// 4. Read `DesiredReportType` from the driver's Parameters key (zero-initialized value buffer: SPI-B24).
/// 5. Return `STATUS_SUCCESS` whatever the registry step did.
pub(super) unsafe extern "C" fn evt_device_prepare_hardware(
    device: WDFDEVICE,
    _resource_list: WDFCMRESLIST,
    _resource_list_translated: WDFCMRESLIST,
) -> NTSTATUS {
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        // C: the (unreachable) `pDeviceContext == NULL` branch, Device.c:197-207.
        return STATUS_INVALID_DEVICE_STATE;
    };

    let mut attributes = LowerAttributesBuffer([0; LOWER_ATTRIBUTES_LEN]);
    let mut output = memory_descriptor_init_buffer(
        attributes.0.as_mut_ptr().cast(),
        LOWER_ATTRIBUTES_DESCRIPTOR_LEN,
    );
    // Not in C (no options): bound the wait for a lower driver that never completes (B-3).
    let mut options = sync_send_options(0);
    // SAFETY: PASSIVE_LEVEL; valid local I/O target; `output` describes the
    // 32-byte local buffer, which outlives this synchronous call (also on a
    // timeout: the framework waits for the cancelled request); no request,
    // input or bytes-returned (NULL, as in C).
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoTargetSendInternalIoctlSynchronously,
            context.spi_trackpad_io_target.get(),
            null_mut(),
            LOWER_ATTRIBUTES_IOCTL,
            null_mut(),
            &raw mut output,
            &raw mut options,
            null_mut()
        )
    };

    let mut config = context.config.load();
    let identified = config.identify(status, &attributes.0);
    // C writes HidVendorID/HidProductID/HidVersionNumber before the table lookup, so they are stored
    // even when the lookup fails (identify leaves `config` unchanged when the lower request failed).
    context.config.store(&config);
    if let Err(status) = identified {
        tlg::write_event!(
            PROVIDER,
            "PrepareHardwareFailed",
            level(Error),
            keyword(keyword::DRIVER),
            win_ntstatus("Status", &status),
            u16_hex("VendorId", &config.hid_vendor_id),
            u16_hex("ProductId", &config.hid_product_id)
        );
        return status;
    }

    // Selective reporting is not kept across a (re)start: report contacts and the button again.
    context.runtime.ptp_settings().reset_selective_reporting();
    // SAFETY: live device context at PASSIVE_LEVEL.
    unsafe { context.with_contacts(|_, contacts| contacts.reset()) };

    // SAFETY: valid device handle, PASSIVE_LEVEL.
    let desired_report_type = unsafe { query_desired_report_type(device) };
    config.apply_desired_report_type(desired_report_type);
    context.config.store(&config);

    tlg::write_event!(
        PROVIDER,
        "PrepareHardware",
        level(Informational),
        keyword(keyword::DRIVER),
        u16_hex("VendorId", &config.hid_vendor_id),
        u16_hex("ProductId", &config.hid_product_id),
        u16_hex("VersionNumber", &config.hid_version_number),
        u32("ReportType", &config.report_type.0)
    );
    STATUS_SUCCESS
}

/// `ASCII_NAME` as a NUL-terminated UTF-16 array of `N` code units (`N` = length + 1), for a
/// `DECLARE_CONST_UNICODE_STRING`. Evaluated only in a constant initializer, where a wrong `N` or a
/// non-ASCII byte is a compile-time error.
const fn ascii_to_utf16z<const N: usize>(ascii_name: &str) -> [u16; N] {
    let mut bytes = ascii_name.as_bytes();
    assert!(
        bytes.len() + 1 == N,
        "N must be the name length plus the NUL"
    );
    let mut out = [0_u16; N];
    let mut units = out.as_mut_slice();
    while let (Some((&byte, rest)), Some((unit, tail))) =
        (bytes.split_first(), units.split_first_mut())
    {
        assert!(byte.is_ascii(), "ASCII names only");
        // ASCII byte -> the same UTF-16 code unit.
        *unit = byte as u16;
        bytes = rest;
        units = tail;
    }
    out
}

/// Number of UTF-16 code units of `L"DesiredReportType"` including the NUL.
const DESIRED_REPORT_TYPE_NAME_UNITS: usize = DESIRED_REPORT_TYPE_VALUE_NAME.len() + 1;
/// `L"DesiredReportType"`.
static DESIRED_REPORT_TYPE_NAME: [u16; DESIRED_REPORT_TYPE_NAME_UNITS] =
    ascii_to_utf16z(DESIRED_REPORT_TYPE_VALUE_NAME);
/// `DECLARE_CONST_UNICODE_STRING`: `Length` = bytes without the NUL, `MaximumLength` = with it.
const DESIRED_REPORT_TYPE_NAME_LENGTH: u16 = {
    let bytes = (DESIRED_REPORT_TYPE_NAME_UNITS - 1) * 2;
    assert!(bytes + 2 <= u16::MAX as usize);
    bytes as u16
};
/// `sizeof(ULONG)`, the `ValueLength` of the query.
const DESIRED_REPORT_TYPE_QUERY_LEN: ULONG = {
    assert!(DESIRED_REPORT_TYPE_VALUE_LEN == core::mem::size_of::<u32>());
    DESIRED_REPORT_TYPE_VALUE_LEN as ULONG
};

/// `PrepareHardware` step 4 (Device.c:268-303): `WdfDriverOpenParametersRegistryKey(KEY_READ)` on the
/// driver (`...\Services\AmtPtpDeviceSpiKm\Parameters`, driver-wide), `WdfRegistryQueryValue(
/// "DesiredReportType", 4 bytes)`, `WdfRegistryClose`. Returns the value when the query succeeded.
///
/// # Safety
/// `device` is a live WDFDEVICE; `PASSIVE_LEVEL`.
unsafe fn query_desired_report_type(device: WDFDEVICE) -> Option<u32> {
    // SAFETY: valid device handle.
    let driver = unsafe { call_unsafe_wdf_function_binding!(WdfDeviceGetDriver, device) };
    let mut key: WDFKEY = null_mut();
    // SAFETY: valid driver handle, PASSIVE_LEVEL, no key attributes, valid out-parameter.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverOpenParametersRegistryKey,
            driver,
            PARAMETERS_KEY_ACCESS,
            WDF_NO_OBJECT_ATTRIBUTES,
            &raw mut key
        )
    };
    if !NT_SUCCESS(status) {
        return None;
    }

    let name = UNICODE_STRING {
        Length: DESIRED_REPORT_TYPE_NAME_LENGTH,
        MaximumLength: DESIRED_REPORT_TYPE_NAME_LENGTH + 2,
        // WDF only reads the name (C: DECLARE_CONST_UNICODE_STRING).
        Buffer: DESIRED_REPORT_TYPE_NAME.as_ptr().cast_mut(),
    };
    // Zero-initialized (SPI-B24): a 1-3 byte value leaves defined zero upper bytes.
    let mut value: u32 = 0;
    let mut length: ULONG = 0;
    let mut value_type: ULONG = 0;
    // SAFETY: `key` is the open key; `name` points at a 'static UTF-16 string;
    // `value` is a 4-byte writable buffer matching ValueLength; the two
    // out-parameters are valid.
    let query_status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRegistryQueryValue,
            key,
            &raw const name,
            DESIRED_REPORT_TYPE_QUERY_LEN,
            (&raw mut value).cast(),
            &raw mut length,
            &raw mut value_type
        )
    };
    // SAFETY: `key` was opened above and is closed exactly once.
    unsafe { call_unsafe_wdf_function_binding!(WdfRegistryClose, key) };

    // The registry wrote raw little-endian bytes into `value`; the native byte order of the u32 is
    // those bytes again.
    desired_report_type_value(query_status, value.to_le_bytes())
}
