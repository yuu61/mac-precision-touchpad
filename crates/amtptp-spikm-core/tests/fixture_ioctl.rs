//! Replays `ioctl.txt`: internal-IOCTL routing, unsupported codes, `IOCTL_HID_GET_STRING` forwarding and
//! `IOCTL_HID_READ_REPORT` dispatch per `DeviceStatus`.

mod common;

use amtptp_spikm_core::ioctl::{InternalIoctl, NOT_SUPPORTED_STATUS, classify_internal_ioctl};
use amtptp_spikm_core::runtime::{DeviceRuntime, DeviceStatus};
use amtptp_spikm_core::status::{STATUS_SUCCESS, STATUS_UNSUCCESSFUL};
use common::{Record, Tally, load, wdf_failures};

const UNSUPPORTED: &str = "driver: NotSupported -> c00000bb";
const STRING_FWD: &str = "GET_STRING forwarded (send-and-forget accepted)";
const STRING_FAIL: &str =
    "framework: GET_STRING WdfRequestSend FALSE -> completed with WdfRequestGetStatus";
const READ_QUEUED: &str = "READ_REPORT queued; lower read decided by DeviceStatus";
const READ_FWD_FAIL: &str = "framework: WdfRequestForwardToIoQueue failed -> completed with it";

/// A runtime in the record's `ds_in` state, reached through the public power API.
pub fn runtime_in(ds: u32) -> DeviceRuntime {
    let rt = DeviceRuntime::new();
    match ds {
        0 => {}
        2 => {
            rt.on_d0_entry(10_000_000);
        }
        1 => {
            rt.on_d0_entry(10_000_000);
            let _ = rt.on_self_managed_io_init_or_restart(STATUS_SUCCESS, || 10_000_000);
        }
        _ => {
            rt.on_d0_entry(10_000_000);
            let _ = rt.on_self_managed_io_init_or_restart(STATUS_UNSUCCESSFUL, || 0);
        }
    }
    assert_eq!(rt.device_status() as u32, ds);
    rt
}

fn get_status_from_ops(r: &Record) -> i32 {
    let ops = r.get("ops");
    let tail = ops
        .split(';')
        .find_map(|op| op.strip_prefix("WdfRequestGetStatus(u1)="))
        .expect("WdfRequestGetStatus in ops");
    u32::from_str_radix(tail, 16).expect("status") as i32
}

#[test]
fn internal_ioctl_routing() {
    let recs = load("ioctl.txt");
    assert_eq!(recs.len(), 76);
    let mut tally = Tally::default();
    for r in &recs {
        let ctx = format!("ioctl.txt:{} {}", r.line, r.case());
        let rt = runtime_in(r.dec("ds_in"));
        let q_in: usize = r.dec("q_in");
        let wdf = wdf_failures(r);
        let (completed, status, q_out, lower_reads): (&str, Option<i32>, usize, u32) =
            match classify_internal_ioctl(r.hex32("code")) {
                InternalIoctl::NotSupported => {
                    assert!(wdf.is_empty(), "{ctx}");
                    tally.add(UNSUPPORTED);
                    ("1", Some(NOT_SUPPORTED_STATUS), q_in, 0)
                }
                InternalIoctl::GetString => {
                    if wdf.is_empty() {
                        tally.add(STRING_FWD);
                        ("fwd", None, q_in, 0)
                    } else {
                        assert_eq!(wdf[0].0, "WdfRequestSend", "{ctx}");
                        tally.add(STRING_FAIL);
                        ("1", Some(get_status_from_ops(r)), q_in, 0)
                    }
                }
                InternalIoctl::ReadReport => match wdf.first() {
                    Some((f, s)) => {
                        assert_eq!(f, "WdfRequestForwardToIoQueue", "{ctx}");
                        tally.add(READ_FWD_FAIL);
                        (
                            "1",
                            Some(u32::from_str_radix(s, 16).unwrap() as i32),
                            q_in,
                            0,
                        )
                    }
                    None => {
                        tally.add(READ_QUEUED);
                        let issue = rt.should_issue_lower_read();
                        assert_eq!(
                            issue,
                            rt.device_status() == DeviceStatus::D0ActiveAndConfigured
                        );
                        ("queued", None, q_in + 1, u32::from(issue))
                    }
                },
                other => panic!("{ctx}: unexpected {other:?}"),
            };
        assert_eq!(r.get("completed"), completed, "{ctx}: completed");
        match status {
            Some(s) => assert_eq!(r.status("status"), s, "{ctx}: status"),
            None => assert_eq!(r.get("status"), "-", "{ctx}"),
        }
        assert_eq!(r.get("info"), "0", "{ctx}");
        assert_eq!(r.get("infoset"), "0", "{ctx}");
        assert_eq!(r.get("chg"), "-", "{ctx}");
        assert_eq!(r.dec::<usize>("q_out"), q_out, "{ctx}: q_out");
        assert_eq!(
            r.dec::<u32>("lower_reads"),
            lower_reads,
            "{ctx}: lower_reads"
        );
    }
    assert_eq!(tally.get(UNSUPPORTED), 64);
    assert_eq!(tally.get(STRING_FWD), 2);
    assert_eq!(tally.get(STRING_FAIL), 4);
    assert_eq!(tally.get(READ_QUEUED), 4);
    assert_eq!(tally.get(READ_FWD_FAIL), 2);
    assert_eq!(tally.total(), 76);
}
