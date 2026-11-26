//! Replays `power.txt`: 12 event sequences over D0Entry, SelfManagedIoInit/Restart, the recovery timer,
//! upstream reads, lower completions, D0Exit and framework cancellation.
//!
//! The core makes every driver decision ([`DeviceRuntime`]); this test is the framework model: the FIFO
//! `HidQueue`, the list of in-flight lower reads, the timer's armed/due state, and the WDF failures the
//! record injects (`wdf=`). After each step the snapshot (`out_*`), the completed request and its bytes,
//! the completion statuses in `ops` and the number of `KeQueryPerformanceCounter` calls are compared.

mod common;

use std::collections::VecDeque;

use amtptp_spikm_core::device::{APPLE_VENDOR_ID, lookup_trackpad};
use amtptp_spikm_core::runtime::{D0_EXIT_DRAIN_STATUS, DeviceRuntime};
use amtptp_spikm_core::status::NtStatus;
use amtptp_spikm_core::wire::LOWER_READ_BUFFER_LEN;
use common::{Record, Tally, load, prefill, to_hex, wdf_failures};

const QPC_FREQUENCY: i64 = 10_000_000;

#[derive(Default)]
struct Framework {
    hidq: VecDeque<String>,
    inflight: Vec<String>,
    timer_armed: bool,
    timer_due: i64,
    lower_created: usize,
    outputs: std::collections::BTreeMap<String, Vec<u8>>,
    /// (label, status, information) completions of this step, in order.
    completions: Vec<(String, NtStatus, usize)>,
    qpc_calls: usize,
}

impl Framework {
    fn list(v: impl Iterator<Item = String>) -> String {
        let s: Vec<String> = v.collect();
        if s.is_empty() {
            "-".into()
        } else {
            s.join(",")
        }
    }

    fn check(&self, r: &Record, prefix: &str, rt: &DeviceRuntime, ctx: &str) {
        assert_eq!(
            rt.device_status() as u32,
            r.dec::<u32>(&format!("{prefix}_ds")),
            "{ctx}: {prefix}_ds"
        );
        assert_eq!(
            rt.last_report_time(),
            r.dec::<i64>(&format!("{prefix}_lrt")),
            "{ctx}: {prefix}_lrt"
        );
        assert_eq!(
            Self::list(self.hidq.iter().cloned()),
            r.get(&format!("{prefix}_hidq")),
            "{ctx}: {prefix}_hidq"
        );
        assert_eq!(
            Self::list(self.inflight.iter().cloned()),
            r.get(&format!("{prefix}_inflight")),
            "{ctx}: {prefix}_inflight"
        );
        let timer = format!("{}:{}", u8::from(self.timer_armed), self.timer_due);
        assert_eq!(
            timer,
            r.get(&format!("{prefix}_timer")),
            "{ctx}: {prefix}_timer"
        );
    }

    /// `AmtPtpSpiInputIssueRequest` as the glue performs it, with the record's injected WDF failures.
    fn issue_lower_read(&mut self, wdf: &[(String, String)]) {
        let failed = |name: &str| wdf.iter().any(|(f, _)| f == name);
        if failed("WdfRequestCreate") {
            return;
        }
        self.lower_created += 1;
        let label = format!("l{}", self.lower_created);
        if failed("WdfMemoryCreateFromLookaside")
            || failed("WdfIoTargetFormatRequestForInternalIoctl")
            || failed("WdfRequestSend")
        {
            return; // deleted again; the upstream request stays queued
        }
        self.inflight.push(label);
    }
}

/// The `pkt1` lower buffer: n=1, click=1, X=100, Y=2000, Pressure=30, TouchMajor=800, TouchMinor=600.
fn pkt1() -> Vec<u8> {
    let mut b = vec![0u8; LOWER_READ_BUFFER_LEN];
    b[0] = 2;
    b[1] = 1;
    b[30] = 1;
    let f = 46;
    b[f + 4..f + 6].copy_from_slice(&100i16.to_le_bytes());
    b[f + 6..f + 8].copy_from_slice(&2000i16.to_le_bytes());
    b[f + 18..f + 20].copy_from_slice(&800i16.to_le_bytes());
    b[f + 20..f + 22].copy_from_slice(&600i16.to_le_bytes());
    b[f + 26..f + 28].copy_from_slice(&30i16.to_le_bytes());
    b
}

/// `Name(args)` -> (`Name`, `args`).
fn call(ev: &str) -> (&str, &str) {
    let (name, rest) = ev.split_once('(').expect("event call");
    (name, rest.strip_suffix(')').expect("closing paren"))
}

fn arg<'a>(args: &'a str, key: &str) -> &'a str {
    args.split(',')
        .find_map(|a| a.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("arg {key} in {args}"))
}

fn hex_status(s: &str) -> NtStatus {
    u32::from_str_radix(s, 16).expect("status") as i32
}

/// `WdfRequestComplete(uN,status,info=N)` entries of `ops`.
fn completions_in_ops(ops: &str) -> Vec<(String, NtStatus, usize)> {
    ops.split(';')
        .filter_map(|op| op.strip_prefix("WdfRequestComplete("))
        .map(|a| {
            let a = a.strip_suffix(')').expect("paren");
            let mut it = a.split(',');
            let label = it.next().expect("label").to_string();
            let status = hex_status(it.next().expect("status"));
            let info = it
                .next()
                .and_then(|i| i.strip_prefix("info="))
                .expect("info")
                .parse()
                .expect("info value");
            (label, status, info)
        })
        .collect()
}

