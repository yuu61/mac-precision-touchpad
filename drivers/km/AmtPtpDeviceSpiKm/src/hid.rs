//! The HID IOCTL handlers (C: Hid.c). All of them run at IRQL <= `DISPATCH_LEVEL` from the internal-IOCTL
//! dispatcher, so none is pageable (the C `PAGED_CODE()` and `_IRQL_requires_(PASSIVE_LEVEL)` annotations
//! were wrong: SPI-B22, SPI-B25).
//!
//! The handlers that return a status leave the completion to the dispatcher, as in C; the Information
//! value they set (or not) is what the request completes with.

use core::ptr::copy_nonoverlapping;

use amtptp_spikm_core::descriptors::{
    DEVICE_ATTRIBUTES_INFORMATION, device_attributes, hid_descriptor_for, report_descriptor_for,
};
use amtptp_spikm_core::feature::{SET_FEATURE_READ_LEN, check_xfer_packet, get_feature};
use ptp_hid::hidport::HID_DEVICE_ATTRIBUTES_LEN;
use tracelogging as tlg;
use wdf_support::hid as wdf_hid;
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_REQUEST, STATUS_SUCCESS};
use wdf_support::request::Request;
use wdk_sys::{HID_XFER_PACKET, NTSTATUS, ULONG_PTR};

use crate::device::DeviceContext;
use crate::{PROVIDER, keyword};

const _: () = assert!(
    core::mem::size_of::<HID_XFER_PACKET>() == amtptp_spikm_core::feature::HID_XFER_PACKET_LEN
);

/// Copies a descriptor to offset 0 of the request's output memory, like the C handlers:
/// `WdfRequestRetrieveOutputMemory` first (failure returns its status, before the descriptor is even
/// chosen), then the descriptor choice, then `WdfMemoryCopyFromBuffer` (a short buffer gives the
/// framework's `STATUS_BUFFER_TOO_SMALL`), then Information = the descriptor length.
fn copy_descriptor(
    request: &Request,
    choose: impl FnOnce() -> Result<&'static [u8], NTSTATUS>,
    what: &[u8],
) -> NTSTATUS {
    // SAFETY: the dispatcher owns the live request for the duration of this handler.
    let memory = match unsafe { wdf_hid::retrieve_output_memory(request.as_raw()) } {
        Ok(memory) => memory,
        Err(status) => {
            trace_hid_failure(b"WdfRequestRetrieveOutputMemory", what, status);
            return status;
        }
    };

    let descriptor = match choose() {
        Ok(descriptor) => descriptor,
        Err(status) => {
            trace_hid_failure(b"DescriptorNotFound", what, status);
            return status;
        }
    };
    // SAFETY: WDF returned the pending request's writable output memory. It cannot overlap the descriptor.
    let status = unsafe { wdf_hid::copy_to_output_memory(memory, descriptor) };
    if !NT_SUCCESS(status) {
        trace_hid_failure(b"WdfMemoryCopyFromBuffer", what, status);
        return status;
    }
    // Lossless: usize and ULONG_PTR are both 64-bit here.
    request.set_information(descriptor.len() as ULONG_PTR);
    STATUS_SUCCESS
}

/// Error trace for the HID handlers.
fn trace_hid_failure(step: &[u8], what: &[u8], status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "HidRequestFailed",
        level(Error),
        keyword(keyword::DRIVER),
        str8("Request", what),
        str8("Step", step),
        win_ntstatus("Status", &status)
    );
}

/// `AmtPtpGetHidDescriptor` (Hid.c:107-304), `IOCTL_HID_GET_DEVICE_DESCRIPTOR`: the 9-byte HID
/// descriptor `09 21 00 01 00 01 22 f5 01`, Information 9; a PID outside the family switch gives
/// `STATUS_NOT_FOUND` (unreachable after a successful `PrepareHardware`).
pub(crate) fn get_hid_descriptor(context: &DeviceContext, request: &Request) -> NTSTATUS {
    copy_descriptor(
        request,
        || hid_descriptor_for(&context.config.load()),
        b"GetHidDescriptor",
    )
}

