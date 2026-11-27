//! Replays `fixtures/ioctl.txt`: `FilterEvtIoIntDeviceControl` routing for every probed internal
//! IOCTL code on a started Bluetooth device.
//!
//! `IOCTL_HID_GET_STRING` changed with the data source: C forwarded it to the PDO (through the detour),
//! the port answers it from the strings it read from the vendor collection (`copy_string`).

#![cfg(test)]

mod common;

use amtptp_hidfilter_core::{DeviceState, dispatch_internal_ioctl};
use common::*;
use ptp_hid::ioctl::{IOCTL_HID_GET_FEATURE, IOCTL_HID_GET_STRING, IOCTL_HID_SET_FEATURE};

const TOTAL: usize = 519;

#[test]
fn replay_ioctl_routing() {
    let recs = load("ioctl.txt");
    assert_eq!(recs.len(), TOTAL);
    let mut tally = Tally::default();
    for r in &recs {
        let code = r.num("code") as u32;
        let outlen = r.num("outlen") as usize;
        let inlen = r.num("inlen") as usize;
        let state = DeviceState::with(&started(0x004C, 4));
        let mut m = Mock::new();
        // The UserBuffer is 1024 bytes prefilled with 0xCC; OutputBufferLength is `outlen`.
        let mut upper = Upper::with_output(1024, 0xCC);
        upper.out_len = outlen;
        upper.feature = match code {
            IOCTL_HID_GET_FEATURE => Some(Feature {
                rid: 0x07,
                buf: vec![0; 3],
                rblen: 3,
            }),
            IOCTL_HID_SET_FEATURE => Some(Feature {
                rid: 0x04,
                buf: vec![0x04, 0x03],
                rblen: 2,
            }),
            _ => None,
        };
        let req = m.add_upper(upper);
        if code == IOCTL_HID_GET_STRING {
            // The source's product string, "Magic Trackpad" and its NUL in UTF-16: 30 bytes.
            m.faults.string = Some(Ok(30));
            dispatch_internal_ioctl(&mut m, &state, req, outlen, inlen, code);
            assert_eq!(r.get("disp"), "forwarded", "{}", r.case());
            assert_eq!(
                m.ev,
                [
                    "CopyString(req=upper1)",
                    "WdfRequestSetInformation(req=upper1,info=30)",
                    "WdfRequestComplete(req=upper1,status=0x00000000,info=30)",
                ],
                "{}",
                r.case()
            );
            assert_eq!(m.uppers[req].completed, Some((0, 30)), "{}", r.case());
            tally.add("GET_STRING answered from the data source");
            continue;
        }
        dispatch_internal_ioctl(&mut m, &state, req, outlen, inlen, code);

        let u = &m.uppers[req];
        let disp = match (u.completed, m.read_queue.contains(&req)) {
            (Some(_), false) => "completed",
            (None, true) => "queued",
            (None, false) => {
                // Forwarded with send-and-forget: the lower driver now owns it.
                assert!(m.ev.iter().any(|e| e.contains("opts=0x8")), "{}", r.case());
                "forwarded"
            }
            (Some(_), true) => panic!("{}: completed and queued", r.case()),
        };
        assert_eq!(disp, r.get("disp"), "{}", r.case());
        if let Some((s, info)) = u.completed {
            assert_eq!(st(s), r.get("status"), "{}", r.case());
            assert_eq!(info as i128, r.num("info"), "{}", r.case());
        } else {
            assert_eq!(r.get("status"), "-");
            assert_eq!(r.get("info"), "-");
        }
        if let Some(changed) = r.opt("out_changed") {
            let any = u.buf.iter().any(|&b| b != 0xCC);
            assert_eq!(u8::from(any).to_string(), changed, "{}", r.case());
        }
        if r.opt("ev").is_some() {
            assert_eq!(m.ev, events(r), "{}", r.case());
            tally.add("routing + ev");
        } else {
            // Sweep codes record no ev: the only call must be the NOT_SUPPORTED completion.
            assert_eq!(m.ev.len(), 1, "{}", r.case());
            tally.add("routing (sweep, no ev)");
        }
    }
    let total = tally.assert(&[
        ("routing + ev", 27),
        ("GET_STRING answered from the data source", 1),
        ("routing (sweep, no ev)", 491),
    ]);
    assert_eq!(total, TOTAL);
}
