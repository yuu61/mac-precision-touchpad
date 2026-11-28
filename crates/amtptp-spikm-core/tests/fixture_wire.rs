//! Replays `wire.txt`: `PrepareHardware` (lower `GET_DEVICE_ATTRIBUTES`, table lookup, `DesiredReportType`),
//! the mode-switch `SET_FEATURE` buffer and the lower `READ_REPORT` issue.
//!
//! The registry is the KMDF model from the fixture README: `nokey` -> the key does not open (`None`);
//! `novalue` -> `c0000034`; `dword:N` / `binary4:N` -> 4 bytes; `qword:N` -> 8 bytes, larger than the
//! 4-byte buffer -> `STATUS_BUFFER_OVERFLOW`.

#![cfg(test)]

mod common;

use amtptp_spikm_core::device::{
    DESIRED_REPORT_TYPE_VALUE_LEN, DeviceConfig, PARAMETERS_KEY_ACCESS, ReportType,
    desired_report_type_value,
};
use amtptp_spikm_core::runtime::{
    DeviceRuntime, DeviceStatus, RECOVERY_TIMER_DUE_TIME, ReadTracker,
};
use amtptp_spikm_core::status::{
    NtStatus, STATUS_BUFFER_OVERFLOW, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_SUCCESS,
};
use amtptp_spikm_core::wire::{
    LOWER_ATTRIBUTES_IOCTL, LOWER_ATTRIBUTES_LEN, LOWER_READ_BUFFER_LEN, LOWER_READ_IOCTL,
    SET_STATE_IOCTL, SET_STATE_PACKET_LEN, with_set_state_packet,
};
use common::{Record, Tally, load, to_hex, wdf_failures};

const PREP: &str = "PrepareHardware (driver logic; lower/registry results from the record)";
const PREP_RESTART: &str = "PrepareHardware twice on one device (ReportType carried over)";
const SETSTATE: &str = "mode switch buffer + SMIO/timer decision";
const READ_ISSUE_OK: &str = "lower READ_REPORT issued";
const READ_ISSUE_FW: &str =
    "framework: lower READ_REPORT creation/format/send failed (request stays queued)";

const QPC_FREQUENCY: i64 = 10_000_000;

/// The registry model: what `desired_report_type_value` sees, plus whether a query happened.
fn registry(spec: &str) -> Option<u32> {
    let (status, data): (NtStatus, [u8; DESIRED_REPORT_TYPE_VALUE_LEN]) = match spec {
        "nokey" => return None,
        "novalue" => (STATUS_OBJECT_NAME_NOT_FOUND, [0; 4]),
        _ => {
            let Some((kind, value)) = spec.split_once(':') else {
                panic!("invalid registry fixture {spec}");
            };
            let Ok(value) = value.parse::<u64>() else {
                panic!("invalid registry fixture value in {spec}");
            };
            match kind {
                "dword" | "binary4" => (STATUS_SUCCESS, (value as u32).to_le_bytes()),
                // 8 bytes do not fit the 4-byte buffer: nothing is copied.
                "qword" => (STATUS_BUFFER_OVERFLOW, [0; 4]),
                other => panic!("reg kind {other}"),
            }
        }
    };
    desired_report_type_value(status, data)
}

fn attrs(r: &Record) -> [u8; LOWER_ATTRIBUTES_LEN] {
    r.bytes("lower_attr")
        .try_into()
        .expect("32 attribute bytes")
}

/// One `PrepareHardware` run as the glue performs it. Returns the status and whether the registry was used.
fn prepare(
    cfg: &mut DeviceConfig,
    lower_status: NtStatus,
    lower: &[u8; 32],
    reg: &str,
) -> (NtStatus, bool) {
    if let Err(s) = cfg.identify(lower_status, lower) {
        (s, false)
    } else {
        cfg.apply_desired_report_type(registry(reg));
        (STATUS_SUCCESS, true)
    }
}

fn check_config(r: &Record, cfg: &DeviceConfig, ctx: &str) {
    assert_eq!(cfg.hid_vendor_id, r.hex16("hid_vid"), "{ctx}: hid_vid");
    assert_eq!(cfg.hid_product_id, r.hex16("hid_pid"), "{ctx}: hid_pid");
    assert_eq!(cfg.hid_version_number, r.hex16("hid_ver"), "{ctx}: hid_ver");
    let t = cfg.trackpad_info;
    let ti = format!(
        "{:04x},{:04x},{},{},{},{}",
        t.vendor_id, t.product_id, t.x_min, t.x_max, t.y_min, t.y_max
    );
    assert_eq!(ti, r.get("ti"), "{ctx}: ti");
    assert_eq!(
        cfg.report_type,
        ReportType(r.dec("rt_out")),
        "{ctx}: rt_out"
    );
}

