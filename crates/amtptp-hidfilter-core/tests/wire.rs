//! Replays `fixtures/wire.txt`: the mode-switch `SET_FEATURE` bytes and the start / resume / timer /
//! PnP-restart sequences that send it, with fault injection.
//!
//! Three C behaviours are fixed (P3), and the replay derives the corrected expectation from the record:
//! - HF-B13: every synchronous mode-switch send is followed by `WdfRequestGetStatus`, and a send that
//!   returned TRUE with a failed lower status is a failed mode switch (`send_true_status_fail`);
//! - the recovery timer works only on a device in D0, does nothing but the backlog for a configured
//!   device, and no longer issues a lower read after a failed mode switch (`timer-*`; HF-B12);
//! - HF-B30: `PrepareHardware` keeps the IDs, so a PnP restart behaves like a resume (`pnp-restart-*`),
//!   except that it also turns both selective-reporting switches back on.
//!
//! The start changed with the data source (the HIDCLASS detour is gone, `amtptp_hidfilter_core::bth`):
//! `SelfManagedIoInit` sets the static Bluetooth identity, and the mode switch is sent when the source
//! opens. A Bluetooth start record is replayed as init + `source_opened` and expects the C sequence
//! without the detour and the attribute query (version 0: nothing reads it); the start records of other
//! identities are counted as removed, and a restart before the source opened does nothing.

#![cfg(test)]

mod common;

use amtptp_hidfilter_core::{DeviceState, NtStatus};
use common::pnp::{
    configure_multitouch, d0_entry, d0_exit, prepare_hardware, recovery_timer_callback,
    self_managed_io_init, self_managed_io_restart, source_opened,
};
use common::*;

const TOTAL: usize = 65;
const EXACT: &str = "exact (plus WdfRequestGetStatus after a TRUE send)";
const FAULT: &str = "fault-injected sequence (fault = README 5 status)";
const P3_SEND_STATUS: &str = "P3 HF-B13: TRUE send with a failed lower status";
const P3_TIMER: &str = "P3: recovery timer gated to D0, no read after a failed switch";
const P3_PNP_RESTART: &str = "P3 HF-B30: PnP restart keeps the IDs (= resume)";
const SOURCE_START: &str =
    "data source: Bluetooth start = init + source opened (no detour, no attribute query)";
const REMOVED_IDENTITY: &str =
    "removed: start of a non-Bluetooth identity (the identity is static now)";
const SOURCE_RESTART: &str = "data source: restart before the source opened does nothing";

/// The Bluetooth start records (VID 0x004C, PID 0x0265), with or without a fault.
fn is_bluetooth_start(case: &str) -> bool {
    case == "start-bt" || case.starts_with("start-bt-")
}

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

/// What the port must do for one record.
struct Expected<'a> {
    ret: String,
    events: Vec<String>,
    st_out: String,
    sent: i128,
    /// Lower reads this record issues (the C value is the accumulated `lower_reads_pending`).
    new_reads: Option<i64>,
    bucket: &'static str,
    /// The record whose `send_*` / `payload` fields describe the last mode-switch send.
    send_record: &'a Rec,
}

/// Inserts `WdfRequestGetStatus` after every mode-switch send that returned TRUE (HF-B13).
fn with_status_reads(events: &[String], status: NtStatus) -> Vec<String> {
    let mut out = Vec::with_capacity(events.len() + 1);
    for e in events {
        out.push(e.clone());
        if e.starts_with("WdfRequestSend(req=cfg_req,") && e.ends_with(")=TRUE") {
            out.push(format!("WdfRequestGetStatus(req=cfg_req)={}", st(status)));
        }
    }
    out
}

