//! Replays `fixtures/lifecycle.txt`: AddDevice (with faults), PrepareHardware, D0Entry/D0Exit, the
//! detour variants, the read-issue path and its faults, the work item, READ_REPORT and GET_STRING
//! dispatch, and the pipeline FIFO order.

mod common;

use amtptp_hidfilter_core::{
    DeviceSnapshot, DeviceState, REPORT_BUFFER_SIZE, add_device, complete_lower_read, config,
    d0_entry, d0_exit, detour_hid_stack, dispatch_internal_ioctl, issue_lower_read,
    prepare_hardware, recovery_timer_callback, self_managed_io_init, work_item_callback,
};
use common::*;
use ptp_hid::ioctl::{IOCTL_HID_GET_STRING, IOCTL_HID_READ_REPORT};

const TOTAL: usize = 53;
const SKIP_DRIVER_ENTRY: &str = "skipped: DriverEntry/driver cleanup are glue-only (WdfDriverCreate status passthrough, no core logic)";
const ADD_EXACT: &str = "AddDevice exact";
const ADD_P2: &str = "AddDevice P2-fixed (HF-B16/HF-B17)";

fn state_in(r: &Rec) -> DeviceSnapshot {
    match r.opt("st_in") {
        Some(s) => parse_state(s),
        // GET_STRING records carry only st_out; the operation does not change state.
        None => parse_state(r.get("st_out")),
    }
}

fn detour_world(case: &str, r: &Rec) -> World {
    let mut w = World::default();
    if let Some(id) = r.opt("client_id") {
        w.client_id = hex(id);
    }
    let variant = case
        .strip_prefix("smio-init-detour-")
        .or_else(|| case.strip_prefix("detour-"))
        .unwrap_or("");
    match variant {
        "own-drvobj-null" => w.own_valid = false,
        "lower-null" => w.lower = false,
        "lower-drvobj-null" => w.lower_driver = false,
        "drvext-null" => w.driver_extension = false,
        "clientext-null" => w.client_extension = false,
        "id-lowercase" => w.client_id = padded(b"hidclass"),
        "id-HIDCLAS" => w.client_id = padded(b"HIDCLAS"),
        "id-HIDCLASSX" => w.client_id = padded(b"HIDCLASSX"),
        "id-empty" => w.client_id = padded(b""),
        "id-XIDCLASS" => w.client_id = padded(b"XIDCLASS"),
        _ => {}
    }
    w
}

fn padded(s: &[u8]) -> Vec<u8> {
    let mut v = s.to_vec();
    v.resize(16, 0);
    v
}

fn check_detour_extras(r: &Rec, m: &Mock) {
    if let Some(p) = r.opt("patched") {
        let want = p == "slot15=mini15@cli+0xa8";
        assert!(want || p == "none", "{p}");
        assert_eq!(m.world.patched, want, "{}", r.case());
    }
    if let Some(refs) = r.opt("lower_refs") {
        assert_eq!(m.world.refs as i128, num(refs), "{}", r.case());
    }
    if let Some(id) = r.opt("client_id") {
        // The detour variants with a client_id field must see exactly that ID.
        assert_eq!(m.world.client_id, hex(id), "{}", r.case());
    }
}