/// `AmtPtpGetReportDescriptor` (Hid.c:364-531), `IOCTL_HID_GET_REPORT_DESCRIPTOR`: the family's 501-byte
/// Touch Pad or Touch Screen report descriptor (by `ReportType`), Information 501; an unknown PID or
/// report type gives `STATUS_NOT_FOUND`.
pub(crate) fn get_report_descriptor(context: &DeviceContext, request: &Request) -> NTSTATUS {
    copy_descriptor(
        request,
        || report_descriptor_for(&context.config.load()),
        b"GetReportDescriptor",
    )
}

/// `AmtPtpGetDeviceAttribs` (Hid.c:306-362), `IOCTL_HID_GET_DEVICE_ATTRIBUTES`.
///
/// `WdfRequestRetrieveOutputBuffer(Request, 32)` (failure: that status), then writes only the first 10
/// bytes: `Size` = 32, `VendorID` = 0x8910 (deliberately fake), `ProductID` and `VersionNumber` of the real
/// device. `Reserved` is left untouched, as in C. Information 32.
pub(crate) fn get_device_attribs(context: &DeviceContext, request: &Request) -> NTSTATUS {
    // SAFETY: the dispatcher owns this request until the handler returns.
    let (buffer, _) = match unsafe {
        wdf_hid::retrieve_output_buffer(request.as_raw(), HID_DEVICE_ATTRIBUTES_LEN)
    } {
        Ok(output) => output,
        Err(status) => {
            trace_hid_failure(
                b"WdfRequestRetrieveOutputBuffer",
                b"GetDeviceAttribs",
                status,
            );
            return status;
        }
    };
    if buffer.is_null() {
        // Unreachable: a successful retrieval returns a buffer of at least 32 bytes.
        return STATUS_INVALID_DEVICE_REQUEST;
    }

    let header = device_attributes(&context.config.load()).header_bytes();
    // SAFETY: the framework guarantees the output buffer holds at least
    // HID_DEVICE_ATTRIBUTES_LEN (32) bytes, more than the 10 copied; the
    // caller's buffer cannot overlap the local array. No reference to the
    // caller's memory is created.
    unsafe { copy_nonoverlapping(header.as_ptr(), buffer, header.len()) };
    // Lossless: 32.
    request.set_information(DEVICE_ATTRIBUTES_INFORMATION as ULONG_PTR);
    STATUS_SUCCESS
}

/// `AmtPtpGetStrings` (Hid.c:533-579), `IOCTL_HID_GET_STRING`: passed down unchanged
/// (`WdfRequestFormatRequestUsingCurrentType` + send-and-forget to the local I/O target), so the strings
/// are Apple's. If the send fails, the request is completed with `WdfRequestGetStatus`.
pub(crate) fn get_strings(context: &DeviceContext, request: Request) {
    // SAFETY: `spi_trackpad_io_target` is the device's local I/O target, valid
    // for the device's lifetime.
    if let Err(rejected) = unsafe { request.send_and_forget(context.spi_trackpad_io_target) } {
        tlg::write_event!(
            PROVIDER,
            "GetStringForwardFailed",
            level(Error),
            keyword(keyword::QUEUE),
            win_ntstatus("Status", &rejected.status)
        );
        rejected.complete();
    }
}

/// Reads the request's `HID_XFER_PACKET` after the C checks (Hid.c:604-618, 749-763): the IOCTL buffer
/// length (`OutputBufferLength` for GET, `InputBufferLength` for SET) must be at least 16, then
/// `Irp->UserBuffer` must not be NULL.
///
/// hidclass puts the address of a kernel `HID_XFER_PACKET` in `Irp->UserBuffer` (HID minidriver
/// contract); that address is trusted exactly as the C driver trusts it.
fn read_xfer_packet(request: &Request, get: bool) -> Result<HID_XFER_PACKET, NTSTATUS> {
    let parameters = request.parameters();
    // SAFETY: an internal device-control request, so `DeviceIoControl` is the
    // active member of the parameters union (plain integer reads).
    let ioctl_buffer_length = unsafe {
        if get {
            parameters.Parameters.DeviceIoControl.OutputBufferLength
        } else {
            parameters.Parameters.DeviceIoControl.InputBufferLength
        }
    };
    check_xfer_packet(ioctl_buffer_length, true)?;
    // SAFETY: the dispatcher owns this HIDCLASS request and the packet length has been validated.
    unsafe { wdf_hid::read_xfer_packet(request.as_raw()) }.ok_or(STATUS_INVALID_DEVICE_REQUEST)
}

