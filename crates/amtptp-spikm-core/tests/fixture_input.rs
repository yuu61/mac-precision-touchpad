//! Replays `input_f{1,2,3a,3b}.txt`: lower `IOCTL_HID_READ_REPORT` completion -> PTP input report.
//!
//! Each record runs [`DeviceRuntime::complete_lower_read`] on a Configured device whose `LastReportTime`
//! is `lrt_in`, with the lookaside buffer = `in` + zeros to 1024 bytes, `Information` = `len` and the
//! QPC out-parameter = `qpcf`. The upstream copy (`WdfRequestRetrieveOutputMemory` +
//! `WdfMemoryCopyFromBuffer`) is the KMDF model from the fixture README.
//!
//! Decisions checked here:
//! - SPI-B5 (P2): the report is zero-initialized, so it must equal `out` (the 0x00 stack-paint run), and
//!   every bit flagged in `undef` must be 0.
//! - SPI-B9 (P2): bytes at or past `(LONG)len` read as 0, so `b9=1` records compare `out_tail0`. As a
//!   cross-check, parsing the whole 1024-byte buffer (what C does) reproduces `out`.
//! - Each isolated frame begins a new reporting session, so its `ScanTime` is zero.

#![cfg(test)]

mod common;

use amtptp_spikm_core::device::{APPLE_VENDOR_ID, Family, family_for_product, lookup_trackpad};
use amtptp_spikm_core::runtime::{DeviceRuntime, DeviceStatus, ReadTracker};
use amtptp_spikm_core::status::{STATUS_BUFFER_TOO_SMALL, STATUS_SUCCESS};
use amtptp_spikm_core::wire::LOWER_READ_BUFFER_LEN;
use common::{Tally, load, prefill, wdf_failures};
use ptp_hid::input::SPI_PTP_REPORT_SCAN_TIME_OFFSET;

const REPORT_LEN: usize = 30;

const DELIVERED: &str = "report delivered (status 0, info 30)";
const LENGTH_GATE: &str = "driver length gate: (LONG)len < 46 -> c000009c";
const FW_RETRIEVE: &str = "framework: WdfRequestRetrieveOutputMemory c0000023 (outlen 0)";
const FW_COPY: &str = "framework: WdfMemoryCopyFromBuffer c0000023 (outlen < 30)";

/// README rule for `undef` (bits the C code leaves uninitialized), over the 30 report bytes.
fn undef_rule(num_of_fingers: u8) -> [u8; REPORT_LEN] {
    let mut mask = [0_u8; REPORT_LEN];
    let filled = usize::from(num_of_fingers.min(5));
    for i in 0..5 {
        let base = 1 + 5 * i;
        if i < filled {
            mask[base] = 0xE0;
        } else {
            mask[base..base + 5].fill(0xFF);
        }
    }
    mask
}

struct Expect {
    file: &'static str,
    records: usize,
    b9: usize,
    family: Family,
    pids: &'static [u16],
    delivered: usize,
    length_gate: usize,
}