/// Replaces the `key:value` entry of a state string.
fn with_state(state: &str, key: &str, value: &str) -> String {
    state
        .split(',')
        .map(|pair| match pair.split_once(':') {
            Some((k, _)) if k == key => format!("{k}:{value}"),
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn state_value<'a>(state: &'a str, key: &str) -> &'a str {
    state
        .split(',')
        .find_map(|pair| pair.strip_prefix(&format!("{key}:")))
        .unwrap_or_else(|| panic!("{key} in {state}"))
}

const TIMER_START: &str = "WdfTimerStart(timer=timer,due=-20000000)";

fn expected<'a>(r: &'a Rec, recs: &'a [Rec]) -> Expected<'a> {
    let case = r.case();
    let fault = r.opt("fault").unwrap_or("none");
    let c_events = events(r);
    let exact = Expected {
        ret: r.get("ret").to_owned(),
        events: with_status_reads(&c_events, 0),
        st_out: r.get("st_out").to_owned(),
        sent: r.num("sent"),
        new_reads: None,
        bucket: if fault == "none" { EXACT } else { FAULT },
        send_record: r,
    };
    if case.starts_with("timer-") {
        let st_in = r.get("st_in");
        let (events, st_out) = if state_value(st_in, "cfg") == "1" {
            // Configured: only the backlog, and nothing is parked.
            (Vec::new(), st_in.to_owned())
        } else {
            // The failed mode switch re-arms the timer; C then issued a lower read anyway.
            let read_starts = c_events
                .iter()
                .position(|e| e.starts_with("WdfRequestCreate(ctx=WORKER_REQUEST_CONTEXT"))
                .expect("C issued a lower read");
            (c_events[..read_starts].to_vec(), r.get("st_out").to_owned())
        };
        let sent = events
            .iter()
            .filter(|e| e.starts_with("WdfRequestSend(req=cfg_req,"))
            .count() as i128;
        return Expected {
            ret: "-".into(),
            events,
            st_out,
            sent,
            new_reads: Some(0),
            bucket: P3_TIMER,
            send_record: r,
        };
    }
    if let Some(cfg) = case.strip_prefix("pnp-restart-") {
        let resume = recs
            .iter()
            .find(|x| x.case() == format!("resume-{cfg}"))
            .expect("matching resume record");
        assert_eq!(
            resume.get("st_in"),
            r.get("st_in"),
            "{case}: same start state"
        );
        // PrepareHardware also restores input mode and both selective-reporting switches to on.
        let st_out = with_state(
            &with_state(
                &with_state(resume.get("st_out"), "inp_on", "1"),
                "touch",
                "1",
            ),
            "btn",
            "1",
        );
        return Expected {
            ret: resume.get("ret").to_owned(),
            events: with_status_reads(&events(resume), 0),
            st_out,
            sent: resume.num("sent"),
            new_reads: Some(0),
            bucket: P3_PNP_RESTART,
            send_record: resume,
        };
    }
    if fault == "send_true_status_fail" {
        let mut events = with_status_reads(&c_events, TRUE_STATUS_FAIL);
        if case.starts_with("cfg-") {
            return Expected {
                ret: st(TRUE_STATUS_FAIL),
                events,
                bucket: P3_SEND_STATUS,
                ..exact
            };
        }
        // SelfManagedIoInit/Restart: the failed switch arms the timer instead of configuring the device.
        let qpc = events
            .iter()
            .position(|e| e == "KeQueryPerformanceCounter(freq_ptr=nonnull)")
            .expect("C configured the device");
        events[qpc] = TIMER_START.to_owned();
        let st_in = r.get("st_in");
        let st_out = with_state(
            &with_state(r.get("st_out"), "cfg", state_value(st_in, "cfg")),
            "last",
            state_value(st_in, "last"),
        );
        return Expected {
            ret: st(0),
            events,
            st_out,
            bucket: P3_SEND_STATUS,
            ..exact
        };
    }
    exact
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
        let op = r.get("op");
        if op == "PtpFilterSelfManagedIoInit" && !is_bluetooth_start(case) {
            tally.add(REMOVED_IDENTITY);
            continue;
        }
        let mut want = expected(r, &recs);
        if op == "PtpFilterSelfManagedIoInit" {
            // C: IoGetLowerDeviceObject, WdfDeviceGetIoTarget, ObfDereferenceObject, then the attribute
            // query; the rest (the mode switch and what follows) is unchanged.
            let attributes = want
                .events
                .iter()
                .position(|e| e.starts_with("WdfIoTargetSendInternalIoctlSynchronously("))
                .expect("C queried the attributes");
            want.events.drain(..=attributes);
            want.st_out = with_state(&want.st_out, "ver", "0x0000");
            want.bucket = SOURCE_START;
        }
        if case == "restart-without-detour" {
            want = Expected {
                ret: st(0),
                events: Vec::new(),
                st_out: r.get("st_in").to_owned(),
                sent: 0,
                new_reads: Some(0),
                bucket: SOURCE_RESTART,
                send_record: r,
            };
        }
        let snap_in = parse_state(r.get("st_in"));
        let state = DeviceState::with(&snap_in);
        let mut m = Mock::new();
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

        // Every record but D0Exit-first ones runs on a device in D0 (the C model has no D0 state).
        let ret = match op {
            "PtpFilterSelfManagedIoInit" => {
                assert_eq!(d0_entry(&state), 0);
                let ret = self_managed_io_init(&mut m, &state);
                source_opened(&mut m, &state);
                Some(ret)
            }
            "PtpFilterSelfManagedIoRestart" => {
                assert_eq!(d0_entry(&state), 0);
                Some(self_managed_io_restart(&mut m, &state))
            }
            "PtpFilterRecoveryTimerCallback" => {
                assert_eq!(d0_entry(&state), 0);
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
                assert_eq!(d0_entry(&state), 0);
                Some(self_managed_io_restart(&mut m, &state))
            }
            "PtpFilterDeviceD0Exit+PtpFilterPrepareHardware+PtpFilterDeviceD0Entry+PtpFilterSelfManagedIoRestart" =>
            {
                assert_eq!(d0_exit(&mut m, &state), 0);
                assert_eq!(prepare_hardware(&state), 0);
                assert_eq!(d0_entry(&state), 0);
                Some(self_managed_io_restart(&mut m, &state))
            }
            other => panic!("{case}: unknown op {other}"),
        };
        match ret {
            Some(s) => assert_eq!(st(s), want.ret, "{case}"),
            None => assert_eq!(want.ret, "-"),
        }
        assert_eq!(m.ev, want.events, "{case}");
        assert_eq!(state.snapshot(), parse_state(&want.st_out), "{case}");
        assert_eq!(m.sends.len() as i128, want.sent, "{case}");
        if let Some(s) = m.sends.last() {
            let r = want.send_record;
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
        match want.new_reads {
            Some(n) => assert_eq!(m.lower_reads_pending, n, "{case}: lower reads issued"),
            None => {
                if let Some(v) = r.opt("lower_reads_pending") {
                    assert_eq!(i128::from(before + m.lower_reads_pending), num(v), "{case}");
                }
            }
        }
        if let Some(g) = group {
            let total = before + m.lower_reads_pending;
            match pending_by_cfg.iter_mut().find(|(c, _)| *c == g) {
                Some(e) => e.1 = total,
                None => pending_by_cfg.push((g, total)),
            }
        }
        tally.add(want.bucket);
    }
    let total = tally.assert(&[
        (EXACT, 16),
        (FAULT, 16),
        (P3_SEND_STATUS, 4),
        (P3_TIMER, 12),
        (P3_PNP_RESTART, 4),
        (SOURCE_START, 5),
        (REMOVED_IDENTITY, 7),
        (SOURCE_RESTART, 1),
    ]);
    assert_eq!(total, TOTAL);
}