/// `AmtPtpReportFeatures` (Hid.c:581-725), `IOCTL_HID_GET_FEATURE`.
///
/// Report 7 (device caps) writes `07 05 00`. Report 8 (HQA) writes its ID and 256-byte blob. The
/// respective minimum buffer lengths are 3 and 257, with `STATUS_INVALID_BUFFER_SIZE` for short buffers.
/// Any other report ID: `STATUS_NOT_SUPPORTED`. Information is never set (stays 0, SPI-B4).
pub(crate) fn report_features(request: &Request) -> NTSTATUS {
    let packet = match read_xfer_packet(request, true) {
        Ok(packet) => packet,
        Err(status) => return status,
    };
    let reply = get_feature(
        packet.reportId,
        packet.reportBufferLen,
        !packet.reportBuffer.is_null(),
    );
    // `get_feature` only returns bytes when `reportBufferLen` covers them and `reportBuffer` is non-NULL;
    // checked again so that no write can exceed the caller's buffer.
    let fits = (reply.write.len() as u64) <= u64::from(packet.reportBufferLen);
    if !reply.write.is_empty() && fits && !packet.reportBuffer.is_null() {
        // SAFETY: `reportBuffer` is the caller's report buffer of
        // `reportBufferLen` bytes (HID contract), and `reply.write` is no
        // longer than that; a 'static source cannot overlap it. No reference
        // to the caller's memory is created; bytes past `reply.write` stay
        // untouched, as in C.
        unsafe {
            copy_nonoverlapping(reply.write.as_ptr(), packet.reportBuffer, reply.write.len());
        };
    }
    if !NT_SUCCESS(reply.status) {
        tlg::write_event!(
            PROVIDER,
            "GetFeatureFailed",
            level(Warning),
            keyword(keyword::DRIVER),
            u8("ReportId", &packet.reportId),
            u32("ReportBufferLen", &packet.reportBufferLen),
            win_ntstatus("Status", &reply.status)
        );
    }
    reply.status
}

/// `AmtPtpSetFeatures` (Hid.c:727-861), `IOCTL_HID_SET_FEATURE`.
///
/// Report 4 (input mode) and 6 (function switch) are accepted and update the PTP flags; the input report
/// honours the selective-reporting switches of report 6 (byte 1: Surface bit 0, Button bit 1). Any other
/// report ID: `STATUS_NOT_SUPPORTED`. `reportBufferLen` is not validated (SPI-B4). C read byte 2 of the
/// 2-byte report 6 (out of bounds) and never used the flags; the port reads only the first
/// `min(reportBufferLen, 2)` bytes. Information stays 0.
pub(crate) fn set_features(context: &DeviceContext, request: &Request) -> NTSTATUS {
    let packet = match read_xfer_packet(request, false) {
        Ok(packet) => packet,
        Err(status) => return status,
    };
    let mut report = [0_u8; SET_FEATURE_READ_LEN];
    let readable = usize::try_from(packet.reportBufferLen)
        .unwrap_or(usize::MAX)
        .min(SET_FEATURE_READ_LEN);
    let report_bytes = if packet.reportBuffer.is_null() {
        None
    } else {
        // SAFETY: `reportBuffer` is the caller's report buffer of
        // `reportBufferLen` bytes (HID contract) and `readable` is at most that
        // (and at most the local array's length); copied out, no reference into
        // the caller's memory.
        unsafe {
            copy_nonoverlapping(
                packet.reportBuffer.cast_const(),
                report.as_mut_ptr(),
                readable,
            );
        };
        report.get(..readable)
    };
    let status = context
        .runtime
        .set_feature(packet.reportId, packet.reportBufferLen, report_bytes);
    tlg::write_event!(
        PROVIDER,
        "SetFeature",
        level(Verbose),
        keyword(keyword::DRIVER),
        u8("ReportId", &packet.reportId),
        u32("ReportBufferLen", &packet.reportBufferLen),
        win_ntstatus("Status", &status)
    );
    status
}
