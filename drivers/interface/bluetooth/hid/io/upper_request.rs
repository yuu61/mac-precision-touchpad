//! Completion, buffers, and queue operations for requests from HIDCLASS.

use core::ptr::null_mut;

use amtptp_core::hid::hidport::{
    HID_STRING_ID_IMANUFACTURER, HID_STRING_ID_IPRODUCT, HID_STRING_ID_ISERIALNUMBER,
};
use amtptp_usb_bt::bluetooth::{
    FeatureChange, NtStatus, STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_DEVICE_STATE,
    XferPacket, XferPacketMut, decode_set_feature,
};
use tracelogging as tlg;
use wdf_support::hid as wdf_hid;
use wdf_support::init::request_parameters_init;
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_PARAMETER};
use wdf_support::request::Request;
use wdk_sys::{ULONG, ULONG_PTR, WDFMEMORY, WDFREQUEST, call_unsafe_wdf_function_binding};

use super::Filter;
use crate::{KW_HID, KW_INPUT, PROVIDER};

#[expect(
    clippy::unused_self,
    reason = "Callbacks use the same operation receiver for live requests and for the recording backend."
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "The exclusive receiver limits the lifetime of borrowed framework request and report buffers."
)]
#[expect(
    clippy::multiple_inherent_impl,
    reason = "HIDCLASS request operations are implemented together in this module."
)]
impl Filter<'_> {
    pub(super) fn complete(&mut self, request: WDFREQUEST, status: NtStatus) {
        tlg::write_event!(
            PROVIDER,
            "RequestCompleted",
            level(Verbose),
            keyword(KW_HID),
            pointer("Request", &(request as usize)),
            win_ntstatus("Status", &status)
        );
        // SAFETY: the core hands over a framework-delivered request it owns (presented to the
        // dispatch callback or retrieved from HidReadQueue) and calls `complete` at most once per
        // request, as its last use (verified by the fixture mock's double-completion assertion on
        // every record). `complete` consumes the guard, so it completes exactly once here.
        unsafe { Request::from_raw(request, status) }.complete(status);
    }

    pub(super) fn set_information(&mut self, request: WDFREQUEST, information: usize) {
        // ULONG_PTR is u64 on x64/ARM64 (the only targets): lossless.
        let information = information as ULONG_PTR;
        // SAFETY: `request` is a live request owned by the core (not yet completed).
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestSetInformation, request, information);
        };
    }

    pub(super) fn retrieve_output_memory(
        &mut self,
        request: WDFREQUEST,
    ) -> Result<WDFMEMORY, NtStatus> {
        // SAFETY: the dispatcher owns the live request; its output memory is used before completion.
        let output = unsafe { wdf_hid::retrieve_output_memory(request) };
        if let Err(status) = output {
            // Input.c:228 (also reached from the descriptor IOCTLs, where it is an error too).
            tlg::write_event!(
                PROVIDER,
                "RetrieveOutputMemoryFailed",
                level(Error),
                keyword(KW_INPUT),
                win_ntstatus("Status", &status)
            );
        }
        output
    }

    pub(super) fn copy_to_memory(&mut self, memory: WDFMEMORY, source: &[u8]) -> NtStatus {
        // SAFETY: the memory belongs to the pending request and is writable. The source is
        // a descriptor or a locally produced report that cannot overlap it.
        let status = unsafe { wdf_hid::copy_to_output_memory(memory, source) };
        if !NT_SUCCESS(status) {
            // Input.c:236 (also reached from the descriptor IOCTLs, where it is an error too).
            tlg::write_event!(
                PROVIDER,
                "CopyToOutputMemoryFailed",
                level(Error),
                keyword(KW_INPUT),
                win_ntstatus("Status", &status)
            );
        }
        status
    }

    pub(super) fn retrieve_output_buffer(
        &mut self,
        request: WDFREQUEST,
        minimum_length: usize,
    ) -> Result<&mut [u8], NtStatus> {
        // SAFETY: the dispatcher owns this live request and keeps it pending while the output is used.
        let (buffer, _) = unsafe { wdf_hid::retrieve_output_buffer(request, minimum_length) }?;
        if buffer.is_null() {
            // Defensive (P2): KMDF does not return success with a NULL buffer for a non-zero
            // minimum length; C would write through it.
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        // SAFETY: on success WDF guarantees an output buffer of at least `minimum_length` bytes
        // that belongs to the request until it is completed; the slice is used before completion
        // (it borrows `self`) and nothing else references that memory meanwhile.
        Ok(unsafe { core::slice::from_raw_parts_mut(buffer, minimum_length) })
    }

    pub(super) fn get_feature_packet(&mut self, request: WDFREQUEST) -> Option<XferPacketMut<'_>> {
        // SAFETY: the core checks the IOCTL buffer size before requesting the live HIDCLASS packet.
        let packet = unsafe { wdf_hid::read_xfer_packet(request) }?;
        let (buffer, length, report_id) =
            (packet.reportBuffer, packet.reportBufferLen, packet.reportId);
        let report_buffer: &mut [u8] = if buffer.is_null() {
            &mut []
        } else {
            // SAFETY: `reportBuffer`/`reportBufferLen` describe the caller's report buffer, which
            // the C code trusts the same way (kernel caller, HIDCLASS). It stays valid until the
            // request is completed, after the slice's last use.
            unsafe { core::slice::from_raw_parts_mut(buffer, length as usize) }
        };
        // Hid.c:217, 240, 253: with the status of `RequestCompleted` this tells "buffer too small"
        // from "unsupported ID".
        let report_buffer_len = report_buffer.len();
        tlg::write_event!(
            PROVIDER,
            "GetFeature",
            level(Informational),
            keyword(KW_HID),
            u8_hex("ReportId", &report_id),
            usize("ReportBufferLen", &report_buffer_len)
        );
        Some(XferPacketMut {
            report_id,
            report_buffer,
        })
    }

    pub(super) fn set_feature_packet(&mut self, request: WDFREQUEST) -> Option<XferPacket<'_>> {
        // SAFETY: the core checks the IOCTL buffer size before requesting the live HIDCLASS packet.
        let packet = unsafe { wdf_hid::read_xfer_packet(request) }?;
        let (buffer, length, report_id) =
            (packet.reportBuffer, packet.reportBufferLen, packet.reportId);
        let report_buffer: &[u8] = if buffer.is_null() {
            &[]
        } else {
            // SAFETY: as in `get_feature_packet` (read-only view).
            unsafe { core::slice::from_raw_parts(buffer, length as usize) }
        };
        trace_set_feature(report_id, report_buffer);
        Some(XferPacket {
            report_id,
            report_buffer,
        })
    }

    pub(super) fn forward_to_read_queue(&mut self, request: WDFREQUEST) -> NtStatus {
        let queue = self.ctx.hid_read_queue();
        // SAFETY: live owned request; `queue` is this device's manual HidReadQueue (created in
        // AddDevice, lives as long as the device). On success the queue owns the request.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestForwardToIoQueue, request, queue)
        };
        if !NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "ForwardToReadQueueFailed",
                level(Error),
                keyword(KW_INPUT),
                win_ntstatus("Status", &status)
            );
        }
        status
    }

    pub(super) fn retrieve_next_parked_read(&mut self) -> Result<WDFREQUEST, NtStatus> {
        let mut request: WDFREQUEST = null_mut();
        // SAFETY: HidReadQueue is a live manual queue of this device; `request` is a valid out
        // pointer. A retrieved request is owned by the core, which completes it.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoQueueRetrieveNextRequest,
                self.ctx.hid_read_queue(),
                &raw mut request
            )
        };
        if NT_SUCCESS(status) {
            Ok(request)
        } else {
            if self.trace_input_drop {
                // Input.c:176-180: no parked upper read, the packet is dropped.
                tlg::write_event!(
                    PROVIDER,
                    "InputDroppedNoParkedRead",
                    level(Error),
                    keyword(KW_INPUT),
                    win_ntstatus("Status", &status)
                );
            }
            Err(status)
        }
    }

    pub(super) fn parked_read_count(&mut self) -> usize {
        let mut queued: ULONG = 0;
        // SAFETY: HidReadQueue is a live manual queue of this device; DriverRequests is optional (NULL).
        let _state = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoQueueGetState,
                self.ctx.hid_read_queue(),
                &raw mut queued,
                null_mut()
            )
        };
        queued as usize
    }

    pub(super) fn copy_string(&mut self, request: WDFREQUEST) -> Result<usize, NtStatus> {
        let mut params = request_parameters_init();
        // SAFETY: live owned request; initialized parameters structure.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestGetParameters, request, &raw mut params);
        }
        // SAFETY: an internal device-control request: KMDF fills `DeviceIoControl`, whose
        // Type3InputBuffer carries the string ID (low 16 bits) and language ID (high 16 bits).
        let id_and_language =
            unsafe { params.Parameters.DeviceIoControl.Type3InputBuffer } as usize;
        let id = (id_and_language & 0xFFFF) as u16;
        let mut units = [0_u16; crate::context::MAX_STRING_UNITS];
        // SAFETY: the spin lock was created in AddDevice; the closure does not call back into WDF.
        let found = unsafe {
            self.ctx.strings.with(|strings| {
                if !strings.known {
                    return Err(STATUS_INVALID_DEVICE_STATE);
                }
                let text = match id {
                    HID_STRING_ID_IMANUFACTURER => &strings.manufacturer,
                    HID_STRING_ID_IPRODUCT => &strings.product,
                    HID_STRING_ID_ISERIALNUMBER => &strings.serial_number,
                    _ => return Err(STATUS_INVALID_PARAMETER),
                };
                let text = text.as_slice();
                if let Some(dst) = units.get_mut(..text.len()) {
                    dst.copy_from_slice(text);
                }
                Ok(text.len())
            })
        }?;
        // The string and its NUL, little-endian UTF-16.
        let bytes = (found + 1) * 2;
        let output = self.retrieve_output_buffer(request, bytes)?;
        let output = output.get_mut(..bytes).ok_or(STATUS_INVALID_PARAMETER)?;
        let (pairs, _) = output.as_chunks_mut::<2>();
        for (pair, unit) in pairs
            .iter_mut()
            .zip(units.iter().take(found).chain(core::iter::once(&0)))
        {
            *pair = unit.to_le_bytes();
        }
        Ok(bytes)
    }
}

