//! Replays `descriptors.txt`: `IOCTL_HID_GET_DEVICE_DESCRIPTOR`, `IOCTL_HID_GET_REPORT_DESCRIPTOR` and
//! `IOCTL_HID_GET_DEVICE_ATTRIBUTES` for every PID and `ReportType`, buffer sizes and injected states.
//!
//! The device config is built through [`DeviceConfig::identify`] / [`DeviceConfig::apply_desired_report_type`]
//! where the record says the real `PrepareHardware` set it (`inj=-`); `inj=pid|rt` records write the field
//! directly, as the harness did. The output-buffer steps are the KMDF model from the fixture README:
//! `WdfRequestRetrieveOutputMemory` fails for length 0, `WdfMemoryCopyFromBuffer` for a buffer shorter
//! than the copy, `WdfRequestRetrieveOutputBuffer(32)` for a buffer shorter than 32.

#![cfg(test)]

mod common;

use amtptp_spikm_core::descriptors::{
    DEVICE_ATTRIBUTES_INFORMATION, HID_DESCRIPTOR, REPORT_DESCRIPTOR_LEN, device_attributes,
    hid_descriptor_for, report_descriptor_for,
};
use amtptp_spikm_core::device::{DeviceConfig, Family, ReportType, family_for_product};
use amtptp_spikm_core::ioctl::{InternalIoctl, classify_internal_ioctl};
use amtptp_spikm_core::status::{NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_SUCCESS};
use common::{Tally, load, prefill, to_hex, wdf_failures};
use ptp_hid::common::{MAX_FINGERS, PTP_COLLECTION_WINDOWS};
use ptp_hid::descriptor::{
    FINGER_COLLECTION_SPI_LEN, FINGER_HEAD_SPI_LEN, FINGER_SEPARATOR_LEN, GLOBAL_RESET,
    SPI_CONTACT_ID_LOGICAL_MAX, SPI_CONTACT_ID_MAX_OFFSETS, TOUCHPAD_TLC_HEADER_LEN,
};

const OK: &str = "driver result: success";
const NOT_FOUND: &str = "driver result: STATUS_NOT_FOUND (injected unreachable state)";
const FW_RETRIEVE: &str = "framework: output retrieval failed (outlen 0 / < 32)";
const FW_COPY: &str = "framework: WdfMemoryCopyFromBuffer too small";

const C_REPORT_DESCRIPTOR_LEN: usize = 501;
const REPORT_DESCRIPTOR_DELTA: usize = C_REPORT_DESCRIPTOR_LEN - REPORT_DESCRIPTOR_LEN;

fn config_for(pid: u16, rt: u32, ver: u16, inj: &str) -> DeviceConfig {
    let mut cfg = DeviceConfig::new();
    // Real PrepareHardware with the record's PID/version (lower attributes VID 05ac).
    let real_pid = if inj == "pid" { 0x0272 } else { pid };
    let mut attrs = [0_u8; 32];
    attrs[..4].copy_from_slice(&32_u32.to_le_bytes());
    attrs[4..6].copy_from_slice(&0x05ac_u16.to_le_bytes());
    attrs[6..8].copy_from_slice(&real_pid.to_le_bytes());
    attrs[8..10].copy_from_slice(&ver.to_le_bytes());
    assert_eq!(cfg.identify(STATUS_SUCCESS, &attrs), Ok(()));
    match inj {
        "-" => cfg.apply_desired_report_type(Some(rt)),
        "pid" => cfg.hid_product_id = pid,
        "rt" => cfg.report_type = ReportType(rt),
        other => panic!("inj={other}"),
    }
    assert_eq!(cfg.hid_product_id, pid);
    assert_eq!(cfg.report_type, ReportType(rt));
    cfg
}

