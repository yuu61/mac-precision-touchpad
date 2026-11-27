//! `IOCTL_HID_GET_FEATURE` (`AmtPtpReportFeatures`, Hid.c:287-418) and `IOCTL_HID_SET_FEATURE`
//! (`AmtPtpSetFeatures`, Hid.c:420-559).
//!
//! hidclass sends both as internal IOCTLs with `Irp->UserBuffer` pointing at a `HID_XFER_PACKET`
//! (`{PUCHAR reportBuffer; ULONG reportBufferLen; UCHAR reportId}`, 16 bytes on x64/ARM64) and no MDL, so
//! the C bypasses WDF buffer retrieval. The handlers check, in this order:
//!
//! 1. the request's `OutputBufferLength` (GET) / `InputBufferLength` (SET) is at least 16, else
//!    `STATUS_BUFFER_TOO_SMALL`;
//! 2. `Irp->UserBuffer` is not NULL, else `STATUS_INVALID_DEVICE_REQUEST`;
//! 3. `switch (reportId)`.
//!
//! The packet is passed as a closure so that the glue dereferences `Irp->UserBuffer` only after check 1,
//! exactly like the C. The closure returns `None` for a NULL `UserBuffer`; otherwise a packet whose
//! `report_buffer` is the `(reportBuffer, reportBufferLen)` slice, or an empty slice when `reportBuffer` is
//! NULL (spec B16: the C writes through a NULL `reportBuffer`; treating it as zero-length gives
//! `STATUS_INVALID_BUFFER_SIZE` for GET and "mode 0" for SET 0x04, the result C produces whenever it does
//! not crash).
//!
//! Neither handler ever sets `Information`: every completion has `Information = 0`, even on success
//! (spec B13, preserved).

use ptp_hid::common::{
    PTP_COLLECTION_MOUSE, PTP_COLLECTION_WINDOWS, REPORTID_DEVICE_CAPS, REPORTID_FUNCSWITCH,
    REPORTID_PTPHQA, REPORTID_REPORTMODE,
};
use ptp_hid::feature::{
    DeviceCapsReport, INPUT_MODE_OFFSET, SelectiveReportBits, write_hqa_report,
};

use crate::state::DeviceState;
use crate::status::{
    Completion, STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_BUFFER_SIZE, STATUS_INVALID_DEVICE_REQUEST,
    STATUS_NOT_SUPPORTED, STATUS_SUCCESS, nt_success,
};
use crate::wellspring::{ControlTransfer, set_wellspring_mode};

/// `sizeof(HID_XFER_PACKET)` on x64 and ARM64: 16.
pub const HID_XFER_PACKET_LEN: usize = 16;

/// A `GET_FEATURE` `HID_XFER_PACKET`: the report ID and the caller's report buffer to fill.
#[derive(Debug)]
pub struct GetFeaturePacket<'a> {
    /// `reportId`.
    pub report_id: u8,
    /// `reportBuffer[0..reportBufferLen]`; empty when `reportBuffer` is NULL.
    pub report_buffer: &'a mut [u8],
}

/// A `SET_FEATURE` `HID_XFER_PACKET`: the report ID and the report bytes.
#[derive(Clone, Copy, Debug)]
pub struct SetFeaturePacket<'a> {
    /// `reportId`.
    pub report_id: u8,
    /// `reportBuffer[0..reportBufferLen]`; empty when `reportBuffer` is NULL.
    pub report_buffer: &'a [u8],
}

/// `AmtPtpReportFeatures`.
///
/// - 0x07 (device caps): `reportBufferLen < 3` → `STATUS_INVALID_BUFFER_SIZE`; else writes `07 05 00`
///   to bytes 0..3 → success.
/// - 0x08 (HQA): `reportBufferLen < 257` → `STATUS_INVALID_BUFFER_SIZE`; otherwise writes the report ID
///   and 256-byte certification blob.
/// - any other ID → `STATUS_NOT_SUPPORTED`.
///
/// `output_buffer_length` is `Parameters.DeviceIoControl.OutputBufferLength`; `packet` is called only when
/// it is at least [`HID_XFER_PACKET_LEN`].
pub fn get_feature<'a, F: FnOnce() -> Option<GetFeaturePacket<'a>>>(
    output_buffer_length: usize,
    packet: F,
) -> Completion {
    if output_buffer_length < HID_XFER_PACKET_LEN {
        return Completion::status(STATUS_BUFFER_TOO_SMALL);
    }
    let Some(packet) = packet() else {
        return Completion::status(STATUS_INVALID_DEVICE_REQUEST);
    };
    let written = match packet.report_id {
        REPORTID_DEVICE_CAPS => DeviceCapsReport::PTP.write_to(packet.report_buffer),
        REPORTID_PTPHQA => write_hqa_report(packet.report_buffer),
        _ => return Completion::status(STATUS_NOT_SUPPORTED),
    };
    match written {
        Ok(()) => Completion::status(STATUS_SUCCESS),
        Err(ptp_hid::BufferTooSmall) => Completion::status(STATUS_INVALID_BUFFER_SIZE),
    }
}

/// The input mode byte `PTP_DEVICE_INPUT_MODE_REPORT.Mode` of a `SET_FEATURE` 0x04 report.
///
/// The C reads `reportBuffer[1]` with no length check (spec B14). When the report is shorter than 2
/// bytes that byte is outside the report, and this returns 0 instead (port policy P2 "treat as 0"), which
/// makes the request fail with `STATUS_NOT_SUPPORTED` like an explicit mode 0.
#[must_use]
pub fn input_mode(report: &[u8]) -> u8 {
    report.get(INPUT_MODE_OFFSET).copied().unwrap_or(0)
}

