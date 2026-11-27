//! Replays `fixtures/input_bt.txt` and `fixtures/input_usb.txt`: the lower-read completion
//! (`PtpFilterInputRequestCompletionCallback`) on every edge and pseudo-random packet.

#![cfg(test)]

mod common;

use amtptp_hidfilter_core::{Bcm5974Param, DeviceSnapshot, DeviceState, complete_lower_read};
use common::*;
use ptp_hid::input::{PTP_REPORT_LEN, PTP_REPORT_SCAN_TIME_OFFSET};

const OUTMEM_P2: &str = "P2-fixed: upper completed with RetrieveOutputMemory status (HF-B9)";
const COPY_P2: &str = "P2-fixed: upper completed with CopyFromBuffer status (HF-B9)";

/// The exact `ev` sequence README 3.2 gives for each `act` (used for `rand` records, which omit `ev`).
fn ev_for_act(act: &str) -> Vec<String> {
    let deletes = [
        "WdfObjectDelete(obj=lower_read)",
        "WdfObjectDelete(obj=lower_mem)",
    ];
    let head: &[&str] = match act {
        "complete" => &[
            "WdfIoQueueRetrieveNextRequest(queue=queue_read)=0x00000000:upper1",
            "KeQueryPerformanceCounter(freq_ptr=null)",
            "WdfRequestRetrieveOutputMemory(req=upper1)=0x00000000",
            "WdfMemoryCopyFromBuffer(mem=upper_mem,off=0,len=50)=0x00000000",
            "WdfRequestSetInformation(req=upper1,info=50)",
            "WdfRequestComplete(req=upper1,status=0x00000000,info=50)",
        ],
        "workitem" => &["WdfWorkItemEnqueue(item=workitem)"],
        "timer3s" => &["WdfTimerStart(timer=timer,due=-30000000)"],
        "noupper" => &["WdfIoQueueRetrieveNextRequest(queue=queue_read)=0x8000001a"],
        "setfailed_norestart" => &["WdfDeviceSetFailed(dev=device,action=no_restart)"],
        other => panic!("rand record with act {other}"),
    };
    head.iter()
        .chain(deletes.iter())
        .map(std::string::ToString::to_string)
        .collect()
}

/// The C `ev` with the P2 completion of the leaked upper request inserted after `SetFailed`.
fn with_b9_completion(mut ev: Vec<String>) -> Vec<String> {
    let at = ev
        .iter()
        .position(|e| e == "WdfDeviceSetFailed(dev=device,action=attempt_restart)")
        .expect("SetFailed(attempt_restart)")
        + 1;
    ev.insert(
        at,
        format!("WdfRequestComplete(req=upper1,status=0xc0000023,info={INFO_IN})"),
    );
    ev
}

struct Outcome {
    ev: Vec<String>,
    completed: Option<(i32, usize)>,
    buf: Vec<u8>,
    state: DeviceSnapshot,
}

fn run(r: &Rec, snap: &DeviceSnapshot, tail: u8) -> Outcome {
    let state = DeviceState::with(snap);
    let mut m = Mock::new();
    m.counter = r.num("freq") as i64;
    let packet = r.bytes("in");
    m.lower_buf = vec![tail; 1024];
    m.lower_buf[..packet.len()].copy_from_slice(&packet);
    if r.get("upper") != "none" {
        let req = m.add_upper(Upper::with_output(r.num("upper") as usize, 0xEE));
        m.read_queue.push_back(req);
    }
    let information = usize::try_from(r.num("rinfo")).expect("64-bit ULONG_PTR");
    complete_lower_read(&mut m, &state, (), (), information);
    let (completed, buf) = match m.uppers.first() {
        Some(u) => (u.completed, u.buf.clone()),
        None => (None, Vec::new()),
    };
    Outcome {
        ev: m.ev,
        completed,
        buf,
        state: state.snapshot(),
    }
}

