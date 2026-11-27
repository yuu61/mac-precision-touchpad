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
    DEVICE_ATTRIBUTES_INFORMATION, device_attributes, hid_descriptor_for, report_descriptor_for,
};
use amtptp_spikm_core::device::{DeviceConfig, ReportType};
use amtptp_spikm_core::ioctl::{InternalIoctl, classify_internal_ioctl};
use amtptp_spikm_core::status::{NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_SUCCESS};
use common::{Tally, load, prefill, to_hex, wdf_failures};
use ptp_hid::descriptor::{SPI_CONTACT_ID_LOGICAL_MAX, SPI_CONTACT_ID_MAX_OFFSETS};

const OK: &str = "driver result: success";
const NOT_FOUND: &str = "driver result: STATUS_NOT_FOUND (injected unreachable state)";
const FW_RETRIEVE: &str = "framework: output retrieval failed (outlen 0 / < 32)";
const FW_COPY: &str = "framework: WdfMemoryCopyFromBuffer too small";

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

#[test]
fn descriptors_and_attributes() {
    let recs = load("descriptors.txt");
    assert_eq!(recs.len(), 85);
    let mut tally = Tally::default();
    for r in &recs {
        let ctx = format!("descriptors.txt:{} {}", r.line, r.case());
        let code = r.hex32("code");
        let cfg = config_for(r.hex16("pid"), r.dec("rt"), r.hex16("ver"), r.get("inj"));
        let outlen: usize = r.dec("outlen");
        let mut out = prefill(outlen);
        let wdf = wdf_failures(r);
        let wdf_fn = wdf.first().map(|(f, _)| f.as_str());

        let (status, info): (NtStatus, usize) = match classify_internal_ioctl(code) {
            kind @ (InternalIoctl::GetDeviceDescriptor | InternalIoctl::GetReportDescriptor) => {
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
        assert_eq!(info, r.dec::<usize>("info"), "{ctx}: info");
        assert_eq!(
            r.dec::<u32>("infoset"),
            u32::from(status == STATUS_SUCCESS),
            "{ctx}"
        );
        assert_eq!(r.get("completed"), "1", "{ctx}");
        let mut expected = common::hex(r.get("out"));
        if classify_internal_ioctl(code) == InternalIoctl::GetDeviceAttributes && info == 32 {
            expected[10..32].fill(0);
        }
        if status == STATUS_SUCCESS
            && classify_internal_ioctl(code) == InternalIoctl::GetReportDescriptor
        {
            // P3: the C descriptors declare a contact-ID logical maximum of 3; the port declares 4.
            for offset in SPI_CONTACT_ID_MAX_OFFSETS {
                assert_eq!(expected[offset], 3, "{ctx}: C contact-ID maximum");
                expected[offset] = SPI_CONTACT_ID_LOGICAL_MAX;
            }
        }
        assert_eq!(to_hex(&out), to_hex(&expected), "{ctx}: out");
    }
    assert_eq!(tally.get(OK), 63);
    assert_eq!(tally.get(NOT_FOUND), 10);
    assert_eq!(tally.get(FW_RETRIEVE), 7);
    assert_eq!(tally.get(FW_COPY), 5);
    assert_eq!(tally.total(), 85);
}
