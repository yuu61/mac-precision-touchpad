//! Replays `setup.txt`: DriverEntry, EvtDriverDeviceAdd (including failure paths), driver cleanup and
//! EvtIoStop.
//!
//! These records are WDF configuration; the driver crate (stage 2) reproduces the call sequence. Here the
//! core's constants are asserted against the arguments the C code passed, the driver-decided status
//! (no local I/O target) is checked, and every framework-decided status is counted under a named reason.

mod common;

use amtptp_spikm_core::runtime::{DeviceRuntime, DeviceStatus, RECOVERY_TIMER_PERIOD};
use amtptp_spikm_core::wire::{
    DEVICE_INTERFACE_GUID, LOWER_READ_BUFFER_LEN, NO_IO_TARGET_STATUS, POOL_TAG,
};
use common::{Tally, load, wdf_failures};

const FRAMEWORK: &str = "framework-decided status (wdf= injected failure): glue/stage 2";
const DRIVER_STATUS: &str = "driver-decided status: no local I/O target -> c0000184";
const OK: &str = "success path: constants asserted";
const NO_LOGIC: &str = "no driver logic (ops=-): cleanup / empty EvtIoStop";

fn guid_text() -> String {
    let g = DEVICE_INTERFACE_GUID;
    let d = g.data4;
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        g.data1, g.data2, g.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

#[test]
fn setup_records() {
    let recs = load("setup.txt");
    assert_eq!(recs.len(), 12);
    let mut tally = Tally::default();
    for r in &recs {
        let ctx = format!("setup.txt:{} {}", r.line, r.case());
        let ops = r.get("ops");
        if ops == "-" {
            tally.add(NO_LOGIC);
            continue;
        }
        if ops.contains("WdfLookasideListCreate(") {
            assert!(
                ops.contains(&format!(
                    "size={LOWER_READ_BUFFER_LEN},pool=NonPagedPoolNx,memattrs=null,tag=0x{POOL_TAG:08x}"
                )),
                "{ctx}: lookaside"
            );
        }
        if ops.contains("WdfTimerCreate(") {
            assert!(
                ops.contains(&format!("period={RECOVERY_TIMER_PERIOD},")),
                "{ctx}: one-shot"
            );
            assert!(
                ops.contains("parent=dev,exec=2,"),
                "{ctx}: passive timer on the device"
            );
        }
        if ops.contains("WdfDeviceCreateDeviceInterface(") {
            assert!(
                ops.contains(&format!("guid={}", guid_text())),
                "{ctx}: interface GUID"
            );
        }
        if let Some(ds) = r.opt("ds_out") {
            // The context after CreateDevice: DeviceStatus = D3.
            assert_eq!(DeviceRuntime::new().device_status(), DeviceStatus::D3);
            assert_eq!(ds, "0", "{ctx}");
        }
        let status = r.status("status");
        if !wdf_failures(r).is_empty() {
            let (_, injected) = &wdf_failures(r)[0];
            assert_eq!(format!("{:08x}", status as u32), *injected, "{ctx}");
            tally.add(FRAMEWORK);
        } else if status == NO_IO_TARGET_STATUS {
            assert!(ops.ends_with("WdfDeviceGetIoTarget(dev)=null"), "{ctx}");
            tally.add(DRIVER_STATUS);
        } else {
            assert_eq!(status, 0, "{ctx}");
            tally.add(OK);
        }
    }
    assert_eq!(tally.get(NO_LOGIC), 2);
    assert_eq!(tally.get(OK), 2);
    assert_eq!(tally.get(DRIVER_STATUS), 1);
    assert_eq!(tally.get(FRAMEWORK), 7);
    assert_eq!(tally.total(), 12);
}
