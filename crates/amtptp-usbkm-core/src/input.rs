//! Decode USB interrupt packets and complete pending HID reads with a five-slot PTP report.

use bcm5974::input::{build_tracked_usb_report, usb_contact_count, wellspring_contact};

pub use ptp_hid::contact_tracker::ContactTracker;
use ptp_hid::input::PTP_REPORT_LEN;

use crate::config::UsbModel;
use crate::output::OutputMemory;
use crate::state::{DeviceState, InputGates};
use crate::status::{Completion, NtStatus, nt_success};

pub use bcm5974::input::TRACKPAD_FINGER_LEN;

/// The outcome of [`process_packet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketAction {
    /// The length gate failed: drop the packet and do not touch the `InputQueue` (the pended `READ_REPORT`
    /// stays queued).
    Discard,
    /// Dequeue the next `READ_REPORT` (`WdfIoQueueRetrieveNextRequest`); if there is none, drop the packet.
    /// Otherwise complete it with [`complete_read_report`].
    Deliver([u8; PTP_REPORT_LEN]),
}

/// Builds a tracked report from a raw packet (`packet.len()` = `NumBytesTransferred`), with the
/// given config row, gates, `ScanTime` and [`ContactTracker`].
#[must_use]
pub fn parse_packet(
    cfg: &UsbModel,
    packet: &[u8],
    gates: InputGates,
    scan_time: u16,
    contacts: &mut ContactTracker,
) -> PacketAction {
    parse_packet_with_clock(cfg, packet, gates, contacts, || scan_time)
}

fn parse_packet_with_clock<F: FnOnce() -> u16>(
    cfg: &UsbModel,
    packet: &[u8],
    gates: InputGates,
    contacts: &mut ContactTracker,
    scan_time: F,
) -> PacketAction {
    let format = cfg.profile.tp_type;
    if usb_contact_count(format, packet.len()).is_none() {
        return PacketAction::Discard;
    }
    let mut report = build_tracked_usb_report(
        format,
        packet,
        gates.report_touch,
        gates.report_button,
        contacts,
        |finger, slot| {
            if format == crate::config::TrackpadType::Type5 {
                let Some(block) = finger.first_chunk() else {
                    return ptp_hid::input::PtpContact::EMPTY;
                };
                bcm5974::input::mt2_contact(block, cfg.profile.x.min, cfg.profile.y.min)
            } else {
                wellspring_contact(
                    &cfg.profile,
                    finger,
                    slot,
                    (cfg.firmware == crate::config::Firmware::T2).then_some(150),
                )
            }
        },
    );
    report.scan_time = scan_time();
    PacketAction::Deliver(report.to_bytes())
}

/// Whether this packet requires a passive-level mode reset on a legacy USB controller.
#[must_use]
pub fn needs_mode_recovery(state: &DeviceState, length: usize) -> bool {
    state.can_recover_mode() && usb_contact_count(state.config().profile.tp_type, length).is_none()
}

/// Parses a packet using the device's selected config, wellspring mode gate, input gates,
/// reporting clock and [`ContactTracker`]. When Wellspring mode is off (`SET_FEATURE` 0x04 mouse
/// mode or after `D0Exit`), `contacts` is reset and the packet is discarded without dequeuing a
/// pending `READ_REPORT`.
pub fn process_packet(
    state: &DeviceState,
    contacts: &mut ContactTracker,
    packet: &[u8],
    counter: i64,
) -> PacketAction {
    if !state.wellspring_mode_on() {
        contacts.reset();
        return PacketAction::Discard;
    }
    parse_packet_with_clock(
        state.config(),
        packet,
        state.input_gates(),
        contacts,
        || state.scan_clock.sample(counter),
    )
}

