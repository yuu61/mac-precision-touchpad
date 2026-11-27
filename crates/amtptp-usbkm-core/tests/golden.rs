//! The golden vectors of the porting spec (usbkm.md §5, §6.3, §7.4, Appendices A-C), typed in from the
//! spec independently of the fixture files, updated for Step 2 descriptor cleanup, T2 13-inch descriptor,
//! `ContactTracker`, and `SET_FEATURE 0x04` mode-0 gating.

#![cfg(test)]

mod common;

use amtptp_usbkm_core::config::{FALLBACK_CONFIG, config_for_pid};
use amtptp_usbkm_core::feature::{GetFeaturePacket, SetFeaturePacket, get_feature, set_feature};
use amtptp_usbkm_core::hid::{
    HID_DESCRIPTOR, REPORT_DESCRIPTOR, REPORT_DESCRIPTOR_LEN, REPORT_DESCRIPTOR_T2_13,
    get_device_attributes, get_report_descriptor, get_report_descriptor_for,
};
use amtptp_usbkm_core::input::{
    PacketAction, ReadReportResult, complete_read_report, parse_packet, process_packet,
};
use amtptp_usbkm_core::lifecycle;
use amtptp_usbkm_core::state::{DeviceState, InputGates, UsbDeviceIds};
use amtptp_usbkm_core::status::{Completion, STATUS_DATA_ERROR, STATUS_INVALID_BUFFER_SIZE};
use amtptp_usbkm_core::wellspring::{
    ControlTransfer as _, POOL_TAG_PTP_CONTROL, set_wellspring_mode, wellspring_read_setup,
    wellspring_write_setup,
};
use common::fake::FakeHw;
use common::{hex, to_legacy_c_u32};
use ptp_hid::contact_tracker::ContactTracker;

/// Appendix A (legacy C, 493 bytes).
const APPENDIX_A: &str = "\
050D0905A10185050922A1022501094709429502750181027501950681039501752027FFFFFFFF09518102050126204E\
7510550E651109304614059501810246520326E02E09318102450055006500C0050D0922A10225010947094295027501\
81027501950681039501752027FFFFFFFF09518102050126204E7510550E651109304614059501810246520326E02E09\
318102450055006500C0050D0922A1022501094709429502750181027501950681039501752027FFFFFFFF0951810205\
0126204E7510550E651109304614059501810246520326E02E09318102C0050D0922A102250109470942950275018102\
7501950681039501752027FFFFFFFF09518102050126204E7510550E651109304614059501810246520326E02E093181\
02450055006500C0050D0922A1022501094709429502750181027501950681039501752027FFFFFFFF09518102050126\
204E7510550E651109304614059501810246520326E02E09318102C0050D550C66011047FFFF000027FFFF0000095681\
020954257F750881020509090125017501810295078103050D850709550959150026FF0075089502B1020600FF850809\
C5150026FF007508960001B102C0050D090EA10185040922A10209521500251075089501B102C0A10085060957095875\
0195022501B1029506B103C0C0";

fn report(action: PacketAction) -> [u8; 50] {
    match action {
        PacketAction::Deliver(ReadReportResult::Report(r)) => r,
        other @ (PacketAction::Discard | PacketAction::Deliver(ReadReportResult::DataError)) => {
            panic!("expected a report, got {other:?}");
        }
    }
}

/// Spaced hex as printed in the spec -> bytes.
fn spaced(s: &str) -> Vec<u8> {
    hex(&s.split_whitespace().collect::<String>())
}

/// §7.4 B input: two finger records (finger 0 active, finger 1 hover `major = 20 < 200`), N = 106.
fn two_finger_packet() -> Vec<u8> {
    spaced(
        "00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 0c fe b8 0b 00 00 00 00 00 00 00 00 00 00
         78 00 32 00 00 00 00 00 00 00 00 00 00 00 00 00 20 d1 f8 2a 00 00 00 00 00 00 00 00 00 00 0a 00
         00 00 00 00 00 00 00 00 00 00",
    )
}

