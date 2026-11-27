//! Replays `power.txt`: 12 event sequences over `D0Entry`, SelfManagedIoInit/Restart, the recovery timer,
//! upstream reads, lower completions, `D0Exit` and framework cancellation.
//!
//! The core makes every driver decision ([`DeviceRuntime`] with a [`ReadTracker`]); this test is the
//! framework model: the FIFO `HidQueue`, the lower reads in flight, the timer's armed/due state, and the
//! WDF failures the record injects (`wdf=`). After each step the snapshot (`out_*`), the completed request
//! and its bytes, the completion statuses in `ops` and the number of `KeQueryPerformanceCounter` calls
//! are compared.
//!
//! Every `D0Exit` is preceded by `SelfManagedIoSuspend`, as KMDF calls it: it stops the recovery timer
//! and cancels the reads in flight (their completions are then stale and leave `HidQueue` alone). Five
//! sequences recorded the C driver's stop/resume defects (SPI-B12/B13: a timer that fires in D3 or after
//! a successful restart, and queued reads that wait for a lower read nobody issues). They are not
//! replayed; the tests after [`power_sequences`] run the same event scripts against the corrected
//! behaviour.

#![cfg(test)]

mod common;

use std::collections::{BTreeMap, VecDeque};

use amtptp_spikm_core::device::{APPLE_VENDOR_ID, SpiTrackpadInfo, lookup_trackpad};
use amtptp_spikm_core::runtime::{
    ContactTracker, D0_EXIT_DRAIN_STATUS, DeviceRuntime, DeviceStatus, LowerReadOrigin,
    RECOVERY_TIMER_DUE_TIME, ReadTicket, ReadTracker,
};
use amtptp_spikm_core::status::{NtStatus, STATUS_SUCCESS};
use amtptp_spikm_core::wire::LOWER_READ_BUFFER_LEN;
use common::{Record, Tally, load, prefill, to_hex, wdf_failures};

const QPC_FREQUENCY: i64 = 10_000_000;

/// The sequences whose C behaviour was a defect (see the module docs), with their record counts.
const SUPERSEDED: [(&str, usize); 5] = [
    ("smio-fail-timer-ok-backlog", 12),
    ("timer-fires-in-d3", 6),
    ("restart-fail-rearms-timer", 8),
    ("stale-timer-after-restart-ok", 9),
    ("reads-before-smio-stay-queued", 9),
];
const REPLAYED: &str = "replayed";
const REWRITTEN: &str = "superseded (SPI-B12/B13, rewritten below)";

/// The framework model plus the device state the driver keeps (runtime and tracker).
struct Framework {
    rt: DeviceRuntime,
    tracker: ReadTracker<u32>,
    contacts: ContactTracker,
    trackpad: &'static SpiTrackpadInfo,
    hidq: VecDeque<String>,
    /// Lower reads outstanding at the lower driver, with the ticket stored in their context.
    inflight: Vec<(String, ReadTicket)>,
    timer_armed: bool,
    timer_due: i64,
    lower_created: usize,
    outputs: BTreeMap<String, Vec<u8>>,
    /// (label, status, information) completions of this step, in order.
    completions: Vec<(String, NtStatus, usize)>,
    qpc_calls: usize,
    /// Mode switches sent (SelfManagedIoInit/Restart and the timer).
    set_state_calls: usize,
    last_completed: Option<String>,
}

impl Framework {
    fn new() -> Self {
        Self {
            rt: DeviceRuntime::new(),
            tracker: ReadTracker::new(),
            contacts: ContactTracker::new(),
            trackpad: lookup_trackpad(APPLE_VENDOR_ID, 0x0272).expect("0272"),
            hidq: VecDeque::new(),
            inflight: Vec::new(),
            timer_armed: false,
            timer_due: 0,
            lower_created: 0,
            outputs: BTreeMap::new(),
            completions: Vec::new(),
            qpc_calls: 0,
            set_state_calls: 0,
            last_completed: None,
        }
    }

    fn list(v: impl Iterator<Item = String>) -> String {
        let s: Vec<String> = v.collect();
        if s.is_empty() {
            "-".into()
        } else {
            s.join(",")
        }
    }

    fn hidq(&self) -> String {
        Self::list(self.hidq.iter().cloned())
    }

