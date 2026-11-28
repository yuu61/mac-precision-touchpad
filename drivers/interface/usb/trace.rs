//! Trace keywords and helpers. The `TraceLogging` provider itself ([`crate::PROVIDER`], replacing the C WPP
//! tracing of `Trace.h`) is defined in the crate root: `define_provider!` symbols are private to their
//! module, and children can use the root's.
//!
//! The provider id is the C WPP control GUID `AmtPtpDeviceUsbKmTraceGuid`
//! {b409cf52-f41a-4193-98c2-bad95204203b}, so existing trace sessions keep the same provider GUID.
//! The keywords reuse the bit values of the C WPP flags (`TRACE_DRIVER` 0x1, `TRACE_DEVICE` 0x2,
//! `TRACE_QUEUE` 0x4, `TRACE_INPUT` 0x8). The payloads are self-describing `TraceLogging` events, not WPP
//! messages (spec §1.2: tracing is not behavior).

use wdk_sys::{_WDF_POWER_DEVICE_STATE, WDF_POWER_DEVICE_STATE};

/// `TRACE_DRIVER`: driver object, PnP/power callbacks.
pub(crate) const KW_DRIVER: u64 = 0x1;
/// `TRACE_DEVICE`: USB device, interface, pipes, control transfers.
pub(crate) const KW_DEVICE: u64 = 0x2;
/// `TRACE_QUEUE`: queues and request dispatch.
pub(crate) const KW_QUEUE: u64 = 0x4;
/// `TRACE_INPUT`: continuous-reader packets and input reports.
pub(crate) const KW_INPUT: u64 = 0x8;

/// `DbgDevicePowerString` (DebugUtils.c:3-29): the name of a `WDF_POWER_DEVICE_STATE`, for traces.
pub(crate) const fn power_state_name(state: WDF_POWER_DEVICE_STATE) -> &'static str {
    match state {
        _WDF_POWER_DEVICE_STATE::WdfPowerDeviceInvalid => "WdfPowerDeviceInvalid",
        _WDF_POWER_DEVICE_STATE::WdfPowerDeviceD0 => "WdfPowerDeviceD0",
        _WDF_POWER_DEVICE_STATE::WdfPowerDeviceD1 => "WdfPowerDeviceD1",
        _WDF_POWER_DEVICE_STATE::WdfPowerDeviceD2 => "WdfPowerDeviceD2",
        _WDF_POWER_DEVICE_STATE::WdfPowerDeviceD3 => "WdfPowerDeviceD3",
        _WDF_POWER_DEVICE_STATE::WdfPowerDeviceD3Final => "WdfPowerDeviceD3Final",
        _WDF_POWER_DEVICE_STATE::WdfPowerDevicePrepareForHibernation => {
            "WdfPowerDevicePrepareForHibernation"
        }
        _WDF_POWER_DEVICE_STATE::WdfPowerDeviceMaximum => "WdfPowerDeviceMaximum",
        _ => "UnKnown Device Power State",
    }
}