/// The switches of `SET_FEATURE` 0x06: byte 1 of the 2-byte report ([`SelectiveReportBits::from_report`];
/// `None` without byte 1). The C read them from `reportBuffer[2]` through a 3-byte struct, only traced
/// them and ignored them (spec B15).
#[must_use]
pub const fn selective_report_bits(report: &[u8]) -> Option<SelectiveReportBits> {
    SelectiveReportBits::from_report(report)
}

/// `AmtPtpSetFeatures`.
///
/// - 0x04 (input mode), mode = [`input_mode`]:
///   - 0 (`PTP_COLLECTION_MOUSE`) → `STATUS_NOT_SUPPORTED`;
///   - 3 (`PTP_COLLECTION_WINDOWS`) → if wellspring mode is off, call [`set_wellspring_mode`]
///     to enable it and return its failure status; otherwise (or on success) `STATUS_SUCCESS`;
///   - any other value → `STATUS_SUCCESS` with no action (spec B14, preserved).
/// - 0x06 (selective reporting) → `STATUS_SUCCESS`; `PtpReportTouch` = Surface Switch (bit 0) and
///   `PtpReportButton` = Button Switch (bit 1) of [`selective_report_bits`], which the reader completion
///   honours (P3 fix of spec B15: C ignored the report). A report without byte 1 changes nothing.
/// - any other ID → `STATUS_NOT_SUPPORTED`.
///
/// Must run at `PASSIVE_LEVEL` (synchronous control transfers). `input_buffer_length` is
/// `Parameters.DeviceIoControl.InputBufferLength`; `packet` is called only when it is at least
/// [`HID_XFER_PACKET_LEN`].
pub fn set_feature<'a, T: ControlTransfer + ?Sized, F: FnOnce() -> Option<SetFeaturePacket<'a>>>(
    input_buffer_length: usize,
    packet: F,
    state: &DeviceState,
    io: &mut T,
) -> Completion {
    if input_buffer_length < HID_XFER_PACKET_LEN {
        return Completion::status(STATUS_BUFFER_TOO_SMALL);
    }
    let Some(packet) = packet() else {
        return Completion::status(STATUS_INVALID_DEVICE_REQUEST);
    };
    match packet.report_id {
        REPORTID_REPORTMODE => {
            let was_on = state.wellspring_mode_on();
            match input_mode(packet.report_buffer) {
                PTP_COLLECTION_MOUSE => Completion::status(STATUS_NOT_SUPPORTED),
                PTP_COLLECTION_WINDOWS if !was_on => {
                    let status = set_wellspring_mode(state, io, true);
                    if nt_success(status) {
                        Completion::status(STATUS_SUCCESS)
                    } else {
                        Completion::status(status)
                    }
                }
                _ => Completion::status(STATUS_SUCCESS),
            }
        }
        REPORTID_FUNCSWITCH => {
            if let Some(bits) = selective_report_bits(packet.report_buffer) {
                state.set_selective_reporting(bits);
            }
            Completion::status(STATUS_SUCCESS)
        }
        _ => Completion::status(STATUS_NOT_SUPPORTED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_feature_checks_in_c_order() {
        let mut called = false;
        let c = get_feature(15, || {
            called = true;
            None
        });
        assert_eq!(c, Completion::status(STATUS_BUFFER_TOO_SMALL));
        assert!(!called);
        assert_eq!(
            get_feature(16, || None),
            Completion::status(STATUS_INVALID_DEVICE_REQUEST)
        );
    }

    #[test]
    fn get_feature_reports() {
        let mut buf = [0xa5_u8; 4];
        let c = get_feature(16, || {
            Some(GetFeaturePacket {
                report_id: 0x07,
                report_buffer: &mut buf,
            })
        });
        assert_eq!(c, Completion::status(STATUS_SUCCESS));
        assert_eq!(buf, [0x07, 0x05, 0x00, 0xa5]);

        let mut hqa = [0xa5_u8; 257];
        let c = get_feature(16, || {
            Some(GetFeaturePacket {
                report_id: 0x08,
                report_buffer: &mut hqa,
            })
        });
        assert_eq!(c, Completion::status(STATUS_SUCCESS));
        assert_eq!(hqa, ptp_hid::feature::HQA_REPORT);

        // NULL reportBuffer (B16): treated as zero-length.
        let c = get_feature(16, || {
            Some(GetFeaturePacket {
                report_id: 0x07,
                report_buffer: &mut [],
            })
        });
        assert_eq!(c, Completion::status(STATUS_INVALID_BUFFER_SIZE));

        let mut other = [0_u8; 8];
        let c = get_feature(64, || {
            Some(GetFeaturePacket {
                report_id: 0x55,
                report_buffer: &mut other,
            })
        });
        assert_eq!(c, Completion::status(STATUS_NOT_SUPPORTED));
    }

    #[test]
    fn input_mode_and_trace_bits() {
        assert_eq!(input_mode(&[0x04, 0x03]), 3);
        assert_eq!(input_mode(&[0x04]), 0);
        assert_eq!(input_mode(&[]), 0);
        assert_eq!(selective_report_bits(&[0x06]), None);
        assert_eq!(
            selective_report_bits(&[0x06, 0x01]),
            Some(SelectiveReportBits {
                surface_report: true,
                button_report: false
            })
        );
    }
}
