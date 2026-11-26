//! Replays `tests/fixtures/wire.txt` (240 records = lifecycle steps of 42 scenarios): DriverEntry,
//! DeviceAdd, PrepareHardware, D0Entry, D0Exit, cleanup, with failure injection, the WDF call order and
//! the control-transfer wire bytes.
//!
//! PrepareHardware / D0Entry / D0Exit run through the core (`lifecycle::*`) over the fake WDF USB side;
//! status, context, wire bytes and (normalized) WDF call order are compared. DriverEntry and DeviceAdd are
//! WDF configuration sequences the glue performs; for them the injected framework status, the core's
//! configuration constants (queues, device interface GUID) and the initial context are asserted.

mod common;

use std::collections::BTreeMap;

use amtptp_usbkm_core::lifecycle::{
    self, DEFAULT_QUEUE, DEVICE_INTERFACE_GUID, INPUT_QUEUE, QueueSpec,
};
use amtptp_usbkm_core::state::DeviceState;
use amtptp_usbkm_core::status::STATUS_INVALID_DEVICE_STATE;
use common::fake::{FakeHw, normalize_ops};
use common::{Record, Tally, hex, load, num, status, status_str, to_hex};

struct Scenario {
    state: Option<DeviceState>,
    hw: FakeHw,
    inputq: bool,
    prepare_calls: usize,
    stale_pipe_note: bool,
}

fn ctx_string(s: &Scenario) -> String {
    let Some(state) = &s.state else {
        return "none".to_owned();
    };
    let snap = state.snapshot();
    let cfg = match snap.config_index {
        None => "none".to_owned(),
        Some(_) => format!("0x{:04x}", state.config().identification),
    };
    let pipe = snap.interrupt_pipe.map_or(-1, i32::from);
    format!(
        "vid:0x{:04x},pid:0x{:04x},bcd:0x{:04x},cfg:{cfg},wsm:{},ptpinputon:0,touch:{},button:{},\
         lrt:{},traits:0x{:08x},usbdev:{},pipe:{pipe},iface:{},inputq:{}",
        snap.ids.vendor_id,
        snap.ids.product_id,
        snap.ids.bcd_device,
        u8::from(snap.wellspring_mode_on),
        u8::from(snap.report_touch),
        u8::from(snap.report_button),
        snap.last_report_time,
        snap.usb_device_traits,
        u8::from(snap.usb_device_created),
        u8::from(s.hw.iface),
        u8::from(s.inputq),
    )
}

fn queue_op(q: QueueSpec) -> String {
    format!(
        "queuecreate:dispatch={},power={},default={},iodefault=0,ioread=0,iowrite=0,iodevctl=0,\
         iointdevctl={},iostop={},ioresume=0,iocanceled=0",
        q.dispatch as i32,
        q.power_managed as i32,
        u8::from(q.default_queue),
        u8::from(q.internal_device_control),
        u8::from(q.io_stop),
    )
}

/// DeviceAdd as the glue performs it (crate docs, "How the driver uses this"), with the framework
/// failures injected by the harness. Returns (status, normalized ops).
fn device_add(s: &mut Scenario) -> (i32, Vec<String>) {
    let inj = |p: &str| s.hw.inject.get(p).copied();
    let mut ops = vec![
        "fdo_init_set_filter".to_owned(),
        // Only PrepareHardware, D0Entry and D0Exit.
        "set_pnp_power_cb:prepare=1,d0entry=1,d0exit=1,others=0".to_owned(),
    ];
    let with_st = |op: String, st: Option<i32>| match st {
        Some(st) => format!(
            "{op}{}st={}",
            if op.contains(':') { "," } else { ":" },
            status_str(st)
        ),
        None => op,
    };
    if let Some(st) = inj("device_create") {
        ops.push(with_st("devcreate".to_owned(), Some(st)));
        return (st, ops);
    }
    ops.push("devcreate".to_owned());
    let state = DeviceState::new(); // WDF zero-initializes the context
    state.on_device_create();
    s.state = Some(state);
    let guid = to_hex(&DEVICE_INTERFACE_GUID.to_bytes());
    let devif = format!("devif:guid={guid},ref=null");
    if let Some(st) = inj("device_interface") {
        ops.push(with_st(devif, Some(st)));
        return (st, ops);
    }
    ops.push(devif);
    if let Some(st) = inj("queue_default") {
        ops.push(with_st(queue_op(DEFAULT_QUEUE), Some(st)));
        return (st, ops);
    }
    ops.push(queue_op(DEFAULT_QUEUE));
    if let Some(st) = inj("queue_input") {
        ops.push(with_st(queue_op(INPUT_QUEUE), Some(st)));
        return (st, ops);
    }
    ops.push(queue_op(INPUT_QUEUE));
    s.inputq = true;
    (0, ops)
}

