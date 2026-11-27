//! Replays `fixtures/feature.txt`: `GET/SET_FEATURE` for all 256 report IDs, every SET payload byte
//! for 0x04 and 0x06, size and NULL edges, and the spec D.5 rows.

#![cfg(test)]

mod common;

use amtptp_hidfilter_core::{DeviceState, dispatch_internal_ioctl};
use common::*;
use ptp_hid::common::REPORTID_PTPHQA;
use ptp_hid::feature::{HQA_REPORT, HQA_REPORT_LEN};

const TOTAL: usize = 1588;

#[test]
fn replay_feature_reports() {
    let recs = load("feature.txt");
    assert_eq!(recs.len(), TOTAL);
    let mut tally = Tally::default();
    for r in &recs {
        let mut snap = started(0x004C, 4);
        snap.ptp_input_on = r.get("inp_on_in") == "1";
        snap.ptp_report_touch = r.get("touch_in") == "1";
        snap.ptp_report_button = r.get("btn_in") == "1";
        let state = DeviceState::with(&snap);

        let rblen = r.num("rblen") as usize;
        let buf_in = r.bytes("buf_in");
        let oob = r.opt("oob") == Some("1");
        // The physical buffer is at least `rblen` bytes (README 4.3), except `oob` (rblen < 2) and
        // SET `set-mode-rblen300`, where only 2 bytes were allocated; pad with zeros, which C never
        // reads there (SET reads byte 1 only).
        let mut buf = buf_in.clone();
        if buf.len() < rblen {
            assert_eq!(
                r.get("op"),
                "set",
                "{}: GET buffer shorter than rblen",
                r.case()
            );
            buf.resize(rblen, 0);
        }
        let mut m = Mock::new();
        let mut upper = Upper::with_output(0, 0);
        upper.feature = (r.get("ub") == "1").then(|| Feature {
            rid: r.num("rid") as u8,
            buf,
            rblen,
        });
        let req = m.add_upper(upper);
        dispatch_internal_ioctl(
            &mut m,
            &state,
            req,
            r.num("outlen") as usize,
            r.num("inlen") as usize,
            r.num("ioctl") as u32,
        );

        // The only framework call is the completion; Information is never set (HF-B21).
        let (s, info) = m.uppers[req].completed.expect("completed");
        assert_eq!(m.ev.len(), 1, "{}", r.case());
        assert_eq!(st(s), r.get("status"), "{}", r.case());
        assert_eq!(info as i128, r.num("info"), "{}", r.case());
        assert_eq!(info, INFO_IN);
        let mut buf_out = r.bytes("buf_out");
        if r.get("op") == "get" && r.num("rid") == i128::from(REPORTID_PTPHQA) && s == 0 {
            buf_out[..HQA_REPORT_LEN].copy_from_slice(&HQA_REPORT);
        }
        if let Some(f) = &m.uppers[req].feature {
            assert_eq!(f.buf[..buf_out.len()], buf_out[..], "{}", r.case());
        } else {
            assert_eq!(buf_out, buf_in);
        }

        let out = state.snapshot();
        let (c_inp, c_touch, c_btn) = (
            r.get("inp_on_out") == "1",
            r.get("touch_out") == "1",
            r.get("btn_out") == "1",
        );
        if oob {
            // HF-B23, port policy P2: C read reportBuffer[1] past reportBufferLen (it held 03 here). The
            // port reads the missing byte as 0: mode 0 = mouse (PtpInputOn FALSE) for 0x04, both
            // switches off for 0x06. The status is SUCCESS either way.
            assert_eq!(buf_in.get(1), Some(&0x03), "{}", r.case());
            match r.num("rid") {
                0x04 => {
                    assert!(c_inp, "C read 03");
                    assert!(!out.ptp_input_on, "{}", r.case());
                    assert_eq!(
                        (out.ptp_report_touch, out.ptp_report_button),
                        (c_touch, c_btn)
                    );
                }
                0x06 => {
                    assert!(c_touch && c_btn, "C read 03");
                    assert_eq!(out.ptp_input_on, c_inp);
                    assert!(
                        !out.ptp_report_touch && !out.ptp_report_button,
                        "{}",
                        r.case()
                    );
                }
                other => panic!("unexpected oob rid {other}"),
            }
            tally.add("P2-fixed: SET reads missing byte 1 as 0 (oob, HF-B23)");
        } else {
            assert_eq!(
                (
                    out.ptp_input_on,
                    out.ptp_report_touch,
                    out.ptp_report_button
                ),
                (c_inp, c_touch, c_btn),
                "{}",
                r.case()
            );
            tally.add("exact");
        }
        // Nothing else in the device state changes.
        let mut rest = out;
        rest.ptp_input_on = snap.ptp_input_on;
        rest.ptp_report_touch = snap.ptp_report_touch;
        rest.ptp_report_button = snap.ptp_report_button;
        assert_eq!(rest, snap);
    }
    let total = tally.assert(&[
        ("exact", 1584),
        ("P2-fixed: SET reads missing byte 1 as 0 (oob, HF-B23)", 4),
    ]);
    assert_eq!(total, TOTAL);
}
