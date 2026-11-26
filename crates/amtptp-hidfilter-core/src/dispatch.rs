//! `FilterEvtIoIntDeviceControl` (Queue.c:56-117) and the synchronous IOCTL handlers (Hid.c).

use ptp_hid::hidport::HID_DEVICE_ATTRIBUTES_LEN;
use ptp_hid::ioctl::{
    IOCTL_HID_GET_DEVICE_ATTRIBUTES, IOCTL_HID_GET_DEVICE_DESCRIPTOR, IOCTL_HID_GET_FEATURE,
    IOCTL_HID_GET_REPORT_DESCRIPTOR, IOCTL_HID_GET_STRING, IOCTL_HID_READ_REPORT,
    IOCTL_HID_SET_FEATURE,
};

use crate::descriptor::{device_attributes, hid_descriptor_for, report_descriptor_for};
use crate::device::DeviceState;
use crate::feature::{check_xfer_length, decode_set_feature, get_feature_report};
use crate::io::FilterIo;
use crate::pipeline::issue_lower_read;
use crate::status::{
    NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_DEVICE_STATE,
    STATUS_NOT_SUPPORTED, STATUS_SUCCESS, nt_success,
};

/// Which handler an internal IOCTL code goes to (Queue.c:76-110). Every other code, including the
/// explicitly listed WRITE_REPORT, ACTIVATE/DEACTIVATE, idle notification and the UMDF codes, is
/// completed with `STATUS_NOT_SUPPORTED`; nothing is passed down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InternalIoctl {
    /// `IOCTL_HID_GET_DEVICE_DESCRIPTOR` (0x000B0003).
    GetDeviceDescriptor,
    /// `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (0x000B0027).
    GetDeviceAttributes,
    /// `IOCTL_HID_GET_REPORT_DESCRIPTOR` (0x000B0007).
    GetReportDescriptor,
    /// `IOCTL_HID_GET_STRING` (0x000B0013): forwarded to the PDO.
    GetString,
    /// `IOCTL_HID_READ_REPORT` (0x000B000B): parked in `HidReadQueue`.
    ReadReport,
    /// `IOCTL_HID_GET_FEATURE` (0x000B0192).
    GetFeature,
    /// `IOCTL_HID_SET_FEATURE` (0x000B0191).
    SetFeature,
    /// Anything else: `STATUS_NOT_SUPPORTED`.
    NotSupported,
}

impl InternalIoctl {
    /// Routes an `IoControlCode`.
    pub const fn from_code(io_control_code: u32) -> Self {
        match io_control_code {
            IOCTL_HID_GET_DEVICE_DESCRIPTOR => Self::GetDeviceDescriptor,
            IOCTL_HID_GET_DEVICE_ATTRIBUTES => Self::GetDeviceAttributes,
            IOCTL_HID_GET_REPORT_DESCRIPTOR => Self::GetReportDescriptor,
            IOCTL_HID_GET_STRING => Self::GetString,
            IOCTL_HID_READ_REPORT => Self::ReadReport,
            IOCTL_HID_GET_FEATURE => Self::GetFeature,
            IOCTL_HID_SET_FEATURE => Self::SetFeature,
            _ => Self::NotSupported,
        }
    }
}

/// `FilterEvtIoIntDeviceControl`: handles one internal IOCTL from the default (parallel) queue.
///
/// The request is completed, parked or forwarded exactly once. Synchronous handlers complete it with
/// their status and leave `Information` as they set it (only the descriptor and attribute handlers set
/// it). `output_buffer_length` / `input_buffer_length` are the callback's arguments (the same values
/// the C handlers read with `WdfRequestGetParameters`).
pub fn dispatch_internal_ioctl<P: FilterIo>(
    io: &mut P,
    state: &DeviceState,
    request: P::Request,
    output_buffer_length: usize,
    input_buffer_length: usize,
    io_control_code: u32,
) {
    let status = match InternalIoctl::from_code(io_control_code) {
        InternalIoctl::GetDeviceDescriptor => {
            copy_descriptor(io, request, hid_descriptor_for(state.product_id()))
        }
        InternalIoctl::GetReportDescriptor => {
            copy_descriptor(io, request, report_descriptor_for(state.product_id()))
        }
        InternalIoctl::GetDeviceAttributes => get_device_attributes(io, state, request),
        InternalIoctl::GetString => {
            forward_get_string(io, state, request);
            return;
        }
        InternalIoctl::ReadReport => {
            process_read_report(io, state, request);
            return;
        }
        InternalIoctl::GetFeature => get_feature(io, request, output_buffer_length),
        InternalIoctl::SetFeature => set_feature(io, state, request, input_buffer_length),
        InternalIoctl::NotSupported => STATUS_NOT_SUPPORTED,
    };
    io.complete(request, status);
}

