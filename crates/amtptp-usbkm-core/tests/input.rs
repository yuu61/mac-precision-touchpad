//! Replays `tests/fixtures/input_<row>.txt`: reader completion → PTP input report.
//!
//! Model of one record (README "input_<row>.txt"): bring-up to D0 for `pid` through the core lifecycle,
//! set the record's driver state (`st_in`), pend `npend` READ_REPORTs (the first with `outlen` bytes of
//! `prefill`), deliver the packet, and compare the first request's completion and buffer, the queue depth
//! and the state afterwards.

mod common;

use std::collections::VecDeque;

use amtptp_usbkm_core::input::{PacketAction, complete_read_report, process_packet};
use amtptp_usbkm_core::lifecycle;
use amtptp_usbkm_core::state::DeviceState;
use amtptp_usbkm_core::status::{Completion, STATUS_BUFFER_TOO_SMALL};
use common::fake::FakeHw;
use common::{Record, Tally, hex, kv, load, num, status, to_hex};

/// A pended READ_REPORT: its output buffer and how it ended.
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
    snap.last_report_time = st["lrt"].parse().expect("lrt");
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
        s.last_report_time
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

/// Replays one record; returns its bucket. `prev_out` is the previous record's `out`.
fn replay(case: &Case<'_>, prev_out: Option<&str>) -> &'static str {
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
    let mut first: Option<PendedRead> = None;

    // The reader completion, in the order the glue runs it.
    let qpc_out = num(case.get("qpc_freq")) as i64;
    let action = process_packet(&state, &packet, qpc_out);
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
                // WdfRequestRetrieveOutputMemory: zero-length output → STATUS_BUFFER_TOO_SMALL
                // (framework, README "Fake framework").
                let output = if req.buf.is_empty() {
                    Err(STATUS_BUFFER_TOO_SMALL)
                } else {
                    Ok(&mut req.buf[..])
                };
                req.completion = Some(complete_read_report(&result, output));
                first = Some(req);
            }
        },
    }
    // A first request that was not dequeued is still the first queued one.
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
            // There is no request. The harness printed a buffer the driver never touched: the previous
            // case's buffer (req-none) or untouched prefill (req-none-6fingers).
            let out = out_hex(case);
            let prefill_only = to_hex(&vec![prefill; outlen]);
            assert!(
                Some(out.as_str()) == prev_out || out == prefill_only,
                "{id}: out is a stale harness buffer"
            );
            bucket = "driver/no_request";
        }
        "leak_outmem" | "leak_copy" => {
            // C: retrieved and never completed (B6/B7). P2: completed with the status of the failing
            // call (WdfRequestRetrieveOutputMemory / WdfMemoryCopyFromBuffer: BUFFER_TOO_SMALL here).
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

    // The first request's buffer after the call.
    if !no_request {
        let expected = match case.get("out") {
            "unchanged" => vec![prefill; outlen],
            out => hex(out),
        };
        assert_eq!(
            first_buf.map(|b| to_hex(&b)),
            Some(to_hex(&expected)),
            "{id}: out"
        );
    }
    // `no_request`: `out` is the previous case's buffer (asserted above), not a request of this record.

    // Queue depth: one request dequeued unless the packet was discarded or no request was pending.
    let dequeued = usize::from(first.is_some());
    assert_eq!(
        num(case.get("qdepth_out")) as usize,
        npend - dequeued,
        "{id}: qdepth_out"
    );
    assert_eq!(queue.len(), npend - dequeued, "{id}");

    // The completion routine writes no context field.
    let st_out = case.get("st_out");
    let st_after = st_string(&state);
    assert_eq!(st_after, st_before, "{id}: state unchanged");
    if st_out != "same_as_st_in" {
        assert_eq!(st_out, st_after, "{id}: st_out");
    }

    if r.has_tag("unreachable_state") {
        // Gates off or LastReportTime != frequency: asserted, not skipped (the core keeps the gates and
        // the ScanTime arithmetic as inputs).
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
    for r in &records[1..] {
        let case = Case { r, defaults };
        tally.add(replay(&case, prev_out.as_deref()));
        prev_out = Some(out_hex(&case));
    }
    tally.assert_exact(name, total, expected);
}

/// Per file: 1 defaults line, 1000 random records, the edge groups. ffff has 11 extra fallback-PID
/// golden-B records.
fn expected(edge: usize) -> [(&'static str, usize); 7] {
    [
        ("defaults_line", 1),
        ("driver/edge", edge),
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