    fn inflight(&self) -> String {
        Self::list(self.inflight.iter().map(|(label, _)| label.clone()))
    }

    fn timer(&self) -> String {
        format!("{}:{}", u8::from(self.timer_armed), self.timer_due)
    }

    fn check(&self, r: &Record, prefix: &str, ctx: &str) {
        assert_eq!(
            self.rt.device_status() as u32,
            r.dec::<u32>(&format!("{prefix}_ds")),
            "{ctx}: {prefix}_ds"
        );
        assert_eq!(
            self.rt.qpc_frequency(),
            r.dec::<i64>(&format!("{prefix}_lrt")),
            "{ctx}: {prefix}_lrt"
        );
        assert_eq!(
            self.hidq(),
            r.get(&format!("{prefix}_hidq")),
            "{ctx}: {prefix}_hidq"
        );
        assert_eq!(
            self.inflight(),
            r.get(&format!("{prefix}_inflight")),
            "{ctx}: {prefix}_inflight"
        );
        assert_eq!(
            self.timer(),
            r.get(&format!("{prefix}_timer")),
            "{ctx}: {prefix}_timer"
        );
    }

    /// `AmtPtpSpiInputIssueRequest` as the glue performs it, with the record's injected WDF failures:
    /// create, memory, format and completion routine, then the claim, then the send.
    fn issue_lower_read(&mut self, wdf: &[(String, String)]) {
        let failed = |name: &str| wdf.iter().any(|(f, _)| f == name);
        if failed("WdfRequestCreate") {
            return;
        }
        self.lower_created += 1;
        let number = u32::try_from(self.lower_created).expect("label");
        let label = format!("l{number}");
        if failed("WdfMemoryCreateFromLookaside")
            || failed("WdfIoTargetFormatRequestForInternalIoctl")
        {
            return; // deleted again; the upstream request stays queued
        }
        let Ok(ticket) = self.rt.claim_lower_read(&mut self.tracker, number) else {
            return; // deleted without being sent
        };
        if failed("WdfRequestSend") {
            let _origin = self.rt.finish_lower_read(&mut self.tracker, number, ticket);
            return;
        }
        self.inflight.push((label, ticket));
    }

    /// After a transition to configured: one lower read per queued upstream read without one.
    fn issue_backlog(&mut self, wdf: &[(String, String)]) {
        let reads = self.rt.backlog_reads(&self.tracker, self.hidq.len());
        for _ in 0..reads {
            self.issue_lower_read(wdf);
        }
    }

    /// `SelfManagedIoSuspend`: stop the timer, cancel the reads in flight (they stay outstanding until
    /// the lower driver completes them).
    fn suspend(&mut self) {
        let cancelled = self.rt.suspend(&mut self.tracker);
        self.contacts.reset();
        self.timer_armed = false;
        for read in cancelled.iter() {
            let label = format!("l{read}");
            assert!(
                self.inflight.iter().any(|(l, _)| *l == label),
                "{label} is cancelled while in flight"
            );
        }
    }