/// AddDevice: checks the fixture's object configuration against `config`, then the call sequence.
fn replay_add_device(r: &Rec, tally: &mut Tally) {
    let case = r.case();
    let mut m = Mock::new();
    let fixture_ev = events(r);
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
    m.faults.add = fault;
    m.faults.wdm_null = case == "device-add-wdm-do-null";
    let ret = add_device(&mut m);

    // C: (name, result) of every call, and its return value.
    let c: Vec<(String, Option<String>)> = fixture_ev.iter().map(|e| ev_name_result(e)).collect();
    let got: Vec<(String, Option<String>)> =
        m.ev.iter()
            .map(|e| match e.split_once('=') {
                Some((n, s)) => (n.to_string(), Some(s.to_string())),
                None => (e.clone(), None),
            })
            .collect();
    let c_ret = r.get("ret");
    match case {
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
    if r.get("st_out") != "nodevice" {
        assert_eq!(DeviceState::new().snapshot(), parse_state(r.get("st_out")));
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
    let (d1, d2, d3, d4) = config::DEVICE_INTERFACE_GUID;
    let guid = format!(
        "{{{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{}}}",
        d4[0],
        d4[1],
        d4[2..]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert_eq!(arg("WdfDeviceCreateDeviceInterface", 0, "guid"), guid);
    assert_eq!(arg("WdfDeviceCreateDeviceInterface", 0, "ref"), "null");
    assert_eq!(
        num(&arg("WdfLookasideListCreate", 0, "size")),
        REPORT_BUFFER_SIZE as i128
    );
    assert_eq!(arg("WdfLookasideListCreate", 0, "pool"), "NonPagedPoolNx");
    assert_eq!(
        num(&arg("WdfLookasideListCreate", 0, "tag")),
        config::READ_BUFFER_POOL_TAG as i128
    );
    // HF-B15: C passes no list attributes (parent = WDFDRIVER); the port parents it to the device.
    assert_eq!(arg("WdfLookasideListCreate", 0, "list_attrs"), "null");
    const { assert!(config::READ_BUFFER_LOOKASIDE_PARENT_IS_DEVICE) };
    assert_eq!(
        num(&arg("WdfTimerCreate", 0, "period")),
        config::RECOVERY_TIMER_PERIOD as i128
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
        let mut m = Mock::new();
        m.world = detour_world(case, r);
        let mut ret = None;
        match op {
            "PtpFilterPrepareHardware" => ret = Some(prepare_hardware(&state)),
            "PtpFilterDeviceD0Entry" => ret = Some(d0_entry()),
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
            "PtpFilterDetourWindowsHIDStack" => ret = Some(detour_hid_stack(&mut m, &state)),
            "PtpFilterSelfManagedIoInit" => {
                if case == "smio-init-attributes-fail" {
                    m.faults.attributes = Some(SEND_FAIL);
                }
                ret = Some(self_managed_io_init(&mut m, &state));
            }
            "PtpFilterInputIssueTransportRequest" => {
                match case {
                    "issue-read-ok" => {}
                    "issue-read-request-create-fails" => {
                        m.faults.lower_create = Some(RESOURCE_FAIL)
                    }
                    "issue-read-lookaside-fails" => m.faults.lookaside = Some(RESOURCE_FAIL),
                    "issue-read-format-fails" => m.faults.lower_format = Some(FORMAT_FAIL),
                    "issue-read-send-fails" => m.faults.lower_send_false = true,
                    other => panic!("unknown case {other}"),
                }
                issue_lower_read(&mut m, &state);
            }
            "PtpFilterWorkItemCallback" => work_item_callback(&mut m, &state),
            "PtpFilterRecoveryTimerCallback" => recovery_timer_callback(&mut m, &state),
            "FilterEvtIoIntDeviceControl" => {
                let code = if case.starts_with("get-string") {
                    if case == "get-string-send-fails" {
                        m.faults.string_send = Some(FORMAT_FAIL);
                    }
                    IOCTL_HID_GET_STRING
                } else {
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
        assert_eq!(m.ev, events(r), "{case}");
        assert_eq!(state.snapshot(), parse_state(r.get("st_out")), "{case}");
        check_detour_extras(r, &m);
        if let Some(d) = r.opt("readq_depth") {
            assert_eq!(m.read_queue.len() as i128, num(d), "{case}");
        }
        if let Some(p) = r.opt("lower_reads_pending") {
            assert_eq!(m.lower_reads_pending as i128, num(p), "{case}");
        }
        tally.add("exact");
    }
    let total = tally.assert(&[
        (SKIP_DRIVER_ENTRY, 3),
        (ADD_EXACT, 5),
        (ADD_P2, 4),
        ("exact", 41),
    ]);
    assert_eq!(total, TOTAL);
}

/// Two upper READ_REPORTs (each issues a lower read), then one lower completion delivering to the
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
    complete_lower_read(m, state, (), (), packet.len());
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