/// `PtpFilterGetHidDescriptor` / `PtpFilterGetReportDescriptor` (Hid.c:6-56, 91-140): retrieve the
/// output memory first (its failure wins), then select by product ID, then copy and set
/// `Information` to the descriptor length.
fn copy_descriptor<P: FilterIo>(
    io: &mut P,
    request: P::Request,
    descriptor: Option<&'static [u8]>,
) -> NtStatus {
    let memory = match io.retrieve_output_memory(request) {
        Ok(m) => m,
        Err(status) => return status,
    };
    let Some(bytes) = descriptor else {
        return STATUS_NOT_SUPPORTED;
    };
    let status = io.copy_to_memory(memory, bytes);
    if !nt_success(status) {
        return status;
    }
    io.set_information(request, bytes.len());
    STATUS_SUCCESS
}

/// `PtpFilterGetDeviceAttribs` (Hid.c:58-89): `Size`, VID 0x8910, the real PID and version 0x0100;
/// `Reserved` untouched (HF-B22); `Information = 32`.
fn get_device_attributes<P: FilterIo>(
    io: &mut P,
    state: &DeviceState,
    request: P::Request,
) -> NtStatus {
    let attributes = device_attributes(state.product_id());
    match io.retrieve_output_buffer(request, HID_DEVICE_ATTRIBUTES_LEN) {
        Ok(buffer) => {
            if attributes.write_to(buffer).is_err() {
                // Unreachable: the framework guarantees the minimum length.
                return STATUS_BUFFER_TOO_SMALL;
            }
        }
        Err(status) => return status,
    }
    io.set_information(request, HID_DEVICE_ATTRIBUTES_LEN);
    STATUS_SUCCESS
}

/// `PtpFilterGetStrings` (Hid.c:142-174): forward the request unchanged to the PDO with
/// send-and-forget; if the send fails, complete it with `WdfRequestGetStatus`.
///
/// P2 (HF-B25): before the detour has set the I/O target, C would send to a NULL handle; the port
/// completes the request with `STATUS_INVALID_DEVICE_STATE` instead (unreachable in practice).
fn forward_get_string<P: FilterIo>(io: &mut P, state: &DeviceState, request: P::Request) {
    if !state.io_target_set() {
        io.complete(request, STATUS_INVALID_DEVICE_STATE);
        return;
    }
    io.format_using_current_type(request);
    if !io.send_and_forget(request) {
        let status = io.request_status(request);
        io.complete(request, status);
    }
}

/// `PtpFilterInputProcessRequest` (Input.c:6-28): park the upper read in `HidReadQueue` (complete it
/// with the error if that fails), then issue one lower read if `DeviceConfigured`.
fn process_read_report<P: FilterIo>(io: &mut P, state: &DeviceState, request: P::Request) {
    let status = io.forward_to_read_queue(request);
    if !nt_success(status) {
        io.complete(request, status);
        return;
    }
    if state.device_configured() {
        issue_lower_read(io, state);
    }
}

/// `PtpFilterGetHidFeatures` (Hid.c:176-262). `Information` is never set (HF-B21).
fn get_feature<P: FilterIo>(
    io: &mut P,
    request: P::Request,
    output_buffer_length: usize,
) -> NtStatus {
    if let Err(status) = check_xfer_length(output_buffer_length) {
        return status;
    }
    match io.get_feature_packet(request) {
        Some(packet) => get_feature_report(packet.report_id, packet.report_buffer),
        None => STATUS_INVALID_DEVICE_REQUEST,
    }
}

/// `PtpFilterSetHidFeatures` (Hid.c:264-345). `Information` is never set (HF-B21).
fn set_feature<P: FilterIo>(
    io: &mut P,
    state: &DeviceState,
    request: P::Request,
    input_buffer_length: usize,
) -> NtStatus {
    if let Err(status) = check_xfer_length(input_buffer_length) {
        return status;
    }
    let decoded = match io.set_feature_packet(request) {
        Some(packet) => decode_set_feature(packet.report_id, packet.report_buffer),
        None => return STATUS_INVALID_DEVICE_REQUEST,
    };
    match decoded {
        Ok(change) => {
            state.apply_feature_change(change);
            STATUS_SUCCESS
        }
        Err(status) => status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing() {
        assert_eq!(
            InternalIoctl::from_code(0x000B_0003),
            InternalIoctl::GetDeviceDescriptor
        );
        assert_eq!(
            InternalIoctl::from_code(0x000B_000F),
            InternalIoctl::NotSupported
        );
        assert_eq!(
            InternalIoctl::from_code(0x000B_01A2),
            InternalIoctl::NotSupported
        );
        assert_eq!(
            InternalIoctl::from_code(0x000B_0191),
            InternalIoctl::SetFeature
        );
        assert_eq!(
            InternalIoctl::from_code(0x000B_0192),
            InternalIoctl::GetFeature
        );
    }
}
