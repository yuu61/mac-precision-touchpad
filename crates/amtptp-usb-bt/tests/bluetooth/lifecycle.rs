//! Replays `fixtures/lifecycle.txt`: `AddDevice` (with faults), `PrepareHardware`, D0Entry/D0Exit, the
//! read-issue path and its faults, the work item, `READ_REPORT` and `GET_STRING` dispatch, and the
//! pipeline FIFO order.
//!
//! The HIDCLASS detour is gone (the data source replaces it, `amtptp_usb_bt::bluetooth::bth`): its
//! records are counted as removed, `SelfManagedIoInit` is checked against the new start (static
//! identity, no I/O until the source opens), and `GET_STRING` against the data source's strings.

use super::common;

use amtptp_usb_bt::bluetooth::{
    DeviceSnapshot, DeviceState, REPORT_BUFFER_SIZE, ReadTicket, STATUS_SUCCESS, config,
};
use common::add::add_device;
use common::dispatch::dispatch_internal_ioctl;
use common::pipeline::{complete_lower_read, issue_lower_read, work_item_callback};
use common::pnp::{
    d0_entry, d0_exit, prepare_hardware, recovery_timer_callback, self_managed_io_init,
};
use common::*;

use amtptp_core::hid::ioctl::{IOCTL_HID_GET_STRING, IOCTL_HID_READ_REPORT};

const TOTAL: usize = 53;
const SKIP_DRIVER_ENTRY: &str = "skipped: DriverEntry/driver cleanup are glue-only (WdfDriverCreate status passthrough, no core logic)";
const ADD_EXACT: &str = "AddDevice exact";
const ADD_P2: &str = "AddDevice P2-fixed (HF-B16/HF-B17)";
const P3: &str = "P3-fixed (HF-B30 IDs kept; work item and timer gated)";
const REMOVED_DETOUR: &str = "removed: the HIDCLASS detour (replaced by the data source)";
const SOURCE_INIT: &str =
    "data source: SelfManagedIoInit sets the static identity and waits for the source";
const SOURCE_STRING: &str = "data source: GET_STRING answered from the source's strings";

/// The corrected expectation of the records whose C behaviour is fixed: (events, `st_out`,
/// `lower_reads_pending`).
fn p3_expectation(case: &str, r: &Rec) -> Option<(Vec<String>, DeviceSnapshot, Option<i128>)> {
    match case {
        // HF-B30: PrepareHardware no longer zeroes the IDs.
        "prepare-hardware-after-start" => {
            let st_in = parse_state(r.get("st_in"));
            let mut out = parse_state(r.get("st_out"));
            out.vendor_id = st_in.vendor_id;
            out.product_id = st_in.product_id;
            out.version_number = st_in.version_number;
            out.ptp_input_on = true;
            Some((events(r), out, None))
        }
        // C issued a read for an unconfigured device, and the timer marked a device without the detour
        // configured; both now do nothing.
        "workitem-run-unconfigured" | "timer-without-detour" => {
            Some((Vec::new(), parse_state(r.get("st_in")), Some(0)))
        }
        _ => None,
    }
}

fn state_in(r: &Rec) -> DeviceSnapshot {
    match r.opt("st_in") {
        Some(s) => parse_state(s),
        // GET_STRING records carry only st_out; the operation does not change state.
        None => parse_state(r.get("st_out")),
    }
}

