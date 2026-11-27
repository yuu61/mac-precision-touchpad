//! Device-control dispatch (`AmtPtpDeviceEvtIoDeviceControl`, Queue.c:118-213) and the HID handlers of
//! Hid.c: descriptors, attributes, strings and feature reports.
//!
//! Every handler returns an [`IoctlOutcome`]; the glue then calls `WdfRequestSetInformation` and
//! `WdfRequestComplete` exactly once (the C dispatcher completes every request except `READ_REPORT`).

use ptp_hid::common::{
    PTP_COLLECTION_MOUSE, PTP_COLLECTION_WINDOWS, REPORTID_DEVICE_CAPS, REPORTID_FUNCSWITCH,
    REPORTID_PTPHQA, REPORTID_REPORTMODE, REPORTID_UMAPP_CONF,
};
use ptp_hid::feature::{
    DEVICE_CAPS_REPORT_LEN, DeviceCapsReport, HQA_REPORT_LEN, INPUT_MODE_REPORT_LEN,
    SELECTIVE_REPORT_LEN, SelectiveReportBits, UMAPP_CONF_REPORT_LEN, UmAppConfReport,
    write_hqa_report,
};
use ptp_hid::hidport::{
    HID_DESCRIPTOR_LEN, HID_DEVICE_ATTRIBUTES_LEN, HID_STRING_ID_IMANUFACTURER,
    HID_STRING_ID_IPRODUCT, HID_STRING_ID_ISERIALNUMBER,
};
use ptp_hid::ioctl::{
    IOCTL_HID_GET_DEVICE_ATTRIBUTES, IOCTL_HID_GET_DEVICE_DESCRIPTOR,
    IOCTL_HID_GET_REPORT_DESCRIPTOR, IOCTL_HID_GET_STRING, IOCTL_HID_READ_REPORT,
    IOCTL_UMDF_HID_GET_FEATURE, IOCTL_UMDF_HID_SET_FEATURE,
};

use crate::descriptors::{HidRegistry, REPORT_DESCRIPTOR_LEN, device_attributes};
use crate::io::{ControlPipe, HidRequest, UsbStringSource};
use crate::state::DeviceState;
use crate::status::{
    NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_BUFFER_SIZE, STATUS_INVALID_DEVICE_STATE,
    STATUS_NOT_SUPPORTED, STATUS_SUCCESS, nt_success,
};
use crate::usb::{get_wellspring_mode, set_wellspring_mode};

/// Where `AmtPtpDeviceEvtIoDeviceControl` sends an IOCTL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IoctlRoute {
    /// `IOCTL_HID_GET_DEVICE_DESCRIPTOR` → [`get_hid_descriptor`].
    GetDeviceDescriptor,
    /// `IOCTL_HID_GET_REPORT_DESCRIPTOR` → [`get_report_descriptor`].
    GetReportDescriptor,
    /// `IOCTL_HID_READ_REPORT` → `WdfRequestForwardToIoQueue(Request, InputQueue)`. On success the request
    /// is pending (not completed by the dispatcher); on failure it is completed with the forward status
    /// and Information 0. There is no buffer-size check (Queue.c:260-302).
    ReadReport,
    /// `IOCTL_HID_GET_STRING` → [`get_string`].
    GetString,
    /// `IOCTL_HID_GET_DEVICE_ATTRIBUTES` → [`get_device_attributes`].
    GetDeviceAttributes,
    /// `IOCTL_UMDF_HID_SET_FEATURE` → [`set_feature`].
    SetFeature,
    /// `IOCTL_UMDF_HID_GET_FEATURE` → [`get_feature`].
    GetFeature,
    /// Everything else (`WRITE_REPORT`, ACTIVATE/DEACTIVATE, idle notification, `SET_OUTPUT_REPORT`,
    /// `GET_INPUT_REPORT`, physical descriptor, unknown codes): completed with [`STATUS_NOT_SUPPORTED`] and
    /// Information 0, **not** forwarded ([`IoctlOutcome::NOT_SUPPORTED`]).
    NotSupported,
}

/// The `switch (IoControlCode)` of Queue.c:150-202.
#[must_use]
pub const fn route_ioctl(io_control_code: u32) -> IoctlRoute {
    match io_control_code {
        IOCTL_HID_GET_DEVICE_DESCRIPTOR => IoctlRoute::GetDeviceDescriptor,
        IOCTL_HID_GET_REPORT_DESCRIPTOR => IoctlRoute::GetReportDescriptor,
        IOCTL_HID_READ_REPORT => IoctlRoute::ReadReport,
        IOCTL_HID_GET_STRING => IoctlRoute::GetString,
        IOCTL_HID_GET_DEVICE_ATTRIBUTES => IoctlRoute::GetDeviceAttributes,
        IOCTL_UMDF_HID_SET_FEATURE => IoctlRoute::SetFeature,
        IOCTL_UMDF_HID_GET_FEATURE => IoctlRoute::GetFeature,
        _ => IoctlRoute::NotSupported,
    }
}

