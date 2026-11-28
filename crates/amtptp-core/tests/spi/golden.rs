//! Golden vectors taken from the porting spec (`spikm.md`), independent of the C-oracle fixtures,
//! updated for Step 2 HID descriptor cleanup, Series 1 Y range, coordinate clamping, `ContactTracker`,
//! and `SET_FEATURE 0x04` input gating.

mod descriptors;

use amtptp_core::hid::descriptor::SPI_CONTACT_ID_LOGICAL_MAX;
use amtptp_core::hid::feature::{HQA_REPORT, SelectiveReportBits};
use amtptp_core::spi::device::{APPLE_VENDOR_ID, lookup_trackpad};
use amtptp_core::spi::feature::{check_xfer_packet, get_feature};
use amtptp_core::spi::input::{SpiPacket, build_report, lower_read_length};
use amtptp_core::spi::runtime::{ContactTracker, DeviceRuntime, ReadTracker};
use amtptp_core::spi::status::*;
use amtptp_core::spi::wire::{POOL_TAG, with_set_state_packet};

fn packet_with_fingers(n: u8, click: u8, fingers: &[(i16, i16, i16, i16, i16)]) -> Vec<u8> {
    let mut b = vec![0_u8; 1024];
    b[1] = click;
    b[30] = n;
    for (i, &(x, y, major, minor, pressure)) in fingers.iter().enumerate() {
        let f = 46 + 30 * i;
        b[f + 4..f + 6].copy_from_slice(&x.to_le_bytes());
        b[f + 6..f + 8].copy_from_slice(&y.to_le_bytes());
        b[f + 18..f + 20].copy_from_slice(&major.to_le_bytes());
        b[f + 20..f + 22].copy_from_slice(&minor.to_le_bytes());
        b[f + 26..f + 28].copy_from_slice(&pressure.to_le_bytes());
    }
    b
}

#[test]
fn worked_example_spec_7_4() {
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).unwrap();
    let pkt = packet_with_fingers(1, 0, &[(100, 2000, 800, 600, 30)]);
    let mut contacts = ContactTracker::new();
    let report = build_report(
        &SpiPacket::new(&pkt),
        info,
        0,
        SelectiveReportBits::POWER_ON,
        &mut contacts,
    )
    .to_bytes();
    assert_eq!(report[..6], [0x05, 0x03, 0xF2, 0x12, 0x7A, 0x12]);
    assert!(
        report[6..28].iter().all(|&b| b == 0),
        "zeroed unused contacts (SPI-B5)"
    );
    assert_eq!((report[28], report[29]), (1, 0));
}

/// The runtime's whole-frame path: five contacts with their records, a header that declares more contacts
/// than the data holds, and more contacts than the report can carry.
#[test]
fn declared_contacts_must_match_the_frame() {
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).unwrap();
    let five = [
        (100, 2000, 800, 600, 30),
        (500, 2000, 800, 600, 30),
        (900, 2000, 800, 600, 30),
        (1300, 2000, 800, 600, 30),
        (1700, 2000, 800, 600, 30),
    ];
    let run = |pkt: &[u8], len: u64| {
        let rt = DeviceRuntime::new();
        rt.on_d0_entry(10_000_000);
        let mut queried = false;
        let mut contacts = ContactTracker::new();
        let report = rt.complete_lower_read(info, &mut contacts, len, pkt, || {
            queried = true;
            0
        });
        (report, queried)
    };

    // Five contacts, each with its 30-byte record: delivered with IDs 0..=4 and ContactCount 5.
    let pkt = packet_with_fingers(5, 0, &five);
    let (report, queried) = run(&pkt, 46 + 5 * 30);
    let report = report.expect("a complete five-contact frame");
    assert!(queried);
    assert_eq!(report[28], 5);
    let ids: Vec<u8> = (0..5).map(|i| (report[1 + 5 * i] >> 2) & 0x07).collect();
    assert_eq!(ids, [0, 1, 2, 3, 4]);
    assert!(ids.iter().all(|&id| id <= SPI_CONTACT_ID_LOGICAL_MAX));

    // The header alone, declaring six contacts: rejected before the clock is sampled.
    let header = packet_with_fingers(6, 0, &[]);
    assert_eq!(
        run(&header[..46], 46),
        (Err(STATUS_DEVICE_DATA_ERROR), false)
    );
    // Two contacts declared, one record present.
    let pkt = packet_with_fingers(2, 0, &five[..1]);
    assert_eq!(run(&pkt, 46 + 30), (Err(STATUS_DEVICE_DATA_ERROR), false));

    // Seven contacts with all seven records: only the first five are reported, ContactCount 5.
    let seven = [
        (100, 2000, 800, 600, 30),
        (500, 2000, 800, 600, 30),
        (900, 2000, 800, 600, 30),
        (1300, 2000, 800, 600, 30),
        (1700, 2000, 800, 600, 30),
        (2100, 2000, 800, 600, 30),
        (2500, 2000, 800, 600, 30),
    ];
    let pkt = packet_with_fingers(7, 1, &seven);
    let (report, _) = run(&pkt, 46 + 7 * 30);
    let report = report.expect("a complete seven-contact frame");
    assert_eq!((report[28], report[29]), (5, 1));
}

