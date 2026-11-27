//! Replays `tests/fixtures/input_<row>.txt`: reader completion -> PTP input report.
//!
//! Model of one record (README "input_<row>.txt"): bring-up to D0 for `pid` through the core lifecycle,
//! set the record's driver state (`st_in`), pend `npend` `READ_REPORTs` (the first with `outlen` bytes of
//! `prefill`), deliver the packet, and compare the first request's completion and buffer, the queue depth
//! and the state afterwards.

#![cfg(test)]

mod common;

use std::collections::VecDeque;

use amtptp_usbkm_core::config::T2Device;
use amtptp_usbkm_core::input::{PacketAction, complete_read_report, process_packet};
use amtptp_usbkm_core::lifecycle;
use amtptp_usbkm_core::state::{DeviceState, InputGates};
use amtptp_usbkm_core::status::{Completion, STATUS_BUFFER_TOO_SMALL};
use common::fake::FakeHw;
use common::{Record, Tally, hex, kv, load, num, status, to_hex};
use ptp_hid::contact_tracker::ContactTracker;
use ptp_hid::input::{PTP_REPORT_LEN, PTP_REPORT_SCAN_TIME_OFFSET};

/// A pended `READ_REPORT`: its output buffer and how it ended.
struct PendedRead {
    buf: Vec<u8>,
    completion: Option<Completion>,
}

/// Fields of a record, with the `kind=defaults` line applied to random records.
struct Case<'a> {
    r: &'a Record,
    defaults: &'a Record,
}

impl Case<'_> {
    fn get(&self, key: &str) -> &str {
        self.r
            .get(key)
            .or_else(|| self.defaults.get(key))
            .unwrap_or_else(|| panic!("{}: missing {key}", self.r.id()))
    }
}

fn state_for(case: &Case<'_>) -> DeviceState {
    let id = case.r.id();
    let pid = num(case.get("pid")) as u16;
    let state = DeviceState::new();
    state.on_device_create();
    let mut hw = FakeHw::new(pid);
    hw.qpc_freq = num(case.get("qpc_freq")) as i64;
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0, "{id}");
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0, "{id}");
    // Record state (`st_in`): wsm/touch/button/lrt.
    let st = kv(case.get("st_in"));
    let mut snap = state.snapshot();
    snap.wellspring_mode_on = st["wsm"] == "1";
    snap.report_touch = st["touch"] == "1";
    snap.report_button = st["button"] == "1";
    snap.qpc_frequency = st["lrt"].parse().expect("lrt");
    state.restore(snap);
    state
}

fn st_string(state: &DeviceState) -> String {
    let s = state.snapshot();
    format!(
        "wsm:{},touch:{},button:{},lrt:{}",
        u8::from(s.wellspring_mode_on),
        u8::from(s.report_touch),
        u8::from(s.report_button),
        s.qpc_frequency
    )
}

/// The record's `out` as hex (`unchanged` = `outlen` bytes of `prefill`).
fn out_hex(case: &Case<'_>) -> String {
    match case.get("out") {
        "unchanged" => to_hex(&vec![
            hex(case.get("prefill"))[0];
            num(case.get("outlen")) as usize
        ]),
        out => out.to_owned(),
    }
}

/// Independent Step 2 specification for frame-0 T2 Type4 report encoding, plus whether C's
/// single-frame output was unaffected by hover filtering, Wellspring `Confidence`, and clamping.
fn expected_frame0_report(
    cfg: &T2Device,
    packet: &[u8],
    gates: InputGates,
) -> ([u8; PTP_REPORT_LEN], bool) {
    let mut report = [0_u8; PTP_REPORT_LEN];
    report[0] = 0x05;
    report[49] = u8::from(gates.report_button && packet[31] != 0);
    if !gates.report_touch {
        return (report, true);
    }
    let x_span = cfg.profile.x.max - cfg.profile.x.min;
    let y_span = cfg.profile.y.max - cfg.profile.y.min;
    let mut count = 0_usize;
    let mut matches_c = true;
    let fingers = (packet.len() - 46) / 30;
    for i in 0..fingers {
        let f = 48 + 30 * i;
        let abs_x = i16::from_le_bytes([packet[f + 2], packet[f + 3]]);
        let abs_y = i16::from_le_bytes([packet[f + 4], packet[f + 5]]);
        let major = i32::from(i16::from_le_bytes([packet[f + 16], packet[f + 17]])) << 1;
        let minor = i32::from(i16::from_le_bytes([packet[f + 18], packet[f + 19]])) << 1;
        let unclamped_x = (i32::from(abs_x) - cfg.profile.x.min).max(0);
        let unclamped_y = (cfg.profile.y.max - i32::from(abs_y)).max(0);
        let tip = major >= 200 || minor >= 150;
        let conf = major < 2500 && minor < 2500;
        let c_conf = minor > 0;
        if !tip || conf != c_conf || unclamped_x > x_span || unclamped_y > y_span {
            matches_c = false;
        }
        if !tip || count >= 5 {
            continue;
        }
        let x = unclamped_x.min(x_span) as u16;
        let y = unclamped_y.min(y_span) as u16;
        let base = 1 + 9 * count;
        report[base] = u8::from(conf) | 0x02;
        report[base + 1..base + 5].copy_from_slice(&(count as u32).to_le_bytes());
        report[base + 5..base + 7].copy_from_slice(&x.to_le_bytes());
        report[base + 7..base + 9].copy_from_slice(&y.to_le_bytes());
        count += 1;
    }
    report[48] = count as u8;
    (report, matches_c)
}

