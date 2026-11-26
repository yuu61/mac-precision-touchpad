//! Replays `tests/fixtures/feature.txt` (140 records): GET_FEATURE / SET_FEATURE through the dispatcher,
//! including the wire bytes and WDF call order of the wellspring mode switch.

mod common;

use amtptp_usbkm_core::feature::{GetFeaturePacket, SetFeaturePacket, get_feature, set_feature};
use amtptp_usbkm_core::hid::{InternalIoctl, route_internal_ioctl};
use amtptp_usbkm_core::lifecycle;
use amtptp_usbkm_core::state::DeviceState;
use amtptp_usbkm_core::status::{Completion, STATUS_NOT_SUPPORTED};
use common::fake::{FakeHw, normalize_ops};
use common::{Record, Tally, hex, kv, load, num, status, status_str, to_hex};

/// What one replay produced.
struct Outcome {
    completion: Completion,
    buf_out: Vec<u8>,
    wsm_out: bool,
    wire: String,
    ops: Vec<String>,
}

fn run(r: &Record, mode_override: Option<u8>) -> Outcome {
    let id = r.id();
    let st_in = kv(r.req("st_in"));

    // Bring-up to D0, then the record's state (README: `st_in.wsm` is set before the call).
    let state = DeviceState::new();
    state.on_device_create();
    let mut hw = FakeHw::new(0x027a);
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0, "{id}");
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0, "{id}");
    let mut snap = state.snapshot();
    snap.wellspring_mode_on = st_in["wsm"] == "1";
    snap.report_touch = st_in["touch"] == "1";
    snap.report_button = st_in["button"] == "1";
    snap.last_report_time = st_in["lrt"].parse().expect("lrt");
    state.restore(snap);
    hw.take_logs();

    match r.req("inject") {
        "-" => {}
        inject => {
            let (point, value) = inject.split_once(':').expect("inject");
            match point {
                "ctl_read_response" => hw.ctl_read_response = hex(value),
                _ => hw.set_inject(point, status(value)),
            }
        }
    }

    let report_id = num(r.req("report_id")) as u8;
    let rblen = num(r.req("report_buffer_len")) as usize;
    let actual = num(r.req("buf_actual")) as usize;
    let mut buf = hex(r.req("buf_in"));
    assert_eq!(buf.len(), actual, "{id}");
    if let Some(m) = mode_override {
        buf[1] = m;
    }
    // The glue builds the slice from (reportBuffer, reportBufferLen); never longer than the allocation.
    let len = rblen.min(actual);
    let null_user_buffer = r.req("userbuffer") == "null";
    let mut irp_read = false;

    let code = num(r.req("ioctl")) as u32;
    let completion = match (r.req("kind"), route_internal_ioctl(code)) {
        ("get_feature", InternalIoctl::GetFeature) => {
            assert!(rblen <= actual, "{id}: GET never needs a clamped slice");
            let outlen = num(r.req("outlen")) as usize;
            get_feature(outlen, || {
                irp_read = true;
                (!null_user_buffer).then(|| GetFeaturePacket {
                    report_id,
                    report_buffer: &mut buf[..len],
                })
            })
        }
        ("set_feature", InternalIoctl::SetFeature) => {
            let inlen = num(r.req("inlen")) as usize;
            let report = buf[..len].to_vec();
            set_feature(
                inlen,
                || {
                    irp_read = true;
                    (!null_user_buffer).then_some(SetFeaturePacket {
                        report_id,
                        report_buffer: &report,
                    })
                },
                &state,
                &mut hw,
            )
        }
        (kind, route) => panic!("{id}: {kind} routed to {route:?}"),
    };

    let (hw_ops, wire) = hw.take_logs();
    let mut ops = vec!["get_params".to_owned()];
    if irp_read {
        ops.push("wdm_get_irp".to_owned());
    }
    ops.extend(hw_ops);
    ops.push(format!("complete:st={}", status_str(completion.status)));
    Outcome {
        completion,
        buf_out: buf,
        wsm_out: state.wellspring_mode_on(),
        wire,
        ops,
    }
}