/// `AddDevice`: checks the fixture's object configuration against `config`, then the call sequence.
fn replay_add_device(record: &Rec, tally: &mut Tally) {
    let case = record.case();
    let mut mock = Mock::new();
    let fixture_ev = events(record);
    let fault: Option<(&'static str, i32)> = match case {
        "device-add-ok" | "device-add-wdm-do-null" => None,
        "device-add-device-create-fails" => Some(("WdfDeviceCreate", RESOURCE_FAIL)),
        "device-add-interface-fails" => Some(("WdfDeviceCreateDeviceInterface", RESOURCE_FAIL)),
        "device-add-lookaside-fails" => Some(("WdfLookasideListCreate", RESOURCE_FAIL)),
        "device-add-timer-fails" => Some(("WdfTimerCreate", EXEC_LEVEL_FAIL)),
        "device-add-workitem-fails" => Some(("WdfWorkItemCreate", EXEC_LEVEL_FAIL)),
        "device-add-default-queue-fails" => Some(("WdfIoQueueCreate", RESOURCE_FAIL)),
        "device-add-read-queue-fails" => Some(("ReadQueue", RESOURCE_FAIL)),
        other => panic!("unknown AddDevice case {other}"),
    };
    mock.faults.add = fault;
    mock.faults.wdm_null = case == "device-add-wdm-do-null";
    let ret = add_device(&mut mock);

    // C: (name, result) of every call, and its return value.
    let c: Vec<(String, Option<String>)> = fixture_ev
        .iter()
        .filter(|e| !e.starts_with("WdfDeviceCreateDeviceInterface("))
        .map(|e| ev_name_result(e))
        .collect();
    let got: Vec<(String, Option<String>)> = mock
        .ev
        .iter()
        .map(|e| match e.split_once('=') {
            Some((n, s)) => (n.to_owned(), Some(s.to_owned())),
            None => (e.clone(), None),
        })
        .collect();
    let c_ret = record.get("ret");
    match case {
        "device-add-interface-fails" => {
            assert_eq!(ret, 0);
            tally.add(ADD_EXACT);
        }
        "device-add-wdm-do-null" => {
            // HF-B17: C returns SUCCESS with a half-built device; the port fails AddDevice.
            assert_eq!(c_ret, "0x00000000");
            assert_eq!(st(ret), "0xc0000001");
            assert_eq!(got, c);
            tally.add(ADD_P2);
        }
        "device-add-lookaside-fails" | "device-add-timer-fails" | "device-add-workitem-fails" => {
            // HF-B16: C logs the failure, creates the rest and returns SUCCESS; the port returns the
            // failure at once.
            let (name, status) = fault.expect("fault");
            assert_eq!(c_ret, "0x00000000");
            assert_eq!(ret, status);
            let cut = c.iter().position(|(n, _)| n == name).expect("failing call") + 1;
            assert_eq!(c[cut - 1].1.as_deref(), Some(st(status).as_str()));
            assert_eq!(got, c[..cut].to_vec(), "{case}");
            tally.add(ADD_P2);
        }
        _ => {
            assert_eq!(st(ret), c_ret, "{case}");
            assert_eq!(got, c, "{case}");
            tally.add(ADD_EXACT);
        }
    }
    if record.get("st_out") != "nodevice" {
        // P3 / Step 2 (B-4): input mode and both selective-reporting switches start on.
        let mut expected = parse_state(record.get("st_out"));
        assert!(
            !expected.ptp_input_on && !expected.ptp_report_touch && !expected.ptp_report_button
        );
        expected.ptp_input_on = true;
        expected.ptp_report_touch = true;
        expected.ptp_report_button = true;
        assert_eq!(DeviceState::new().snapshot(), expected);
    }
    if case == "device-add-ok" {
        check_object_config(&fixture_ev);
    }
}

/// The arguments C passed when creating each object, against the port's `config` (with the P2
/// deviations HF-B15 and HF-B16 called out).
fn check_object_config(ev: &[String]) {
    let arg = |name: &str, nth: usize, key: &str| -> String {
        let e = ev
            .iter()
            .filter(|e| e.starts_with(&format!("{name}(")))
            .nth(nth)
            .unwrap_or_else(|| panic!("{name} #{nth}"));
        ev_args(e)
            .into_iter()
            .find(|(k, _)| k == key)
            .unwrap_or_else(|| panic!("{name}: no {key}"))
            .1
    };
    assert_eq!(
        num(&arg("WdfLookasideListCreate", 0, "size")),
        REPORT_BUFFER_SIZE as i128
    );
    assert_eq!(arg("WdfLookasideListCreate", 0, "pool"), "NonPagedPoolNx");
    assert_eq!(
        num(&arg("WdfLookasideListCreate", 0, "tag")),
        i128::from(config::READ_BUFFER_POOL_TAG)
    );
    // HF-B15: C passes no list attributes (parent = WDFDRIVER); the port parents it to the device.
    assert_eq!(arg("WdfLookasideListCreate", 0, "list_attrs"), "null");
    const { assert!(config::READ_BUFFER_LOOKASIDE_PARENT_IS_DEVICE) };
    assert_eq!(
        num(&arg("WdfTimerCreate", 0, "period")),
        i128::from(config::RECOVERY_TIMER_PERIOD)
    );
    assert_eq!(arg("WdfTimerCreate", 0, "exec"), "passive");
    assert_eq!(arg("WdfTimerCreate", 0, "parent"), "device");
    assert_eq!(
        arg("WdfTimerCreate", 0, "func"),
        "PtpFilterRecoveryTimerCallback"
    );
    // HF-B16: C uses AutomaticSerialization TRUE on both; the port uses FALSE.
    assert_eq!(arg("WdfTimerCreate", 0, "auto_ser"), "1");
    const { assert!(!config::RECOVERY_TIMER_AUTOMATIC_SERIALIZATION) };
    assert_eq!(arg("WdfWorkItemCreate", 0, "auto_ser"), "1");
    const { assert!(!config::RECOVERY_WORK_ITEM_AUTOMATIC_SERIALIZATION) };
    assert_eq!(arg("WdfWorkItemCreate", 0, "parent"), "device");
    assert_eq!(arg("WdfIoQueueCreate", 0, "default"), "1");
    assert_eq!(arg("WdfIoQueueCreate", 0, "dispatch"), "parallel");
    const { assert!(config::DEFAULT_QUEUE_PARALLEL) };
    assert_eq!(arg("WdfIoQueueCreate", 0, "power"), "default");
    assert_eq!(
        arg("WdfIoQueueCreate", 0, "internal_ioctl"),
        "FilterEvtIoIntDeviceControl"
    );
    assert_eq!(arg("WdfIoQueueCreate", 0, "stop"), "FilterEvtIoStop");
    assert_eq!(arg("WdfIoQueueCreate", 1, "default"), "0");
    assert_eq!(arg("WdfIoQueueCreate", 1, "dispatch"), "manual");
    assert_eq!(arg("WdfIoQueueCreate", 1, "power"), "false");
    const { assert!(!config::READ_QUEUE_POWER_MANAGED) };
}