fn check_prepare_ops(r: &Record, used_registry: bool, ctx: &str) {
    let ops = r.get("ops");
    assert!(
        ops.starts_with(&format!(
            "WdfIoTargetSendInternalIoctlSynchronously(target=lower,req=null,ioctl={LOWER_ATTRIBUTES_IOCTL:08x},in=null,out=buf(len={LOWER_ATTRIBUTES_LEN},"
        )),
        "{ctx}: identification request"
    );
    assert_eq!(
        ops.contains("WdfDriverOpenParametersRegistryKey"),
        used_registry,
        "{ctx}: registry only after a table match"
    );
    if used_registry {
        assert!(
            ops.contains(&format!("access=0x{PARAMETERS_KEY_ACCESS:08x}")),
            "{ctx}"
        );
        if r.get("reg") != "nokey" {
            assert!(
                ops.contains(&format!(
                    "WdfRegistryQueryValue(name=DesiredReportType,len={DESIRED_REPORT_TYPE_VALUE_LEN},"
                )),
                "{ctx}"
            );
        }
    }
}

/// A device brought up like the harness: `PrepareHardware` with PID 0x0272 and no registry value, `D0Entry`.
fn bring_up() -> (DeviceConfig, DeviceRuntime) {
    let mut cfg = DeviceConfig::new();
    let mut lower = [0_u8; 32];
    lower[..10].copy_from_slice(&[0x20, 0, 0, 0, 0xac, 0x05, 0x72, 0x02, 0x02, 0x01]);
    assert_eq!(
        prepare(&mut cfg, STATUS_SUCCESS, &lower, "novalue"),
        (STATUS_SUCCESS, true)
    );
    let rt = DeviceRuntime::new();
    rt.on_d0_entry(QPC_FREQUENCY);
    (cfg, rt)
}

