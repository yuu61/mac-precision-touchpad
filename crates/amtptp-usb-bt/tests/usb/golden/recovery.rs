//! Legacy Wellspring and T2 input and mode recovery through the KMDF port.

use super::report;
use crate::common::hex;
use crate::fake::FakeHw;
use amtptp_core::domain::ContactTracker;
use amtptp_usb_bt::usb::config::{TrackpadType, config_for_pid};
use amtptp_usb_bt::usb::feature::{SetFeaturePacket, set_feature};
use amtptp_usb_bt::usb::input::{PacketAction, parse_packet};
use amtptp_usb_bt::usb::lifecycle;
use amtptp_usb_bt::usb::state::{DeviceState, InputGates};
use amtptp_usb_bt::usb::status::Completion;

/// Original UMDF fixture inputs and reports, with corrected MT2 Y origin from Step 2.
#[test]
fn legacy_usb_families_report_through_kmdf() {
    for line in include_str!("../fixtures/legacy_usb.txt").lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let [case, pids, input, output] = fields.as_slice() else {
            panic!("invalid fixture")
        };
        for pid in pids.split(',') {
            let model = config_for_pid(u16::from_str_radix(pid, 16).unwrap());
            let mut contacts = ContactTracker::new();
            let mut pkt = hex(input);
            let mut empty = vec![0; model.profile.tp_type.header() as usize];
            if model.profile.tp_type == TrackpadType::Type5 {
                assert_eq!(
                    parse_packet(model, &pkt, InputGates::ON, 0xff, &mut contacts),
                    PacketAction::Discard,
                    "MT2 USB frames without 0x02 report ID must be discarded"
                );
                pkt[0] = 0x02;
                empty[0] = 0x02;
            }
            let bytes = report(parse_packet(
                model,
                &pkt,
                InputGates::ON,
                0xff,
                &mut contacts,
            ));
            assert_eq!(bytes.as_slice(), hex(output), "{case} PID {pid}");
            let up = report(parse_packet(
                model,
                &empty,
                InputGates::ON,
                0x100,
                &mut contacts,
            ));
            assert_eq!((up[1], up[48]), (1, 1), "{case} liftoff");
            assert_eq!(
                &up[2..10],
                &bytes[2..10],
                "{case} liftoff ID and coordinates"
            );
        }
    }
}

#[test]
fn legacy_recovery_obeys_input_mode_and_power() {
    use amtptp_usb_bt::usb::{input::needs_mode_recovery, wellspring::recover_mode};
    let state = DeviceState::new();
    let mut hw = FakeHw::new(0x0265);
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
    assert!(needs_mode_recovery(&state, &[0; 3]));
    assert!(needs_mode_recovery(&state, &[0; 21]));
    let mut valid_mt2 = [0_u8; 21];
    valid_mt2[0] = 0x02;
    assert!(!needs_mode_recovery(&state, &valid_mt2));
    let _entry = hw.take_logs();
    recover_mode(&state, &mut hw);
    let (_, register) = hw.take_logs();
    assert!(register.contains("out:2109020301000400:02000000"));
    assert!(register.ends_with("out:2109020301000400:02010000:00000000"));
    assert_eq!(
        set_feature(
            16,
            || Some(SetFeaturePacket {
                report_id: 4,
                report_buffer: &[4, 0]
            }),
            &state,
            &mut hw
        ),
        Completion::success(0)
    );
    let _mouse = hw.take_logs();
    recover_mode(&state, &mut hw);
    assert_eq!(hw.take_logs().1, "-");
    assert_eq!(lifecycle::d0_exit(&state, &mut hw).status, 0);
    let _exit = hw.take_logs();
    recover_mode(&state, &mut hw);
    assert_eq!(hw.take_logs().1, "-");
}

#[test]
fn t2_supports_runtime_mode_recovery() {
    use amtptp_usb_bt::usb::{input::needs_mode_recovery, wellspring::recover_mode};
    let state = DeviceState::new();
    let mut hw = FakeHw::new(0x027a);
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
    assert!(state.can_recover_mode());
    assert!(needs_mode_recovery(&state, &[0; 8]));
    assert!(!needs_mode_recovery(&state, &[0; 46]));
    let _entry = hw.take_logs();
    recover_mode(&state, &mut hw);
    let (_, wire) = hw.take_logs();
    assert!(wire.contains("out:2109020302000400:02000000"));
    assert!(wire.ends_with("out:2109020302000400:02010000:00000000"));
}

#[test]
fn wellspring8_enters_raw_mode_without_a_transfer() {
    for pid in [0x0290, 0x0291, 0x0292] {
        let state = DeviceState::new();
        let mut hw = FakeHw::new(pid);
        assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
        assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
        assert!(state.wellspring_mode_on());
        assert!(!state.can_recover_mode());
        assert_eq!(hw.take_logs().1, "-");
    }
}