#[test]
fn replay_lifecycle() {
    let recs = load("lifecycle.txt");
    assert_eq!(recs.len(), TOTAL);
    let mut tally = Tally::default();
    for r in &recs {
        let case = r.case();
        let op = r.get("op");
        if op == "DriverEntry" || op == "PtpFilterEvtDriverContextCleanup" {
            tally.add(SKIP_DRIVER_ENTRY);
            continue;
        }
        if op == "PtpFilterEvtDeviceAdd" {
            replay_add_device(r, &mut tally);
            continue;
        }
        let snap = state_in(r);
        let state = DeviceState::with(&snap);
        if op == "PtpFilterDetourWindowsHIDStack" {
            tally.add(REMOVED_DETOUR);
            continue;
        }
        let mut m = Mock::new();
        let mut ret = None;
        match op {
            "PtpFilterPrepareHardware" => ret = Some(prepare_hardware(&state)),
            "PtpFilterDeviceD0Entry" => ret = Some(d0_entry(&state)),
            "PtpFilterDeviceD0Exit" => {
                let parked: usize = case
                    .strip_prefix("d0-exit-parked")
                    .and_then(|n| n.parse().ok())
                    .expect("parked count");
                for _ in 0..parked {
                    let q = m.add_upper(Upper::with_output(50, 0xEE));
                    m.read_queue.push_back(q);
                }
                ret = Some(d0_exit(&mut m, &state));
                for (i, u) in m.uppers.iter().enumerate() {
                    let (s, info) = u.completed.expect("drained");
                    assert_eq!(
                        format!("completed:{}:{info}", st(s)),
                        r.get(&format!("upper{}", i + 1))
                    );
                }
            }
            "PtpFilterSelfManagedIoInit" => {
                // Every record starts before the detour, i.e. without a data source.
                assert!(!snap.source_open, "{case}");
                assert_eq!(
                    self_managed_io_init(&mut m, &state),
                    STATUS_SUCCESS,
                    "{case}"
                );
                assert!(m.ev.is_empty(), "{case}: {:?}", m.ev);
                let mut want = snap;
                want.vendor_id = 0x004C;
                want.product_id = 0x0265;
                want.version_number = 0;
                assert_eq!(state.snapshot(), want, "{case}");
                tally.add(SOURCE_INIT);
                continue;
            }
            "PtpFilterInputIssueTransportRequest" => {
                match case {
                    "issue-read-ok" => {}
                    "issue-read-request-create-fails" => {
                        m.faults.lower_create = Some(RESOURCE_FAIL);
                    }
                    "issue-read-lookaside-fails" => m.faults.lookaside = Some(RESOURCE_FAIL),
                    "issue-read-format-fails" => m.faults.lower_format = Some(FORMAT_FAIL),
                    "issue-read-send-fails" => m.faults.lower_send_false = true,
                    other => panic!("unknown case {other}"),
                }
                issue_lower_read(&mut m, &state);
            }
            "PtpFilterWorkItemCallback" => {
                assert_eq!(d0_entry(&state), 0);
                work_item_callback(&mut m, &state);
            }
            "PtpFilterRecoveryTimerCallback" => {
                assert_eq!(d0_entry(&state), 0);
                recovery_timer_callback(&mut m, &state);
            }
            "FilterEvtIoIntDeviceControl" if case.starts_with("get-string") => {
                // Before the source is open there are no strings: completed with the failure.
                let req = m.add_upper(Upper::with_output(128, 0xCC));
                dispatch_internal_ioctl(&mut m, &state, req, 128, 0, IOCTL_HID_GET_STRING);
                assert_eq!(
                    m.ev,
                    [
                        "CopyString(req=upper1)",
                        // Information untouched (the mock's sentinel 0x5A5A).
                        "WdfRequestComplete(req=upper1,status=0xc0000184,info=23130)",
                    ],
                    "{case}"
                );
                assert_eq!(
                    m.uppers[req].completed,
                    Some((
                        amtptp_usb_bt::bluetooth::STATUS_INVALID_DEVICE_STATE,
                        0x5A5A
                    )),
                    "{case}"
                );
                assert!(!m.read_queue.contains(&req));
                assert_eq!(state.snapshot(), snap, "{case}");
                tally.add(SOURCE_STRING);
                continue;
            }
            "FilterEvtIoIntDeviceControl" => {
                let code = {
                    if case == "read-report-forward-fails" {
                        m.faults.forward = Some(FORMAT_FAIL);
                    }
                    IOCTL_HID_READ_REPORT
                };
                let req = m.add_upper(Upper::with_output(128, 0xCC));
                dispatch_internal_ioctl(&mut m, &state, req, 128, 0, code);
                let u = &m.uppers[req];
                let disp = match (u.completed, m.read_queue.contains(&req)) {
                    (Some((s, info)), false) => {
                        assert_eq!(st(s), r.get("status"));
                        assert_eq!(info as i128, r.num("info"));
                        "completed"
                    }
                    (None, true) => "queued",
                    (None, false) => "forwarded",
                    _ => panic!("{case}: completed and queued"),
                };
                assert_eq!(disp, r.get("disp"), "{case}");
            }
            "FilterEvtIoIntDeviceControl*2+PtpFilterInputRequestCompletionCallback" => {
                replay_pipeline(r, &mut m, &state);
            }
            other => panic!("{case}: unknown op {other}"),
        }
        match (ret, r.get("ret")) {
            (Some(s), want) => assert_eq!(st(s), want, "{case}"),
            (None, want) => assert_eq!(want, "-", "{case}"),
        }
        let p3 = p3_expectation(case, r);
        let (mut expected_events, expected_state, expected_pending) = match p3.clone() {
            Some(expected) => expected,
            None => (
                events(r),
                parse_state(r.get("st_out")),
                r.opt("lower_reads_pending").map(num),
            ),
        };
        if op == "FilterEvtIoIntDeviceControl*2+PtpFilterInputRequestCompletionCallback" {
            for event in &mut expected_events {
                *event = event.replace("freq_ptr=nonnull", "freq_ptr=null");
            }
        }
        assert_eq!(m.ev, expected_events, "{case}");
        assert_eq!(state.snapshot(), expected_state, "{case}");
        if let Some(d) = r.opt("readq_depth") {
            assert_eq!(m.read_queue.len() as i128, num(d), "{case}");
        }
        if let Some(p) = expected_pending {
            assert_eq!(i128::from(m.lower_reads_pending), p, "{case}");
        }
        tally.add(if p3.is_some() { P3 } else { "exact" });
    }
    let total = tally.assert(&[
        (SKIP_DRIVER_ENTRY, 3),
        (ADD_EXACT, 5),
        (ADD_P2, 4),
        (P3, 3),
        (REMOVED_DETOUR, 13),
        (SOURCE_INIT, 11),
        (SOURCE_STRING, 2),
        ("exact", 12),
    ]);
    assert_eq!(total, TOTAL);
}