/// `SET_FEATURE` 0x06 `[06, bits]` (bit 0 Surface Switch, bit 1 Button Switch) for each of the four
/// combinations, then one frame with a contact and the button down.
#[test]
fn selective_reporting_shapes_the_input_report() {
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).unwrap();
    let pkt = packet_with_fingers(1, 1, &[(100, 2000, 800, 600, 30)]);
    for (bits, contacts, button) in [(0_u8, 0_u8, 0_u8), (1, 1, 0), (2, 0, 1), (3, 1, 1)] {
        let rt = DeviceRuntime::new();
        rt.on_d0_entry(10_000_000);
        let mut tracker = ContactTracker::new();
        assert_eq!(rt.set_feature(6, 2, Some(&[6, bits])), STATUS_SUCCESS);
        let r = rt
            .complete_lower_read(info, &mut tracker, 76, &pkt, || 0)
            .unwrap();
        assert_eq!(
            (r[28], r[29]),
            (contacts, button),
            "switch byte {bits:#04x}"
        );
        if contacts == 0 {
            assert!(r[1..26].iter().all(|&b| b == 0), "no contact data");
        } else {
            assert_eq!(r[1..6], [0x03, 0xF2, 0x12, 0x7A, 0x12]);
        }
        // A restart (PrepareHardware) restores both switches.
        rt.ptp_settings().reset_selective_reporting();
        let r = rt
            .complete_lower_read(info, &mut tracker, 76, &pkt, || 0)
            .unwrap();
        assert_eq!((r[28], r[29]), (1, 1));
    }
}

#[test]
fn contact_flags_and_coordinate_clamping() {
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0278).unwrap();
    // On frame 0, hover contacts (pressure <= 0) are ignored while active contacts receive sequential IDs.
    let fingers = [
        (0, 0, -1, -0x8000, -5),
        (0, 0, 2499, 2499, 1),
        (500, 500, 2500, 0, 0),
        (1000, 1000, 0, 2500, 0x7FFF),
        (2000, 2000, 0, 0, 1),
    ];
    let pkt = packet_with_fingers(255, 0xFF, &fingers);
    let mut contacts = ContactTracker::new();
    let r = build_report(
        &SpiPacket::new(&pkt),
        info,
        0,
        SelectiveReportBits::POWER_ON,
        &mut contacts,
    )
    .to_bytes();
    let flag = |i: usize| r[1 + 5 * i];
    assert_eq!(flag(0), 0x03, "ID 0: major/minor 2499 -> confident + tip");
    assert_eq!(
        flag(1),
        0x02 | (1 << 2),
        "ID 1: minor 2500 -> palm (!conf) + tip"
    );
    assert_eq!(flag(2), 0x03 | (2 << 2), "ID 2: confident + tip");
    assert_eq!((r[28], r[29]), (3, 0xFF));
    assert_eq!(u16::from_le_bytes([r[2], r[3]]), 7456);
    assert_eq!(u16::from_le_bytes([r[4], r[5]]), 9283);

    // X/Y clamp to 0..=(max - min): 15432 x 9446 on 0x0278, and 10666 x 6217 on Series 1 (0x0275).
    let pkt = packet_with_fingers(
        2,
        0,
        &[
            (-0x8000, 0x7FFF, 100, 100, 10),
            (0x7FFF, -0x8000, 100, 100, 10),
        ],
    );
    let r = build_report(
        &SpiPacket::new(&pkt),
        info,
        0,
        SelectiveReportBits::POWER_ON,
        &mut ContactTracker::new(),
    )
    .to_bytes();
    assert_eq!(r[2..6], [0, 0, 0, 0]);
    assert_eq!(u16::from_le_bytes([r[7], r[8]]), 15_432);
    assert_eq!(u16::from_le_bytes([r[9], r[10]]), 9_446);

    let f1 = lookup_trackpad(APPLE_VENDOR_ID, 0x0275).unwrap();
    let r_f1 = build_report(
        &SpiPacket::new(&pkt),
        f1,
        0,
        SelectiveReportBits::POWER_ON,
        &mut ContactTracker::new(),
    )
    .to_bytes();
    assert_eq!(u16::from_le_bytes([r_f1[7], r_f1[8]]), 10_666);
    assert_eq!(u16::from_le_bytes([r_f1[9], r_f1[10]]), 6_217);
}

