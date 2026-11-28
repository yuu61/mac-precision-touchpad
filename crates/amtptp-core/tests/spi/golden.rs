//! Golden vectors taken from the porting spec (`spikm.md`), independent of the C-oracle fixtures,
//! updated for Step 2 HID descriptor cleanup, Series 1 Y range, coordinate clamping, `ContactTracker`,
//! and `SET_FEATURE 0x04` input gating.

use amtptp_core::hid::common::{MAX_FINGERS, PTP_COLLECTION_WINDOWS};
use amtptp_core::hid::descriptor::{
    FINGER_COLLECTION_SPI_LEN, FINGER_SEPARATOR_LEN, GLOBAL_RESET, SPI_CONTACT_ID_LOGICAL_MAX,
    SPI_CONTACT_ID_MAX_OFFSETS, TOUCHPAD_TLC_HEADER_LEN,
};
use amtptp_core::hid::feature::HQA_BLOB;
use amtptp_core::hid::feature::HQA_REPORT;
use amtptp_core::hid::feature::SelectiveReportBits;
use amtptp_core::spi::descriptors::{
    HID_DESCRIPTOR, REPORT_DESCRIPTOR_F1, REPORT_DESCRIPTOR_F1_TOUCHSCREEN, REPORT_DESCRIPTOR_F2,
    REPORT_DESCRIPTOR_F2_TOUCHSCREEN, REPORT_DESCRIPTOR_F3A, REPORT_DESCRIPTOR_F3A_TOUCHSCREEN,
    REPORT_DESCRIPTOR_F3B, REPORT_DESCRIPTOR_F3B_TOUCHSCREEN, REPORT_DESCRIPTOR_LEN,
    device_attributes,
};
use amtptp_core::spi::device::{
    APPLE_VENDOR_ID, DeviceConfig, Family, ReportType, family_for_product, lookup_trackpad,
};
use amtptp_core::spi::feature::{DEVICE_CAPS_REPLY, check_xfer_packet, get_feature};
use amtptp_core::spi::input::{SpiPacket, build_report, lower_read_length};
use amtptp_core::spi::runtime::{ContactTracker, DeviceRuntime, ReadTracker};
use amtptp_core::spi::status::*;
use amtptp_core::spi::wire::{POOL_TAG, with_set_state_packet};
use core::fmt::Write as _;

/// Minimal SHA-256 (FIPS 180-4) for checking the spec's descriptor hashes.
fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut hash_state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0_u32; 64];
        for (slot, word) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *slot = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = hash_state;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (state_word, working_word) in hash_state.iter_mut().zip(v) {
            *state_word = state_word.wrapping_add(working_word);
        }
    }
    let mut digest = String::with_capacity(64);
    for word in hash_state {
        write!(digest, "{word:08x}").expect("formatting a digest into a String");
    }
    digest
}