/// Two upper `READ_REPORTs` (each issues a lower read), then one lower completion delivering to the
/// first parked request (FIFO).
fn replay_pipeline(r: &Rec, m: &mut Mock, state: &DeviceState) {
    for _ in 0..2 {
        let req = m.add_upper(Upper::with_output(50, 0xEE));
        dispatch_internal_ioctl(m, state, req, 50, 0, IOCTL_HID_READ_REPORT);
    }
    assert_eq!(
        format!(
            "readq:{},lower:{}",
            m.read_queue.len(),
            m.lower_reads_pending
        ),
        r.get("after_dispatch")
    );
    let packet = r.bytes("lower_in");
    m.lower_buf[..packet.len()].copy_from_slice(&packet);
    complete_lower_read(
        m,
        state,
        0,
        (),
        ReadTicket::default(),
        STATUS_SUCCESS,
        packet.len(),
    );
    m.lower_reads_pending -= 1;
    assert_eq!(
        format!(
            "readq:{},lower:{}",
            m.read_queue.len(),
            m.lower_reads_pending
        ),
        r.get("after_completion")
    );
    assert_eq!(r.get("upper1"), "completed");
    assert_eq!(m.uppers[0].completed, Some((0, 50)));
    assert_eq!(r.get("upper2"), "queued");
    assert!(m.uppers[1].completed.is_none() && m.read_queue.contains(&1));
}