fn replay(e: &Expect) -> Tally {
    let recs = load(e.file);
    assert_eq!(recs.len(), e.records, "{}: record count", e.file);
    let mut tally = Tally::default();
    let mut b9 = 0_usize;
    for r in &recs {
        let ctx = format!("{}:{} {}", e.file, r.line, r.case());
        let pid = r.hex16("pid");
        assert!(e.pids.contains(&pid), "{ctx}: pid {pid:04x} not in family");
        assert_eq!(family_for_product(pid), Some(e.family), "{ctx}");
        let info = lookup_trackpad(APPLE_VENDOR_ID, pid).expect("pid in table");
        assert_eq!(i64::from(info.x_min), r.dec::<i64>("xmin"), "{ctx}: xmin");
        assert_eq!(i64::from(info.y_max), r.dec::<i64>("ymax"), "{ctx}: ymax");

        let lrt_in: i64 = r.dec("lrt_in");
        let qpcf: i64 = r.dec("qpcf");
        // Bring the runtime to Configured with LastReportTime = lrt_in through the real API.
        let rt = DeviceRuntime::new();
        rt.on_d0_entry(lrt_in);
        let smio = rt.on_self_managed_io_init_or_restart(
            &mut ReadTracker::<u32>::new(),
            STATUS_SUCCESS,
            || lrt_in,
        );
        assert_eq!(smio.start_recovery_timer, None);
        assert_eq!(rt.device_status(), DeviceStatus::D0ActiveAndConfigured);
        assert_eq!(rt.qpc_frequency(), lrt_in);

        let input = r.bytes("in");
        let mut buffer = vec![0_u8; LOWER_READ_BUFFER_LEN];
        buffer[..input.len()].copy_from_slice(&input);
        let len: u64 = r.dec("len");
        let mut qpc_calls = 0_u32;
        let result = rt.complete_lower_read(info, len, &buffer, || {
            qpc_calls += 1;
            qpcf
        });

        // KMDF model of the upstream copy.
        let outlen: usize = r.dec("outlen");
        let mut out = prefill(outlen);
        let wdf = wdf_failures(r);
        let (status, information, infoset) = match result {
            Ok(_) if outlen == 0 => {
                assert_eq!(
                    wdf,
                    [("WdfRequestRetrieveOutputMemory".into(), "c0000023".into())]
                );
                tally.add(FW_RETRIEVE);
                (STATUS_BUFFER_TOO_SMALL, 0_u64, 0_u32)
            }
            Ok(_) if outlen < REPORT_LEN => {
                assert_eq!(wdf, [("WdfMemoryCopyFromBuffer".into(), "c0000023".into())]);
                tally.add(FW_COPY);
                (STATUS_BUFFER_TOO_SMALL, 0, 0)
            }
            Ok(bytes) => {
                assert!(wdf.is_empty(), "{ctx}");
                out[..REPORT_LEN].copy_from_slice(&bytes);
                tally.add(DELIVERED);
                (STATUS_SUCCESS, REPORT_LEN as u64, 1)
            }
            Err(s) => {
                assert!(wdf.is_empty(), "{ctx}");
                tally.add(LENGTH_GATE);
                (s, 0, 0)
            }
        };
        assert_eq!(status, r.status("status"), "{ctx}: status");
        assert_eq!(information, r.dec::<u64>("info"), "{ctx}: info");
        assert_eq!(infoset, r.dec::<u32>("infoset"), "{ctx}: infoset");
        assert_eq!(rt.qpc_frequency(), lrt_in, "{ctx}: cached frequency");
        assert_eq!(qpc_calls, r.dec::<u32>("qpc_calls"), "{ctx}: qpc_calls");
        assert_eq!(rt.device_status(), DeviceStatus::D0ActiveAndConfigured);
        if let Some(ops) = r.opt("ops") {
            let in_ops = ops
                .matches("KeQueryPerformanceCounter(pfreq=nonnull)")
                .count();
            assert_eq!(in_ops as u32, qpc_calls, "{ctx}: QPC calls in ops");
        }

        // SPI-B9 (P2): compare the tail-zeroed C run where the C code read stale bytes past len.
        let is_b9 = r.opt("b9") == Some("1");
        let expected = if is_b9 {
            b9 += 1;
            r.get("out_tail0")
        } else {
            assert!(!r.has("out_tail0"), "{ctx}");
            r.get("out")
        };
        let mut expected = common::hex(expected);
        if status == STATUS_SUCCESS {
            expected[SPI_PTP_REPORT_SCAN_TIME_OFFSET..SPI_PTP_REPORT_SCAN_TIME_OFFSET + 2]
                .copy_from_slice(&0_u16.to_le_bytes());
        }
        assert_eq!(out, expected, "{ctx}: out");

        if is_b9 {
            // Cross-check: reading the whole buffer like C reproduces the C output `out`.
            let rt_c = DeviceRuntime::new();
            rt_c.on_d0_entry(lrt_in);
            let _status = rt_c.on_self_managed_io_init_or_restart(
                &mut ReadTracker::<u32>::new(),
                STATUS_SUCCESS,
                || lrt_in,
            );
            let whole = rt_c
                .complete_lower_read(info, LOWER_READ_BUFFER_LEN as u64, &buffer, || qpcf)
                .expect("b9 records pass the length gate");
            let mut c_out = common::hex(r.get("out"));
            c_out[SPI_PTP_REPORT_SCAN_TIME_OFFSET..SPI_PTP_REPORT_SCAN_TIME_OFFSET + 2]
                .copy_from_slice(&0_u16.to_le_bytes());
            assert_eq!(whole[..], c_out[..REPORT_LEN], "{ctx}: whole-buffer parse");
        }

        // SPI-B5 (P2): every bit C leaves uninitialized is zero in our report.
        match r.opt("undef").filter(|u| *u != "-") {
            Some(undef) => {
                let undef = common::hex(undef);
                assert_eq!(undef.len(), outlen, "{ctx}: undef length");
                let n = input.get(30).copied().unwrap_or(0);
                let rule = undef_rule(n);
                assert_eq!(undef[..REPORT_LEN], rule, "{ctx}: undef rule");
                assert!(undef[REPORT_LEN..].iter().all(|&b| b == 0), "{ctx}");
                for (o, m) in out.iter().zip(&undef) {
                    assert_eq!(o & m, 0, "{ctx}: uninitialized bit not zero");
                }
            }
            None if status == STATUS_SUCCESS => {
                // Random records omit `undef`; it follows the same rule, so the masked bits must be 0.
                let n = if len as u32 as i32 >= 46 {
                    input.get(30).copied().unwrap_or(0)
                } else {
                    0
                };
                for (o, m) in out.iter().zip(undef_rule(n)) {
                    assert_eq!(o & m, 0, "{ctx}: uninitialized bit not zero");
                }
            }
            None => {}
        }
    }
    assert_eq!(b9, e.b9, "{}: b9 records", e.file);
    assert_eq!(tally.get(FW_RETRIEVE), 1, "{}", e.file);
    assert_eq!(tally.get(FW_COPY), 2, "{}", e.file);
    assert_eq!(tally.get(DELIVERED), e.delivered, "{}", e.file);
    assert_eq!(tally.get(LENGTH_GATE), e.length_gate, "{}", e.file);
    assert_eq!(
        tally.total(),
        e.records,
        "{}: every record processed",
        e.file
    );
    tally
}

#[test]
fn input_f1() {
    replay(&Expect {
        file: "input_f1.txt",
        records: 1242,
        b9: 94,
        family: Family::F1,
        pids: &[0x0275, 0x0279, 0x0290, 0x0291],
        delivered: 1136,
        length_gate: 103,
    });
}

#[test]
fn input_f2() {
    replay(&Expect {
        file: "input_f2.txt",
        records: 1174,
        b9: 97,
        family: Family::F2,
        pids: &[0x0272, 0x0273],
        delivered: 1082,
        length_gate: 89,
    });
}

#[test]
fn input_f3a() {
    replay(&Expect {
        file: "input_f3a.txt",
        records: 1174,
        b9: 101,
        family: Family::F3a,
        pids: &[0x0276, 0x0277],
        delivered: 1079,
        length_gate: 92,
    });
}

#[test]
fn input_f3b() {
    replay(&Expect {
        file: "input_f3b.txt",
        records: 1140,
        b9: 92,
        family: Family::F3b,
        pids: &[0x0278],
        delivered: 1037,
        length_gate: 100,
    });
}
