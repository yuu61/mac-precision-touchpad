//! `FilterEvtIoIntDeviceControl` (Queue.c:56-117) and the synchronous IOCTL handlers (Hid.c).

use amtptp_core::hid::hidport::HID_DEVICE_ATTRIBUTES_LEN;

use super::Filter;
use super::pipeline::issue_lower_read;
use amtptp_usb_bt::bluetooth::DeviceState;
use amtptp_usb_bt::bluetooth::{
    NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_DEVICE_REQUEST, STATUS_NOT_SUPPORTED,
    STATUS_SUCCESS, nt_success,
};
use amtptp_usb_bt::bluetooth::{check_xfer_length, decode_set_feature, get_feature_report};
use amtptp_usb_bt::bluetooth::{device_attributes, hid_descriptor_for, report_descriptor_for};

pub use amtptp_core::hid::ioctl::InternalIoctl;

/// `FilterEvtIoIntDeviceControl`: handles one internal IOCTL from the default (parallel) queue.
///
/// The request is completed, parked or forwarded exactly once. Synchronous handlers complete it with
/// their status and leave `Information` as they set it (only the descriptor and attribute handlers set
/// it). `output_buffer_length` / `input_buffer_length` are the callback's arguments (the same values
/// the C handlers read with `WdfRequestGetParameters`).
pub(crate) fn dispatch_internal_ioctl(
    io: &mut Filter<'_>,
    state: &DeviceState,
    request: super::UpperRequest,
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
        InternalIoctl::GetString => get_string(io, request),
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
fn copy_descriptor(
    io: &mut Filter<'_>,
    request: super::UpperRequest,
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
/// `Reserved` zeroed; `Information = 32`.
fn get_device_attributes(
    io: &mut Filter<'_>,
    state: &DeviceState,
    request: super::UpperRequest,
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

/// `PtpFilterGetStrings` (Hid.c:142-174) forwarded the request to the PDO, where the detour delivered
/// it to `HidBth`. Without the detour the strings come from the data source: the glue reads the
/// vendor collection's manufacturer, product and serial-number strings when it opens it, and
/// `copy_string` copies the requested one; `Information` is the bytes written.
fn get_string(io: &mut Filter<'_>, request: super::UpperRequest) -> NtStatus {
    match io.copy_string(request) {
        Ok(len) => {
            io.set_information(request, len);
            STATUS_SUCCESS
        }
        Err(status) => status,
    }
}

/// `PtpFilterInputProcessRequest` (Input.c:6-28): park the upper read in `HidReadQueue` (complete it
/// with the error if that fails), then issue one lower read if `DeviceConfigured`.
fn process_read_report(io: &mut Filter<'_>, state: &DeviceState, request: super::UpperRequest) {
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
fn get_feature(
    io: &mut Filter<'_>,
    request: super::UpperRequest,
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
fn set_feature(
    io: &mut Filter<'_>,
    state: &DeviceState,
    request: super::UpperRequest,
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
            if !state.effective_reporting().surface_report {
                io.with_contacts(amtptp_core::domain::ContactTracker::reset);
            }
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