#[test]
fn wire_records() {
    let recs = load("wire.txt");
    assert_eq!(recs.len(), 46);
    let mut tally = Tally::default();
    for r in &recs {
        let ctx = format!("wire.txt:{} {}", r.line, r.case());
        let case = r.case();
        if case.starts_with("prep-restart-") {
            // These records carry no `lower_attr`: the lower device answers with the recorded IDs.
            let mut cfg = DeviceConfig::new();
            let mut lower = [0_u8; 32];
            lower[..4].copy_from_slice(&32_u32.to_le_bytes());
            lower[4..6].copy_from_slice(&r.hex16("hid_vid").to_le_bytes());
            lower[6..8].copy_from_slice(&r.hex16("hid_pid").to_le_bytes());
            lower[8..10].copy_from_slice(&r.hex16("hid_ver").to_le_bytes());
            let (s1, _) = prepare(&mut cfg, STATUS_SUCCESS, &lower, r.get("reg1"));
            assert_eq!(s1, r.status("status1"), "{ctx}: status1");
            assert_eq!(cfg.report_type, ReportType(r.dec("rt_in")), "{ctx}: rt_in");
            let (s2, used) = prepare(&mut cfg, STATUS_SUCCESS, &lower, r.get("reg"));
            assert_eq!(s2, r.status("status"), "{ctx}: status");
            check_config(r, &cfg, &ctx);
            check_prepare_ops(r, used, &ctx);
            assert_eq!(r.get("ds_out"), "0", "{ctx}");
            tally.add(PREP_RESTART);
        } else if case.starts_with("prep-") {
            let mut cfg = DeviceConfig::new();
            assert_eq!(cfg.report_type, ReportType(r.dec("rt_in")), "{ctx}: rt_in");
            let lower_status = r.status("lower_st");
            let (status, used) = prepare(&mut cfg, lower_status, &attrs(r), r.get("reg"));
            assert_eq!(status, r.status("status"), "{ctx}: status");
            check_config(r, &cfg, &ctx);
            check_prepare_ops(r, used, &ctx);
            // DeviceStatus is untouched by PrepareHardware (D3 from CreateDevice).
            assert_eq!(
                DeviceRuntime::new().device_status() as u32,
                r.dec::<u32>("ds_out")
            );
            tally.add(PREP);
        } else if case.starts_with("setstate-") {
            let (cfg, rt) = bring_up();
            check_config(r, &cfg, &ctx);
            assert_eq!(
                rt.device_status() as u32,
                r.dec::<u32>("ds_in"),
                "{ctx}: ds_in"
            );
            // The 255-byte SET_FEATURE input buffer; bytes 0..8 are recorded as the offset from the buffer
            // start, so build the image for a buffer at address 0.
            let image = with_set_state_packet(true, |bytes| {
                let mut image = *bytes;
                let pointer = u64::from_le_bytes(image[..8].try_into().unwrap());
                assert_eq!(pointer, bytes.as_ptr() as u64 + 16);
                image[..8].copy_from_slice(&16_u64.to_le_bytes());
                image
            })
            .unwrap();
            assert_eq!(
                to_hex(&image),
                r.get("setfeature_in"),
                "{ctx}: setfeature_in"
            );
            let ops = r.get("ops");
            assert!(
                ops.starts_with(&format!(
                    "WdfIoTargetSendInternalIoctlSynchronously(target=lower,req=null,ioctl={SET_STATE_IOCTL:08x},in=buf(len={SET_STATE_PACKET_LEN},"
                )),
                "{ctx}"
            );
            assert!(ops.contains("relptr=0>16"), "{ctx}: self-pointer");
            let lower_status = match wdf_failures(r).first() {
                Some((f, s)) => {
                    assert_eq!(f, "WdfIoTargetSendInternalIoctlSynchronously", "{ctx}");
                    u32::from_str_radix(s, 16).unwrap() as i32
                }
                None => STATUS_SUCCESS,
            };
            let ev = r.get("ev");
            if ev.starts_with("SelfManagedIo") {
                let mut qpc = 0;
                let out = rt.on_self_managed_io_init_or_restart(
                    &mut ReadTracker::<u32>::new(),
                    lower_status,
                    || {
                        qpc += 1;
                        QPC_FREQUENCY
                    },
                );
                assert_eq!(out.status, r.status("status"), "{ctx}");
                if let Some(due) = out.start_recovery_timer {
                    assert_eq!(due, RECOVERY_TIMER_DUE_TIME);
                    assert_eq!(r.get("timer_armed"), "1", "{ctx}");
                    assert_eq!(r.dec::<i64>("timer_due"), due, "{ctx}");
                    assert!(
                        ops.contains(&format!("WdfTimerStart(timer,due={due})")),
                        "{ctx}"
                    );
                } else {
                    assert!(!r.has("timer_armed"), "{ctx}");
                    assert_eq!(rt.qpc_frequency(), r.dec::<i64>("lrt_out"), "{ctx}");
                }
                assert_eq!(
                    qpc,
                    ops.matches("KeQueryPerformanceCounter").count(),
                    "{ctx}: QPC calls"
                );
            } else {
                assert_eq!(ev, "TimerCallback", "{ctx}");
                assert_eq!(r.get("status"), "-", "{ctx}");
                let mut tracker = ReadTracker::<u32>::new();
                let ticket = rt.begin_recovery(&tracker).expect("unconfigured device");
                assert!(rt.finish_recovery(&mut tracker, ticket, lower_status, || QPC_FREQUENCY));
                // SPI-B12 (P3): the C callback issued exactly one read whatever was queued; the port
                // issues one per queued upstream read, and nothing is queued in this record.
                assert_eq!(r.dec::<u32>("lower_reads"), 1, "{ctx}: C lower_reads");
                assert_eq!(rt.backlog_reads(&tracker, 0), 0, "{ctx}: lower_reads");
                assert_eq!(rt.backlog_reads(&tracker, 1), 1, "{ctx}: lower_reads");
            }
            assert_eq!(
                rt.device_status() as u32,
                r.dec::<u32>("ds_out"),
                "{ctx}: ds_out"
            );
            tally.add(SETSTATE);
        } else if case.starts_with("lower-read-") {
            let (_, rt) = bring_up();
            let _status = rt.on_self_managed_io_init_or_restart(
                &mut ReadTracker::<u32>::new(),
                STATUS_SUCCESS,
                || QPC_FREQUENCY,
            );
            assert_eq!(rt.device_status(), DeviceStatus::D0ActiveAndConfigured);
            assert_eq!(r.dec::<u32>("ds_in"), 1);
            assert_eq!(r.hex32("code"), LOWER_READ_IOCTL);
            // The upstream read is queued and the driver decides to issue a lower read.
            assert!(rt.should_issue_lower_read(), "{ctx}");
            assert_eq!(r.get("completed"), "queued", "{ctx}");
            assert_eq!(r.get("q_out"), "1", "{ctx}");
            let wdf = wdf_failures(r);
            if wdf.is_empty() {
                assert_eq!(r.hex32("lower_ioctl"), LOWER_READ_IOCTL, "{ctx}");
                assert_eq!(
                    r.dec::<usize>("lower_outlen"),
                    LOWER_READ_BUFFER_LEN,
                    "{ctx}"
                );
                assert_eq!(r.get("lower_reads"), "1", "{ctx}");
                tally.add(READ_ISSUE_OK);
            } else {
                assert_eq!(r.get("lower_reads"), "0", "{ctx}");
                tally.add(READ_ISSUE_FW);
            }
        } else {
            panic!("{ctx}: unknown record kind");
        }
    }
    assert_eq!(tally.get(PREP), 28);
    assert_eq!(tally.get(PREP_RESTART), 10);
    assert_eq!(tally.get(SETSTATE), 3);
    assert_eq!(tally.get(READ_ISSUE_OK), 1);
    assert_eq!(tally.get(READ_ISSUE_FW), 4);
    assert_eq!(tally.total(), 46);
}
