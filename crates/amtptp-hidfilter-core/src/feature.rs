//! `IOCTL_HID_GET_FEATURE` / `IOCTL_HID_SET_FEATURE` (Hid.c:176-345) as pure functions.
//!
//! The common preamble (transfer-buffer length, then `Irp->UserBuffer != NULL`) lives in
//! [`crate::dispatch_internal_ioctl`], which then calls [`get_feature_report`] or
//! [`decode_set_feature`] with the `HID_XFER_PACKET` contents. Neither handler sets `Information`
//! (HF-B21); the dispatcher completes the request with the returned status only.
//!
//! The switch is on `HID_XFER_PACKET.reportId`; byte 0 of the report buffer is never consulted.

use ptp_hid::common::{
    PTP_COLLECTION_MOUSE, PTP_COLLECTION_WINDOWS, REPORTID_DEVICE_CAPS, REPORTID_FUNCSWITCH,
    REPORTID_PTPHQA, REPORTID_REPORTMODE,
};
use ptp_hid::feature::{
    DeviceCapsReport, INPUT_MODE_OFFSET, SelectiveReportBits,
    write_hqa_report,
};

use crate::device::HID_XFER_PACKET_LEN;
use crate::status::{
    NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_BUFFER_SIZE, STATUS_NOT_SUPPORTED,
    STATUS_SUCCESS,
};

/// The first check of both handlers: the IOCTL's transfer buffer (`OutputBufferLength` for GET,
/// `InputBufferLength` for SET) must hold a `HID_XFER_PACKET` (16 bytes), else
/// `STATUS_BUFFER_TOO_SMALL`. It runs before the `UserBuffer` NULL check.
///
/// # Errors
/// Returns `STATUS_BUFFER_TOO_SMALL` if the transfer buffer is shorter than a HID packet.
pub const fn check_xfer_length(transfer_length: usize) -> Result<(), NtStatus> {
    if transfer_length < HID_XFER_PACKET_LEN {
        Err(STATUS_BUFFER_TOO_SMALL)
    } else {
        Ok(())
    }
}

/// `GET_FEATURE` for `report_id` into `report_buffer` (the `reportBufferLen` bytes at `reportBuffer`).
///
/// - 0x07 (device caps): needs 3 bytes, else `STATUS_INVALID_BUFFER_SIZE`; writes `07 05 00`.
/// - 0x08 (HQA): needs 257 bytes, else `STATUS_INVALID_BUFFER_SIZE`; writes the report ID and
///   256-byte certification blob.
/// - anything else: `STATUS_NOT_SUPPORTED`, nothing written.
pub fn get_feature_report(report_id: u8, report_buffer: &mut [u8]) -> NtStatus {
    let written = match report_id {
        REPORTID_DEVICE_CAPS => DeviceCapsReport::PTP.write_to(report_buffer),
        REPORTID_PTPHQA => write_hqa_report(report_buffer),
        _ => return STATUS_NOT_SUPPORTED,
    };
    match written {
        Ok(()) => STATUS_SUCCESS,
        Err(_) => STATUS_INVALID_BUFFER_SIZE,
    }
}

/// The device-state change a `SET_FEATURE` asks for. `PtpInputOn` is never read (HF-B20); the input
/// report honours the two selective-reporting switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureChange {
    /// Nothing changes (input mode other than 0 or 3).
    None,
    /// `PtpInputOn` = `ptp_input_on` (mode 3 = TRUE, mode 0 = FALSE).
    InputMode {
        /// New `PtpInputOn`.
        ptp_input_on: bool,
    },
    /// `PtpReportTouch` = bit 0 (Surface Switch), `PtpReportButton` = bit 1 (Button Switch) of report
    /// byte 1.
    SelectiveReporting {
        /// New `PtpReportButton`.
        button_report: bool,
        /// New `PtpReportTouch`.
        surface_report: bool,
    },
}