#[test]
fn appendix_a_report_descriptor() {
    assert_eq!(REPORT_DESCRIPTOR_LEN, 511);
    assert_eq!(
        hex(&APPENDIX_A.to_ascii_lowercase()),
        to_legacy_c_u32(&REPORT_DESCRIPTOR, false)
    );
    let mut out = vec![0_u8; REPORT_DESCRIPTOR_LEN];
    assert_eq!(
        get_report_descriptor(Ok(&mut out)),
        Completion::success(REPORT_DESCRIPTOR_LEN)
    );
    assert_eq!(out, REPORT_DESCRIPTOR.to_vec());

    // 13-inch T2 (0x027a / 0x027b) selects the 12992x7855 descriptor.
    assert_eq!(
        get_report_descriptor_for(config_for_pid(0x027a), Ok(&mut out)),
        Completion::success(REPORT_DESCRIPTOR_LEN)
    );
    assert_eq!(out, REPORT_DESCRIPTOR_T2_13.to_vec());
    assert_eq!(
        hex(&APPENDIX_A.to_ascii_lowercase()),
        to_legacy_c_u32(&REPORT_DESCRIPTOR_T2_13, true)
    );
}

#[test]
fn appendix_b_hid_descriptor() {
    assert_eq!(
        HID_DESCRIPTOR,
        [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xFF, 0x01]
    );
}

#[test]
fn golden_a_header_only_button() {
    let mut packet = vec![0_u8; 46];
    packet[31] = 0x01;
    let mut expect = [0_u8; 50];
    expect[0] = 0x05;
    expect[49] = 0x01;
    for pid in [0xffff, 0x027a, 0x027c] {
        let r = report(parse_packet(
            config_for_pid(pid),
            &packet,
            InputGates::ON,
            0,
            &mut ContactTracker::new(),
        ));
        assert_eq!(r, expect);
    }
}

#[test]
fn golden_b_two_fingers_fallback() {
    let packet = two_finger_packet();
    assert_eq!(packet.len(), 106);
    // Finger 0: flags 03, ID 0, X 9500, Y 7000. Finger 1 is hover (major 20 < 200), ignored on frame 0.
    let want = spaced(
        "05 03 00 00 00 00 1c 25 58 1b 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 01 00",
    );
    for pid in [0xffff, 0x027c, 0x027d, 0x0273, 0x1234] {
        let r = report(parse_packet(
            config_for_pid(pid),
            &packet,
            InputGates::ON,
            0,
            &mut ContactTracker::new(),
        ));
        assert_eq!(r.to_vec(), want, "pid {pid:04x}");
    }
}

#[test]
fn golden_b_prime_13_inch() {
    let packet = two_finger_packet();
    // Finger 0: X = -500 + 6243 = 5743, Y = 7685 - 3000 = 4685; hover finger 1 ignored on frame 0.
    let want = spaced(
        "05 03 00 00 00 00 6f 16 4d 12 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 01 00",
    );
    for pid in [0x027a, 0x027b] {
        let r = report(parse_packet(
            config_for_pid(pid),
            &packet,
            InputGates::ON,
            0,
            &mut ContactTracker::new(),
        ));
        assert_eq!(r.to_vec(), want, "pid {pid:04x}");
    }
}

#[test]
fn golden_c_six_fingers_data_error() {
    let packet = vec![0_u8; 226];
    let action = parse_packet(
        &FALLBACK_CONFIG,
        &packet,
        InputGates::ON,
        0,
        &mut ContactTracker::new(),
    );
    assert_eq!(action, PacketAction::Deliver(ReadReportResult::DataError));
    let mut out = [0xa5_u8; 50];
    let PacketAction::Deliver(result) = action else {
        panic!("expected a delivered data error");
    };
    assert_eq!(
        complete_read_report(&result, Ok(&mut out)),
        Completion::status(STATUS_DATA_ERROR)
    );
    assert_eq!(out, [0xa5; 50]);
}

#[test]
fn golden_d_bad_length_dropped() {
    assert_eq!(
        parse_packet(
            &FALLBACK_CONFIG,
            &[0_u8; 47],
            InputGates::ON,
            0,
            &mut ContactTracker::new(),
        ),
        PacketAction::Discard
    );
}

