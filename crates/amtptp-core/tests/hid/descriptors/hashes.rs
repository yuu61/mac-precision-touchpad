//! Cross-checks against the descriptor hashes recorded in the porting specs.

use core::fmt::Write as _;

use super::{
    SPI_S1, SPI_SERIES1, SPI_SERIES2, SPI_SERIES3_13, SPI_SERIES3_15, UM_MT2, WELLSPRING3,
    WELLSPRING5, WELLSPRING6, WELLSPRING7A, WELLSPRING8, to_legacy_c_spi, to_legacy_um,
};
use amtptp_core::hid::descriptor::{TlcUsage, report_descriptor_spi, report_descriptor_u32_id};
use amtptp_core::hid::feature::HQA_BLOB;

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
    let (blocks, tail) = msg.as_chunks::<64>();
    assert_eq!(tail, []);
    for block in blocks {
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
        let [
            mut word_a,
            mut word_b,
            mut word_c,
            mut word_d,
            mut word_e,
            mut word_f,
            mut word_g,
            mut word_h,
        ] = hash_state;
        for i in 0..64 {
            let s1 = word_e.rotate_right(6) ^ word_e.rotate_right(11) ^ word_e.rotate_right(25);
            let ch = (word_e & word_f) ^ (!word_e & word_g);
            let t1 = word_h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = word_a.rotate_right(2) ^ word_a.rotate_right(13) ^ word_a.rotate_right(22);
            let maj = (word_a & word_b) ^ (word_a & word_c) ^ (word_b & word_c);
            let t2 = s0.wrapping_add(maj);
            word_h = word_g;
            word_g = word_f;
            word_f = word_e;
            word_e = word_d.wrapping_add(t1);
            word_d = word_c;
            word_c = word_b;
            word_b = word_a;
            word_a = t1.wrapping_add(t2);
        }
        for (x, y) in hash_state.iter_mut().zip([
            word_a, word_b, word_c, word_d, word_e, word_f, word_g, word_h,
        ]) {
            *x = x.wrapping_add(y);
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
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn legacy_projected_builders_match_spec_sha256() {
    // usbum.md 5.1, spikm.md 5.1, crosscut A7.1.
    let cases: [(&str, Vec<u8>); 15] = [
        (
            "a94733137748c90e10982b8e472b2f82f4e1fac862b32b83ee67e7629aed214c",
            to_legacy_um(&report_descriptor_u32_id(WELLSPRING3)),
        ),
        (
            "3b57036a6f16393ef1be451395fc4eec774f0630d798288a762b5e38133285ca",
            to_legacy_um(&report_descriptor_u32_id(WELLSPRING5)),
        ),
        (
            "83e1b906e55b42957a3a504d36219004ffcb83065ed2fd2d6d035bb14485d007",
            to_legacy_um(&report_descriptor_u32_id(WELLSPRING6)),
        ),
        (
            "9a509e4407b52d68371a8e49b999cc7c55e268c5e4e5d75437c8ba4c34e21703",
            to_legacy_um(&report_descriptor_u32_id(WELLSPRING7A)),
        ),
        (
            "13d94c5f486b201ef4d7919d2f955415560799cc78b12524b874bce6dee7724a",
            to_legacy_um(&report_descriptor_u32_id(WELLSPRING8)),
        ),
        (
            "c803f5c305a07a33d24e518be786ade6338bad844105657e64c582ba7fa13f25",
            to_legacy_um(&UM_MT2),
        ),
        (
            "21a4d11df641c918bbe5b9718c7b054f6d399b21d4296b81bd3e9f85cc617cd3",
            to_legacy_c_spi(&SPI_S1),
        ),
        (
            "868c9d50ac9ca620a578e717daf8dfb215086e83e8bf175851b712a6f809b1aa",
            to_legacy_c_spi(&report_descriptor_spi(TlcUsage::TouchScreen, SPI_SERIES1)),
        ),
        (
            "5dab7a973382bbada6cedb4c47e0f1d7bc2d72d263ed5fd86545f96f2f8c9a3f",
            to_legacy_c_spi(&report_descriptor_spi(TlcUsage::TouchPad, SPI_SERIES2)),
        ),
        (
            "d4d5f0b8146fa01c0421653fff153cee5ff73679576546a1e8378f44cf479bbe",
            to_legacy_c_spi(&report_descriptor_spi(TlcUsage::TouchScreen, SPI_SERIES2)),
        ),
        (
            "fb0db27b1cdf967200ffd68b01005a15ffcebfd0e2786f8dba7bd163b324fcf9",
            to_legacy_c_spi(&report_descriptor_spi(TlcUsage::TouchPad, SPI_SERIES3_13)),
        ),
        (
            "28d6043da39c1cc339b20f49f20059f5bf91c32573ca74c25b9fa865e01582bf",
            to_legacy_c_spi(&report_descriptor_spi(
                TlcUsage::TouchScreen,
                SPI_SERIES3_13,
            )),
        ),
        (
            "31cd97cce4b5bfafc67bb490a1b3d2f2cf0d573c03e811181e2e7b424490361b",
            to_legacy_c_spi(&report_descriptor_spi(TlcUsage::TouchPad, SPI_SERIES3_15)),
        ),
        (
            "976e5046059319cf83acb73000237753a9d2e74e52f5f73da65b5f74b16359d7",
            to_legacy_c_spi(&report_descriptor_spi(
                TlcUsage::TouchScreen,
                SPI_SERIES3_15,
            )),
        ),
        (
            "b57b851d567808906f61a0122273da05c972140f1007355390aaf5dda8a072af",
            HQA_BLOB.to_vec(),
        ),
    ];
    for (expected, bytes) in &cases {
        assert_eq!(&sha256(bytes), expected);
    }
}
