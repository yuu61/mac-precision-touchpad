//! `IOCTL_HID_GET_FEATURE` / `IOCTL_HID_SET_FEATURE` (`AmtPtpReportFeatures`, `AmtPtpSetFeatures`,
//! Hid.c:581-861).
//!
//! The request carries a `HID_XFER_PACKET` whose address is in `Irp->UserBuffer` (the HID minidriver
//! contract). The glue:
//! 1. calls [`check_xfer_packet`] with `OutputBufferLength` (GET) or `InputBufferLength` (SET) and
//!    whether `UserBuffer` is non-NULL, and completes with the error if there is one;
//! 2. reads `reportId`, `reportBufferLen` and `reportBuffer` from the packet;
//! 3. GET: calls [`get_feature`] and copies [`GetFeatureReply::write`] to the start of `reportBuffer`.
//!    It never builds a slice of `reportBufferLen` bytes;
//!    SET: builds a slice of `min(reportBufferLen, SET_FEATURE_READ_LEN)` bytes (or `None` for a NULL
//!    `reportBuffer`) and calls [`PtpSettings::set_feature`];
//! 4. completes with the returned status **without** calling `WdfRequestSetInformation`: Information stays
//!    0 on every path (preserved, SPI-B4).

use core::sync::atomic::{AtomicBool, Ordering};

use ptp_hid::common::{
    PTP_COLLECTION_MOUSE, PTP_COLLECTION_WINDOWS, REPORTID_DEVICE_CAPS, REPORTID_FUNCSWITCH,
    REPORTID_PTPHQA, REPORTID_REPORTMODE,
};
use ptp_hid::feature::{
    DEVICE_CAPS_REPORT, DEVICE_CAPS_REPORT_LEN, HQA_REPORT, HQA_REPORT_LEN, INPUT_MODE_OFFSET,
    SELECTIVE_REPORT_BITS_OFFSET, SelectiveReportBits,
};

use crate::status::{
    NtStatus, STATUS_INVALID_BUFFER_SIZE, STATUS_INVALID_DEVICE_REQUEST, STATUS_NOT_SUPPORTED,
    STATUS_SUCCESS,
};

/// `sizeof(HID_XFER_PACKET)` on x64/ARM64: 16 bytes (`reportBuffer` pointer @0, `reportBufferLen` u32 @8,
/// `reportId` u8 @12, 3 padding bytes).
pub use ptp_hid::hidport::HID_XFER_PACKET_LEN;

/// Information of every `GET/SET_FEATURE` completion: neither handler calls `WdfRequestSetInformation`.
pub const FEATURE_INFORMATION: usize = 0;

/// The checks both handlers make before looking at the packet (Hid.c:608-618, 753-763), in C order:
/// the IOCTL buffer length first (`< 16`: `STATUS_BUFFER_TOO_SMALL`, even when `UserBuffer` is NULL),
/// then `UserBuffer == NULL` (`STATUS_INVALID_DEVICE_REQUEST`).
///
/// `ioctl_buffer_length` is `Parameters.DeviceIoControl.OutputBufferLength` for GET and
/// `InputBufferLength` for SET.
///
/// # Errors
/// Returns `STATUS_BUFFER_TOO_SMALL` for a short transfer buffer or
/// `STATUS_INVALID_DEVICE_REQUEST` for a missing user buffer.
pub const fn check_xfer_packet(
    ioctl_buffer_length: usize,
    user_buffer_present: bool,
) -> Result<(), NtStatus> {
    if let Err(status) = ptp_hid::hidport::check_xfer_length(ioctl_buffer_length) {
        Err(status)
    } else if !user_buffer_present {
        Err(STATUS_INVALID_DEVICE_REQUEST)
    } else {
        Ok(())
    }
}

/// What a `GET_FEATURE` request does to the caller's report buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GetFeatureReply {
    /// The completion status.
    pub status: NtStatus,
    /// Bytes to copy to the start of `reportBuffer`; empty when nothing is written. Every other byte of
    /// the caller's buffer stays untouched.
    pub write: &'static [u8],
}

/// The bytes C writes for `GET_FEATURE` 0x07: `07 05 00` (5 contacts, click pad).
pub static DEVICE_CAPS_REPLY: [u8; DEVICE_CAPS_REPORT_LEN] = DEVICE_CAPS_REPORT;