/// `SET_FEATURE` 0x06 `[06, bits]` for each combination, then the two-finger frame with the button byte
/// set: the surface switch decides the contacts, the button switch the button; `PrepareHardware`
/// restores both.
#[test]
fn selective_reporting_shapes_the_input_report() {
    let state = DeviceState::new();
    state.on_device_create();
    let mut hw = FakeHw::new(0x027c);
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
    let mut packet = two_finger_packet();
    packet[31] = 0x01;
    let mut tracker = ContactTracker::new();
    for (bits, contacts, button) in [(0_u8, 0_u8, 0_u8), (1, 1, 0), (2, 0, 1), (3, 1, 1)] {
        let switches = [0x06, bits];
        let c = set_feature(
            16,
            || {
                Some(SetFeaturePacket {
                    report_id: 0x06,
                    report_buffer: &switches,
                })
            },
            &state,
            &mut hw,
        );
        assert_eq!(c, Completion::status(0));
        if contacts == 0 {
            tracker.reset();
        }
        let r = report(process_packet(&state, &mut tracker, &packet, 0));
        assert_eq!((r[48], r[49]), (contacts, button), "switch byte {bits}");
        if contacts == 0 {
            assert!(r[1..46].iter().all(|&b| b == 0), "no contact data");
        }
    }
    let _logs = hw.take_logs();
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    assert_eq!(state.input_gates(), InputGates::ON);
}

#[test]
fn d0_entry_switches_the_mode_after_selective_reporting_off() {
    let state = DeviceState::new();
    state.on_device_create();
    let mut hw = FakeHw::new(0x027c);
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    let set_switches = |hw: &mut FakeHw, bits: u8| {
        let switches = [0x06, bits];
        set_feature(
            16,
            || {
                Some(SetFeaturePacket {
                    report_id: 0x06,
                    report_buffer: &switches,
                })
            },
            &state,
            hw,
        )
    };
    assert_eq!(set_switches(&mut hw, 0x00), Completion::status(0));
    assert_eq!(
        state.input_gates(),
        InputGates {
            report_touch: false,
            report_button: false,
        }
    );
    assert_eq!(lifecycle::d0_exit(&state, &mut hw).status, 0);
    assert!(!state.wellspring_mode_on());
    let _before = hw.take_logs();
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
    assert!(state.wellspring_mode_on());
    let (_, wire) = hw.take_logs();
    assert!(
        wire.contains("out:2109020302000400:02010000"),
        "mode switch sent: {wire}"
    );

    assert_eq!(set_switches(&mut hw, 0x03), Completion::status(0));
    let r = report(process_packet(
        &state,
        &mut ContactTracker::new(),
        &two_finger_packet(),
        0,
    ));
    assert_eq!(r[48], 1, "active contact reported again");
}

#[test]
fn input_scan_time_restarts_on_d0_entry() {
    let state = DeviceState::new();
    state.on_device_create();
    let mut hw = FakeHw::new(0x027a);
    hw.qpc_freq = 24_000_000;
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
    let pack = [0_u8; 46];
    let mut contacts = ContactTracker::new();
    assert_eq!(
        process_packet(&state, &mut contacts, &[], 0),
        PacketAction::Discard
    );
    let mut scan_time = |counter| {
        let PacketAction::Deliver(ReadReportResult::Report(report)) =
            process_packet(&state, &mut contacts, &pack, counter)
        else {
            panic!("valid input packet did not produce a report");
        };
        u16::from_le_bytes([report[46], report[47]])
    };

    assert_eq!(scan_time(200_000), 0);
    assert_eq!(scan_time(204_800), 2);
    assert_eq!(lifecycle::d0_exit(&state, &mut hw).status, 0);
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
    assert_eq!(scan_time(900_000), 0);
    assert_eq!(scan_time(902_400), 1);
}