#[test]
fn sha256_self_test() {
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn report_descriptor_hashes_spec_5_1() {
    let table: [(&[u8; REPORT_DESCRIPTOR_LEN], bool, &str); 8] = [
        (
            &REPORT_DESCRIPTOR_F1,
            true,
            "21a4d11df641c918bbe5b9718c7b054f6d399b21d4296b81bd3e9f85cc617cd3",
        ),
        (
            &REPORT_DESCRIPTOR_F1_TOUCHSCREEN,
            true,
            "868c9d50ac9ca620a578e717daf8dfb215086e83e8bf175851b712a6f809b1aa",
        ),
        (
            &REPORT_DESCRIPTOR_F2,
            false,
            "5dab7a973382bbada6cedb4c47e0f1d7bc2d72d263ed5fd86545f96f2f8c9a3f",
        ),
        (
            &REPORT_DESCRIPTOR_F2_TOUCHSCREEN,
            false,
            "d4d5f0b8146fa01c0421653fff153cee5ff73679576546a1e8378f44cf479bbe",
        ),
        (
            &REPORT_DESCRIPTOR_F3A,
            false,
            "fb0db27b1cdf967200ffd68b01005a15ffcebfd0e2786f8dba7bd163b324fcf9",
        ),
        (
            &REPORT_DESCRIPTOR_F3A_TOUCHSCREEN,
            false,
            "28d6043da39c1cc339b20f49f20059f5bf91c32573ca74c25b9fa865e01582bf",
        ),
        (
            &REPORT_DESCRIPTOR_F3B,
            false,
            "31cd97cce4b5bfafc67bb490a1b3d2f2cf0d573c03e811181e2e7b424490361b",
        ),
        (
            &REPORT_DESCRIPTOR_F3B_TOUCHSCREEN,
            false,
            "976e5046059319cf83acb73000237753a9d2e74e52f5f73da65b5f74b16359d7",
        ),
    ];
    for (desc, is_f1, hash) in table {
        let mut patched = desc.to_vec();
        for offset in SPI_CONTACT_ID_MAX_OFFSETS {
            assert_eq!(patched[offset], SPI_CONTACT_ID_LOGICAL_MAX);
            patched[offset] = 3;
        }
        let step = FINGER_COLLECTION_SPI_LEN + FINGER_SEPARATOR_LEN;
        if is_f1 {
            for finger in 0..5 {
                let y_max_off = TOUCHPAD_TLC_HEADER_LEN + finger * step + 54;
                assert_eq!(&patched[y_max_off..y_max_off + 2], &6217_u16.to_le_bytes());
                patched[y_max_off..y_max_off + 2].copy_from_slice(&7855_u16.to_le_bytes());
            }
        }
        let f3_reset = TOUCHPAD_TLC_HEADER_LEN
            + 2 * step
            + (FINGER_COLLECTION_SPI_LEN - 1 - GLOBAL_RESET.len());
        let f5_reset = TOUCHPAD_TLC_HEADER_LEN
            + 4 * step
            + (FINGER_COLLECTION_SPI_LEN - 1 - GLOBAL_RESET.len());
        let tail_start =
            TOUCHPAD_TLC_HEADER_LEN + 5 * FINGER_COLLECTION_SPI_LEN + 4 * FINGER_SEPARATOR_LEN;
        let scan_time_reset = tail_start + 21;
        let input_mode_max_offset = tail_start + 88 + 17;
        assert_eq!(patched[input_mode_max_offset], PTP_COLLECTION_WINDOWS);

        let mut c_bytes = Vec::with_capacity(501);
        c_bytes.extend_from_slice(&patched[..f3_reset]);
        c_bytes.extend_from_slice(&patched[f3_reset + 6..f5_reset]);
        c_bytes.extend_from_slice(&patched[f5_reset + 6..scan_time_reset]);
        c_bytes.extend_from_slice(&patched[scan_time_reset + 6..input_mode_max_offset]);
        c_bytes.push(MAX_FINGERS);
        c_bytes.extend_from_slice(&patched[input_mode_max_offset + 1..]);
        c_bytes.extend_from_slice(&[
            6, 0, 255, 9, 1, 161, 1, 133, 9, 9, 1, 21, 0, 38, 255, 0, 117, 8, 149, 3, 177, 2, 192,
        ]);
        assert_eq!(sha256(&c_bytes), hash);
    }
}

#[test]
fn hqa_blob_hash() {
    assert_eq!(
        sha256(&HQA_BLOB),
        "b57b851d567808906f61a0122273da05c972140f1007355390aaf5dda8a072af"
    );
    assert_eq!(DEVICE_CAPS_REPLY, [0x07, 0x05, 0x00]);
}

#[test]
fn appendix_a_head_and_family_patches() {
    assert_eq!(
        REPORT_DESCRIPTOR_F1[..16],
        [
            0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x05, 0x09, 0x22, 0xA1, 0x02, 0x25, 0x01,
            0x09, 0x47
        ]
    );
    assert_eq!(
        REPORT_DESCRIPTOR_F1[REPORT_DESCRIPTOR_LEN - 5..],
        [0x06, 0xB1, 0x03, 0xC0, 0xC0]
    );
    // F1 -> F2 patch at the first finger collection; F1 Y logical maximum is 6217 (0x1849).
    let f1 = &REPORT_DESCRIPTOR_F1;
    let f2 = &REPORT_DESCRIPTOR_F2;
    assert_eq!(
        (f1[43], f1[44], f1[54], f1[61], f1[62], f1[64], f1[65]),
        (0xaa, 0x29, 0x5e, 0xc7, 0x02, 0x49, 0x18)
    );
    assert_eq!(
        (f2[43], f2[44], f2[54], f2[61], f2[62], f2[64], f2[65]),
        (0x2e, 0x27, 0x28, 0x05, 0x03, 0xe0, 0x1a)
    );
}

#[test]
fn hid_descriptor_spec_5_3() {
    assert_eq!(
        HID_DESCRIPTOR,
        [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xf0, 0x01]
    );
}

#[test]
fn attributes_spec_4_2() {
    let mut cfg = DeviceConfig::new();
    let mut lower = [0_u8; 32];
    lower[..10].copy_from_slice(&[0x20, 0, 0, 0, 0xac, 0x05, 0x78, 0x02, 0x34, 0x12]);
    assert_eq!(cfg.identify(STATUS_SUCCESS, &lower), Ok(()));
    let mut buf = [0xEE_u8; 32];
    device_attributes(&cfg).write_to(&mut buf).unwrap();
    assert_eq!(
        buf[..10],
        [0x20, 0, 0, 0, 0x10, 0x89, 0x78, 0x02, 0x34, 0x12]
    );
    assert!(buf[10..].iter().all(|&b| b == 0), "Reserved zeroed");
}

#[test]
fn table_and_families_spec_8() {
    let f2 = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).unwrap();
    assert_eq!(
        (f2.x_min, f2.x_max, f2.y_min, f2.y_max),
        (-4750, 5280, -150, 6730)
    );
    let f3b = lookup_trackpad(APPLE_VENDOR_ID, 0x0278).unwrap();
    assert_eq!((f3b.x_min, f3b.y_max), (-7456, 9283));
    assert_eq!(family_for_product(0x0291), Some(Family::F1));
    assert_eq!(family_for_product(0x0277), Some(Family::F3a));
    assert!(
        lookup_trackpad(APPLE_VENDOR_ID, 0x0280).is_none(),
        "0x0280 is a USB KMDF binding, not an SPI model"
    );
    assert!(Family::F1.report_descriptor(ReportType::INVALID).is_none());
}

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