/// Projects a clean 496-byte SPI report descriptor back to the 501-byte C descriptor so every
/// non-fix byte is cross-checked against the C fixture.
fn to_legacy_c_spi(bytes: &[u8], family: Option<Family>) -> Vec<u8> {
    let mut patched = bytes.to_vec();
    for offset in SPI_CONTACT_ID_MAX_OFFSETS {
        assert_eq!(patched[offset], SPI_CONTACT_ID_LOGICAL_MAX);
        patched[offset] = 3;
    }
    if family == Some(Family::F1) {
        // Step 2 (A-2.4): Series 1 Y logical maximum is 6217 (0x1849); C copied Series 3's 7855 (0x1eaf).
        let step = FINGER_COLLECTION_SPI_LEN + FINGER_SEPARATOR_LEN;
        for finger in 0..5 {
            let y_max_off = TOUCHPAD_TLC_HEADER_LEN + finger * step + FINGER_HEAD_SPI_LEN + 24;
            assert_eq!(&patched[y_max_off..y_max_off + 2], &6217_u16.to_le_bytes());
            patched[y_max_off..y_max_off + 2].copy_from_slice(&7855_u16.to_le_bytes());
        }
    }
    let step = FINGER_COLLECTION_SPI_LEN + FINGER_SEPARATOR_LEN;
    let f3_reset =
        TOUCHPAD_TLC_HEADER_LEN + 2 * step + (FINGER_COLLECTION_SPI_LEN - 1 - GLOBAL_RESET.len());
    let f5_reset =
        TOUCHPAD_TLC_HEADER_LEN + 4 * step + (FINGER_COLLECTION_SPI_LEN - 1 - GLOBAL_RESET.len());
    let tail_start =
        TOUCHPAD_TLC_HEADER_LEN + 5 * FINGER_COLLECTION_SPI_LEN + 4 * FINGER_SEPARATOR_LEN;
    let scan_time_reset = tail_start + 21;
    let cfg_tlc_start = tail_start + 88;
    let input_mode_max_offset = cfg_tlc_start + 17;

    assert_eq!(&patched[f3_reset..f3_reset + 6], &GLOBAL_RESET);
    assert_eq!(&patched[f5_reset..f5_reset + 6], &GLOBAL_RESET);
    assert_eq!(
        &patched[scan_time_reset..scan_time_reset + 6],
        &GLOBAL_RESET
    );
    assert_eq!(patched[input_mode_max_offset], PTP_COLLECTION_WINDOWS);

    let mut out = Vec::with_capacity(C_REPORT_DESCRIPTOR_LEN);
    out.extend_from_slice(&patched[..f3_reset]);
    out.extend_from_slice(&patched[f3_reset + 6..f5_reset]);
    out.extend_from_slice(&patched[f5_reset + 6..scan_time_reset]);
    out.extend_from_slice(&patched[scan_time_reset + 6..input_mode_max_offset]);
    out.push(MAX_FINGERS);
    out.extend_from_slice(&patched[input_mode_max_offset + 1..]);
    out
}