#[test]
fn multi_finger_tracking_liftoff_and_mode0_gate() {
    let state = DeviceState::new();
    state.on_device_create();
    let mut hw = FakeHw::new(0x027a);
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);

    // Build a 2-finger Type4 packet where both fingers are active (major = 300 >= 200).
    let mut f0 = vec![0_u8; 46 + 2 * 30];
    // Finger 0 at (-500, 3000), major 300, minor 200.
    f0[50..52].copy_from_slice(&(-500_i16).to_le_bytes());
    f0[52..54].copy_from_slice(&3000_i16.to_le_bytes());
    f0[64..66].copy_from_slice(&150_i16.to_le_bytes());
    f0[66..68].copy_from_slice(&100_i16.to_le_bytes());
    // Finger 1 at (1500, 3000), major 300, minor 0 (still confident: < 2500).
    f0[80..82].copy_from_slice(&1500_i16.to_le_bytes());
    f0[82..84].copy_from_slice(&3000_i16.to_le_bytes());
    f0[94..96].copy_from_slice(&150_i16.to_le_bytes());

    let mut contacts = ContactTracker::new();
    let r0 = report(process_packet(&state, &mut contacts, &f0, 0));
    assert_eq!(r0[48], 2);
    assert_eq!((r0[1], u32::from_le_bytes([r0[2], r0[3], r0[4], r0[5]])), (0x03, 0));
    assert_eq!((r0[10], u32::from_le_bytes([r0[11], r0[12], r0[13], r0[14]])), (0x03, 1));

    // Finger 0 lifts: 1-finger packet containing only finger 1 near (1510, 3000).
    let mut f1 = vec![0_u8; 46 + 30];
    f1[50..52].copy_from_slice(&1510_i16.to_le_bytes());
    f1[52..54].copy_from_slice(&3000_i16.to_le_bytes());
    f1[64..66].copy_from_slice(&150_i16.to_le_bytes());
    let r1 = report(process_packet(&state, &mut contacts, &f1, 100));
    assert_eq!(r1[48], 2, "slot 0 liftoff + slot 1 active");
    assert_eq!((r1[1], u32::from_le_bytes([r1[2], r1[3], r1[4], r1[5]])), (0x01, 0));
    assert_eq!((r1[10], u32::from_le_bytes([r1[11], r1[12], r1[13], r1[14]])), (0x03, 1));

    // Next frame: liftoff finished, only ID 1 remains.
    let r2 = report(process_packet(&state, &mut contacts, &f1, 200));
    assert_eq!(r2[48], 1);
    assert_eq!((r2[1], u32::from_le_bytes([r2[2], r2[3], r2[4], r2[5]])), (0x03, 1));

    // Mode 0 switches Wellspring mode off; packets are discarded and tracker is reset.
    let mode0 = [0x04_u8, 0x00];
    assert_eq!(
        set_feature(
            16,
            || Some(SetFeaturePacket { report_id: 0x04, report_buffer: &mode0 }),
            &state,
            &mut hw,
        ),
        Completion::status(0)
    );
    assert!(!state.wellspring_mode_on());
    assert_eq!(process_packet(&state, &mut contacts, &f1, 300), PacketAction::Discard);
    assert!(!contacts.has_active_contacts());
}

#[test]
fn wellspring_wire_bytes_6_3() {
    assert_eq!(
        wellspring_read_setup(&FALLBACK_CONFIG).to_bytes(),
        [0xA1, 0x01, 0x02, 0x03, 0x02, 0x00, 0x04, 0x00]
    );
    assert_eq!(
        wellspring_write_setup(&FALLBACK_CONFIG).to_bytes(),
        [0x21, 0x09, 0x02, 0x03, 0x02, 0x00, 0x04, 0x00]
    );
    assert_eq!(POOL_TAG_PTP_CONTROL, 0x5054_5043);

    let state = DeviceState::new();
    state.on_device_create();
    let mut hw = FakeHw::new(0x027a);
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    hw.take_logs();
    assert_eq!(hw.serialized(|hw| set_wellspring_mode(&state, hw, true)), 0);
    assert_eq!(
        hw.take_logs().1,
        "in:a101020302000400:0200:00000000;out:2109020302000400:02010000:00000000"
    );
    assert!(state.wellspring_mode_on());
    assert_eq!(hw.serialized(|hw| set_wellspring_mode(&state, hw, false)), 0);
    assert_eq!(
        hw.take_logs().1,
        "in:a101020302000400:0200:00000000;out:2109020302000400:02000000:00000000"
    );
    assert!(!state.wellspring_mode_on());
}