/// Traces a `SET_FEATURE` (Hid.c:298-336): the report, then the change the core will apply (decoded
/// the same way, from the same bytes) or the rejection. The core reaches `set_feature_packet` only
/// after its length and NULL `UserBuffer` checks, as in C.
fn trace_set_feature(report_id: u8, report_buffer: &[u8]) {
    let report_buffer_len = report_buffer.len();
    tlg::write_event!(
        PROVIDER,
        "SetFeature",
        level(Informational),
        keyword(KW_HID),
        u8_hex("ReportId", &report_id),
        usize("ReportBufferLen", &report_buffer_len)
    );
    match decode_set_feature(report_id, report_buffer) {
        Ok(FeatureChange::InputMode { ptp_input_on }) => {
            tlg::write_event!(
                PROVIDER,
                "SetInputMode",
                level(Informational),
                keyword(KW_HID),
                bool8("PtpInputOn", &ptp_input_on)
            );
        }
        Ok(FeatureChange::SelectiveReporting {
            button_report,
            surface_report,
        }) => {
            tlg::write_event!(
                PROVIDER,
                "SelectiveReporting",
                level(Informational),
                keyword(KW_HID),
                bool8("ButtonReport", &button_report),
                bool8("SurfaceReport", &surface_report)
            );
        }
        Ok(FeatureChange::None) => {}
        Err(status) => {
            tlg::write_event!(
                PROVIDER,
                "SetFeatureNotSupported",
                level(Informational),
                keyword(KW_HID),
                u8_hex("ReportId", &report_id),
                win_ntstatus("Status", &status)
            );
        }
    }
}