#[test]
fn multi_finger_tracking_and_liftoff_frame() {
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).unwrap();
    let mut contacts = ContactTracker::new();
    // Frame 0: two fingers touch down -> IDs 0 and 1.
    let f0 = packet_with_fingers(
        2,
        0,
        &[(-1000, 2000, 600, 500, 25), (1500, 2000, 600, 500, 25)],
    );
    let r0 = build_report(
        &SpiPacket::new(&f0),
        info,
        0,
        SelectiveReportBits::POWER_ON,
        &mut contacts,
    );
    assert_eq!(r0.contact_count, 2);
    assert_eq!(
        (r0.contacts[0].contact_id, r0.contacts[0].tip_switch),
        (0, true)
    );
    assert_eq!(
        (r0.contacts[1].contact_id, r0.contacts[1].tip_switch),
        (1, true)
    );

    // Frame 1: finger 0 lifts (only finger 1 remains in packet) -> finger 1 keeps ID 1, finger 0 emits liftoff.
    let f1 = packet_with_fingers(1, 0, &[(1520, 1990, 600, 500, 25)]);
    let r1 = build_report(
        &SpiPacket::new(&f1),
        info,
        10,
        SelectiveReportBits::POWER_ON,
        &mut contacts,
    );
    assert_eq!(r1.contact_count, 2);
    assert_eq!(
        (r1.contacts[0].contact_id, r1.contacts[0].tip_switch),
        (0, false)
    );
    assert_eq!(
        (r1.contacts[1].contact_id, r1.contacts[1].tip_switch),
        (1, true)
    );

    // Frame 2: liftoff is complete; only ID 1 remains.
    let r2 = build_report(
        &SpiPacket::new(&f1),
        info,
        20,
        SelectiveReportBits::POWER_ON,
        &mut contacts,
    );
    assert_eq!(r2.contact_count, 1);
    assert_eq!(
        (r2.contacts[0].contact_id, r2.contacts[0].tip_switch),
        (1, true)
    );
}

#[test]
fn input_mode_feature_gates_spi_reporting() {
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).unwrap();
    let pkt = packet_with_fingers(1, 1, &[(100, 2000, 800, 600, 30)]);
    let rt = DeviceRuntime::new();
    rt.on_d0_entry(10_000_000);
    let mut contacts = ContactTracker::new();

    // Fresh runtime starts in PTP mode (input_on = true).
    let r = rt
        .complete_lower_read(info, &mut contacts, 76, &pkt, || 0)
        .unwrap();
    assert_eq!((r[28], r[29]), (1, 1));

    // Mode 0 (mouse collection) gates both surface and button reporting.
    assert_eq!(rt.set_feature(4, 2, Some(&[4, 0])), STATUS_SUCCESS);
    contacts.reset();
    let r_off = rt
        .complete_lower_read(info, &mut contacts, 76, &pkt, || 0)
        .unwrap();
    assert_eq!((r_off[28], r_off[29]), (0, 0));

    // PrepareHardware reset restores input_on = true alongside selective reporting.
    rt.ptp_settings().reset_selective_reporting();
    let r_on = rt
        .complete_lower_read(info, &mut contacts, 76, &pkt, || 0)
        .unwrap();
    assert_eq!((r_on[28], r_on[29]), (1, 1));
}