fn step(s: &mut Scenario, r: &Record) -> &'static str {
    let id = r.id();
    let arg = r.req("arg");
    let call = r.req("call");
    match call {
        "harness_inject" => {
            let (point, st) = arg.split_once(':').expect("inject");
            s.hw.set_inject(point, status(st));
            return "harness_directive";
        }
        "harness_pipes" => {
            s.hw.pipes = if arg == "none" {
                Vec::new()
            } else {
                arg.split(',')
                    .map(|p| {
                        let (t, ep) = p.split_once('/').expect("pipe");
                        (num(t) as u8, num(ep) as u8)
                    })
                    .collect()
            };
            return "harness_directive";
        }
        "harness_descriptor" => {
            let (vid, pid) = arg.split_once(':').expect("vid:pid");
            s.hw.ids.vendor_id = u16::from_str_radix(vid, 16).expect("vid");
            s.hw.ids.product_id = u16::from_str_radix(pid, 16).expect("pid");
            return "harness_directive";
        }
        "harness_ctl_read_response" => {
            s.hw.ctl_read_response = hex(arg);
            return "harness_directive";
        }
        "note" => {
            let tags = arg.strip_prefix("tags:").expect("note tags");
            for t in tags.split(',') {
                assert!(
                    ["c_bugcheck_B4", "c_pool_overrun_B3", "c_stale_handle_B20"].contains(&t),
                    "{id}: unknown note tag {t}"
                );
            }
            return "harness_note";
        }
        _ => {}
    }

    let expected_status = r.req("status");
    let (expected_ops, null_deletes) = normalize_ops(r.req("ops"));
    let bucket = match call {
        "driver_entry" => {
            // WdfDriverCreate with the cleanup callback; its status is the framework's.
            let st = s.hw.inject.get("driver_create").copied().unwrap_or(0);
            assert_eq!(status_str(st), expected_status, "{id}");
            assert_eq!(r.req("ctx_out"), "none", "{id}");
            let drv = r.req("ops");
            assert!(
                drv.starts_with("drvcreate:")
                    && drv.contains("devadd=1,unload=0,initflags=0x00000000")
                    && drv.contains("cleanup=1"),
                "{id}: driver config"
            );
            "framework/driver_entry"
        }
        "device_add" => {
            let (st, ops) = device_add(s);
            assert_eq!(status_str(st), expected_status, "{id}");
            // B12 (P2): the PDO-only WdfPdoInitAllowForwardingRequestToParent is not called.
            let c_ops: Vec<String> = expected_ops
                .into_iter()
                .filter(|o| o != "pdo_init_allow_fwd_to_parent")
                .collect();
            assert_eq!(ops, c_ops, "{id}: ops");
            "framework/device_add"
        }
        "prepare_hardware" => {
            s.prepare_calls += 1;
            let state = s.state.as_ref().expect("context");
            let st = lifecycle::prepare_hardware(state, &mut s.hw);
            let (ops, wire) = s.hw.take_logs();
            assert_eq!(wire, r.req("wire"), "{id}");
            if s.stale_pipe_note && s.prepare_calls == 2 {
                // B20 (P2): C keeps the stale (deleted) pipe handle and succeeds; the port resets the
                // pipe and fails with STATUS_INVALID_DEVICE_STATE without configuring a reader.
                assert_eq!(expected_status, "0x00000000", "{id}: C record");
                assert_eq!(st, STATUS_INVALID_DEVICE_STATE, "{id}");
                assert_eq!(state.interrupt_pipe(), None, "{id}");
                let c_ops: Vec<String> = expected_ops
                    .into_iter()
                    .filter(|o| !o.starts_with("pipe_config_cont_reader"))
                    .collect();
                assert_eq!(ops, c_ops, "{id}: ops");
                let c_ctx = r.req("ctx_out").replace("pipe:0", "pipe:-1");
                assert_eq!(ctx_string(s), c_ctx, "{id}: ctx");
                return "p2_fixed/c_stale_handle_B20";
            }
            assert_eq!(status_str(st), expected_status, "{id}");
            assert_eq!(ops, expected_ops, "{id}: ops");
            "driver/prepare_hardware"
        }
        "d0_entry" | "d0_exit" => {
            let state = s.state.as_ref().expect("context");
            let st = if call == "d0_entry" {
                lifecycle::d0_entry(state, &mut s.hw)
            } else {
                lifecycle::d0_exit(state, &mut s.hw)
            };
            let (ops, wire) = s.hw.take_logs();
            assert_eq!(status_str(st), expected_status, "{id}");
            assert_eq!(wire, r.req("wire"), "{id}: wire");
            assert_eq!(ops, expected_ops, "{id}: ops");
            if null_deletes > 0 {
                // C called WdfObjectDelete(NULL) after the failed WdfMemoryCreate (B4); the port does
                // not. Everything else is identical.
                "p2_fixed/c_bugcheck_B4"
            } else if call == "d0_entry" {
                "driver/d0_entry"
            } else {
                "driver/d0_exit"
            }
        }
        "driver_context_cleanup" => {
            // Trace cleanup only: no WDF call, no context change.
            assert_eq!(expected_status, "-", "{id}");
            assert!(expected_ops.is_empty(), "{id}");
            "driver/cleanup_noop"
        }
        other => panic!("{id}: unknown call {other}"),
    };
    assert_eq!(null_deletes > 0, bucket == "p2_fixed/c_bugcheck_B4", "{id}");
    assert_eq!(ctx_string(s), r.req("ctx_out"), "{id}: ctx_out");
    bucket
}

