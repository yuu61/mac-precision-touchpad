//! Replays `input_f{1,2,3a,3b}.txt`: lower `IOCTL_HID_READ_REPORT` completion -> PTP input report.
//!
//! Each record runs [`DeviceRuntime::complete_lower_read`] on a Configured device whose `LastReportTime`
//! is `lrt_in`, with the lookaside buffer = `in` + zeros to 1024 bytes, `Information` = `len` and the
//! QPC out-parameter = `qpcf`. The upstream copy (`WdfRequestRetrieveOutputMemory` +
//! `WdfMemoryCopyFromBuffer`) is the KMDF model from the fixture README.
//!
//! Decisions checked here:
//! - SPI-B5 (P2): the report is zero-initialized, so every bit flagged in `undef` must be 0.
//! - P3 (SPI-B6/B9): a frame whose valid bytes do not hold every finger record `NumOfFingers` declares is
//!   rejected with `STATUS_DEVICE_DATA_ERROR` before the clock is sampled (C delivered it, reading the
//!   missing fingers as stale or zero bytes). Every `b9=1` record (C read stale bytes past `len`) is such
//!   a frame. These records are counted, not compared with the C bytes.
//! - Step 2 (A-2.1 / A-2.4): on frame 0 (`ContactTracker::new()`), hover records (`pressure <= 0`) are
//!   ignored, active contacts are packed into slots `0..count` (up to 5), and X/Y are clamped to
//!   `0..=(max - min)`. Records unaffected by those fixes are also checked byte-for-byte against `out`.
//! - Each isolated frame begins a new reporting session, so its `ScanTime` is zero.

use super::common;

use amtptp_core::hid::input::SPI_PTP_REPORT_SCAN_TIME_OFFSET;
use amtptp_core::spi::device::{
    APPLE_VENDOR_ID, Family, SpiTrackpadInfo, family_for_product, lookup_trackpad,
};
use amtptp_core::spi::input::{MAX_FILLED_CONTACTS, packet_len_for};
use amtptp_core::spi::runtime::{ContactTracker, DeviceRuntime, DeviceStatus, ReadTracker};
use amtptp_core::spi::status::{STATUS_BUFFER_TOO_SMALL, STATUS_DEVICE_DATA_ERROR, STATUS_SUCCESS};
use amtptp_core::spi::wire::LOWER_READ_BUFFER_LEN;
use common::{Tally, load, prefill, wdf_failures};

const REPORT_LEN: usize = 30;

const DELIVERED: &str = "report delivered (status 0, info 30)";
const LENGTH_GATE: &str = "driver length gate: (LONG)len < 46 -> c000009c";
const FW_RETRIEVE: &str = "framework: WdfRequestRetrieveOutputMemory c0000023 (outlen 0)";
const FW_COPY: &str = "framework: WdfMemoryCopyFromBuffer c0000023 (outlen < 30)";
const SHORT_FRAME: &str = "P3: contact data shorter than NumOfFingers declares -> c000009c";
/// Offset of `ContactCount` in the 30-byte SPI report.
const CONTACT_COUNT_OFFSET: usize = 28;

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

/// Independent Step 2 specification for frame-0 SPI report encoding, plus whether C's single-frame
/// output was unaffected by hover filtering and upper-bound clamping.
fn expected_frame0_report(buffer: &[u8], info: &SpiTrackpadInfo) -> ([u8; REPORT_LEN], bool) {
    assert!(buffer.len() > 30);
    let mut report = [0_u8; REPORT_LEN];
    report[0] = 0x05;
    report[29] = buffer[1];
    let declared = usize::from(buffer[30]);
    let x_span = i32::from(info.x_max) - i32::from(info.x_min);
    let y_span = i32::from(info.y_max) - i32::from(info.y_min);
    let mut count = 0_usize;
    let mut matches_c = true;
    for i in 0..declared {
        let finger = &buffer[(46 + 30 * i)..(76 + 30 * i)];
        assert!(finger.len() > 27);
        let raw_x = i16::from_le_bytes([finger[4], finger[5]]);
        let raw_y = i16::from_le_bytes([finger[6], finger[7]]);
        let major = i16::from_le_bytes([finger[18], finger[19]]);
        let minor = i16::from_le_bytes([finger[20], finger[21]]);
        let pressure = i16::from_le_bytes([finger[26], finger[27]]);
        let unclamped_x = (i32::from(raw_x) - i32::from(info.x_min)).max(0);
        let unclamped_y = (i32::from(info.y_max) - i32::from(raw_y)).max(0);
        if i < usize::from(MAX_FILLED_CONTACTS)
            && (pressure <= 0 || unclamped_x > x_span || unclamped_y > y_span)
        {
            matches_c = false;
        }
        if pressure <= 0 || count >= usize::from(MAX_FILLED_CONTACTS) {
            continue;
        }
        let x = unclamped_x.min(x_span) as u16;
        let y = unclamped_y.min(y_span) as u16;
        let conf = major < 2500 && minor < 2500;
        let slot = &mut report[(1 + 5 * count)..(6 + 5 * count)];
        assert!(slot.len() > 4);
        slot[0] = u8::from(conf) | 0x02 | ((count as u8) << 2);
        slot[1..3].copy_from_slice(&x.to_le_bytes());
        slot[3..5].copy_from_slice(&y.to_le_bytes());
        count += 1;
    }
    report[CONTACT_COUNT_OFFSET] = count as u8;
    (report, matches_c)
}