#[test]
fn scan_time_advances_and_wraps_in_spi_reports() {
    let rt = DeviceRuntime::new();
    let frequency = 10_000_000;
    rt.on_d0_entry(frequency);
    let _status = rt.on_self_managed_io_init_or_restart(
        &mut ReadTracker::<u32>::new(),
        STATUS_SUCCESS,
        || frequency,
    );
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).unwrap();
    let pkt = packet_with_fingers(1, 1, &[(100, 2000, 800, 600, 30)]);
    let mut contacts = ContactTracker::new();
    let mut scan_time = |counter| {
        let report = rt
            .complete_lower_read(info, &mut contacts, 76, &pkt, || counter)
            .unwrap();
        u16::from_le_bytes([report[26], report[27]])
    };
    let start = 20_000_000;
    assert_eq!(scan_time(start), 0);
    assert_eq!(scan_time(start + 256_000), 256);
    assert_eq!(scan_time(start + 65_535_000), u16::MAX);
    assert_eq!(scan_time(start + 65_536_000), 0);
    let _status = rt.on_self_managed_io_init_or_restart(
        &mut ReadTracker::<u32>::new(),
        STATUS_SUCCESS,
        || frequency,
    );
    assert_eq!(scan_time(start + 70_000_000), 0);
}

#[test]
fn b9_bytes_past_length_are_never_read() {
    let rt = DeviceRuntime::new();
    rt.on_d0_entry(0);
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).unwrap();
    let pkt = packet_with_fingers(1, 1, &[(100, 2000, 800, 600, 30)]);
    let mut contacts = ContactTracker::new();
    // Information 46: the declared finger lies past the valid data, so the frame is rejected (P3).
    assert_eq!(
        rt.complete_lower_read(info, &mut contacts, 46, &pkt, || 0),
        Err(STATUS_DEVICE_DATA_ERROR)
    );
    // With its record inside Information the same finger is converted.
    let r = rt
        .complete_lower_read(info, &mut contacts, 76, &pkt, || 0)
        .unwrap();
    assert_eq!(r[1..6], [0x03, 0xF2, 0x12, 0x7A, 0x12]);
    assert_eq!(lower_read_length(45), Err(STATUS_DEVICE_DATA_ERROR));
    assert_eq!(lower_read_length(1024), Ok(1024));
    assert_eq!(lower_read_length(1025), Err(STATUS_DEVICE_DATA_ERROR));
    assert_eq!(
        lower_read_length(0x1_0000_004C),
        Err(STATUS_DEVICE_DATA_ERROR)
    );
}

#[test]
fn set_state_buffer_spec_6_2() {
    let img = with_set_state_packet(true, |bytes| {
        let mut image = *bytes;
        let pointer = u64::from_le_bytes(image[..8].try_into().unwrap());
        assert_eq!(pointer, bytes.as_ptr() as u64 + 16);
        image[..8].copy_from_slice(&(0x1000_u64 + 16).to_le_bytes());
        image
    })
    .unwrap();
    assert_eq!(img.len(), 255);
    assert_eq!(img[..8], 0x1010_u64.to_le_bytes());
    assert_eq!(img[8..18], [2, 0, 0, 0, 2, 0, 0, 0, 2, 1]);
    assert!(img[18..].iter().all(|&b| b == 0));
}

#[test]
fn feature_checks_spec_4_6() {
    assert_eq!(check_xfer_packet(15, false), Err(STATUS_BUFFER_TOO_SMALL));
    assert_eq!(
        check_xfer_packet(16, false),
        Err(STATUS_INVALID_DEVICE_REQUEST)
    );
    assert_eq!(check_xfer_packet(16, true), Ok(()));
    assert_eq!(get_feature(9, 4, true).status, STATUS_NOT_SUPPORTED);
    assert_eq!(get_feature(8, 256, true).status, STATUS_INVALID_BUFFER_SIZE);
    assert_eq!(get_feature(8, 257, true).write, HQA_REPORT);
    let rt = DeviceRuntime::new();
    assert_eq!(rt.set_feature(9, 4, Some(&[9, 1, 2])), STATUS_NOT_SUPPORTED);
}

#[test]
fn pool_tag_and_statuses() {
    assert_eq!(POOL_TAG, 0x4C54_5041);
    assert_eq!(STATUS_NOT_SUPPORTED as u32, 0xC000_00BB);
    assert_eq!(STATUS_INVALID_DEVICE_STATE as u32, 0xC000_0184);
    assert_eq!(STATUS_INVALID_BUFFER_SIZE as u32, 0xC000_0206);
    assert_eq!(STATUS_CANCELLED as u32, 0xC000_0120);
}
