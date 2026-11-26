//! Golden vectors taken from the porting spec (`spikm.md`), independent of the C-oracle fixtures.

#![cfg(test)]

use amtptp_spikm_core::descriptors::{
    HID_DESCRIPTOR, REPORT_DESCRIPTOR_F1, REPORT_DESCRIPTOR_F1_TOUCHSCREEN, REPORT_DESCRIPTOR_F2,
    REPORT_DESCRIPTOR_F2_TOUCHSCREEN, REPORT_DESCRIPTOR_F3A, REPORT_DESCRIPTOR_F3A_TOUCHSCREEN,
    REPORT_DESCRIPTOR_F3B, REPORT_DESCRIPTOR_F3B_TOUCHSCREEN, device_attributes,
};
use amtptp_spikm_core::device::{
    APPLE_VENDOR_ID, DeviceConfig, Family, ReportType, family_for_product, lookup_trackpad,
};
use amtptp_spikm_core::feature::{
    DEVICE_CAPS_REPLY, HQA_REPORT_AS_C, check_xfer_packet, get_feature,
};
use amtptp_spikm_core::input::{SpiPacket, build_report, lower_read_length, scan_time};
use amtptp_spikm_core::runtime::DeviceRuntime;
use amtptp_spikm_core::status::*;
use amtptp_spikm_core::wire::{POOL_TAG, SetStatePacket};
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
    let table: [(&[u8; 501], &str); 8] = [
        (
            &REPORT_DESCRIPTOR_F1,
            "21a4d11df641c918bbe5b9718c7b054f6d399b21d4296b81bd3e9f85cc617cd3",
        ),
        (
            &REPORT_DESCRIPTOR_F1_TOUCHSCREEN,
            "868c9d50ac9ca620a578e717daf8dfb215086e83e8bf175851b712a6f809b1aa",
        ),
        (
            &REPORT_DESCRIPTOR_F2,
            "5dab7a973382bbada6cedb4c47e0f1d7bc2d72d263ed5fd86545f96f2f8c9a3f",
        ),
        (
            &REPORT_DESCRIPTOR_F2_TOUCHSCREEN,
            "d4d5f0b8146fa01c0421653fff153cee5ff73679576546a1e8378f44cf479bbe",
        ),
        (
            &REPORT_DESCRIPTOR_F3A,
            "fb0db27b1cdf967200ffd68b01005a15ffcebfd0e2786f8dba7bd163b324fcf9",
        ),
        (
            &REPORT_DESCRIPTOR_F3A_TOUCHSCREEN,
            "28d6043da39c1cc339b20f49f20059f5bf91c32573ca74c25b9fa865e01582bf",
        ),
        (
            &REPORT_DESCRIPTOR_F3B,
            "31cd97cce4b5bfafc67bb490a1b3d2f2cf0d573c03e811181e2e7b424490361b",
        ),
        (
            &REPORT_DESCRIPTOR_F3B_TOUCHSCREEN,
            "976e5046059319cf83acb73000237753a9d2e74e52f5f73da65b5f74b16359d7",
        ),
    ];
    for (desc, hash) in table {
        assert_eq!(sha256(desc), hash);
    }
}

#[test]
fn hqa_blob_hash_and_as_c_bytes() {
    assert_eq!(
        sha256(&ptp_hid::feature::HQA_BLOB),
        "b57b851d567808906f61a0122273da05c972140f1007355390aaf5dda8a072af"
    );
    assert_eq!(HQA_REPORT_AS_C, [0x08, 0xFC]);
    assert_eq!(DEVICE_CAPS_REPLY, [0x07, 0x05, 0x00]);
}