/// `AmtPtpReportFeatures` after [`check_xfer_packet`] passed.
///
/// - 0x07: `reportBufferLen < 3` gives `STATUS_INVALID_BUFFER_SIZE`; otherwise `07 05 00` is written.
/// - 0x08: `reportBufferLen < 257` gives `STATUS_INVALID_BUFFER_SIZE`; otherwise the report ID and
///   256-byte certification blob are written.
/// - Every other ID: `STATUS_NOT_SUPPORTED`.
///
/// `report_buffer_present` is false when `reportBuffer` is NULL. C would dereference it once the length
/// check passed; the port answers `STATUS_INVALID_DEVICE_REQUEST` instead (P2, not reachable from
/// hidclass).
#[must_use]
pub fn get_feature(
    report_id: u8,
    report_buffer_len: u32,
    report_buffer_present: bool,
) -> GetFeatureReply {
    let (required, bytes): (usize, &'static [u8]) = match report_id {
        REPORTID_DEVICE_CAPS => (DEVICE_CAPS_REPORT_LEN, &DEVICE_CAPS_REPLY),
        REPORTID_PTPHQA => (HQA_REPORT_LEN, &HQA_REPORT),
        _ => {
            return GetFeatureReply {
                status: STATUS_NOT_SUPPORTED,
                write: &[],
            };
        }
    };
    // `reportBufferLen < ReportSize` with ULONG vs size_t: compare in u64 (lossless widening).
    if u64::from(report_buffer_len) < required as u64 {
        return GetFeatureReply {
            status: STATUS_INVALID_BUFFER_SIZE,
            write: &[],
        };
    }
    if !report_buffer_present {
        return GetFeatureReply {
            status: STATUS_INVALID_DEVICE_REQUEST,
            write: &[],
        };
    }
    GetFeatureReply {
        status: STATUS_SUCCESS,
        write: bytes,
    }
}

/// The most bytes the SET handler reads from `reportBuffer` (byte 1 of reports 0x04 and 0x06). The glue
/// passes `min(reportBufferLen, SET_FEATURE_READ_LEN)` bytes.
pub const SET_FEATURE_READ_LEN: usize = SELECTIVE_REPORT_BITS_OFFSET + 1;
const _: () = assert!(INPUT_MODE_OFFSET < SET_FEATURE_READ_LEN);

/// Byte `offset` of a SET report as the port reads it (P2 for SPI-B2):
/// - at or past `reportBufferLen` the byte reads as 0 (C read past the end of the report);
/// - inside `reportBufferLen` with a NULL `reportBuffer`: `STATUS_INVALID_DEVICE_REQUEST` (C dereferenced
///   NULL);
/// - otherwise the byte itself (0 if the glue passed a shorter slice than promised).
fn set_report_byte(
    report_buffer_len: u32,
    report: Option<&[u8]>,
    offset: usize,
) -> Result<u8, NtStatus> {
    if offset as u64 >= u64::from(report_buffer_len) {
        return Ok(0);
    }
    match report {
        Some(bytes) => Ok(bytes.get(offset).copied().unwrap_or(0)),
        None => Err(STATUS_INVALID_DEVICE_REQUEST),
    }
}

/// `PtpInputOn`, `PtpReportButton` and `PtpReportTouch`, written by `SET_FEATURE` 0x04/0x06. The input
/// report honours both `PtpInputOn` (`SET_FEATURE` 0x04) and the two selective-reporting switches
/// (`SET_FEATURE` 0x06) via [`PtpSettings::effective_reporting`]. Atomics, because `SET_FEATURE` can run
/// concurrently at `DISPATCH_LEVEL` (SPI-B15).
#[derive(Debug)]
pub struct PtpSettings {
    input_on: AtomicBool,
    reporting: ptp_hid::feature::SelectiveReportState,
}

impl Default for PtpSettings {
    fn default() -> Self {
        Self::new()
    }
}

