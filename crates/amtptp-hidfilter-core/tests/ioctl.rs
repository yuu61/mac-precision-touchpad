//! Replays `fixtures/ioctl.txt`: `FilterEvtIoIntDeviceControl` routing for every probed internal
//! IOCTL code on a started Bluetooth device.

mod common;

use amtptp_hidfilter_core::{DeviceState, dispatch_internal_ioctl};
use common::*;
use ptp_hid::ioctl::{IOCTL_HID_GET_FEATURE, IOCTL_HID_SET_FEATURE};

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
        match u.completed {
            Some((s, info)) => {
                assert_eq!(st(s), r.get("status"), "{}", r.case());
                assert_eq!(info as i128, r.num("info"), "{}", r.case());
            }
            None => {
                assert_eq!(r.get("status"), "-");
                assert_eq!(r.get("info"), "-");
            }
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
    let total = tally.assert(&[("routing + ev", 28), ("routing (sweep, no ev)", 491)]);
    assert_eq!(total, TOTAL);
}