fn replay(file: &str, fam: &str, total: usize, acts: &[(&str, usize)]) {
    let recs = load(file);
    assert_eq!(recs.len(), total);
    let mut tally = Tally::default();
    let (mut undef_records, mut ub_records) = (0, 0);
    for r in &recs {
        assert_eq!(r.get("fam"), fam);
        let mut snap = started(r.num("vid") as u16, r.num("hdr") as usize);
        snap.input_finger_size = r.num("fsz") as usize;
        snap.input_finger_delta = r.num("fdelta") as usize;
        snap.input_button_delta = r.num("bdelta") as usize;
        snap.x = Bcm5974Param {
            min: r.num("xmin") as i32,
            ..snap.x
        };
        snap.y = Bcm5974Param {
            min: r.num("ymin") as i32,
            ..snap.y
        };
        snap.qpc_frequency = r.num("last_in") as i64;

        // The 1024-byte lookaside buffer after `in` is filled with 0x00 and then 0xFF: the port must
        // read nothing past `in` (README 3.1), so both runs must agree.
        let a = run(r, &snap, 0x00);
        let b = run(r, &snap, 0xFF);
        assert_eq!(a.ev, b.ev, "{}", r.case());
        assert_eq!(a.buf, b.buf, "{}: read past `in`", r.case());
        assert_eq!(a.state, b.state);

        let act = r.get("act");
        let c_ev = if r.get("kind") == "edge" {
            events(r)
                .into_iter()
                .map(|event| event.replace("freq_ptr=nonnull", "freq_ptr=null"))
                .collect()
        } else {
            ev_for_act(act)
        };
        let expected_ev = match act {
            "outmem_fail" | "copy_fail" => with_b9_completion(c_ev),
            _ => c_ev,
        };
        assert_eq!(a.ev, expected_ev, "{}", r.case());

        // Upper request outcome.
        match act {
            "complete" => {
                let (s, info) = a.completed.expect("completed");
                assert_eq!(st(s), r.get("status"), "{}", r.case());
                assert_eq!(info as i128, r.num("info"), "{}", r.case());
                // `out` has the C-uninitialised bits as 0; the port zero-initialises (HF-B4).
                let mut expected = r.bytes("out");
                expected[PTP_REPORT_SCAN_TIME_OFFSET..PTP_REPORT_SCAN_TIME_OFFSET + 2]
                    .copy_from_slice(&0_u16.to_le_bytes());
                assert_eq!(a.buf[..PTP_REPORT_LEN], expected, "{}", r.case());
                assert!(a.buf[50..].iter().all(|&x| x == 0xEE), "{}", r.case());
                if r.get("undef").bytes().any(|c| c != b'0') {
                    undef_records += 1;
                }
            }
            "outmem_fail" | "copy_fail" => {
                // C leaks the request (status=-); P2 completes it with the failure status, Information
                // untouched, nothing copied.
                assert_eq!(r.get("status"), "-");
                assert_eq!(a.completed, Some((0xC000_0023_u32 as i32, INFO_IN)));
                assert!(a.buf.iter().all(|&x| x == 0xEE));
            }
            _ => {
                assert_eq!(r.get("status"), "-");
                assert!(a.completed.is_none(), "{}", r.case());
                assert!(a.buf.iter().all(|&x| x == 0xEE));
            }
        }
        if r.opt("ub") == Some("int_min_neg") {
            ub_records += 1;
        }

        assert_eq!(a.state, snap, "{}", r.case());

        tally.add(match act {
            "outmem_fail" => OUTMEM_P2,
            "copy_fail" => COPY_P2,
            other => other,
        });
    }
    let n = tally.assert(acts);
    assert_eq!(n, total);
    println!(
        "{file}: {n} records, {undef_records} with uninitialised C bytes compared as zero, {ub_records} with ub=int_min_neg"
    );
    assert!(undef_records > 0 && ub_records > 0);
}

#[test]
fn replay_input_bt() {
    replay(
        "input_bt.txt",
        "BT",
        1653,
        &[
            ("complete", 1342),
            ("timer3s", 239),
            ("setfailed_norestart", 30),
            ("noupper", 26),
            ("workitem", 14),
            (OUTMEM_P2, 1),
            (COPY_P2, 1),
        ],
    );
}

#[test]
fn replay_input_usb() {
    replay(
        "input_usb.txt",
        "USB",
        1661,
        &[
            ("complete", 1384),
            ("timer3s", 219),
            ("setfailed_norestart", 24),
            ("noupper", 23),
            ("workitem", 9),
            (OUTMEM_P2, 1),
            (COPY_P2, 1),
        ],
    );
}

#[test]
fn lower_read_scan_time_uses_counter_instead_of_frequency() {
    let mut snap = started(0x004C, 4);
    snap.qpc_frequency = 24_000_000;
    let state = DeviceState::with(&snap);
    let mut m = Mock::new();
    m.lower_buf = vec![0x31, 0, 0, 0];
    for (counter, expected) in [(100_000, 0_u16), (102_400, 1), (716_800, 257)] {
        m.counter = counter;
        let req = m.add_upper(Upper::with_output(PTP_REPORT_LEN, 0xEE));
        m.read_queue.push_back(req);
        complete_lower_read(&mut m, &state, (), (), 4);
        assert_eq!(m.uppers[req].completed, Some((0, PTP_REPORT_LEN)));
        let offset = PTP_REPORT_SCAN_TIME_OFFSET;
        assert_eq!(m.uppers[req].buf[offset..offset + 2], expected.to_le_bytes());
    }
}
