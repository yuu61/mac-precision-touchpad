//! Replays `fixtures/input_bt.txt` and `fixtures/input_usb.txt`: the lower-read completion
//! (`PtpFilterInputRequestCompletionCallback`) on every edge and pseudo-random packet.
//!
//! Every record completes successfully (C never read the status). Two P3 fixes change the outcome of
//! some records, which are checked against the outcome the port defines and counted separately:
//! `Information` above the 1024-byte buffer (HF-B29) and a first byte that is not the transport's
//! multitouch report ID (HF-B8).

use super::common;

use amtptp_usb_bt::bluetooth::{
    Bcm5974Param, DeviceSnapshot, DeviceState, HID_VID_APPLE_USB, REPORT_BUFFER_SIZE, ReadTicket,
    STATUS_SUCCESS, multitouch_report_id,
};
use common::pipeline::complete_lower_read;
use common::*;

use amtptp_core::hid::common::MAX_FINGERS;
use amtptp_core::hid::input::{PTP_REPORT_LEN, PTP_REPORT_SCAN_TIME_OFFSET};

const OUTMEM_P2: &str = "P2-fixed: upper completed with RetrieveOutputMemory status (HF-B9)";
const COPY_P2: &str = "P2-fixed: upper completed with CopyFromBuffer status (HF-B9)";
const OVERSIZE_P3: &str =
    "P3-fixed: Information above the buffer starts the 3 s timer instead (HF-B29)";
const OTHER_REPORT_P3: &str =
    "P3-fixed: another report ID than multitouch enqueues the work item instead (HF-B8)";

/// The port's `act` for a record whose C outcome was `c_act`, and the P3 bucket if they differ. The
/// vendor check comes first and a zero length is an empty read, as in C; then `Information` above the
/// buffer, then the report ID.
fn port_act<'a>(r: &Rec, c_act: &'a str) -> (&'a str, Option<&'static str>) {
    let information = r.num("rinfo");
    if c_act == "setfailed_norestart" || information == 0 {
        return (c_act, None);
    }
    let diverge = |act: &'a str, bucket| {
        if act == c_act {
            (c_act, None)
        } else {
            (act, Some(bucket))
        }
    };
    if information > REPORT_BUFFER_SIZE as i128 {
        return diverge("timer3s", OVERSIZE_P3);
    }
    let vendor = r.num("vid") as u16;
    assert!(vendor == HID_VID_APPLE_USB || r.get("fam") == "BT");
    if r.bytes("in").first().copied() != Some(multitouch_report_id(vendor)) {
        return diverge("workitem", OTHER_REPORT_P3);
    }
    (c_act, None)
}

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