#[test]
fn power_sequences() {
    let recs = load("power.txt");
    assert_eq!(recs.len(), 100);
    let trackpad = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).expect("0272");
    let mut tally = Tally::default();
    let mut seqs = 0;
    let mut fw = Framework::default();
    let mut rt = DeviceRuntime::new();
    for r in &recs {
        let ctx = format!("power.txt:{} {} step {}", r.line, r.case(), r.get("step"));
        if r.get("step") == "0" {
            seqs += 1;
            fw = Framework::default();
            rt = DeviceRuntime::new();
        }
        fw.completions.clear();
        fw.qpc_calls = 0;
        fw.check(r, "in", &rt, &ctx);
        let wdf = wdf_failures(r);
        let (name, args) = call(r.get("ev"));
        let mut last_completed: Option<String> = None;
        let ret: String = match name {
            "D0Entry" => {
                fw.qpc_calls += 1;
                format!("{:08x}", rt.on_d0_entry(QPC_FREQUENCY) as u32)
            }
            "SelfManagedIoInit" | "SelfManagedIoRestart" => {
                let lower = hex_status(arg(args, "lower_setfeature"));
                let mut q = 0;
                let out = rt.on_self_managed_io_init_or_restart(lower, || {
                    q += 1;
                    QPC_FREQUENCY
                });
                fw.qpc_calls += q;
                if let Some(due) = out.start_recovery_timer {
                    // WdfTimerStart (re-)arms the one-shot timer.
                    fw.timer_armed = true;
                    fw.timer_due = due;
                }
                format!("{:08x}", out.status as u32)
            }
            "TimerCallback" => {
                // The one-shot timer fired; its due time stays as the harness shows it.
                fw.timer_armed = false;
                let lower = hex_status(arg(args, "lower_setfeature"));
                let mut reads = 0;
                rt.on_recovery_timer(lower, || {
                    reads += 1;
                    fw.issue_lower_read(&wdf);
                });
                assert!(reads <= 1);
                "-".into()
            }
            "READ_REPORT" => {
                let label = args.split(',').next().expect("label").to_string();
                let outlen: usize = arg(args, "outlen").parse().unwrap();
                fw.outputs.insert(label.clone(), prefill(outlen));
                // WdfRequestForwardToIoQueue succeeds in every power record.
                fw.hidq.push_back(label);
                if rt.should_issue_lower_read() {
                    fw.issue_lower_read(&wdf);
                }
                "queued".into()
            }
            "LowerReadComplete" => {
                let label = args.split(',').next().expect("label").to_string();
                let pos = fw
                    .inflight
                    .iter()
                    .position(|l| *l == label)
                    .expect("in flight");
                fw.inflight.remove(pos);
                assert_eq!(arg(args, "pkt"), "pkt1");
                let len: u64 = arg(args, "len").parse().unwrap();
                // Retrieve the oldest upstream read first; none -> the data is dropped (SPI-B11).
                if let Some(up) = fw.hidq.pop_front() {
                    let mut q = 0;
                    let result = rt.complete_lower_read(trackpad, len, &pkt1(), || {
                        q += 1;
                        QPC_FREQUENCY
                    });
                    fw.qpc_calls += q;
                    match result {
                        Ok(bytes) => {
                            fw.outputs.get_mut(&up).unwrap()[..30].copy_from_slice(&bytes);
                            fw.completions.push((up.clone(), 0, 30));
                        }
                        Err(s) => fw.completions.push((up.clone(), s, 0)),
                    }
                    last_completed = Some(up);
                }
                "-".into()
            }
            "D0Exit" => {
                let ret = rt.on_d0_exit();
                while let Some(up) = fw.hidq.pop_front() {
                    fw.completions.push((up.clone(), D0_EXIT_DRAIN_STATUS, 0));
                    last_completed = Some(up);
                }
                format!("{:08x}", ret as u32)
            }
            "FrameworkCancelsQueued" => {
                let label = args.to_string();
                assert_eq!(fw.hidq.front(), Some(&label), "{ctx}: oldest is cancelled");
                fw.hidq.pop_front();
                "-".into()
            }
            other => panic!("{ctx}: event {other}"),
        };
        tally.add(match name {
            "D0Entry" => "D0Entry",
            "SelfManagedIoInit" | "SelfManagedIoRestart" => "SelfManagedIoInit/Restart",
            "TimerCallback" => "TimerCallback",
            "READ_REPORT" => "READ_REPORT",
            "LowerReadComplete" => "LowerReadComplete",
            "D0Exit" => "D0Exit",
            _ => "FrameworkCancelsQueued (framework action)",
        });
        assert_eq!(ret, r.get("ret"), "{ctx}: ret");
        fw.check(r, "out", &rt, &ctx);
        match &last_completed {
            Some(up) => {
                assert_eq!(r.get("completed_req"), up, "{ctx}: completed_req");
                assert_eq!(
                    to_hex(&fw.outputs[up]),
                    r.get("completed_out"),
                    "{ctx}: completed_out"
                );
            }
            None => assert!(!r.has("completed_req"), "{ctx}"),
        }
        let ops = r.get("ops");
        assert_eq!(
            completions_in_ops(ops),
            fw.completions,
            "{ctx}: completions"
        );
        assert_eq!(
            ops.matches("KeQueryPerformanceCounter").count(),
            fw.qpc_calls,
            "{ctx}: QPC calls"
        );
    }
    assert_eq!(seqs, 12);
    assert_eq!(tally.get("D0Entry"), 15);
    assert_eq!(tally.get("D0Exit"), 11);
    assert_eq!(tally.get("SelfManagedIoInit/Restart"), 15);
    assert_eq!(tally.get("TimerCallback"), 5);
    assert_eq!(tally.get("READ_REPORT"), 31);
    assert_eq!(tally.get("LowerReadComplete"), 22);
    assert_eq!(tally.get("FrameworkCancelsQueued (framework action)"), 1);
    assert_eq!(tally.total(), 100);
}