    /// Runs one event (fixture syntax) and returns the callback's `ret` field.
    fn step(&mut self, ev: &str, wdf: &[(String, String)]) -> String {
        self.completions.clear();
        self.qpc_calls = 0;
        self.last_completed = None;
        let (name, args) = call(ev);
        match name {
            "D0Entry" => {
                self.qpc_calls += 1;
                self.contacts.reset();
                format!("{:08x}", self.rt.on_d0_entry(QPC_FREQUENCY) as u32)
            }
            "SelfManagedIoInit" | "SelfManagedIoRestart" => {
                self.set_state_calls += 1;
                let lower = hex_status(arg(args, "lower_setfeature"));
                let mut q = 0;
                let out =
                    self.rt
                        .on_self_managed_io_init_or_restart(&mut self.tracker, lower, || {
                            q += 1;
                            QPC_FREQUENCY
                        });
                self.qpc_calls += q;
                if let Some(due) = out.start_recovery_timer {
                    // WdfTimerStart (re-)arms the one-shot timer.
                    self.timer_armed = true;
                    self.timer_due = due;
                }
                if out.configured {
                    self.issue_backlog(wdf);
                }
                format!("{:08x}", out.status as u32)
            }
            "TimerCallback" => {
                // The one-shot timer fired (or its callback was already queued when it was stopped); its
                // due time stays as the harness shows it.
                self.timer_armed = false;
                if let Some(ticket) = self.rt.begin_recovery(&self.tracker) {
                    self.set_state_calls += 1;
                    let lower = hex_status(arg(args, "lower_setfeature"));
                    let mut q = 0;
                    let configured =
                        self.rt
                            .finish_recovery(&mut self.tracker, ticket, lower, || {
                                q += 1;
                                QPC_FREQUENCY
                            });
                    self.qpc_calls += q;
                    if configured {
                        self.issue_backlog(wdf);
                    }
                }
                "-".into()
            }
            "READ_REPORT" => {
                let label = args.split(',').next().expect("label").to_owned();
                let outlen: usize = arg(args, "outlen").parse().expect("outlen");
                self.outputs.insert(label.clone(), prefill(outlen));
                // WdfRequestForwardToIoQueue succeeds in every power record.
                self.hidq.push_back(label);
                if self.rt.should_issue_lower_read() {
                    self.issue_lower_read(wdf);
                }
                "queued".into()
            }
            "LowerReadComplete" => {
                let label = args.split(',').next().expect("label").to_owned();
                let pos = self
                    .inflight
                    .iter()
                    .position(|(l, _)| *l == label)
                    .unwrap_or_else(|| panic!("{label} in flight"));
                let (_, ticket) = self.inflight.remove(pos);
                assert_eq!(arg(args, "pkt"), "pkt1");
                let len: u64 = arg(args, "len").parse().expect("len");
                let number: u32 = label.trim_start_matches('l').parse().expect("label number");
                let origin = self.rt.finish_lower_read(&mut self.tracker, number, ticket);
                // A stale read is dropped. A current one retrieves the oldest upstream read first; none ->
                // the data is dropped (SPI-B11).
                if origin == LowerReadOrigin::Current
                    && let Some(up) = self.hidq.pop_front()
                {
                    let mut q = 0;
                    let result = self.rt.complete_lower_read(
                        self.trackpad,
                        &mut self.contacts,
                        len,
                        &pkt1(),
                        || {
                            q += 1;
                            QPC_FREQUENCY
                        },
                    );
                    self.qpc_calls += q;
                    match result {
                        Ok(bytes) => {
                            self.outputs.get_mut(&up).expect("output")[..30]
                                .copy_from_slice(&bytes);
                            self.completions.push((up.clone(), 0, 30));
                        }
                        Err(s) => self.completions.push((up.clone(), s, 0)),
                    }
                    self.last_completed = Some(up);
                }
                "-".into()
            }
            "D0Exit" => {
                self.suspend();
                let (ret, cancelled) = self.rt.on_d0_exit(&mut self.tracker);
                assert_eq!(
                    cancelled.iter().count(),
                    0,
                    "SelfManagedIoSuspend already ran"
                );
                while let Some(up) = self.hidq.pop_front() {
                    self.completions.push((up.clone(), D0_EXIT_DRAIN_STATUS, 0));
                    self.last_completed = Some(up);
                }
                format!("{:08x}", ret as u32)
            }
            "FrameworkCancelsQueued" => {
                let label = args.to_owned();
                assert_eq!(self.hidq.front(), Some(&label), "oldest is cancelled");
                self.hidq.pop_front();
                "-".into()
            }
            other => panic!("event {other}"),
        }
    }

    /// The report bytes of a completed upstream read.
    fn output(&self, label: &str) -> String {
        to_hex(&self.outputs[label])
    }
}

/// The `pkt1` lower buffer: n=1, click=1, X=100, Y=2000, Pressure=30, TouchMajor=800, TouchMinor=600.
fn pkt1() -> Vec<u8> {
    let mut b = vec![0_u8; LOWER_READ_BUFFER_LEN];
    b[0] = 2;
    b[1] = 1;
    b[30] = 1;
    let f = 46;
    b[f + 4..f + 6].copy_from_slice(&100_i16.to_le_bytes());
    b[f + 6..f + 8].copy_from_slice(&2000_i16.to_le_bytes());
    b[f + 18..f + 20].copy_from_slice(&800_i16.to_le_bytes());
    b[f + 20..f + 22].copy_from_slice(&600_i16.to_le_bytes());
    b[f + 26..f + 28].copy_from_slice(&30_i16.to_le_bytes());
    b
}

