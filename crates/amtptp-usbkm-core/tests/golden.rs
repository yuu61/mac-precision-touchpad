//! The golden vectors of the porting spec (usbkm.md §5, §6.3, §7.4, Appendices A-C), typed in from the
//! spec independently of the fixture files.

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
use amtptp_usbkm_core::status::{
    Completion, STATUS_DATA_ERROR, STATUS_INVALID_BUFFER_SIZE, STATUS_NOT_SUPPORTED,
};
use amtptp_usbkm_core::wellspring::{
    ControlTransfer as _, POOL_TAG_PTP_CONTROL, set_wellspring_mode, wellspring_read_setup,
    wellspring_write_setup,
};
use common::fake::FakeHw;
use common::{hex, to_legacy_c_u32};

/// Appendix A, 493 bytes.
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

/// Spaced hex as printed in the spec → bytes.
fn spaced(s: &str) -> Vec<u8> {
    hex(&s.split_whitespace().collect::<String>())
}

/// §7.4 B input: two fingers, N = 106.
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
        ));
        assert_eq!(r, expect);
    }
}

#[test]
fn golden_b_two_fingers_fallback() {
    let packet = two_finger_packet();
    assert_eq!(packet.len(), 106);
    // Contact 0: flags 03, ID 0, X 9500, Y 7000. Contact 1: flags 00, ID 1, X 0, Y 0. Count 2, button 0.
    let want = spaced(
        "05 03 00 00 00 00 1c 25 58 1b 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02 00",
    );
    for pid in [0xffff, 0x027c, 0x027d, 0x0273, 0x1234] {
        let r = report(parse_packet(
            config_for_pid(pid),
            &packet,
            InputGates::ON,
            0,
        ));
        assert_eq!(r.to_vec(), want, "pid {pid:04x}");
    }
}

#[test]
fn golden_b_prime_13_inch() {
    let packet = two_finger_packet();
    // Contact 0: X = -500 + 6243 = 5743, Y = 7685 - 3000 = 4685.
    let want = spaced(
        "05 03 00 00 00 00 6f 16 4d 12 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02 00",
    );
    for pid in [0x027a, 0x027b] {
        let r = report(parse_packet(
            config_for_pid(pid),
            &packet,
            InputGates::ON,
            0,
        ));
        assert_eq!(r.to_vec(), want, "pid {pid:04x}");
    }
}

#[test]
fn golden_c_six_fingers_data_error() {
    let packet = vec![0_u8; 226];
    let action = parse_packet(&FALLBACK_CONFIG, &packet, InputGates::ON, 0);
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
        parse_packet(&FALLBACK_CONFIG, &[0_u8; 47], InputGates::ON, 0),
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
    let mut packet = two_finger_packet();
    packet[31] = 0x01;
    for (bits, contacts, button) in [(0_u8, 0_u8, 0_u8), (1, 2, 0), (2, 0, 1), (3, 2, 1)] {
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
        let r = report(process_packet(&state, &packet, 0));
        assert_eq!((r[48], r[49]), (contacts, button), "switch byte {bits}");
        if contacts == 0 {
            assert!(r[1..46].iter().all(|&b| b == 0), "no contact data");
        }
    }
    let _logs = hw.take_logs();
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    assert_eq!(state.input_gates(), InputGates::ON);
}

/// Windows turns both selective-reporting switches off, then the device is suspended and resumed:
/// `D0Entry` still switches Wellspring mode on (C gated it on `PtpReportButton`), so turning the switches
/// back on brings the contacts back.
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
    assert!(wire.contains("out:2109020302000400:02010000"), "mode switch sent: {wire}");

    assert_eq!(set_switches(&mut hw, 0x03), Completion::status(0));
    let r = report(process_packet(&state, &two_finger_packet(), 0));
    assert_eq!(r[48], 2, "contacts reported again");
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
    let scan_time = |counter| {
        let PacketAction::Deliver(ReadReportResult::Report(report)) =
            process_packet(&state, &pack, counter)
        else {
            panic!("valid input packet did not produce a report");
        };
        u16::from_le_bytes([report[46], report[47]])
    };

    assert_eq!(process_packet(&state, &[], 0), PacketAction::Discard);
    assert_eq!(scan_time(200_000), 0);
    assert_eq!(scan_time(204_800), 2);
    assert_eq!(lifecycle::d0_exit(&state, &mut hw).status, 0);
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
    assert_eq!(scan_time(900_000), 0);
    assert_eq!(scan_time(902_400), 1);
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
    // GET 0x07: 07 05 00, Information 0.
    let mut caps = [0xaa_u8; 3];
    let c = get_feature(16, || {
        Some(GetFeaturePacket {
            report_id: 0x07,
            report_buffer: &mut caps,
        })
    });
    assert_eq!(c, Completion::status(0));
    assert_eq!(caps, [0x07, 0x05, 0x00]);
    // GET 0x08: report ID and the full certification blob.
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

    // SET 0x04 mode 0 → NOT_SUPPORTED; mode 3 while on → success, no transfer; mode 7 → success.
    let state = DeviceState::new();
    state.on_device_create();
    let mut hw = FakeHw::new(0x027a);
    assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
    assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
    hw.take_logs();
    for (mode, expect) in [(0_u8, STATUS_NOT_SUPPORTED), (3, 0), (7, 0)] {
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
        assert_eq!(c, Completion::status(expect), "mode {mode}");
        assert_eq!(hw.take_logs().1, "-");
    }
    // SET 0x06 [06, bits]: bit 0 Surface Switch -> report_touch, bit 1 Button Switch -> report_button.
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
    // A report without byte 1 changes nothing.
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
