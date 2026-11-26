//! Replays `fixtures/wire.txt`: the mode-switch `SET_FEATURE` bytes and the start / resume / timer /
//! PnP-restart sequences that send it, with fault injection.

#![cfg(test)]

mod common;

use amtptp_hidfilter_core::{
    DeviceState, configure_multitouch, d0_entry, d0_exit, prepare_hardware,
    recovery_timer_callback, self_managed_io_init, self_managed_io_restart,
};
use common::*;

const TOTAL: usize = 65;

fn apply_fault(m: &mut Mock, fault: &str) {
    match fault {
        "none" => {}
        "send_false" => m.faults.cfg_send_false = Some(SEND_FAIL),
        "req_create" => m.faults.cfg_create = Some(RESOURCE_FAIL),
        "irp_null" => m.faults.irp_null = true,
        "send_true_status_fail" => m.faults.cfg_send_true_status = Some(TRUE_STATUS_FAIL),
        "mem_prealloc" => m.faults.cfg_mem = Some(RESOURCE_FAIL),
        "format" => m.faults.cfg_format = Some(FORMAT_FAIL),
        other => panic!("unknown fault {other}"),
    }
}

#[test]
fn replay_wire() {
    let recs = load("wire.txt");
    assert_eq!(recs.len(), TOTAL);
    let mut tally = Tally::default();
    // start-X, resume-X, timer-X and pnp-restart-X run in that order on one device per
    // configuration, so `lower_reads_pending` accumulates within those groups.
    let mut pending_by_cfg: Vec<(String, i64)> = Vec::new();
    for r in &recs {
        let case = r.case();
        let snap_in = parse_state(r.get("st_in"));
        let state = DeviceState::with(&snap_in);
        let mut m = Mock::new();
        m.attr = match r.opt("attr_vid") {
            Some(v) => (
                num(v) as u16,
                r.num("attr_pid") as u16,
                r.num("attr_ver") as u16,
            ),
            // `start-{bt,usb}-<fault>` records carry no attr_* fields; the PDO answered with the
            // transport's VID, PID 0x0265 and version 0x0100 (their st_out confirms it).
            None if case.starts_with("start-bt-") => (0x004C, 0x0265, 0x0100),
            None if case.starts_with("start-usb-") => (0x05AC, 0x0265, 0x0100),
            None => (0, 0, 0),
        };
        apply_fault(&mut m, r.opt("fault").unwrap_or("none"));
        let group = ["start-", "resume-", "timer-", "pnp-restart-"]
            .iter()
            .find_map(|p| case.strip_prefix(p))
            .filter(|cfg| ["bt", "usb", "nonapple", "bt_pid0266"].contains(cfg))
            .map(str::to_owned);
        let before = group
            .as_ref()
            .and_then(|g| pending_by_cfg.iter().find(|(c, _)| c == g))
            .map_or(0, |(_, n)| *n);

        let ret = match r.get("op") {
            "PtpFilterSelfManagedIoInit" => Some(self_managed_io_init(&mut m, &state)),
            "PtpFilterSelfManagedIoRestart" => Some(self_managed_io_restart(&mut m, &state)),
            "PtpFilterRecoveryTimerCallback" => {
                recovery_timer_callback(&mut m, &state);
                None
            }
            "PtpFilterConfigureMultiTouch" => {
                assert_eq!(i128::from(snap_in.vendor_id), r.num("vid"));
                assert_eq!(i128::from(snap_in.product_id), r.num("pid"));
                Some(configure_multitouch(&mut m, &state))
            }
            "PtpFilterDeviceD0Exit+PtpFilterDeviceD0Entry+PtpFilterSelfManagedIoRestart" => {
                assert_eq!(d0_exit(&mut m, &state), 0);
                assert_eq!(d0_entry(), 0);
                Some(self_managed_io_restart(&mut m, &state))
            }
            "PtpFilterDeviceD0Exit+PtpFilterPrepareHardware+PtpFilterDeviceD0Entry+PtpFilterSelfManagedIoRestart" =>
            {
                assert_eq!(d0_exit(&mut m, &state), 0);
                assert_eq!(prepare_hardware(&state), 0);
                assert_eq!(d0_entry(), 0);
                Some(self_managed_io_restart(&mut m, &state))
            }
            other => panic!("{case}: unknown op {other}"),
        };
        match ret {
            Some(s) => assert_eq!(st(s), r.get("ret"), "{case}"),
            None => assert_eq!(r.get("ret"), "-"),
        }
        assert_eq!(m.ev, events(r), "{case}");
        assert_eq!(state.snapshot(), parse_state(r.get("st_out")), "{case}");
        assert_eq!(m.sends.len() as i128, r.num("sent"), "{case}");
        if let Some(s) = m.sends.last() {
            // The send fields restate the last WdfRequestSend event; check them individually too.
            if let Some(v) = r.opt("send_ioctl") {
                assert_eq!(num(v), 0x000B_0191);
                assert_eq!(r.num("send_in_len"), 32);
                assert_eq!(r.num("send_flags"), 0x2);
                assert_eq!(r.num("send_timeout"), 0);
                assert_eq!(
                    i128::from(u8::from(s.in_is_userbuffer)),
                    r.num("send_in_is_userbuffer")
                );
                assert_eq!(i128::from(s.xfer_id), r.num("xfer_id"), "{case}");
                assert_eq!(i128::from(s.xfer_len), r.num("xfer_len"), "{case}");
                assert_eq!(i128::from(s.rb_off), r.num("rb_off"), "{case}");
                assert_eq!(s.raw32, r.bytes("raw32"), "{case}");
            }
            assert_eq!(s.payload, r.bytes("payload"), "{case}");
        }
        if let Some(v) = r.opt("lower_reads_pending") {
            assert_eq!(i128::from(before + m.lower_reads_pending), num(v), "{case}");
        }
        if let Some(g) = group {
            let total = before + m.lower_reads_pending;
            match pending_by_cfg.iter_mut().find(|(c, _)| *c == g) {
                Some(e) => e.1 = total,
                None => pending_by_cfg.push((g, total)),
            }
        }
        tally.add(if r.opt("fault").is_some_and(|f| f != "none") {
            "fault-injected sequence (fault = README 5 status)"
        } else {
            "exact"
        });
    }
    let total = tally.assert(&[
        ("exact", 29),
        ("fault-injected sequence (fault = README 5 status)", 36),
    ]);
    assert_eq!(total, TOTAL);
}