/// The report `pkt1` gives in a fresh reporting session (`ScanTime` 0).
const PKT1_REPORT: &str = "0503f2127a12000000000000000000000000000000000000000000000101";

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
            let label = it.next().expect("label").to_owned();
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
    let mut tally = Tally::default();
    let mut events = Tally::default();
    let mut seqs = 0;
    let mut fw = Framework::new();
    for r in &recs {
        let ctx = format!("power.txt:{} {} step {}", r.line, r.case(), r.get("step"));
        if r.get("step") == "0" {
            seqs += 1;
            fw = Framework::new();
        }
        if SUPERSEDED.iter().any(|(seq, _)| *seq == r.case()) {
            tally.add(REWRITTEN);
            continue;
        }
        fw.check(r, "in", &ctx);
        let wdf = wdf_failures(r);
        let ev = r.get("ev");
        let ret = fw.step(ev, &wdf);
        events.add(match call(ev).0 {
            "D0Entry" => "D0Entry",
            "SelfManagedIoInit" | "SelfManagedIoRestart" => "SelfManagedIoInit/Restart",
            "TimerCallback" => "TimerCallback",
            "READ_REPORT" => "READ_REPORT",
            "LowerReadComplete" => "LowerReadComplete",
            "D0Exit" => "D0Exit",
            _ => "FrameworkCancelsQueued (framework action)",
        });
        assert_eq!(ret, r.get("ret"), "{ctx}: ret");
        fw.check(r, "out", &ctx);
        match &fw.last_completed {
            Some(up) => {
                assert_eq!(r.get("completed_req"), up, "{ctx}: completed_req");
                assert_eq!(
                    fw.output(up),
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
        tally.add(REPLAYED);
    }
    assert_eq!(seqs, 12);
    assert_eq!(
        tally.get(REWRITTEN),
        SUPERSEDED.iter().map(|(_, n)| n).sum::<usize>()
    );
    assert_eq!(tally.get(REPLAYED), 56);
    assert_eq!(tally.total(), 100);
    assert_eq!(events.get("D0Entry"), 8);
    assert_eq!(events.get("D0Exit"), 6);
    assert_eq!(events.get("SelfManagedIoInit/Restart"), 8);
    assert_eq!(events.get("TimerCallback"), 1);
    assert_eq!(events.get("READ_REPORT"), 19);
    assert_eq!(events.get("LowerReadComplete"), 13);
    assert_eq!(events.get("FrameworkCancelsQueued (framework action)"), 1);
}

// The stop/resume sequences of `power.txt` that recorded the C defects, against the corrected driver.

/// Runs `events` from a new device and checks the state after each one: `(event, status, hidq,
/// inflight, timer)`.
fn run(steps: &[(&str, DeviceStatus, &str, &str, &str)]) -> Framework {
    let mut fw = Framework::new();
    for (i, (ev, status, hidq, inflight, timer)) in steps.iter().enumerate() {
        let ctx = format!("step {i} {ev}");
        let _ret = fw.step(ev, &[]);
        assert_eq!(fw.rt.device_status(), *status, "{ctx}: status");
        assert_eq!(fw.hidq(), *hidq, "{ctx}: hidq");
        assert_eq!(fw.inflight(), *inflight, "{ctx}: inflight");
        assert_eq!(fw.timer(), *timer, "{ctx}: timer");
    }
    fw
}

const ARMED: &str = "1:-50000000";
const FIRED: &str = "0:-50000000";
const OFF: &str = "0:0";
use DeviceStatus::{D0ActiveAndConfigured as ON, D0ActiveAndUnconfigured as UNCONF, D3};

const FAIL: &str = "c0000001";

fn smio(kind: &str, status: &str) -> String {
    format!("{kind}(lower_setfeature={status})")
}

/// `timer-fires-in-d3`: the timer armed by a failed `SelfManagedIoInit` is stopped by the suspend. A
/// callback that still runs afterwards does nothing: no mode switch, no read, the device stays in D3.
/// The restart then configures the device and serves the read queued meanwhile.
#[test]
fn recovery_timer_after_d0_exit_does_nothing() {
    let init = smio("SelfManagedIoInit", FAIL);
    let restart = smio("SelfManagedIoRestart", "00000000");
    let fw = run(&[
        ("D0Entry(prev=D3)", UNCONF, "-", "-", OFF),
        (&init, UNCONF, "-", "-", ARMED),
        ("D0Exit(target=D3)", D3, "-", "-", "0:-50000000"),
        (
            "TimerCallback(lower_setfeature=00000000)",
            D3,
            "-",
            "-",
            FIRED,
        ),
        ("READ_REPORT(u1,outlen=30)", D3, "u1", "-", FIRED),
        ("D0Entry(prev=D3)", UNCONF, "u1", "-", FIRED),
        (&restart, ON, "u1", "l1", FIRED),
        ("LowerReadComplete(l1,len=76,pkt=pkt1)", ON, "-", "-", FIRED),
    ]);
    // Only SelfManagedIoInit and the restart sent the mode switch.
    assert_eq!(fw.set_state_calls, 2);
    assert_eq!(fw.last_completed.as_deref(), Some("u1"));
    assert_eq!(fw.output("u1"), PKT1_REPORT);
}

/// `reads-before-smio-stay-queued`: reads queued while unconfigured each get a lower read as soon as
/// `SelfManagedIoInit` configures the device.
#[test]
fn reads_queued_before_configuration_are_served() {
    let init = smio("SelfManagedIoInit", "00000000");
    let mut fw = run(&[
        ("D0Entry(prev=D3)", UNCONF, "-", "-", OFF),
        ("READ_REPORT(u1,outlen=30)", UNCONF, "u1", "-", OFF),
        ("READ_REPORT(u2,outlen=30)", UNCONF, "u1,u2", "-", OFF),
        (&init, ON, "u1,u2", "l1,l2", OFF),
        ("READ_REPORT(u3,outlen=30)", ON, "u1,u2,u3", "l1,l2,l3", OFF),
    ]);
    for (read, served) in [("l1", "u1"), ("l2", "u2"), ("l3", "u3")] {
        let _ret = fw.step(&format!("LowerReadComplete({read},len=76,pkt=pkt1)"), &[]);
        assert_eq!(fw.last_completed.as_deref(), Some(served));
        assert_eq!(fw.completions, [(served.to_owned(), STATUS_SUCCESS, 30)]);
    }
    assert_eq!(fw.hidq(), "-");
    assert_eq!(fw.inflight(), "-");
}

/// `smio-fail-timer-ok-backlog`: a successful recovery timer issues one read per queued request
/// (the C driver issued exactly one), and restarts the reporting clock.
#[test]
fn recovery_timer_serves_the_whole_backlog() {
    let init = smio("SelfManagedIoInit", FAIL);
    let mut fw = run(&[
        ("D0Entry(prev=D3)", UNCONF, "-", "-", OFF),
        (&init, UNCONF, "-", "-", ARMED),
        ("READ_REPORT(u1,outlen=30)", UNCONF, "u1", "-", ARMED),
        ("READ_REPORT(u2,outlen=30)", UNCONF, "u1,u2", "-", ARMED),
        ("READ_REPORT(u3,outlen=30)", UNCONF, "u1,u2,u3", "-", ARMED),
    ]);
    let _ret = fw.step("TimerCallback(lower_setfeature=00000000)", &[]);
    assert_eq!(fw.rt.device_status(), ON);
    assert_eq!(fw.inflight(), "l1,l2,l3");
    assert_eq!(fw.qpc_calls, 1, "the timer restarts the reporting clock");
    for (read, served) in [("l1", "u1"), ("l2", "u2"), ("l3", "u3")] {
        let _ret = fw.step(&format!("LowerReadComplete({read},len=76,pkt=pkt1)"), &[]);
        assert_eq!(fw.last_completed.as_deref(), Some(served));
    }
    let _ret = fw.step("READ_REPORT(u4,outlen=30)", &[]);
    assert_eq!(fw.inflight(), "l4");
    let _ret = fw.step("D0Exit(target=D3)", &[]);
    assert_eq!(fw.completions, [("u4".to_owned(), D0_EXIT_DRAIN_STATUS, 0)]);
}

/// `stale-timer-after-restart-ok`: the timer armed before `D0Exit` is stopped; a stale callback after
/// the successful restart neither sends the mode switch nor issues a read.
#[test]
fn stale_timer_after_successful_restart_does_nothing() {
    let init = smio("SelfManagedIoInit", FAIL);
    let restart = smio("SelfManagedIoRestart", "00000000");
    let mut fw = run(&[
        ("D0Entry(prev=D3)", UNCONF, "-", "-", OFF),
        (&init, UNCONF, "-", "-", ARMED),
        ("D0Exit(target=D3)", D3, "-", "-", FIRED),
        ("D0Entry(prev=D3)", UNCONF, "-", "-", FIRED),
        (&restart, ON, "-", "-", FIRED),
        (
            "TimerCallback(lower_setfeature=00000000)",
            ON,
            "-",
            "-",
            FIRED,
        ),
        ("READ_REPORT(u1,outlen=30)", ON, "u1", "l1", FIRED),
    ]);
    assert_eq!(fw.set_state_calls, 2);
    let _ret = fw.step("LowerReadComplete(l1,len=76,pkt=pkt1)", &[]);
    assert_eq!(fw.last_completed.as_deref(), Some("u1"));
}

/// `restart-fail-rearms-timer`: the suspend stops the timer, the failed restart arms it again, and
/// its successful callback serves the read queued meanwhile.
#[test]
fn failed_restart_rearms_the_timer() {
    let init = smio("SelfManagedIoInit", FAIL);
    let restart = smio("SelfManagedIoRestart", FAIL);
    let mut fw = run(&[
        ("D0Entry(prev=D3)", UNCONF, "-", "-", OFF),
        (&init, UNCONF, "-", "-", ARMED),
        ("D0Exit(target=D3)", D3, "-", "-", FIRED),
        ("D0Entry(prev=D3)", UNCONF, "-", "-", FIRED),
        (&restart, UNCONF, "-", "-", ARMED),
        ("READ_REPORT(u1,outlen=30)", UNCONF, "u1", "-", ARMED),
        (
            "TimerCallback(lower_setfeature=00000000)",
            ON,
            "u1",
            "l1",
            FIRED,
        ),
    ]);
    let _ret = fw.step("LowerReadComplete(l1,len=76,pkt=pkt1)", &[]);
    assert_eq!(fw.last_completed.as_deref(), Some("u1"));
    assert_eq!(fw.output("u1"), PKT1_REPORT);
}

/// A read issued before the suspend completes after the resume: it is stale, so it does not consume
/// the upstream read queued after the resume; that one is served by its own read.
#[test]
fn read_in_flight_across_suspend_does_not_touch_the_new_queue() {
    let init = smio("SelfManagedIoInit", "00000000");
    let restart = smio("SelfManagedIoRestart", "00000000");
    let mut fw = run(&[
        ("D0Entry(prev=D3)", UNCONF, "-", "-", OFF),
        (&init, ON, "-", "-", OFF),
        ("READ_REPORT(u1,outlen=30)", ON, "u1", "l1", OFF),
        ("D0Exit(target=D3)", D3, "-", "l1", OFF),
        ("D0Entry(prev=D3)", UNCONF, "-", "l1", OFF),
        (&restart, ON, "-", "l1", OFF),
        ("READ_REPORT(u2,outlen=30)", ON, "u2", "l1,l2", OFF),
    ]);
    let _ret = fw.step("LowerReadComplete(l1,len=76,pkt=pkt1)", &[]);
    assert_eq!(fw.last_completed, None);
    assert_eq!(fw.qpc_calls, 0, "a stale read is not converted");
    assert_eq!(fw.hidq(), "u2");
    let _ret = fw.step("LowerReadComplete(l2,len=76,pkt=pkt1)", &[]);
    assert_eq!(fw.last_completed.as_deref(), Some("u2"));
    assert_eq!(fw.output("u2"), PKT1_REPORT);
}

/// `D0Exit` cancels the queued reads; the recovery timer never re-arms in D3.
#[test]
fn suspend_disarms_a_pending_timer() {
    let init = smio("SelfManagedIoInit", FAIL);
    let fw = run(&[
        ("D0Entry(prev=D3)", UNCONF, "-", "-", OFF),
        (&init, UNCONF, "-", "-", ARMED),
        ("READ_REPORT(u1,outlen=30)", UNCONF, "u1", "-", ARMED),
        ("D0Exit(target=D3)", D3, "-", "-", FIRED),
    ]);
    assert_eq!(fw.completions, [("u1".to_owned(), D0_EXIT_DRAIN_STATUS, 0)]);
    assert_eq!(RECOVERY_TIMER_DUE_TIME, -50_000_000);
}