/// Replays one record; returns its bucket. `prev_out` is the previous record's `out`.
fn replay(case: &Case<'_>, prev_out: Option<&str>, exact_c_matches: &mut usize) -> &'static str {
    let r = case.r;
    let id = r.id();
    let state = state_for(case);
    let st_before = st_string(&state);
    assert_eq!(st_before, case.get("st_in"), "{id}: st_in applied");

    let n = num(case.get("n")) as usize;
    let packet = hex(case.get("in"));
    assert_eq!(packet.len(), n, "{id}: n");
    let outlen = num(case.get("outlen")) as usize;
    let prefill = hex(case.get("prefill"))[0];
    let npend = num(case.get("npend")) as usize;
    assert_eq!(num(case.get("qdepth_in")) as usize, npend, "{id}");

    let mut queue: VecDeque<PendedRead> = (0..npend)
        .map(|i| PendedRead {
            buf: vec![prefill; if i == 0 { outlen } else { 50 }],
            completion: None,
        })
        .collect();

    // Step 2 (B-4): packets arriving with Wellspring mode off are discarded without dequeuing.
    let counter = num(case.get("qpc_freq")) as i64;
    let mut contacts = ContactTracker::new();
    let action = process_packet(&state, &mut contacts, &packet, counter);
    if !state.wellspring_mode_on() {
        assert_eq!(action, PacketAction::Discard, "{id}");
        assert_eq!(queue.len(), npend, "{id}");
        assert_eq!(st_string(&state), st_before, "{id}");
        return "b4_fixed/wsm0_discard";
    }

    let mut first: Option<PendedRead> = None;
    let res = case.get("res");
    let mut no_request = false;
    match action {
        PacketAction::Discard => assert_eq!(res, "drop_len", "{id}"),
        PacketAction::Deliver(result) => match queue.pop_front() {
            None => {
                assert_eq!(res, "no_request", "{id}");
                no_request = true;
            }
            Some(mut req) => {
                let output = if req.buf.is_empty() {
                    Err(STATUS_BUFFER_TOO_SMALL)
                } else {
                    Ok(&mut *req.buf)
                };
                req.completion = Some(complete_read_report(&result, output));
                first = Some(req);
            }
        },
    }
    let first_buf = match (&first, queue.front()) {
        (Some(f), _) => Some(f.buf.clone()),
        (None, Some(q)) if npend > 0 => Some(q.buf.clone()),
        _ => None,
    };
    let completion = first.as_ref().and_then(|f| f.completion);

    let mut bucket = if r.get("case").is_some_and(|c| c.contains("-rand-")) {
        "driver/random"
    } else {
        "driver/edge"
    };
    match res {
        "report" | "data_error" => {
            assert_eq!(case.get("completed"), "1", "{id}");
            assert_eq!(case.get("owned"), "0", "{id}");
            assert_eq!(
                completion,
                Some(Completion {
                    status: status(case.get("status")),
                    information: num(case.get("info")) as usize,
                }),
                "{id}"
            );
        }
        "drop_len" => {
            assert_eq!(case.get("completed"), "0", "{id}");
            assert!(completion.is_none(), "{id}");
        }
        "no_request" => {
            assert!(no_request);
            assert_eq!(case.get("completed"), "0", "{id}");
            let out = out_hex(case);
            let prefill_only = to_hex(&vec![prefill; outlen]);
            assert!(
                Some(out.as_str()) == prev_out || out == prefill_only,
                "{id}: out is a stale harness buffer"
            );
            bucket = "driver/no_request";
        }
        "leak_outmem" | "leak_copy" => {
            let tag = if res == "leak_outmem" {
                "c_leak_B6"
            } else {
                "c_leak_B7"
            };
            assert!(r.has_tag(tag), "{id}");
            assert_eq!(case.get("completed"), "0", "{id}");
            assert_eq!(case.get("owned"), "1", "{id}");
            assert_eq!(
                completion,
                Some(Completion::status(STATUS_BUFFER_TOO_SMALL)),
                "{id}: P2 completes the leaked request"
            );
            bucket = if tag == "c_leak_B6" {
                "p2_fixed/c_leak_B6"
            } else {
                "p2_fixed/c_leak_B7"
            };
        }
        other => panic!("{id}: unknown res {other}"),
    }

    if !no_request {
        let mut c_expected = match case.get("out") {
            "unchanged" => vec![prefill; outlen],
            out => hex(out),
        };
        if completion.is_some_and(|c| c.status == 0) {
            let buf = first_buf.expect("completed request buffer");
            let (spec_report, matches_c) =
                expected_frame0_report(state.config(), &packet, state.input_gates());
            assert_eq!(&buf[..PTP_REPORT_LEN], &spec_report, "{id}: spec report");
            assert_eq!(
                &buf[PTP_REPORT_LEN..],
                &c_expected[PTP_REPORT_LEN..],
                "{id}: trailing prefill"
            );
            c_expected[PTP_REPORT_SCAN_TIME_OFFSET..PTP_REPORT_SCAN_TIME_OFFSET + 2]
                .copy_from_slice(&0_u16.to_le_bytes());
            if matches_c {
                assert_eq!(to_hex(&buf), to_hex(&c_expected), "{id}: exact C match");
                *exact_c_matches += 1;
            }
        } else {
            assert_eq!(
                first_buf.map(|b| to_hex(&b)),
                Some(to_hex(&c_expected)),
                "{id}: out"
            );
        }
    }

    let dequeued = usize::from(first.is_some());
    assert_eq!(
        num(case.get("qdepth_out")) as usize,
        npend - dequeued,
        "{id}: qdepth_out"
    );
    assert_eq!(queue.len(), npend - dequeued, "{id}");

    let st_out = case.get("st_out");
    let st_after = st_string(&state);
    assert_eq!(st_after, st_before, "{id}: state unchanged");
    if st_out != "same_as_st_in" {
        assert_eq!(st_out, st_after, "{id}: st_out");
    }

    if r.has_tag("unreachable_state") {
        assert!(bucket == "driver/edge", "{id}");
        bucket = "driver/unreachable_state";
    }
    bucket
}

fn replay_file(name: &str, total: usize, expected: &[(&str, usize)]) {
    let records = load(name);
    let defaults = &records[0];
    assert_eq!(defaults.req("kind"), "defaults");
    assert_eq!(defaults.req("st_out"), "same_as_st_in");
    assert_eq!(defaults.req("owned"), "0");
    let mut tally = Tally::default();
    tally.add("defaults_line");
    let mut prev_out: Option<String> = None;
    let mut exact_c_matches = 0_usize;
    for r in &records[1..] {
        let case = Case { r, defaults };
        tally.add(replay(&case, prev_out.as_deref(), &mut exact_c_matches));
        prev_out = Some(out_hex(&case));
    }
    assert!(exact_c_matches > 0, "{name}: expected unaffected C-matching records");
    tally.assert_exact(name, total, expected);
}

const fn expected(edge: usize) -> [(&'static str, usize); 8] {
    [
        ("b4_fixed/wsm0_discard", 1),
        ("defaults_line", 1),
        ("driver/edge", edge - 1),
        ("driver/no_request", 2),
        ("driver/random", 1000),
        ("driver/unreachable_state", 22),
        ("p2_fixed/c_leak_B6", 2),
        ("p2_fixed/c_leak_B7", 2),
    ]
}

#[test]
fn input_027a() {
    replay_file("input_027a.txt", 1134, &expected(105));
}

#[test]
fn input_027b() {
    replay_file("input_027b.txt", 1134, &expected(105));
}

#[test]
fn input_027c() {
    replay_file("input_027c.txt", 1134, &expected(105));
}

#[test]
fn input_027d() {
    replay_file("input_027d.txt", 1134, &expected(105));
}

#[test]
fn input_ffff() {
    replay_file("input_ffff.txt", 1145, &expected(116));
}
