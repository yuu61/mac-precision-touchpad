//! Replays `feature.txt`: `IOCTL_HID_GET_FEATURE` / `IOCTL_HID_SET_FEATURE` handling.
//!
//! GET: [`check_xfer_packet`] then [`get_feature`]; the reply's bytes are written to the start of the
//! report buffer as allocated (`rbuf`) and diffed against it (`chg`). SET: the flags `pio/prb/prt` are
//! first brought to `*_in` with SET requests, then the request runs through [`DeviceRuntime::set_feature`]
//! with the first `min(blen, 2)` bytes of `rbuf`. No record has a `wdf` failure: every status here comes
//! from driver code.
//!
//! SET 0x06 is fixed (P3, SPI-B2/B3): the switches come from byte 1 (C read byte 2), bit 0 is the Surface
//! Switch (`prt`) and bit 1 the Button Switch (`prb`), and a report without byte 1 changes nothing. Those
//! records are checked against that rule instead of the C flags.

use super::common;

use amtptp_core::hid::common::{REPORTID_FUNCSWITCH, REPORTID_PTPHQA};
use amtptp_core::hid::feature::{HQA_REPORT, HQA_REPORT_LEN, SelectiveReportBits};
use amtptp_core::spi::feature::{SET_FEATURE_READ_LEN, check_xfer_packet, get_feature};
use amtptp_core::spi::runtime::DeviceRuntime;
use amtptp_core::spi::status::{STATUS_INVALID_DEVICE_REQUEST, STATUS_SUCCESS};
use common::{Record, Tally, chg, load};

const GET_OK: &str = "GET: bytes written, success";
const GET_ERR: &str = "GET: driver error status";
const SET_OK: &str = "SET: success";
const SET_ERR: &str = "SET: driver error status";
const SET_SELECTIVE: &str = "P3: SET 0x06 decoded from byte 1 (Surface bit 0, Button bit 1)";

/// Brings the write-only flags to the record's `*_in` state through the public SET path.
fn preset(rt: &DeviceRuntime, r: &Record) {
    let pio = r.get("pio_in") == "1";
    let prb = r.get("prb_in") == "1";
    let prt = r.get("prt_in") == "1";
    let mode = if pio { 3_u8 } else { 0 };
    assert_eq!(rt.set_feature(4, 2, Some(&[4, mode])), STATUS_SUCCESS);
    let bits = u8::from(prt) | (u8::from(prb) << 1);
    assert_eq!(rt.set_feature(6, 2, Some(&[6, bits])), STATUS_SUCCESS);
    let s = rt.ptp_settings();
    assert_eq!(
        (s.input_on(), s.report_button(), s.report_touch()),
        (pio, prb, prt)
    );
}

fn flags(rt: &DeviceRuntime) -> (bool, bool, bool) {
    let s = rt.ptp_settings();
    (s.input_on(), s.report_button(), s.report_touch())
}

#[test]
fn feature_reports() {
    let recs = load("feature.txt");
    assert_eq!(recs.len(), 572);
    let mut tally = Tally::default();
    let (mut gets, mut sets) = (0, 0);
    for r in &recs {
        let ctx = format!("feature.txt:{} {}", r.line, r.case());
        assert_eq!(r.get("wdf"), "-", "{ctx}");
        assert_eq!(r.get("completed"), "1", "{ctx}");
        assert_eq!(r.get("info"), "0", "{ctx}: Information is never set");
        assert_eq!(r.get("infoset"), "0", "{ctx}");

        let rt = DeviceRuntime::new();
        preset(&rt, r);
        let before = flags(&rt);

        let user_buffer = r.get("ub") == "1";
        let report_id: u8 = r.dec("rid");
        let blen: u32 = r.dec("blen");
        let rb_present = r.get("rb") == "1";
        let rbuf = r.bytes("rbuf");
        assert!(
            rb_present || rbuf.is_empty(),
            "{ctx}: a NULL reportBuffer has no bytes"
        );
        let mut after = rbuf.clone();

        let status = match r.get("op") {
            "get" => {
                gets += 1;
                assert_eq!(r.hex32("code"), 0x000B_0192);
                let outlen: usize = r.dec("outlen");
                let status = if let Err(s) = check_xfer_packet(outlen, user_buffer) {
                    s
                } else {
                    let reply = get_feature(report_id, blen, rb_present);
                    // The glue copies `write` to the start of reportBuffer; never more.
                    after[..reply.write.len()].copy_from_slice(reply.write);
                    reply.status
                };
                tally.add(if status == STATUS_SUCCESS {
                    GET_OK
                } else {
                    GET_ERR
                });
                status
            }
            "set" => {
                sets += 1;
                assert_eq!(r.hex32("code"), 0x000B_0191);
                let inlen: usize = r.dec("inlen");
                let status = if let Err(s) = check_xfer_packet(inlen, user_buffer) {
                    s
                } else {
                    // The glue passes min(reportBufferLen, 2) bytes (None for a NULL buffer).
                    let take = (blen as usize).min(SET_FEATURE_READ_LEN).min(rbuf.len());
                    let report = rb_present.then(|| &rbuf[..take]);
                    rt.set_feature(report_id, blen, report)
                };
                if report_id == REPORTID_FUNCSWITCH && check_xfer_packet(inlen, user_buffer).is_ok()
                {
                    // P3: byte 1 decides; no byte 1 -> no change; byte 1 behind a NULL buffer -> error.
                    let expected = if blen < 2 || rb_present {
                        STATUS_SUCCESS
                    } else {
                        STATUS_INVALID_DEVICE_REQUEST
                    };
                    assert_eq!(status, expected, "{ctx}: status");
                    let (pio, _, _) = before;
                    let flags_after = if status == STATUS_SUCCESS && blen >= 2 {
                        let bits = SelectiveReportBits::from_byte(rbuf[1]);
                        (pio, bits.button_report, bits.surface_report)
                    } else {
                        before
                    };
                    assert_eq!(flags(&rt), flags_after, "{ctx}: flags");
                    assert_eq!(chg(&rbuf, &after), r.get("chg"), "{ctx}: chg");
                    tally.add(SET_SELECTIVE);
                    continue;
                }
                tally.add(if status == STATUS_SUCCESS {
                    SET_OK
                } else {
                    SET_ERR
                });
                status
            }
            other => panic!("{ctx}: op {other}"),
        };
        assert_eq!(status, r.status("status"), "{ctx}: status");
        if r.get("op") == "get" && report_id == REPORTID_PTPHQA && status == STATUS_SUCCESS {
            let mut expected = rbuf;
            expected[..HQA_REPORT_LEN].copy_from_slice(&HQA_REPORT);
            assert_eq!(after, expected, "{ctx}: certification report");
        } else {
            assert_eq!(chg(&rbuf, &after), r.get("chg"), "{ctx}: chg");
        }
        let expect_flags = (
            r.get("pio_out") == "1",
            r.get("prb_out") == "1",
            r.get("prt_out") == "1",
        );
        assert_eq!(flags(&rt), expect_flags, "{ctx}: flags (before {before:?})");
    }
    assert_eq!((gets, sets), (278, 294));
    assert_eq!(tally.get(SET_SELECTIVE), 13);
    assert_eq!(tally.get(GET_OK), 12);
    assert_eq!(tally.get(GET_ERR), 266);
    assert_eq!(tally.get(SET_OK), 21);
    assert_eq!(tally.get(SET_ERR), 260);
    assert_eq!(tally.total(), 572);
}