/// A `TraceLogging` event the C driver writes on this path (provider `AmtPtpDeviceTraceProvider`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceEvent {
    /// `DeviceIdentificationEvent`, Routine `"AmtPtpGetHidDescriptor"`, `idProduct`, Subtype
    /// `"DeviceDescriptorNotFoundInRegistry"` (Hid.c:168-174).
    HidDescriptorNotFound {
        /// `DeviceDescriptor.idProduct`.
        id_product: u16,
    },
    /// `DeviceIdentificationEvent`, Routine `"AmtPtpEvtDevicePrepareHardware"`, `idProduct`, `idVendor`,
    /// Subtype `"DeviceNotFoundInRegistry"` (Device.c:181-188).
    DeviceNotFound {
        /// `DeviceDescriptor.idProduct`.
        id_product: u16,
        /// `DeviceDescriptor.idVendor`.
        id_vendor: u16,
    },
}

impl TraceEvent {
    /// The `TraceLogging` event name.
    #[must_use]
    pub const fn event_name(self) -> &'static str {
        "DeviceIdentificationEvent"
    }

    /// The `"Routine"` field.
    #[must_use]
    pub const fn routine(self) -> &'static str {
        match self {
            Self::HidDescriptorNotFound { .. } => "AmtPtpGetHidDescriptor",
            Self::DeviceNotFound { .. } => "AmtPtpEvtDevicePrepareHardware",
        }
    }

    /// The `"Subtype"` field.
    #[must_use]
    pub const fn subtype(self) -> &'static str {
        match self {
            Self::HidDescriptorNotFound { .. } => "DeviceDescriptorNotFoundInRegistry",
            Self::DeviceNotFound { .. } => "DeviceNotFoundInRegistry",
        }
    }
}

/// How to complete a device-control request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoctlOutcome {
    /// Completion status.
    pub status: NtStatus,
    /// `WdfRequestSetInformation` value (0 when the C handler never set it).
    pub information: usize,
    /// `TraceLogging` event to write before completing, if any.
    pub event: Option<TraceEvent>,
}

impl IoctlOutcome {
    /// Completion for an unsupported IOCTL: `STATUS_NOT_SUPPORTED`, Information 0.
    pub const NOT_SUPPORTED: Self = Self::status(STATUS_NOT_SUPPORTED);

    /// A completion with `status` and Information 0.
    #[must_use]
    pub const fn status(status: NtStatus) -> Self {
        Self {
            status,
            information: 0,
            event: None,
        }
    }

    /// A successful completion with `information`.
    #[must_use]
    pub const fn success(information: usize) -> Self {
        Self {
            status: STATUS_SUCCESS,
            information,
            event: None,
        }
    }
}

/// Retrieve the output memory, then copy `bytes` into it (descriptor handlers).
fn copy_descriptor<R: HidRequest>(request: &mut R, bytes: &[u8]) -> IoctlOutcome {
    match request.copy_to_output_memory(bytes) {
        Ok(()) => IoctlOutcome::success(bytes.len()),
        Err(status) => IoctlOutcome::status(status),
    }
}

/// `AmtPtpGetHidDescriptor` (Hid.c:7-186).
///
/// 1. Retrieve the output memory (failure: that status).
/// 2. PID switch: known → copy the 9-byte HID descriptor (copy failure: that status, Information 0;
///    success: Information 9). Unknown → `STATUS_INVALID_DEVICE_STATE` plus
///    [`TraceEvent::HidDescriptorNotFound`].
pub fn get_hid_descriptor<R: HidRequest>(state: &DeviceState, request: &mut R) -> IoctlOutcome {
    if let Err(status) = request.output_memory_len() {
        return IoctlOutcome::status(status);
    }
    let id_product = state.id_product();
    match HidRegistry::for_pid(id_product) {
        Some(registry) => {
            let descriptor: &[u8; HID_DESCRIPTOR_LEN] = registry.hid_descriptor();
            copy_descriptor(request, descriptor)
        }
        None => IoctlOutcome {
            status: STATUS_INVALID_DEVICE_STATE,
            information: 0,
            event: Some(TraceEvent::HidDescriptorNotFound { id_product }),
        },
    }
}