#[test]
fn wire_fixture() {
    let records = load("wire.txt");
    let mut scenarios: BTreeMap<String, Vec<&Record>> = BTreeMap::new();
    for r in &records {
        scenarios
            .entry(r.req("scenario").to_owned())
            .or_default()
            .push(r);
    }
    assert_eq!(scenarios.len(), 42);
    let mut tally = Tally::default();
    for (name, steps) in &scenarios {
        for (i, r) in steps.iter().enumerate() {
            assert_eq!(num(r.req("step")) as usize, i + 1, "{name}: step order");
        }
        let pid = name
            .strip_prefix("std-")
            .map_or(0x027a, |p| u16::from_str_radix(p, 16).expect("pid"));
        let mut s = Scenario {
            state: None,
            hw: FakeHw::new(pid),
            inputq: false,
            prepare_calls: 0,
            stale_pipe_note: steps
                .iter()
                .any(|r| r.req("call") == "note" && r.req("arg").contains("c_stale_handle_B20")),
        };
        if let Some(freq) = name.strip_prefix("qpc-freq-") {
            s.hw.qpc_freq = freq.parse().expect("freq");
        }
        if name == "traits-all" {
            s.hw.traits = 0x7;
        }
        for r in steps {
            tally.add(step(&mut s, r));
        }
    }
    tally.assert_exact(
        "wire.txt",
        240,
        &[
            ("driver/cleanup_noop", 7),
            ("driver/d0_entry", 45),
            ("driver/d0_exit", 26),
            ("driver/prepare_hardware", 39),
            ("framework/device_add", 41),
            ("framework/driver_entry", 42),
            ("harness_directive", 32),
            ("harness_note", 5),
            ("p2_fixed/c_bugcheck_B4", 2),
            ("p2_fixed/c_stale_handle_B20", 1),
        ],
    );
}