fn check(r: &Record, o: &Outcome) -> usize {
    let id = r.id();
    assert_eq!(r.req("completed"), "1", "{id}");
    assert_eq!(
        o.completion,
        Completion {
            status: status(r.req("status")),
            information: num(r.req("info")) as usize,
        },
        "{id}"
    );
    assert_eq!(o.completion.information, 0, "{id}: B13");
    assert_eq!(to_hex(&o.buf_out), r.req("buf_out"), "{id}: report buffer");
    let st_out = kv(r.req("st_out"));
    assert_eq!(o.wsm_out, st_out["wsm"] == "1", "{id}: wsm");
    assert_eq!(
        st_out["touch"], "1",
        "{id}: SET 0x06 never changes the gates"
    );
    assert_eq!(st_out["button"], "1", "{id}");
    assert_eq!(o.wire, r.req("wire"), "{id}: wire");
    let (expected_ops, null_deletes) = normalize_ops(r.req("ops"));
    assert_eq!(o.ops, expected_ops, "{id}: ops");
    null_deletes
}

#[test]
fn feature_fixture() {
    let records = load("feature.txt");
    let mut tally = Tally::default();
    let mut ub_differs_from_c = 0;
    for r in &records {
        let id = r.id();
        if r.has_tag("ub_oob_read") {
            // SET_FEATURE 0x04 with reportBufferLen < 2: C read reportBuffer[1] past the report. P2: the
            // byte is outside the report and read as 0 → mode 0 → STATUS_NOT_SUPPORTED, no transfer.
            let rblen = num(r.req("report_buffer_len")) as usize;
            assert!(rblen < 2, "{id}");
            let o = run(r, None);
            assert_eq!(
                o.completion,
                Completion::status(STATUS_NOT_SUPPORTED),
                "{id}"
            );
            assert_eq!(o.wire, "-", "{id}");
            assert_eq!(o.wsm_out, kv(r.req("st_in"))["wsm"] == "1", "{id}");
            assert_eq!(to_hex(&o.buf_out), r.req("buf_in"), "{id}");
            if status(r.req("status")) != STATUS_NOT_SUPPORTED {
                ub_differs_from_c += 1;
            }
            // The in-report behavior equals C: with byte 1 actually inside a 2-byte report the C
            // result is reproduced exactly.
            let mut longer = Record::parse(
                r.line_no,
                &record_line(r)
                    .replace(&format!("report_buffer_len={rblen}"), "report_buffer_len=2"),
            );
            longer.fields.retain(|(k, _)| k != "tags");
            check(&longer, &run(&longer, None));
            tally.add("p2_fixed/ub_oob_read");
            continue;
        }
        let o = run(r, None);
        let null_deletes = check(r, &o);
        let bucket = if r.has_tag("c_bugcheck_B4") {
            // ops identical except the C's WdfObjectDelete(NULL), which the port does not do.
            "p2_fixed/c_bugcheck_B4"
        } else if r.has_tag("c_pool_overrun_B3") {
            // The harness models the P2 allocation (4 zeroed bytes): asserted as written.
            "driver/c_pool_overrun_B3_as_written"
        } else {
            assert!(r.tags().is_empty(), "{id}: unhandled tags");
            "driver"
        };
        if null_deletes > 0 {
            assert!(
                r.has_tag("c_bugcheck_B4"),
                "{id}: obj_delete:null only on the B4 path"
            );
        }
        tally.add(bucket);
    }
    // Two of the four ub_oob_read records (mode byte 3 past the report) differ from the C record.
    assert_eq!(ub_differs_from_c, 2);
    tally.assert_exact(
        "feature.txt",
        140,
        &[
            ("driver", 128),
            ("driver/c_pool_overrun_B3_as_written", 6),
            ("p2_fixed/c_bugcheck_B4", 2),
            ("p2_fixed/ub_oob_read", 4),
        ],
    );
}

/// Re-serializes a record (for the ub_oob_read cross-check).
fn record_line(r: &Record) -> String {
    r.fields
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}
