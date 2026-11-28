//! The golden vectors of the porting spec (`specs/hidfilter.md`, Appendix A.4 and D.1-D.6),
//! updated for Step 2 descriptor cleanup, `ContactTracker` hardware-ID liftoff synthesis, and
//! `SET_FEATURE 0x04` input-mode gating.

#![cfg(test)]

use amtptp_hidfilter_core::*;
use ptp_hid::feature::SelectiveReportBits;

const BT: InputLayout = InputLayout {
    header_size: 4,
    finger_size: 9,
    finger_delta: 0,
    button_delta: 1,
    x_min: -3678,
    y_min: -2479,
};
const USB: InputLayout = InputLayout {
    header_size: 12,
    ..BT
};

fn h(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

#[test]
fn d1_single_finger() {
    let rows: [(&str, u16, u16, u32, bool, bool); 10] = [
        ("00 00 00 00 0a 0a 00 00 01", 3678, 2479, 1, true, true),
        ("64 00 19 00 0a 0a 00 00 02", 3778, 2279, 2, true, true),
        ("00 00 19 00 0a 0a 00 00 03", 3678, 2279, 3, true, true),
        ("8c 11 89 02 0a 0a 00 00 04", 0, 5065, 4, true, true),
        ("ff 0f 00 02 0a 0a 00 00 05", 7612, 5065, 5, true, true),
        ("00 00 00 02 0a 0a 00 00 06", 3678, 5065, 6, true, true),
        ("0a 40 01 00 01 ac 00 00 07", 3688, 2469, 7, true, true),
        ("0a 40 01 00 01 ad 00 00 08", 3688, 2469, 8, true, false),
        ("0a 40 01 00 00 00 00 00 09", 3688, 2469, 9, false, true),
        ("0a 40 01 00 05 05 00 00 af", 3688, 2469, 15, true, true),
    ];
    for (raw, x, y, id, tip, conf) in rows {
        let f: [u8; 9] = h(raw).try_into().unwrap();
        let c = parse_finger(&f, -3678, -2479);
        assert_eq!(
            (c.x, c.y, c.contact_id, c.tip_switch, c.confidence),
            (x, y, id, tip, conf),
            "{raw}"
        );
        let mut g = f;
        g[3] |= 0xFC;
        g[6] = 0x5A;
        g[7] = 0xA5;
        g[8] |= 0xB0;
        assert_eq!(parse_finger(&g, -3678, -2479), c, "{raw} ignored bits");
    }
}

#[test]
fn d2_whole_bt_packet() {
    let packet = h("31 01 00 00
        00 00 00 00 14 14 00 00 00
        64 40 06 00 14 14 00 00 01
        c8 80 0c 00 14 14 00 00 02
        2c c1 12 00 14 14 00 00 03
        90 01 19 00 14 14 00 00 04
        f4 41 1f 00 14 14 00 00 05");
    assert_eq!(packet.len(), 58);
    let expected = h("05
        03 00 00 00 00 5e 0e af 09
        03 01 00 00 00 c2 0e 7d 09
        03 02 00 00 00 26 0f 4b 09
        03 03 00 00 00 8a 0f 19 09
        03 04 00 00 00 ee 0f e7 08
        00 00 05 01");
    let mut buffer = vec![0_u8; REPORT_BUFFER_SIZE];
    buffer[..58].copy_from_slice(&packet);
    assert_eq!(
        classify_lower_read(0x004C, &BT, STATUS_SUCCESS, 58, buffer.first().copied()),
        LowerReadClass::Report { length: 58 }
    );
    assert_eq!(
        build_report(
            &BT,
            58,
            &buffer,
            0,
            SelectiveReportBits::POWER_ON,
            &mut ContactTracker::new(),
        )
        .to_vec(),
        expected
    );
}

#[test]
fn selective_reporting_shapes_the_input_report() {
    let packet = h("31 01 00 00 00 00 00 00 14 14 00 00 00");
    let mut buffer = vec![0_u8; REPORT_BUFFER_SIZE];
    buffer[..packet.len()].copy_from_slice(&packet);
    let state = DeviceState::new();
    let mut tracker = ContactTracker::new();
    for (bits, contacts, button) in [(0_u8, 0_u8, 0_u8), (1, 1, 0), (2, 0, 1), (3, 1, 1)] {
        state.apply_feature_change(decode_set_feature(0x06, &[0x06, bits]).unwrap());
        let r = build_report(
            &BT,
            packet.len(),
            &buffer,
            0,
            state.effective_reporting(),
            &mut tracker,
        );
        assert_eq!((r[48], r[49]), (contacts, button), "switch byte {bits}");
        if contacts == 0 {
            assert!(r[1..46].iter().all(|&b| b == 0), "no contact data");
        }
    }
    state.reset_selective_reporting();
    assert_eq!(state.selective_reporting(), SelectiveReportBits::POWER_ON);
}

#[test]
fn input_mode_0x04_gates_hidfilter_delivery() {
    let packet = h("31 01 00 00 00 00 00 00 14 14 00 00 00");
    let mut buffer = vec![0_u8; REPORT_BUFFER_SIZE];
    buffer[..packet.len()].copy_from_slice(&packet);
    let state = DeviceState::new();
    let mut tracker = ContactTracker::new();

    // Fresh DeviceState starts in PTP mode (ptp_input_on = true).
    let r_on = build_report(
        &BT,
        packet.len(),
        &buffer,
        0,
        state.effective_reporting(),
        &mut tracker,
    );
    assert_eq!((r_on[48], r_on[49]), (1, 1));
    assert!(tracker.has_active_contacts());

    // SET_FEATURE 0x04 mode 0 (mouse collection) suppresses both surface and button and resets contacts.
    state.apply_feature_change(decode_set_feature(0x04, &[0x04, 0x00]).unwrap());
    let r_off = build_report(
        &BT,
        packet.len(),
        &buffer,
        0,
        state.effective_reporting(),
        &mut tracker,
    );
    assert_eq!((r_off[48], r_off[49]), (0, 0));
    assert!(!tracker.has_active_contacts());

    // PrepareHardware restores ptp_input_on = true.
    state.reset_selective_reporting();
    assert_eq!(state.effective_reporting(), SelectiveReportBits::POWER_ON);
}

#[test]
fn mt2_hardware_id_tracking_and_liftoff_synthesis() {
    let mut tracker = ContactTracker::new();

    // Pre-touch hover (touch_major = 0) produces ContactCount = 0.
    let hover = h("31 00 00 00 00 00 00 00 00 14 00 00 07");
    let r0 = build_report(
        &BT,
        hover.len(),
        &hover,
        0,
        SelectiveReportBits::POWER_ON,
        &mut tracker,
    );
    assert_eq!(r0[48], 0);

    // Two contacts touch down: ID 7 (normal) and ID 12 (palm: minor = 0xad > 0xac -> Confidence = 0).
    let down = h("31 00 00 00
        00 00 00 00 14 14 00 00 07
        64 00 19 00 14 ad 00 00 0c");
    let r1 = build_report(
        &BT,
        down.len(),
        &down,
        10,
        SelectiveReportBits::POWER_ON,
        &mut tracker,
    );
    assert_eq!(r1[48], 2);
    assert_eq!((r1[1], r1[2]), (0x03, 7));
    assert_eq!((r1[10], r1[11]), (0x02, 12));

    // ID 7 disappears implicitly while ID 12 shrinks (Confidence must stay latched 0!) and lifts explicitly.
    let lift = h("31 00 00 00 64 00 19 00 00 14 00 00 0c");
    let r2 = build_report(
        &BT,
        lift.len(),
        &lift,
        20,
        SelectiveReportBits::POWER_ON,
        &mut tracker,
    );
    assert_eq!(r2[48], 2);
    assert_eq!(
        (r2[1], r2[2]),
        (0x01, 7),
        "ID 7 implicit liftoff (Confidence=1, TipSwitch=0)"
    );
    assert_eq!(
        (r2[10], r2[11]),
        (0x00, 12),
        "ID 12 explicit liftoff keeps Confidence=0 latched"
    );

    // Subsequent empty header emits ContactCount = 0.
    let empty = h("31 00 00 00");
    let r3 = build_report(
        &BT,
        empty.len(),
        &empty,
        30,
        SelectiveReportBits::POWER_ON,
        &mut tracker,
    );
    assert_eq!(r3[48], 0);
}

#[test]
fn d3_length_classification() {
    let report = |n: usize| LowerReadClass::Report { length: n };
    for (layout, info, class) in [
        (BT, 0, LowerReadClass::Empty),
        (USB, 0, LowerReadClass::Empty),
        (BT, 3, LowerReadClass::Malformed),
        (BT, 4, report(4)),
        (BT, 12, LowerReadClass::Malformed),
        (BT, 13, report(13)),
        (BT, 58, report(58)),
        (USB, 12, report(12)),
        (USB, 21, report(21)),
        (USB, 11, LowerReadClass::Malformed),
    ] {
        let (vendor, report_id) = if layout == USB {
            (0x05AC, 0x02)
        } else {
            (0x004C, 0x31)
        };
        assert_eq!(
            classify_lower_read(vendor, &layout, STATUS_SUCCESS, info, Some(report_id)),
            class,
            "{info}"
        );
    }
    assert_eq!(contact_count(&BT, 4), 0);
    assert_eq!(contact_count(&BT, 13), 1);
    assert_eq!(contact_count(&BT, 58), 5);
    let r = build_report(
        &BT,
        4,
        &[0x31, 0x01, 0, 0],
        0,
        SelectiveReportBits::POWER_ON,
        &mut ContactTracker::new(),
    );
    assert_eq!((r[48], r[49]), (0, 1));
}

#[test]
fn d5_feature_requests() {
    assert_eq!(check_xfer_length(8), Err(STATUS_BUFFER_TOO_SMALL));
    let mut b = [0_u8; 3];
    assert_eq!(get_feature_report(0x07, &mut b), STATUS_SUCCESS);
    assert_eq!(b, [0x07, 0x05, 0x00]);
    let mut b = [0xAA_u8; 2];
    assert_eq!(get_feature_report(0x07, &mut b), STATUS_INVALID_BUFFER_SIZE);
    assert_eq!(b, [0xAA; 2]);
    let mut b = [0xAA_u8; 257];
    assert_eq!(get_feature_report(0x08, &mut b), STATUS_SUCCESS);
    assert_eq!(b, ptp_hid::feature::HQA_REPORT);
    assert_eq!(
        get_feature_report(0x08, &mut [0_u8; 256]),
        STATUS_INVALID_BUFFER_SIZE
    );
    for id in [0x04, 0x06, 0x09] {
        assert_eq!(get_feature_report(id, &mut [0_u8; 8]), STATUS_NOT_SUPPORTED);
    }
    let state = DeviceState::new();
    for (bytes, on) in [([0x04_u8, 0x03], true), ([0x04, 0x00], false)] {
        state.apply_feature_change(decode_set_feature(0x04, &bytes).unwrap());
        assert_eq!(state.snapshot().ptp_input_on, on);
    }
    assert_eq!(
        decode_set_feature(0x04, &[0x04, 0x05]),
        Ok(FeatureChange::None)
    );
    for (byte, button, touch) in [
        (0x03_u8, true, true),
        (0x02, true, false),
        (0x01, false, true),
    ] {
        state.apply_feature_change(decode_set_feature(0x06, &[0x06, byte]).unwrap());
        let s = state.snapshot();
        assert_eq!((s.ptp_report_button, s.ptp_report_touch), (button, touch));
    }
    for id in [0x07, 0x08] {
        assert_eq!(decode_set_feature(id, &[id, 0]), Err(STATUS_NOT_SUPPORTED));
    }
}

#[test]
fn d6_identity_and_mode_switch() {
    assert_eq!(
        hid_descriptor_for(0x0265).unwrap(),
        h("09 21 00 01 00 01 22 FF 01")
    );
    assert_eq!(report_descriptor_for(0x0265).unwrap().len(), 511);
    assert!(hid_descriptor_for(0x0266).is_none() && report_descriptor_for(0x0266).is_none());
    let mut attrs = [0xCC_u8; 32];
    device_attributes(0x0265).write_to(&mut attrs).unwrap();
    assert_eq!(attrs[..10], h("20 00 00 00 10 89 65 02 00 01")[..]);
    assert!(attrs[10..].iter().all(|&x| x == 0));

    for (transport, id, payload) in [
        (Transport::Usb, 0x02_u8, h("02 01 00 00")),
        (Transport::Bluetooth, 0xF1, h("F1 02 01")),
    ] {
        let ms = transport.mode_switch();
        assert_eq!((ms.report_id, ms.payload), (id, (&*payload)));
    }
}

#[test]
fn a1_report_descriptor_rows() {
    assert_eq!(
        REPORT_DESCRIPTOR[..16],
        h("05 0D 09 05 A1 01 85 05 09 22 A1 02 25 01 09 47")[..]
    );
    assert_eq!(
        REPORT_DESCRIPTOR[REPORT_DESCRIPTOR.len() - 13..],
        h("01 95 02 25 01 B1 02 95 06 B1 03 C0 C0")[..]
    );
}
