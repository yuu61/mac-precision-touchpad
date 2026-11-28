//! Power and read behavior while the vendor collection opens and closes.

use super::{bluetooth, read_report, start};
use crate::common::pipeline::work_item_callback;
use crate::common::pnp::{
    d0_entry, d0_exit, recovery_timer_callback, self_managed_io_init, self_managed_io_restart,
    source_closed, source_opened,
};
use amtptp_usb_bt::bluetooth::STATUS_SUCCESS;

/// Until the data source opens, the start sends nothing and parks the upper reads; when it opens, the
/// mode switch goes out and every parked read gets its lower read.
#[test]
fn the_source_opening_configures_the_device_and_serves_parked_reads() {
    let (state, mut m) = bluetooth();
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_init(&mut m, &state), 0);
    assert_eq!((state.vendor_id(), state.product_id()), (0x004C, 0x0265));
    assert!(!state.source_open() && !state.device_configured());
    assert!(m.ev.is_empty(), "{:?}", m.ev);
    let _first = read_report(&state, &mut m);
    let _second = read_report(&state, &mut m);
    assert_eq!((m.read_queue.len(), m.lower_reads_pending), (2, 0));

    source_opened(&mut m, &state);
    assert!(state.source_open() && state.device_configured());
    assert_eq!(m.sends.len(), 1);
    assert_eq!(m.sends[0].payload, [0xF1, 0x02, 0x01]);
    assert_eq!(m.lower_reads_pending, 2);
}

/// The source going away (query-remove) unconfigures the device and cancels the reads in flight; the
/// upper reads stay parked, and the source coming back configures the device again.
#[test]
fn the_source_closing_cancels_reads_and_reopening_reconfigures() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    let _first = read_report(&state, &mut m);
    assert_eq!(m.in_flight(), 1);

    source_closed(&mut m, &state);
    assert!(!state.source_open() && !state.device_configured());
    assert_eq!(m.cancelled_reads, 1);
    assert_eq!(m.in_flight(), 0);
    assert_eq!(m.read_queue.len(), 1, "the upper read stays parked");
    // Nothing is sent while the source is closed.
    let pending = m.lower_reads_pending;
    let _second = read_report(&state, &mut m);
    work_item_callback(&mut m, &state);
    recovery_timer_callback(&mut m, &state);
    assert_eq!(m.lower_reads_pending, pending);
    assert_eq!(m.sends.len(), 1);

    source_opened(&mut m, &state);
    assert!(state.device_configured());
    assert_eq!(m.sends.len(), 2);
    assert_eq!(m.in_flight(), 2, "both parked reads get a lower read");
}

/// A resume before the source opened does nothing (C: HF-B11 marked the device configured and failed
/// the restart); a resume with the source open sends the mode switch.
#[test]
fn restart_waits_for_the_source() {
    let (state, mut m) = bluetooth();
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_init(&mut m, &state), 0);
    assert_eq!(d0_exit(&mut m, &state), 0);
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_restart(&mut m, &state), STATUS_SUCCESS);
    assert!(!state.device_configured());
    assert!(m.sends.is_empty());

    source_opened(&mut m, &state);
    assert!(state.device_configured());
    assert_eq!(d0_exit(&mut m, &state), 0);
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_restart(&mut m, &state), STATUS_SUCCESS);
    assert!(state.device_configured());
    assert_eq!(m.sends.len(), 2);
}

/// The source opening while the device is out of D0 only records it; the restart configures.
#[test]
fn the_source_opening_out_of_d0_waits_for_the_restart() {
    let (state, mut m) = bluetooth();
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_init(&mut m, &state), 0);
    assert_eq!(d0_exit(&mut m, &state), 0);
    source_opened(&mut m, &state);
    assert!(state.source_open() && !state.device_configured());
    assert!(m.sends.is_empty());
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_restart(&mut m, &state), STATUS_SUCCESS);
    assert!(state.device_configured());
    assert_eq!(m.sends.len(), 1);
}