fn verify_output_and_undef(
    r: &common::Record,
    ctx: &str,
    buffer: &[u8],
    info: &SpiTrackpadInfo,
    status: i32,
    out: &[u8],
) -> bool {
    assert!(!r.has("out_tail0"), "{ctx}");
    let num_of_fingers = buffer[30];
    let mut c_expected = common::hex(r.get("out"));
    let mut exact_c_match = false;
    if status == STATUS_SUCCESS {
        let (spec_report, matches_c) = expected_frame0_report(buffer, info);
        assert_eq!(&out[..REPORT_LEN], &spec_report, "{ctx}: spec report");
        assert_eq!(
            &out[REPORT_LEN..],
            &c_expected[REPORT_LEN..],
            "{ctx}: trailing prefill"
        );
        c_expected[SPI_PTP_REPORT_SCAN_TIME_OFFSET..SPI_PTP_REPORT_SCAN_TIME_OFFSET + 2]
            .copy_from_slice(&0_u16.to_le_bytes());
        assert_eq!(
            c_expected[CONTACT_COUNT_OFFSET], num_of_fingers,
            "{ctx}: C count"
        );
        c_expected[CONTACT_COUNT_OFFSET] = num_of_fingers.min(MAX_FILLED_CONTACTS);
        if matches_c {
            assert_eq!(out, c_expected, "{ctx}: exact C match");
            exact_c_match = true;
        }
    } else {
        assert_eq!(out, c_expected, "{ctx}: out");
    }

    // SPI-B5 (P2): every reserved bit in contact byte 0 and every unused contact slot is zero.
    let actual_count = if status == STATUS_SUCCESS {
        out[CONTACT_COUNT_OFFSET]
    } else {
        0
    };
    if let Some(undef) = r.opt("undef").filter(|u| *u != "-") {
        let undef = common::hex(undef);
        assert_eq!(undef.len(), out.len(), "{ctx}: undef length");
        let n = r.bytes("in").get(30).copied().unwrap_or(0);
        assert_eq!(undef[..REPORT_LEN], undef_rule(n), "{ctx}: undef rule");
        assert!(undef[REPORT_LEN..].iter().all(|&b| b == 0), "{ctx}");
        if status != STATUS_SUCCESS {
            for (o, m) in out.iter().zip(&undef) {
                assert_eq!(o & m, 0, "{ctx}: uninitialized bit not zero");
            }
        }
    }
    if status == STATUS_SUCCESS {
        for (o, m) in out[..REPORT_LEN].iter().zip(undef_rule(actual_count)) {
            assert_eq!(o & m, 0, "{ctx}: reserved/unused bit not zero");
        }
    }
    exact_c_match
}

struct Expect {
    file: &'static str,
    records: usize,
    b9: usize,
    family: Family,
    pids: &'static [u16],
    delivered: usize,
    length_gate: usize,
    short_frames: usize,
    fw_retrieve: usize,
    fw_copy: usize,
}

fn replay(e: &Expect) -> Tally {
    let recs = load(e.file);
    assert_eq!(recs.len(), e.records, "{}: record count", e.file);
    let mut tally = Tally::default();
    let mut b9 = 0_usize;
    let mut exact_c_matches = 0_usize;
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
        let mut contacts = ContactTracker::new();
        let result = rt.complete_lower_read(info, &mut contacts, len, &buffer, || {
            qpc_calls += 1;
            qpcf
        });
        let is_b9 = r.opt("b9") == Some("1");
        b9 += usize::from(is_b9);

        // P3: the valid bytes must hold every declared finger record.
        let num_of_fingers = buffer[30];
        let gate_len = len as u32 as i32;
        let valid = usize::try_from(gate_len).map_or(0, |l| l.min(LOWER_READ_BUFFER_LEN));
        if gate_len >= 46 && valid < packet_len_for(num_of_fingers) {
            assert_eq!(result, Err(STATUS_DEVICE_DATA_ERROR), "{ctx}: short frame");
            assert_eq!(qpc_calls, 0, "{ctx}: no clock sample for a rejected frame");
            tally.add(SHORT_FRAME);
            continue;
        }
        assert!(
            !is_b9,
            "{ctx}: a b9 record reads past len, so it is a short frame"
        );

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

        exact_c_matches += usize::from(verify_output_and_undef(
            r, &ctx, &buffer, info, status, &out,
        ));
    }
    assert!(
        exact_c_matches > 0,
        "{}: expected unaffected C-matching records",
        e.file
    );
    assert_eq!(b9, e.b9, "{}: b9 records", e.file);
    assert_eq!(tally.get(FW_RETRIEVE), e.fw_retrieve, "{}", e.file);
    assert_eq!(tally.get(FW_COPY), e.fw_copy, "{}", e.file);
    assert_eq!(tally.get(DELIVERED), e.delivered, "{}", e.file);
    assert_eq!(tally.get(LENGTH_GATE), e.length_gate, "{}", e.file);
    assert_eq!(tally.get(SHORT_FRAME), e.short_frames, "{}", e.file);
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
        delivered: 834,
        length_gate: 103,
        short_frames: 302,
        fw_retrieve: 1,
        fw_copy: 2,
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
        delivered: 756,
        length_gate: 89,
        short_frames: 326,
        fw_retrieve: 1,
        fw_copy: 2,
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
        delivered: 778,
        length_gate: 92,
        short_frames: 301,
        fw_retrieve: 1,
        fw_copy: 2,
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
        delivered: 701,
        length_gate: 100,
        short_frames: 336,
        fw_retrieve: 1,
        fw_copy: 2,
    });
}