/// Completes a dequeued `READ_REPORT`.
///
/// `output` is the result of `WdfRequestRetrieveOutputMemory`: the driver passes its `WDFMEMORY` wrapper
/// (so the copy is the framework's `WdfMemoryCopyFromBuffer(mem, 0, report, 50)`), tests pass a byte slice.
///
#[must_use]
pub fn complete_read_report_to<M: OutputMemory + ?Sized>(
    result: &[u8; PTP_REPORT_LEN],
    output: Result<&mut M, NtStatus>,
) -> Completion {
    let memory = match output {
        Ok(memory) => memory,
        Err(status) => return Completion::status(status),
    };
    let status = memory.copy_from_buffer(result);
    if nt_success(status) {
        Completion {
            status,
            information: PTP_REPORT_LEN,
        }
    } else {
        Completion::status(status)
    }
}

/// [`complete_read_report_to`] on a byte slice (the [`OutputMemory`] model of `WdfMemoryCopyFromBuffer`:
/// `STATUS_BUFFER_TOO_SMALL` below 50 bytes).
#[must_use]
pub fn complete_read_report(
    result: &[u8; PTP_REPORT_LEN],
    output: Result<&mut [u8], NtStatus>,
) -> Completion {
    complete_read_report_to(result, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FALLBACK_CONFIG, config_for_pid};
    use crate::status::{STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_BUFFER_SIZE};

    /// An output memory whose copy fails with a framework status other than `BUFFER_TOO_SMALL`.
    struct FailingMemory(NtStatus);

    impl OutputMemory for FailingMemory {
        fn copy_from_buffer(&mut self, _src: &[u8]) -> NtStatus {
            self.0
        }
    }

    #[test]
    fn copy_status_is_the_frameworks() {
        let report = [5; 50];
        let mut memory = FailingMemory(STATUS_INVALID_BUFFER_SIZE);
        assert_eq!(
            complete_read_report_to(&report, Ok(&mut memory)),
            Completion::status(STATUS_INVALID_BUFFER_SIZE)
        );
    }

    #[test]
    fn golden_a_header_only() {
        let mut contacts = ContactTracker::new();
        let mut packet = [0_u8; 46];
        packet[31] = 1;
        let PacketAction::Deliver(r) =
            parse_packet(&FALLBACK_CONFIG, &packet, InputGates::ON, 0, &mut contacts)
        else {
            panic!("expected a report");
        };
        let mut expect = [0_u8; 50];
        expect[0] = 5;
        expect[49] = 1;
        assert_eq!(r, expect);
    }

    #[test]
    fn malformed_lengths_and_six_contact_frames() {
        let cfg = config_for_pid(0x027a);
        let mut contacts = ContactTracker::new();
        assert_eq!(
            parse_packet(cfg, &[0; 45], InputGates::ON, 0, &mut contacts),
            PacketAction::Discard
        );
        assert_eq!(
            parse_packet(cfg, &[0; 47], InputGates::ON, 0, &mut contacts),
            PacketAction::Discard
        );
        assert!(matches!(
            parse_packet(cfg, &[0; 226], InputGates::ON, 0, &mut contacts),
            PacketAction::Deliver(_)
        ));
        let gates = InputGates {
            report_touch: false,
            report_button: true,
        };
        assert!(matches!(
            parse_packet(cfg, &[0; 226], gates, 0, &mut contacts),
            PacketAction::Deliver(_)
        ));
    }

    #[test]
    fn completion_precedence() {
        let report = [5; 50];
        assert_eq!(
            complete_read_report(&report, Err(STATUS_BUFFER_TOO_SMALL)),
            Completion::status(STATUS_BUFFER_TOO_SMALL)
        );
        let mut short = [0xa5_u8; 49];
        assert_eq!(
            complete_read_report(&report, Ok(&mut short)),
            Completion::status(STATUS_BUFFER_TOO_SMALL)
        );
        assert!(short.iter().all(|&b| b == 0xa5));
        let mut long = [0xa5_u8; 51];
        assert_eq!(
            complete_read_report(&report, Ok(&mut long)),
            Completion::success(50)
        );
        assert_eq!(long[50], 0xa5);
    }
}