#[test]
fn descriptors_and_attributes() {
    let recs = load("descriptors.txt");
    assert_eq!(recs.len(), 85);
    let mut tally = Tally::default();
    for r in &recs {
        let ctx = format!("descriptors.txt:{} {}", r.line, r.case());
        let code = r.hex32("code");
        let kind = classify_internal_ioctl(code);
        let cfg = config_for(r.hex16("pid"), r.dec("rt"), r.hex16("ver"), r.get("inj"));
        let c_outlen: usize = r.dec("outlen");
        // Shift the exact-fit boundary probes (500..=502) by the +18-byte descriptor cleanup delta.
        let outlen = if kind == InternalIoctl::GetReportDescriptor
            && (C_REPORT_DESCRIPTOR_LEN - 1..=C_REPORT_DESCRIPTOR_LEN + 1).contains(&c_outlen)
        {
            c_outlen - REPORT_DESCRIPTOR_DELTA
        } else {
            c_outlen
        };
        let mut out = prefill(outlen);
        let wdf = wdf_failures(r);
        let wdf_fn = wdf.first().map(|(f, _)| f.as_str());

        let (status, info): (NtStatus, usize) = match kind {
            InternalIoctl::GetDeviceDescriptor | InternalIoctl::GetReportDescriptor => {
                if outlen == 0 {
                    // WdfRequestRetrieveOutputMemory fails before the PID switch.
                    assert_eq!(wdf_fn, Some("WdfRequestRetrieveOutputMemory"), "{ctx}");
                    tally.add(FW_RETRIEVE);
                    (STATUS_BUFFER_TOO_SMALL, 0)
                } else {
                    let reply = if kind == InternalIoctl::GetDeviceDescriptor {
                        hid_descriptor_for(&cfg)
                    } else {
                        report_descriptor_for(&cfg)
                    };
                    match reply {
                        Ok(bytes) if bytes.len() > outlen => {
                            assert_eq!(wdf_fn, Some("WdfMemoryCopyFromBuffer"), "{ctx}");
                            tally.add(FW_COPY);
                            (STATUS_BUFFER_TOO_SMALL, 0)
                        }
                        Ok(bytes) => {
                            out[..bytes.len()].copy_from_slice(bytes);
                            tally.add(OK);
                            (STATUS_SUCCESS, bytes.len())
                        }
                        Err(s) => {
                            tally.add(NOT_FOUND);
                            (s, 0)
                        }
                    }
                }
            }
            InternalIoctl::GetDeviceAttributes => {
                if outlen < 32 {
                    assert_eq!(wdf_fn, Some("WdfRequestRetrieveOutputBuffer"), "{ctx}");
                    tally.add(FW_RETRIEVE);
                    (STATUS_BUFFER_TOO_SMALL, 0)
                } else {
                    device_attributes(&cfg)
                        .write_to(&mut out)
                        .expect("buffer >= 32");
                    tally.add(OK);
                    (STATUS_SUCCESS, DEVICE_ATTRIBUTES_INFORMATION)
                }
            }
            other @ (InternalIoctl::ReadReport
            | InternalIoctl::GetString
            | InternalIoctl::GetFeature
            | InternalIoctl::SetFeature
            | InternalIoctl::NotSupported) => panic!("{ctx}: unexpected {other:?}"),
        };
        if status != STATUS_SUCCESS && wdf.is_empty() {
            // Driver-decided errors only come from injected (unreachable) context states.
            assert_ne!(r.get("inj"), "-", "{ctx}");
        }
        assert_eq!(status, r.status("status"), "{ctx}: status");
        let expected_info =
            if status == STATUS_SUCCESS && kind == InternalIoctl::GetReportDescriptor {
                r.dec::<usize>("info") - REPORT_DESCRIPTOR_DELTA
            } else {
                r.dec::<usize>("info")
            };
        assert_eq!(info, expected_info, "{ctx}: info");
        assert_eq!(
            r.dec::<u32>("infoset"),
            u32::from(status == STATUS_SUCCESS),
            "{ctx}"
        );
        assert_eq!(r.get("completed"), "1", "{ctx}");
        let mut expected = common::hex(r.get("out"));
        match (status, kind) {
            (STATUS_SUCCESS, InternalIoctl::GetDeviceAttributes) => {
                expected[10..32].fill(0);
                assert_eq!(to_hex(&out), to_hex(&expected), "{ctx}: out");
            }
            (STATUS_SUCCESS, InternalIoctl::GetDeviceDescriptor) => {
                assert_eq!(&out[..9], &HID_DESCRIPTOR, "{ctx}: HID descriptor");
                expected[7..9].copy_from_slice(&(REPORT_DESCRIPTOR_LEN as u16).to_le_bytes());
                assert_eq!(to_hex(&out), to_hex(&expected), "{ctx}: out");
            }
            (STATUS_SUCCESS, InternalIoctl::GetReportDescriptor) => {
                assert_eq!(
                    Some(&out[..REPORT_DESCRIPTOR_LEN]),
                    report_descriptor_for(&cfg).ok(),
                    "{ctx}: clean report descriptor"
                );
                let projected = to_legacy_c_spi(
                    &out[..REPORT_DESCRIPTOR_LEN],
                    family_for_product(cfg.hid_product_id),
                );
                assert_eq!(
                    to_hex(&projected),
                    to_hex(&expected[..C_REPORT_DESCRIPTOR_LEN - 23]),
                    "{ctx}: legacy C projection"
                );
                assert_eq!(
                    &out[REPORT_DESCRIPTOR_LEN..],
                    &prefill(outlen)[REPORT_DESCRIPTOR_LEN..],
                    "{ctx}: trailing prefill"
                );
                assert_eq!(
                    &expected[C_REPORT_DESCRIPTOR_LEN..],
                    &prefill(c_outlen)[C_REPORT_DESCRIPTOR_LEN..],
                    "{ctx}: C trailing prefill"
                );
            }
            (_, InternalIoctl::GetReportDescriptor) => {
                assert_eq!(out, prefill(outlen), "{ctx}: untouched prefill");
                assert_eq!(expected, prefill(c_outlen), "{ctx}: C untouched prefill");
            }
            _ => {
                assert_eq!(to_hex(&out), to_hex(&expected), "{ctx}: out");
            }
        }
    }
    assert_eq!(tally.get(OK), 63);
    assert_eq!(tally.get(NOT_FOUND), 10);
    assert_eq!(tally.get(FW_RETRIEVE), 7);
    assert_eq!(tally.get(FW_COPY), 5);
    assert_eq!(tally.total(), 85);
}
