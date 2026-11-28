//! Golden vectors for the SPI model table and HID descriptors.

use amtptp_core::hid::common::{MAX_FINGERS, PTP_COLLECTION_WINDOWS};
use amtptp_core::hid::descriptor::{
    FINGER_COLLECTION_SPI_LEN, FINGER_SEPARATOR_LEN, GLOBAL_RESET, SPI_CONTACT_ID_LOGICAL_MAX,
    SPI_CONTACT_ID_MAX_OFFSETS, TOUCHPAD_TLC_HEADER_LEN,
};
use amtptp_core::hid::feature::HQA_BLOB;
use amtptp_core::spi::descriptors::{
    HID_DESCRIPTOR, REPORT_DESCRIPTOR_F1, REPORT_DESCRIPTOR_F1_TOUCHSCREEN, REPORT_DESCRIPTOR_F2,
    REPORT_DESCRIPTOR_F2_TOUCHSCREEN, REPORT_DESCRIPTOR_F3A, REPORT_DESCRIPTOR_F3A_TOUCHSCREEN,
    REPORT_DESCRIPTOR_F3B, REPORT_DESCRIPTOR_F3B_TOUCHSCREEN, REPORT_DESCRIPTOR_LEN,
    device_attributes,
};
use amtptp_core::spi::device::{
    APPLE_VENDOR_ID, DeviceConfig, Family, ReportType, family_for_product, lookup_trackpad,
};
use amtptp_core::spi::feature::DEVICE_CAPS_REPLY;
use amtptp_core::spi::status::STATUS_SUCCESS;
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