/// Decodes `SET_FEATURE` for `report_id` with `report_buffer` (the `reportBufferLen` bytes at
/// `reportBuffer`). The status is `STATUS_SUCCESS` for 0x04 and 0x06 whatever the value, and
/// `STATUS_NOT_SUPPORTED` for every other ID.
///
/// C reads byte 1 without checking `reportBufferLen` (HF-B23). This port reads it only if it is inside
/// the buffer. For 0x04 a missing byte reads as 0 (port policy P2); for 0x06 a report without its switch
/// byte changes nothing ([`SelectiveReportBits::from_report`], P3; C also applied the switches swapped).
///
/// # Errors
/// Returns `STATUS_NOT_SUPPORTED` for unsupported report IDs.
pub fn decode_set_feature(report_id: u8, report_buffer: &[u8]) -> Result<FeatureChange, NtStatus> {
    match report_id {
        REPORTID_REPORTMODE => {
            let mode = report_buffer.get(INPUT_MODE_OFFSET).copied().unwrap_or(0);
            Ok(match mode {
                PTP_COLLECTION_MOUSE => FeatureChange::InputMode {
                    ptp_input_on: false,
                },
                PTP_COLLECTION_WINDOWS => FeatureChange::InputMode { ptp_input_on: true },
                _ => FeatureChange::None,
            })
        }
        REPORTID_FUNCSWITCH => Ok(SelectiveReportBits::from_report(report_buffer).map_or(
            FeatureChange::None,
            |bits| FeatureChange::SelectiveReporting {
                button_report: bits.button_report,
                surface_report: bits.surface_report,
            },
        )),
        _ => Err(STATUS_NOT_SUPPORTED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_d5_vectors() {
        let mut b = [0xAA_u8; 3];
        assert_eq!(get_feature_report(7, &mut b), STATUS_SUCCESS);
        assert_eq!(b, [7, 5, 0]);
        let mut b = [0xAA_u8; 2];
        assert_eq!(get_feature_report(7, &mut b), STATUS_INVALID_BUFFER_SIZE);
        assert_eq!(b, [0xAA; 2]);
        let mut b = [0xAA_u8; 257];
        assert_eq!(get_feature_report(8, &mut b), STATUS_SUCCESS);
        assert_eq!(b, ptp_hid::feature::HQA_REPORT);
        let mut b = [0xAA_u8; 256];
        assert_eq!(get_feature_report(8, &mut b), STATUS_INVALID_BUFFER_SIZE);
        for id in [4_u8, 6, 9] {
            assert_eq!(get_feature_report(id, &mut [0; 8]), STATUS_NOT_SUPPORTED);
        }
        assert_eq!(
            decode_set_feature(4, &[4, 3]),
            Ok(FeatureChange::InputMode { ptp_input_on: true })
        );
        assert_eq!(
            decode_set_feature(4, &[4, 0]),
            Ok(FeatureChange::InputMode {
                ptp_input_on: false
            })
        );
        assert_eq!(decode_set_feature(4, &[4, 5]), Ok(FeatureChange::None));
        assert_eq!(
            decode_set_feature(6, &[6, 2]),
            Ok(FeatureChange::SelectiveReporting {
                button_report: true,
                surface_report: false
            })
        );
        assert_eq!(decode_set_feature(6, &[6]), Ok(FeatureChange::None));
        assert_eq!(decode_set_feature(7, &[7, 0]), Err(STATUS_NOT_SUPPORTED));
        assert_eq!(decode_set_feature(8, &[8, 0]), Err(STATUS_NOT_SUPPORTED));
        // P2: a missing byte 1 reads as 0.
        assert_eq!(
            decode_set_feature(4, &[4]),
            Ok(FeatureChange::InputMode {
                ptp_input_on: false
            })
        );
        assert_eq!(check_xfer_length(15), Err(STATUS_BUFFER_TOO_SMALL));
        assert_eq!(check_xfer_length(16), Ok(()));
    }
}