#[test]
fn feature_reports_5_5_and_5_6() {
    let mut caps = [0xaa_u8; 3];
    let c = get_feature(16, || {
        Some(GetFeaturePacket {
            report_id: 0x07,
            report_buffer: &mut caps,
        })
    });
    assert_eq!(c, Completion::status(0));
    assert_eq!(caps, [0x07, 0x05, 0x00]);

    let mut hqa = [0xaa_u8; 257];
    let c = get_feature(16, || {
        Some(GetFeaturePacket {
            report_id: 0x08,
            report_buffer: &mut hqa,
        })
    });
    assert_eq!(c, Completion::status(0));
    assert_eq!(hqa, ptp_hid::feature::HQA_REPORT);
    let mut short = [0_u8; 256];
    let c = get_feature(16, || {
        Some(GetFeaturePacket {
            report_id: 0x08,
            report_buffer: &mut short,
        })
    });
    assert_eq!(c, Completion::status(STATUS_INVALID_BUFFER_SIZE));

    // SET 0x04: short report -> INVALID_BUFFER_SIZE; mode 0 while on -> switches Wellspring mode off;
    // mode 3 while off -> switches Wellspring mode on; mode 3/7 while on -> no transfer.
    let state = DeviceState::new();
    state.on_device_create();
    let mut hw = FakeHw::new(0x027a);
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
    hw.take_logs();

    assert_eq!(
        set_feature(
            16,
            || Some(SetFeaturePacket { report_id: 0x04, report_buffer: &[0x04] }),
            &state,
            &mut hw,
        ),
        Completion::status(STATUS_INVALID_BUFFER_SIZE)
    );
    assert_eq!(hw.take_logs().1, "-");

    let mode0 = [0x04_u8, 0x00];
    assert_eq!(
        set_feature(
            16,
            || Some(SetFeaturePacket { report_id: 0x04, report_buffer: &mode0 }),
            &state,
            &mut hw,
        ),
        Completion::status(0)
    );
    assert!(!state.wellspring_mode_on());
    assert_eq!(
        hw.take_logs().1,
        "in:a101020302000400:0200:00000000;out:2109020302000400:02000000:00000000"
    );

    let mode3 = [0x04_u8, 0x03];
    assert_eq!(
        set_feature(
            16,
            || Some(SetFeaturePacket { report_id: 0x04, report_buffer: &mode3 }),
            &state,
            &mut hw,
        ),
        Completion::status(0)
    );
    assert!(state.wellspring_mode_on());
    assert_eq!(
        hw.take_logs().1,
        "in:a101020302000400:0200:00000000;out:2109020302000400:02010000:00000000"
    );

    for mode in [3_u8, 7] {
        let report = [0x04, mode];
        let c = set_feature(
            16,
            || {
                Some(SetFeaturePacket {
                    report_id: 0x04,
                    report_buffer: &report,
                })
            },
            &state,
            &mut hw,
        );
        assert_eq!(c, Completion::status(0), "mode {mode}");
        assert_eq!(hw.take_logs().1, "-");
    }

    for bits in [0_u8, 1, 2, 3] {
        let report = [0x06, bits];
        let c = set_feature(
            16,
            || {
                Some(SetFeaturePacket {
                    report_id: 0x06,
                    report_buffer: &report,
                })
            },
            &state,
            &mut hw,
        );
        assert_eq!(c, Completion::status(0));
        assert_eq!(
            state.input_gates(),
            InputGates {
                report_touch: bits & 1 != 0,
                report_button: bits & 2 != 0,
            },
            "switch byte {bits}"
        );
    }
    let c = set_feature(
        16,
        || {
            Some(SetFeaturePacket {
                report_id: 0x06,
                report_buffer: &[0x06],
            })
        },
        &state,
        &mut hw,
    );
    assert_eq!(c, Completion::status(0));
    assert_eq!(state.input_gates(), InputGates::ON);
}

#[test]
fn attributes_5_3() {
    let ids = UsbDeviceIds {
        vendor_id: 0x05ac,
        product_id: 0x0273,
        bcd_device: 0x0905,
    };
    let mut out = [0x11_u8; 32];
    assert_eq!(
        get_device_attributes(Ok(&mut out), ids),
        Completion::success(32)
    );
    assert_eq!(
        out[..10],
        [0x20, 0, 0, 0, 0xac, 0x05, 0x73, 0x02, 0x01, 0x00]
    );
    assert_eq!(out[10..], [0; 22]);
}