/// `AmtPtpGetReportDescriptor` (Hid.c:245-397): as [`get_hid_descriptor`] with the 516-byte report
/// descriptor (Information 516). An unknown PID gives `STATUS_INVALID_DEVICE_STATE` without a trace
/// event (the C event branch is unreachable).
pub fn get_report_descriptor<R: HidRequest>(state: &DeviceState, request: &mut R) -> IoctlOutcome {
    if let Err(status) = request.output_memory_len() {
        return IoctlOutcome::status(status);
    }
    match HidRegistry::for_pid(state.id_product()) {
        Some(registry) => {
            let descriptor: &[u8; REPORT_DESCRIPTOR_LEN] = registry.report_descriptor();
            copy_descriptor(request, descriptor)
        }
        None => IoctlOutcome::status(STATUS_INVALID_DEVICE_STATE),
    }
}

/// `AmtPtpGetDeviceAttribs` (Hid.c:188-243): `WdfRequestRetrieveOutputBuffer(32)` (failure: that
/// status), then write `Size` 32, `VendorID` 0x8910, `ProductID` = `idProduct`, `VersionNumber` 1 into bytes
/// 0..10 and zero `Reserved` (bytes 10..32). Information 32.
pub fn get_device_attributes<R: HidRequest>(state: &DeviceState, request: &mut R) -> IoctlOutcome {
    let buffer = match request.output_buffer(HID_DEVICE_ATTRIBUTES_LEN) {
        Ok(buffer) => buffer,
        Err(status) => return IoctlOutcome::status(status),
    };
    match device_attributes(state.id_product()).write_to(buffer) {
        Ok(()) => IoctlOutcome::success(HID_DEVICE_ATTRIBUTES_LEN),
        // Unreachable when the glue honours `minimum_len`; report what WDF would have reported.
        Err(_) => IoctlOutcome::status(STATUS_BUFFER_TOO_SMALL),
    }
}

/// Returns the requested USB string as NUL-terminated UTF-16LE. Information includes the
/// terminator. A short output buffer receives no part of the string.
pub fn get_string<R: HidRequest, S: UsbStringSource>(
    state: &DeviceState,
    request: &mut R,
    usb: &mut S,
) -> IoctlOutcome {
    let value = match request.input_memory() {
        Ok(input) => match input.first_chunk::<4>() {
            Some(bytes) => u32::from_le_bytes(*bytes),
            None => return IoctlOutcome::status(STATUS_INVALID_BUFFER_SIZE),
        },
        Err(status) => return IoctlOutcome::status(status),
    };
    let string_id = value & 0xffff;
    // C: `(USHORT) languageId` where `languageId = inputValue >> 16` (fits in 16 bits).
    let language_id = (value >> 16) as u16;
    let descriptor = state.device_descriptor();
    // C switch on the ULONG string ID; the HID_STRING_ID_* values are small positive constants.
    let string_index = match string_id {
        id if id == u32::from(HID_STRING_ID_IMANUFACTURER) => descriptor.i_manufacturer,
        id if id == u32::from(HID_STRING_ID_IPRODUCT) => descriptor.i_product,
        id if id == u32::from(HID_STRING_ID_ISERIALNUMBER) => descriptor.i_serial_number,
        _ => return IoctlOutcome::NOT_SUPPORTED,
    };
    let string = match usb.alloc_and_query_string(string_index, language_id) {
        Ok(string) => string,
        Err(status) => return IoctlOutcome::status(status),
    };
    let bytes = string.as_ref();
    let (characters, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return IoctlOutcome::status(STATUS_INVALID_BUFFER_SIZE);
    }
    let string_bytes = characters
        .split(|ch| *ch == [0, 0])
        .next()
        .unwrap_or_default()
        .as_flattened();
    let required = string_bytes.len() + 2;
    let output = match request.output_buffer(required) {
        Ok(buffer) => buffer,
        Err(status) => return IoctlOutcome::status(status),
    };
    let Some(destination) = output.get_mut(..required) else {
        return IoctlOutcome::status(STATUS_BUFFER_TOO_SMALL);
    };
    let (text, terminator) = destination.split_at_mut(string_bytes.len());
    text.copy_from_slice(string_bytes);
    terminator.fill(0);
    IoctlOutcome::success(required)
}