impl PtpSettings {
    /// Power-on defaults: `PtpInputOn` TRUE (`SelfManagedIoInit` immediately switches the SPI trackpad
    /// into multitouch mode) and both selective-reporting switches on ([`SelectiveReportBits::POWER_ON`]).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            input_on: AtomicBool::new(true),
            reporting: ptp_hid::feature::SelectiveReportState::new(SelectiveReportBits::POWER_ON),
        }
    }

    /// The selective-reporting switches configured by `SET_FEATURE` 0x06.
    pub fn selective_reporting(&self) -> SelectiveReportBits {
        self.reporting.load()
    }

    /// Effective input gates combining `PtpInputOn` (`SET_FEATURE` 0x04) with
    /// [`Self::selective_reporting`] (`SET_FEATURE` 0x06). When `PtpInputOn` is FALSE (mouse mode),
    /// both surface and button reporting are suppressed on the PTP collection.
    pub fn effective_reporting(&self) -> SelectiveReportBits {
        if self.input_on() {
            self.selective_reporting()
        } else {
            SelectiveReportBits {
                surface_report: false,
                button_report: false,
            }
        }
    }

    /// Resets `PtpInputOn` and both selective-reporting switches back to power-on enabled state
    /// (`PrepareHardware`).
    pub fn reset_selective_reporting(&self) {
        self.input_on.store(true, Ordering::Relaxed);
        self.store_selective_reporting(SelectiveReportBits::POWER_ON);
    }

    fn store_selective_reporting(&self, bits: SelectiveReportBits) {
        self.reporting.store(bits);
    }

    /// `PtpInputOn`.
    pub fn input_on(&self) -> bool {
        self.input_on.load(Ordering::Relaxed)
    }

    /// `PtpReportButton`.
    pub fn report_button(&self) -> bool {
        self.reporting.load().button_report
    }

    /// `PtpReportTouch`.
    pub fn report_touch(&self) -> bool {
        self.reporting.load().surface_report
    }

    /// `AmtPtpSetFeatures` after [`check_xfer_packet`] passed. `reportBufferLen` is not validated (C does
    /// not validate it either, SPI-B4).
    ///
    /// - 0x04 (input mode): mode = byte 1. 0 sets `PtpInputOn` FALSE, 3 sets it TRUE, other values change
    ///   nothing. `STATUS_SUCCESS`.
    /// - 0x06 (function switch): byte 1, the descriptor's layout ([`SelectiveReportBits::from_byte`]:
    ///   `PtpReportTouch` = bit 0, Surface Switch; `PtpReportButton` = bit 1, Button Switch).
    ///   `STATUS_SUCCESS`. A report without byte 1 (`reportBufferLen < 2`) changes nothing (P3 fix of
    ///   SPI-B2: C read byte 2, past the 2-byte report, so the real report turned both switches off).
    /// - Every other ID: `STATUS_NOT_SUPPORTED`.
    ///
    /// `report` holds up to [`SET_FEATURE_READ_LEN`] bytes, bounded by `reportBufferLen`, from
    /// `reportBuffer`, or `None` when `reportBuffer` is NULL. Byte rule (P2): a byte at or past
    /// `reportBufferLen` reads as 0 (C read past the report); a byte inside it with a NULL buffer returns
    /// `STATUS_INVALID_DEVICE_REQUEST` (C dereferenced NULL). For report 0x04 with `reportBufferLen < 2`
    /// the mode therefore reads as 0 (mouse).
    pub fn set_feature(
        &self,
        report_id: u8,
        report_buffer_len: u32,
        report: Option<&[u8]>,
    ) -> NtStatus {
        match report_id {
            REPORTID_REPORTMODE => {
                let mode = match set_report_byte(report_buffer_len, report, INPUT_MODE_OFFSET) {
                    Ok(b) => b,
                    Err(status) => return status,
                };
                match mode {
                    PTP_COLLECTION_MOUSE => self.input_on.store(false, Ordering::Relaxed),
                    PTP_COLLECTION_WINDOWS => self.input_on.store(true, Ordering::Relaxed),
                    _ => {}
                }
                STATUS_SUCCESS
            }
            REPORTID_FUNCSWITCH => {
                if u64::from(report_buffer_len) <= SELECTIVE_REPORT_BITS_OFFSET as u64 {
                    return STATUS_SUCCESS;
                }
                let byte = match set_report_byte(
                    report_buffer_len,
                    report,
                    SELECTIVE_REPORT_BITS_OFFSET,
                ) {
                    Ok(b) => b,
                    Err(status) => return status,
                };
                self.store_selective_reporting(SelectiveReportBits::from_byte(byte));
                STATUS_SUCCESS
            }
            _ => STATUS_NOT_SUPPORTED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_report_buffer_paths() {
        // P2: C would dereference NULL here.
        assert_eq!(
            get_feature(7, 3, false).status,
            STATUS_INVALID_DEVICE_REQUEST
        );
        assert_eq!(get_feature(7, 0, false).status, STATUS_INVALID_BUFFER_SIZE);
        let s = PtpSettings::new();
        assert_eq!(s.set_feature(4, 2, None), STATUS_INVALID_DEVICE_REQUEST);
        assert_eq!(s.set_feature(6, 2, None), STATUS_INVALID_DEVICE_REQUEST);
        // Nothing inside reportBufferLen is read.
        assert_eq!(s.set_feature(6, 1, None), STATUS_SUCCESS);
        assert_eq!(s.selective_reporting(), SelectiveReportBits::POWER_ON);
        assert_eq!(s.set_feature(4, 0, None), STATUS_SUCCESS);
    }

    #[test]
    fn selective_reporting_reads_byte_one() {
        let s = PtpSettings::new();
        assert_eq!(s.selective_reporting(), SelectiveReportBits::POWER_ON);
        for byte in 0..4_u8 {
            assert_eq!(s.set_feature(6, 2, Some(&[6, byte])), STATUS_SUCCESS);
            assert_eq!(
                s.selective_reporting(),
                SelectiveReportBits {
                    surface_report: byte & 1 != 0,
                    button_report: byte & 2 != 0,
                }
            );
        }
        s.reset_selective_reporting();
        assert_eq!(s.selective_reporting(), SelectiveReportBits::POWER_ON);
    }

    #[test]
    fn set_mode_short_report_reads_zero_and_gates_effective_reporting() {
        let s = PtpSettings::new();
        assert!(s.input_on());
        assert_eq!(s.effective_reporting(), SelectiveReportBits::POWER_ON);
        // reportBufferLen 1: byte 1 is past the report and reads as 0 = mouse mode.
        assert_eq!(s.set_feature(4, 1, Some(&[4])), STATUS_SUCCESS);
        assert!(!s.input_on());
        assert_eq!(
            s.effective_reporting(),
            SelectiveReportBits {
                surface_report: false,
                button_report: false,
            }
        );
        assert_eq!(s.set_feature(4, 2, Some(&[4, 3])), STATUS_SUCCESS);
        assert!(s.input_on());
        assert_eq!(s.effective_reporting(), SelectiveReportBits::POWER_ON);
    }
}