#[test]
fn appendix_a_head_and_family_patches() {
    // Appendix A row 0000 and the end of the descriptor.
    assert_eq!(
        REPORT_DESCRIPTOR_F1[..16],
        [
            0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x05, 0x09, 0x22, 0xA1, 0x02, 0x25, 0x01,
            0x09, 0x47
        ]
    );
    assert_eq!(
        REPORT_DESCRIPTOR_F1[0x1F0..],
        [0x95, 0x03, 0xB1, 0x02, 0xC0]
    );
    // F1 -> F2 patch at the first finger collection (spec 5.1).
    let f1 = &REPORT_DESCRIPTOR_F1;
    let f2 = &REPORT_DESCRIPTOR_F2;
    assert_eq!(
        (f1[43], f1[44], f1[54], f1[61], f1[62], f1[64], f1[65]),
        (0xaa, 0x29, 0x5e, 0xc7, 0x02, 0xaf, 0x1e)
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
        [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xF5, 0x01]
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
    assert!(buf[10..].iter().all(|&b| b == 0xEE), "Reserved untouched");
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
        "INF-only PID (SPI-B17)"
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
    let report = build_report(&SpiPacket::new(&pkt), info, 0).to_bytes();
    assert_eq!(report[..6], [0x05, 0x03, 0xF2, 0x12, 0x7A, 0x12]);
    assert!(
        report[6..28].iter().all(|&b| b == 0),
        "zeroed unused contacts (SPI-B5)"
    );
    assert_eq!((report[28], report[29]), (1, 0));
}

#[test]
fn contact_flags_and_quirks() {
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0278).unwrap();
    // 255 fingers announced: ContactCount raw (SPI-B6), 5 contacts filled with IDs 0..4 (SPI-B7).
    let fingers = [
        (0, 0, -1, -0x8000, -5),
        (1, 1, 2499, 2499, 1),
        (2, 2, 2500, 0, 0),
        (3, 3, 0, 2500, 0x7FFF),
        (4, 4, 0, 0, 1),
    ];
    let pkt = packet_with_fingers(255, 0xFF, &fingers);
    let r = build_report(&SpiPacket::new(&pkt), info, 0).to_bytes();
    let flag = |i: usize| r[1 + 5 * i];
    assert_eq!(
        flag(0),
        0x01,
        "negative touch -> confident, negative pressure -> no tip"
    );
    assert_eq!(flag(1), 0x03 | (1 << 2));
    assert_eq!(flag(2), 2 << 2, "TouchMajor 2500 -> not confident");
    assert_eq!(flag(3), 0x02 | (3 << 2));
    assert_eq!(flag(4), 0x13, "conf + tip + ID 4 (spec addendum)");
    assert_eq!((r[28], r[29]), (255, 0xFF));
    // X = X - XMin, Y = YMax - Y, no upper clamp (SPI-B19).
    assert_eq!(u16::from_le_bytes([r[2], r[3]]), 7456);
    assert_eq!(u16::from_le_bytes([r[4], r[5]]), 9283);
    // X far below XMin -> 0; Y above YMax -> 0; 32767 - (-7456) -> 40223 (beyond the logical max).
    let pkt = packet_with_fingers(
        2,
        0,
        &[(-0x8000, 0x7FFF, 0, 0, 0), (0x7FFF, -0x8000, 0, 0, 0)],
    );
    let r = build_report(&SpiPacket::new(&pkt), info, 0).to_bytes();
    assert_eq!(r[2..6], [0, 0, 0, 0]);
    assert_eq!(u16::from_le_bytes([r[7], r[8]]), 40223);
    assert_eq!(u16::from_le_bytes([r[9], r[10]]), 42051);
}

#[test]
fn scan_time_is_zero_in_the_shipped_driver() {
    // SPI-B8: every LastReportTime writer stores the QPC frequency, so delta is always 0.
    let rt = DeviceRuntime::new();
    rt.on_d0_entry(10_000_000);
    let _status = rt.on_self_managed_io_init_or_restart(STATUS_SUCCESS, || 10_000_000);
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).unwrap();
    let pkt = packet_with_fingers(1, 1, &[(100, 2000, 800, 600, 30)]);
    for _ in 0..3 {
        let r = rt
            .complete_lower_read(info, 76, &pkt, || 10_000_000)
            .unwrap();
        assert_eq!(r[26..28], [0, 0]);
    }
    assert_eq!(scan_time(10_000_000 + 25_500, 10_000_000), 0xFF);
}

#[test]
fn b9_bytes_past_length_read_as_zero() {
    let rt = DeviceRuntime::new();
    rt.on_d0_entry(0);
    let info = lookup_trackpad(APPLE_VENDOR_ID, 0x0272).unwrap();
    let pkt = packet_with_fingers(1, 1, &[(100, 2000, 800, 600, 30)]);
    // Information 46: the finger lies past the valid data and reads as zeros.
    let r = rt.complete_lower_read(info, 46, &pkt, || 0).unwrap();
    // X = 0 - (-4750), Y = 6730 - 0, Confidence (0 < 2500), no tip.
    assert_eq!(r[1..6], [0x01, 0x8E, 0x12, 0x4A, 0x1A]);
    assert_eq!(lower_read_length(45), Err(STATUS_DEVICE_DATA_ERROR));
}

#[test]
fn set_state_buffer_spec_6_2() {
    let img = SetStatePacket::image(true, 0x1000);
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
    assert_eq!(get_feature(8, 257, true).write, [0x08, 0xFC]);
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