/// The C `ev` with per-request `SetFailed(attempt_restart)` replaced by completing the upper request
/// with its failure status (HF-B9 + Stage 2 §4: no device restart on single-request buffer error).
fn with_b9_completion(mut ev: Vec<String>) -> Vec<String> {
    let at = ev
        .iter()
        .position(|e| e == "WdfDeviceSetFailed(dev=device,action=attempt_restart)")
        .expect("SetFailed(attempt_restart)");
    ev[at] = format!("WdfRequestComplete(req=upper1,status=0xc0000023,info={INFO_IN})");
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
    complete_lower_read(
        &mut m,
        &state,
        0,
        (),
        ReadTicket::default(),
        STATUS_SUCCESS,
        information,
    );
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

/// Independent Step 2 specification for frame-0 Magic Trackpad 2 report encoding over `snap`.
fn expected_frame0_mt2_report(
    snap: &DeviceSnapshot,
    packet: &[u8],
    rinfo: usize,
) -> [u8; PTP_REPORT_LEN] {
    let mut report = [0_u8; PTP_REPORT_LEN];
    report[0] = 0x05;
    let byte_at = |base: Option<usize>, off: usize| -> u8 {
        base.and_then(|b| b.checked_add(off))
            .and_then(|i| (i < rinfo).then(|| packet.get(i).copied().unwrap_or(0)))
            .unwrap_or(0)
    };
    report[49] = byte_at(Some(snap.input_button_delta), 0) & 1;

    let raw_count = rinfo
        .checked_sub(snap.input_header_size)
        .and_then(|p| p.checked_div(snap.input_finger_size))
        .unwrap_or(0)
        .min(usize::from(MAX_FINGERS));
    let first = snap.input_header_size.checked_add(snap.input_finger_delta);
    let signed13 = |bits: u32| (bits & 0xfff) as i32 - (bits & 0x1000) as i32;
    let mut seen_ids = 0_u16;
    let mut count = 0_usize;

    for i in 0..raw_count {
        let base = first.and_then(|f| f.checked_add(i.checked_mul(snap.input_finger_size)?));
        let mut rec = [0_u8; 9];
        for (j, b) in rec.iter_mut().enumerate() {
            *b = byte_at(base, j);
        }
        let coords = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
        let x = signed13(coords).saturating_sub(snap.x.min).clamp(0, 7612) as u16;
        let y = (-signed13(coords >> 13))
            .saturating_sub(snap.y.min)
            .clamp(0, 5065) as u16;
        let tip = rec[4] > 0;
        let conf = rec[5] <= 172;
        let id = u32::from(rec[8] & 0x0f);
        if !tip || (seen_ids & (1 << id)) != 0 || count >= 5 {
            continue;
        }
        seen_ids |= 1 << id;
        let off = 1 + 9 * count;
        report[off] = u8::from(conf) | 0x02;
        report[off + 1..off + 5].copy_from_slice(&id.to_le_bytes());
        report[off + 5..off + 7].copy_from_slice(&x.to_le_bytes());
        report[off + 7..off + 9].copy_from_slice(&y.to_le_bytes());
        count += 1;
    }
    report[48] = count as u8;
    report
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

        let (act, p3) = port_act(r, r.get("act"));
        let c_ev = if p3.is_some() {
            ev_for_act(act)
        } else if r.get("kind") == "edge" {
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
                let packet = r.bytes("in");
                let rinfo = usize::try_from(r.num("rinfo")).expect("rinfo");
                let expected = expected_frame0_mt2_report(&snap, &packet, rinfo);
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
                if p3.is_none() {
                    assert_eq!(r.get("status"), "-");
                }
                assert!(a.completed.is_none(), "{}", r.case());
                assert!(a.buf.iter().all(|&x| x == 0xEE));
            }
        }
        if r.opt("ub") == Some("int_min_neg") {
            ub_records += 1;
        }

        assert_eq!(a.state, snap, "{}", r.case());

        tally.add(match (p3, act) {
            (Some(bucket), _) => bucket,
            (None, "outmem_fail") => OUTMEM_P2,
            (None, "copy_fail") => COPY_P2,
            (None, other) => other,
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
            ("complete", 1197),
            ("timer3s", 226),
            ("setfailed_norestart", 30),
            ("noupper", 26),
            ("workitem", 13),
            (OUTMEM_P2, 1),
            (COPY_P2, 1),
            (OVERSIZE_P3, 47),
            (OTHER_REPORT_P3, 112),
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
            ("complete", 1202),
            ("timer3s", 209),
            ("setfailed_norestart", 24),
            ("noupper", 19),
            ("workitem", 8),
            (OUTMEM_P2, 1),
            (COPY_P2, 1),
            (OVERSIZE_P3, 55),
            (OTHER_REPORT_P3, 142),
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
        complete_lower_read(
            &mut m,
            &state,
            0,
            (),
            ReadTicket::default(),
            STATUS_SUCCESS,
            4,
        );
        assert_eq!(m.uppers[req].completed, Some((0, PTP_REPORT_LEN)));
        let offset = PTP_REPORT_SCAN_TIME_OFFSET;
        assert_eq!(
            m.uppers[req].buf[offset..offset + 2],
            expected.to_le_bytes()
        );
    }
}