/// `AmtPtpReportFeatures` (Hid.c:711-895): `IOCTL_UMDF_HID_GET_FEATURE`.
///
/// Packet retrieval (`RequestGetHidXferPacketToReadFromDevice`): the report ID is input byte 0 (input
/// retrieval failure: that status; empty input: `STATUS_INVALID_BUFFER_SIZE`), the report buffer is the
/// whole output memory (failure: that status). Then:
///
/// | ID | needs | writes | Information |
/// |---|---|---|---|
/// | 0x07 | 3 | `07 05 00` | 3 |
/// | 0x08 | 257 | Report ID and 256-byte certification blob | 257 |
/// | 0x09 | 4 | `09 pq sq mq` | 4 |
/// | other | - | nothing, `STATUS_NOT_SUPPORTED` | 0 |
///
/// A report buffer shorter than "needs" gives `STATUS_INVALID_BUFFER_SIZE`, Information 0.
pub fn get_feature<R: HidRequest>(state: &DeviceState, request: &mut R) -> IoctlOutcome {
    let report_id = match request.input_memory() {
        Ok(input) => match input.first() {
            Some(&id) => id,
            None => return IoctlOutcome::status(STATUS_INVALID_BUFFER_SIZE),
        },
        Err(status) => return IoctlOutcome::status(status),
    };
    let buffer = match request.output_memory() {
        Ok(buffer) => buffer,
        Err(status) => return IoctlOutcome::status(status),
    };
    let (written, size) = match report_id {
        REPORTID_DEVICE_CAPS => (
            DeviceCapsReport::PTP.write_to(buffer),
            DEVICE_CAPS_REPORT_LEN,
        ),
        REPORTID_PTPHQA => (write_hqa_report(buffer), HQA_REPORT_LEN),
        REPORTID_UMAPP_CONF => {
            let [pressure, single, multiple] = state.qual_levels();
            let report = UmAppConfReport {
                pressure_qualification_level: pressure,
                single_contact_size_qualification_level: single,
                multiple_contact_size_qualification_level: multiple,
            };
            (report.write_to(buffer), UMAPP_CONF_REPORT_LEN)
        }
        _ => return IoctlOutcome::status(STATUS_NOT_SUPPORTED),
    };
    match written {
        Ok(()) => IoctlOutcome::success(size),
        Err(_) => IoctlOutcome::status(STATUS_INVALID_BUFFER_SIZE),
    }
}

/// Applies a UMDF feature report. The HID mapper passes the report ID in the output
/// length and the payload in the input buffer. Only successful reports set Information.
pub fn set_feature<R: HidRequest, C: ControlPipe>(
    state: &DeviceState,
    request: &mut R,
    control: &mut C,
) -> IoctlOutcome {
    let report_id = match request.output_memory_len() {
        // C: `Packet->reportId = (UCHAR) outputBufferLength;` (truncation)
        Ok(length) => length as u8,
        Err(status) => return IoctlOutcome::status(status),
    };
    let input = match request.input_memory() {
        Ok(input) => input,
        Err(status) => return IoctlOutcome::status(status),
    };
    match report_id {
        REPORTID_REPORTMODE => match input.get(1) {
            Some(&mode) => set_input_mode(state, control, mode),
            None => IoctlOutcome::status(STATUS_INVALID_BUFFER_SIZE),
        },
        REPORTID_FUNCSWITCH => {
            let Some(bits) = SelectiveReportBits::from_report(input) else {
                return IoctlOutcome::status(STATUS_INVALID_BUFFER_SIZE);
            };
            state.set_button_report_on(bits.button_report);
            state.set_surface_report_on(bits.surface_report);
            IoctlOutcome::success(SELECTIVE_REPORT_LEN)
        }
        REPORTID_UMAPP_CONF => {
            let Some(&[_, pressure, single, multiple]) = input.first_chunk::<UMAPP_CONF_REPORT_LEN>()
            else {
                return IoctlOutcome::status(STATUS_INVALID_BUFFER_SIZE);
            };
            state.set_qual_levels(pressure, single, multiple);
            IoctlOutcome::success(UMAPP_CONF_REPORT_LEN)
        }
        _ => IoctlOutcome::NOT_SUPPORTED,
    }
}

/// `SET_FEATURE` 0x04 (Hid.c:935-1041).
fn set_input_mode<C: ControlPipe>(state: &DeviceState, control: &mut C, mode: u8) -> IoctlOutcome {
    // P2: the C code dereferences DeviceInfo, which is never NULL once the device has started.
    let Some(cfg) = state.config() else {
        return IoctlOutcome::status(STATUS_INVALID_DEVICE_STATE);
    };
    // The read of the mode register and the switch are one serialized read-modify-write.
    let switch_status = control.serialized(|control| {
        let device_mode_on = match get_wellspring_mode(cfg, control) {
            Ok(enabled) => enabled,
            Err(status) => return status,
        };
        match mode {
            PTP_COLLECTION_MOUSE if device_mode_on => {
                set_wellspring_mode(state, cfg, control, false)
            }
            PTP_COLLECTION_WINDOWS if !device_mode_on => {
                set_wellspring_mode(state, cfg, control, true)
            }
            _ => STATUS_SUCCESS,
        }
    });
    if nt_success(switch_status) {
        IoctlOutcome::success(INPUT_MODE_REPORT_LEN)
    } else {
        IoctlOutcome::status(switch_status)
    }
}
