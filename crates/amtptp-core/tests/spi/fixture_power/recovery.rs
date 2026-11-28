//! Stop/resume and retry scenarios that supersede legacy power fixtures.

use super::{Framework, PKT1_REPORT, QPC_FREQUENCY};
use amtptp_core::spi::runtime::{
    D0_EXIT_DRAIN_STATUS, DeviceRuntime, DeviceStatus, MAX_READ_RETRIES, MAX_RECOVERY_RETRIES,
    READ_RETRY_INITIAL_DUE_TIME, RECOVERY_TIMER_DUE_TIME, ReadTracker, ReplenishAction,
};
use amtptp_core::spi::status::STATUS_SUCCESS;

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

/// `smio-fail-timer-fail`: failed recovery timer callbacks re-arm up to `MAX_RECOVERY_RETRIES - 1`
/// times and then escalate to `WdfDeviceSetFailed(AttemptRestart)` instead of stranding the device.
#[test]
fn failed_recovery_timer_retries_then_escalates() {
    let init = smio("SelfManagedIoInit", FAIL);
    let cb_fail = format!("TimerCallback(lower_setfeature={FAIL})");
    let mut fw = run(&[
        ("D0Entry(prev=D3)", UNCONF, "-", "-", OFF),
        (&init, UNCONF, "-", "-", ARMED),
        ("READ_REPORT(u1,outlen=30)", UNCONF, "u1", "-", ARMED),
    ]);
    for _ in 1..MAX_RECOVERY_RETRIES {
        let _ret = fw.step(&cb_fail, &[]);
        assert!(!fw.device_failed);
        assert_eq!(fw.timer(), ARMED);
    }
    let _ret = fw.step(&cb_fail, &[]);
    assert!(fw.device_failed);
    assert_eq!(fw.timer(), FIRED);
}

#[test]
fn lower_read_failure_replenishes_with_backoff_and_escalates() {
    let rt = DeviceRuntime::new();
    let mut tracker = ReadTracker::<u32>::new();
    rt.on_d0_entry(QPC_FREQUENCY);
    let _out =
        rt.on_self_managed_io_init_or_restart(&mut tracker, STATUS_SUCCESS, || QPC_FREQUENCY);

    for attempt in 0..MAX_READ_RETRIES {
        let expected_due = READ_RETRY_INITIAL_DUE_TIME * (1_i64 << attempt.min(5));
        assert_eq!(
            rt.note_lower_read_failure(),
            ReplenishAction::Retry(expected_due)
        );
    }
    assert_eq!(rt.note_lower_read_failure(), ReplenishAction::FailDevice);
    rt.reset_read_failures();
    assert_eq!(
        rt.note_lower_read_failure(),
        ReplenishAction::Retry(READ_RETRY_INITIAL_DUE_TIME)
    );
}
