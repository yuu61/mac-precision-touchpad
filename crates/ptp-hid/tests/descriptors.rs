//! Byte-for-byte golden tests: all 16 C report descriptors, the HQA blob and the C struct sizes, against the
//! fixtures generated from the C headers at commit 2edb0f2 (see `fixtures/README.md`).

use std::collections::BTreeMap;

use ptp_hid::descriptor::{
    Axes, REPORT_DESCRIPTOR_SPI_UMAPP_LEN, REPORT_DESCRIPTOR_U32_ID_LEN,
    REPORT_DESCRIPTOR_U32_ID_UMAPP_LEN, TlcUsage, report_descriptor_spi_umapp,
    report_descriptor_u32_id, report_descriptor_u32_id_umapp,
};
use ptp_hid::feature::{
    DEVICE_CAPS_REPORT_LEN, HQA_BLOB, HQA_REPORT_LEN, INPUT_MODE_REPORT_LEN,
    SELECTIVE_REPORT_DEVICE_MODE_STRUCT_LEN, SELECTIVE_REPORT_LEN, UMAPP_CONF_REPORT_LEN,
};
use ptp_hid::hidport::{
    HID_DESCRIPTOR_SPI_UMAPP, HID_DESCRIPTOR_U32_ID, HID_DESCRIPTOR_U32_ID_UMAPP, hid_descriptor,
};
use ptp_hid::input::{PTP_CONTACT_LEN, PTP_REPORT_LEN, SPI_PTP_CONTACT_LEN, SPI_PTP_REPORT_LEN};

// ---- per-family axis values (crosscut spec A4; they belong to the core crates) ----

const fn axes(
    x_logical_max: u16,
    x_physical_max: u16,
    y_physical_max: u16,
    y_logical_max: u16,
) -> Axes {
    Axes {
        x_logical_max,
        x_physical_max,
        y_physical_max,
        y_logical_max,
    }
}

const WELLSPRING3: Axes = axes(9626, 1050, 760, 6775);
const WELLSPRING5: Axes = axes(9465, 1050, 810, 6735);
const WELLSPRING6: Axes = axes(9760, 1067, 762, 6750);
const WELLSPRING7A: Axes = axes(10030, 1064, 773, 6880);
const WELLSPRING8: Axes = axes(9760, 1045, 750, 6750);
const MAGIC_TRACKPAD2: Axes = axes(7612, 1600, 1149, 5065);
const WELLSPRING_T2: Axes = axes(20000, 1300, 850, 12000);
const SPI_SERIES1: Axes = axes(10666, 1118, 711, 7855);
const SPI_SERIES2: Axes = axes(10030, 1064, 773, 6880);
const SPI_SERIES3_13: Axes = axes(12992, 1352, 836, 7855);
const SPI_SERIES3_15: Axes = axes(15432, 1584, 992, 9446);

// Built in const/static context, exactly as a core crate would.
const UM_MT2: [u8; REPORT_DESCRIPTOR_U32_ID_UMAPP_LEN] =
    report_descriptor_u32_id_umapp(MAGIC_TRACKPAD2);
static KM_T2: [u8; REPORT_DESCRIPTOR_U32_ID_LEN] = report_descriptor_u32_id(WELLSPRING_T2);
static FLT_MT2: [u8; REPORT_DESCRIPTOR_U32_ID_LEN] = report_descriptor_u32_id(MAGIC_TRACKPAD2);
const SPI_S1: [u8; REPORT_DESCRIPTOR_SPI_UMAPP_LEN] =
    report_descriptor_spi_umapp(TlcUsage::TouchPad, SPI_SERIES1);

/// One parsed fixture file: named byte entries plus the `sizeof` key/value pairs.
struct Fixture {
    entries: BTreeMap<String, Vec<u8>>,
    sizes: BTreeMap<String, usize>,
}

/// Parses the fixture format (see `cmp.py`): `<name> len=<n>` starts an entry, hex bytes follow until the
/// next entry or a `sizeof` line. Asserts that every entry has exactly the declared number of bytes.
fn parse(text: &str) -> Fixture {
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut declared: BTreeMap<String, usize> = BTreeMap::new();
    let mut sizes = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("sizeof") {
            current = None;
            for kv in rest.split_whitespace() {
                let (k, v) = kv.split_once('=').expect("sizeof key=value");
                sizes.insert(k.to_owned(), v.parse().expect("sizeof value"));
            }
            continue;
        }
        let mut words = line.split_whitespace();
        if let (Some(name), Some(len)) = (words.next(), words.next())
            && let Some(len) = len.strip_prefix("len=")
        {
            assert!(words.next().is_none(), "trailing text after {name} len=");
            let name = name.to_owned();
            assert!(
                !entries.contains_key(&name),
                "duplicate fixture entry {name}"
            );
            declared.insert(name.clone(), len.parse().expect("len value"));
            entries.insert(name.clone(), Vec::new());
            current = Some(name);
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let name = current.as_ref().expect("hex bytes outside an entry");
        let bytes = entries.get_mut(name).expect("entry");
        for tok in line.split_whitespace() {
            assert_eq!(tok.len(), 2, "bad hex token {tok:?} in {name}");
            bytes.push(u8::from_str_radix(tok, 16).expect("hex byte"));
        }
    }
    for (name, bytes) in &entries {
        assert_eq!(
            bytes.len(),
            declared[name],
            "{name}: parsed byte count != len="
        );
    }
    Fixture { entries, sizes }
}

fn um() -> Fixture {
    parse(include_str!("fixtures/um.txt"))
}
fn km() -> Fixture {
    parse(include_str!("fixtures/km.txt"))
}
fn spi() -> Fixture {
    parse(include_str!("fixtures/spi.txt"))
}
fn flt() -> Fixture {
    parse(include_str!("fixtures/flt.txt"))
}
fn hqa() -> Fixture {
    parse(include_str!("fixtures/hqa.txt"))
}

/// The 16 descriptors: (fixture file, entry name, Rust builder output).
fn all_sixteen() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let u32_umapp = |a| report_descriptor_u32_id_umapp(a).to_vec();
    let spi_pad = |a| report_descriptor_spi_umapp(TlcUsage::TouchPad, a).to_vec();
    let spi_ts = |a| report_descriptor_spi_umapp(TlcUsage::TouchScreen, a).to_vec();
    vec![
        ("um", "AmtPtp3ReportDescriptor", u32_umapp(WELLSPRING3)),
        ("um", "AmtPtp5ReportDescriptor", u32_umapp(WELLSPRING5)),
        ("um", "AmtPtp6ReportDescriptor", u32_umapp(WELLSPRING6)),
        ("um", "AmtPtp7aReportDescriptor", u32_umapp(WELLSPRING7A)),
        ("um", "AmtPtp8ReportDescriptor", u32_umapp(WELLSPRING8)),
        (
            "um",
            "AmtPtpMt2ReportDescriptor",
            u32_umapp(MAGIC_TRACKPAD2),
        ),
        (
            "km",
            "AmtPtpT2ReportDescriptor",
            report_descriptor_u32_id(WELLSPRING_T2).to_vec(),
        ),
        ("spi", "S1", spi_pad(SPI_SERIES1)),
        ("spi", "S1T", spi_ts(SPI_SERIES1)),
        ("spi", "S2", spi_pad(SPI_SERIES2)),
        ("spi", "S2T", spi_ts(SPI_SERIES2)),
        ("spi", "S3a", spi_pad(SPI_SERIES3_13)),
        ("spi", "S3aT", spi_ts(SPI_SERIES3_13)),
        ("spi", "S3b", spi_pad(SPI_SERIES3_15)),
        ("spi", "S3bT", spi_ts(SPI_SERIES3_15)),
        (
            "flt",
            "PtpReportDescriptorMagicTrackpad2",
            report_descriptor_u32_id(MAGIC_TRACKPAD2).to_vec(),
        ),
    ]
}

fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .or(if a.len() == b.len() {
            None
        } else {
            Some(a.len().min(b.len()))
        })
}

#[test]
fn all_sixteen_descriptors_match_byte_for_byte() {
    let files = [("um", um()), ("km", km()), ("spi", spi()), ("flt", flt())];
    let total: usize = files.iter().map(|(_, f)| f.entries.len()).sum();
    assert_eq!(total, 16, "the fixtures must hold exactly 16 descriptors");
    let cases = all_sixteen();
    assert_eq!(cases.len(), 16);
    for (file, fixture) in &files {
        let mapped = cases.iter().filter(|(f, _, _)| f == file).count();
        assert_eq!(
            mapped,
            fixture.entries.len(),
            "every {file} entry must be mapped to a builder"
        );
    }
    for (file, name, built) in &cases {
        let fixture = &files.iter().find(|(f, _)| f == file).expect("file").1;
        let golden = fixture
            .entries
            .get(*name)
            .unwrap_or_else(|| panic!("{file}:{name} missing"));
        assert_eq!(
            first_difference(built, golden),
            None,
            "{file}:{name}: first differing byte (built len {}, golden len {})",
            built.len(),
            golden.len()
        );
        assert_eq!(built, golden, "{file}:{name}");
    }
}

#[test]
fn descriptor_lengths_per_driver() {
    for (name, bytes) in um().entries {
        assert_eq!(bytes.len(), 516, "{name}");
    }
    for (name, bytes) in km().entries.into_iter().chain(flt().entries) {
        assert_eq!(bytes.len(), 493, "{name}");
    }
    for (name, bytes) in spi().entries {
        assert_eq!(bytes.len(), 501, "{name}");
    }
}

#[test]
fn const_and_static_builders_match_fixtures() {
    assert_eq!(UM_MT2[..], um().entries["AmtPtpMt2ReportDescriptor"][..]);
    assert_eq!(KM_T2[..], km().entries["AmtPtpT2ReportDescriptor"][..]);
    assert_eq!(
        FLT_MT2[..],
        flt().entries["PtpReportDescriptorMagicTrackpad2"][..]
    );
    assert_eq!(SPI_S1[..], spi().entries["S1"][..]);
}

#[test]
fn hidfilter_is_the_usbum_mt2_prefix() {
    assert_eq!(FLT_MT2[..], UM_MT2[..REPORT_DESCRIPTOR_U32_ID_LEN]);
}

#[test]
fn touchscreen_variants_differ_only_at_byte_3() {
    let spi = spi();
    for family in ["S1", "S2", "S3a", "S3b"] {
        let pad = &spi.entries[family];
        let ts = &spi.entries[&format!("{family}T")];
        let diffs: Vec<usize> = (0..pad.len()).filter(|&i| pad[i] != ts[i]).collect();
        assert_eq!(diffs, [3], "{family}");
        assert_eq!((pad[3], ts[3]), (0x05, 0x04), "{family}");
    }
}

#[test]
fn hid_descriptors_match_fixture_lengths() {
    let len = |f: &Fixture, n: &str| u16::try_from(f.entries[n].len()).expect("u16");
    assert_eq!(
        HID_DESCRIPTOR_U32_ID_UMAPP,
        hid_descriptor(len(&um(), "AmtPtp3ReportDescriptor"))
    );
    assert_eq!(
        HID_DESCRIPTOR_U32_ID,
        hid_descriptor(len(&km(), "AmtPtpT2ReportDescriptor"))
    );
    assert_eq!(
        HID_DESCRIPTOR_U32_ID,
        hid_descriptor(len(&flt(), "PtpReportDescriptorMagicTrackpad2"))
    );
    assert_eq!(
        HID_DESCRIPTOR_SPI_UMAPP,
        hid_descriptor(len(&spi(), "S3bT"))
    );
    assert_eq!(
        HID_DESCRIPTOR_U32_ID_UMAPP,
        [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0x04, 0x02]
    );
    assert_eq!(
        HID_DESCRIPTOR_U32_ID,
        [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xed, 0x01]
    );
    assert_eq!(
        HID_DESCRIPTOR_SPI_UMAPP,
        [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xf5, 0x01]
    );
}

#[test]
fn hqa_blob_matches_fixture() {
    let f = hqa();
    assert_eq!(f.entries.len(), 1);
    assert_eq!(f.entries["DEFAULT_PTP_HQA_BLOB"][..], HQA_BLOB[..]);
}

#[test]
fn c_struct_sizes_match_the_c_harness() {
    // `sizeof` lines printed by the C harness for each driver's own headers.
    let um = um().sizes;
    assert_eq!(um["PTP_CONTACT"], PTP_CONTACT_LEN);
    assert_eq!(um["PTP_REPORT"], PTP_REPORT_LEN);
    assert_eq!(um["SEL"], SELECTIVE_REPORT_LEN); // UsbUm: {ReportID, bits}
    assert_eq!(um["UMAPP"], UMAPP_CONF_REPORT_LEN);
    assert_eq!(um["CAPS"], DEVICE_CAPS_REPORT_LEN);
    assert_eq!(um["HQA"], HQA_REPORT_LEN);
    let km = km().sizes;
    assert_eq!(km["PTP_CONTACT"], PTP_CONTACT_LEN);
    assert_eq!(km["PTP_REPORT"], PTP_REPORT_LEN);
    assert_eq!(km["SEL"], SELECTIVE_REPORT_DEVICE_MODE_STRUCT_LEN); // UsbKm: {ReportID, DeviceMode, bits}
    assert_eq!(km["UMAPP"], UMAPP_CONF_REPORT_LEN);
    let spi = spi().sizes;
    assert_eq!(spi["PTP_CONTACT"], SPI_PTP_CONTACT_LEN);
    assert_eq!(spi["PTP_REPORT"], SPI_PTP_REPORT_LEN);
    assert_eq!(spi["SEL"], SELECTIVE_REPORT_DEVICE_MODE_STRUCT_LEN); // SpiKm: {ReportID, DeviceMode, bits}
    assert_eq!(spi["UMAPP"], UMAPP_CONF_REPORT_LEN);
    assert_eq!(INPUT_MODE_REPORT_LEN, 2);
}

// ---- SHA-256 cross-check against the hashes the per-driver specs computed independently ----

fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    let (blocks, tail) = msg.as_chunks::<64>();
    assert!(tail.is_empty());
    for block in blocks {
        let mut w = [0u32; 64];
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
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
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
fn builders_match_spec_sha256() {
    // usbum.md 5.1, spikm.md 5.1, crosscut A7.1.
    let cases: [(&str, Vec<u8>); 15] = [
        (
            "a94733137748c90e10982b8e472b2f82f4e1fac862b32b83ee67e7629aed214c",
            report_descriptor_u32_id_umapp(WELLSPRING3).to_vec(),
        ),
        (
            "3b57036a6f16393ef1be451395fc4eec774f0630d798288a762b5e38133285ca",
            report_descriptor_u32_id_umapp(WELLSPRING5).to_vec(),
        ),
        (
            "83e1b906e55b42957a3a504d36219004ffcb83065ed2fd2d6d035bb14485d007",
            report_descriptor_u32_id_umapp(WELLSPRING6).to_vec(),
        ),
        (
            "9a509e4407b52d68371a8e49b999cc7c55e268c5e4e5d75437c8ba4c34e21703",
            report_descriptor_u32_id_umapp(WELLSPRING7A).to_vec(),
        ),
        (
            "13d94c5f486b201ef4d7919d2f955415560799cc78b12524b874bce6dee7724a",
            report_descriptor_u32_id_umapp(WELLSPRING8).to_vec(),
        ),
        (
            "c803f5c305a07a33d24e518be786ade6338bad844105657e64c582ba7fa13f25",
            UM_MT2.to_vec(),
        ),
        (
            "21a4d11df641c918bbe5b9718c7b054f6d399b21d4296b81bd3e9f85cc617cd3",
            SPI_S1.to_vec(),
        ),
        (
            "868c9d50ac9ca620a578e717daf8dfb215086e83e8bf175851b712a6f809b1aa",
            report_descriptor_spi_umapp(TlcUsage::TouchScreen, SPI_SERIES1).to_vec(),
        ),
        (
            "5dab7a973382bbada6cedb4c47e0f1d7bc2d72d263ed5fd86545f96f2f8c9a3f",
            report_descriptor_spi_umapp(TlcUsage::TouchPad, SPI_SERIES2).to_vec(),
        ),
        (
            "d4d5f0b8146fa01c0421653fff153cee5ff73679576546a1e8378f44cf479bbe",
            report_descriptor_spi_umapp(TlcUsage::TouchScreen, SPI_SERIES2).to_vec(),
        ),
        (
            "fb0db27b1cdf967200ffd68b01005a15ffcebfd0e2786f8dba7bd163b324fcf9",
            report_descriptor_spi_umapp(TlcUsage::TouchPad, SPI_SERIES3_13).to_vec(),
        ),
        (
            "28d6043da39c1cc339b20f49f20059f5bf91c32573ca74c25b9fa865e01582bf",
            report_descriptor_spi_umapp(TlcUsage::TouchScreen, SPI_SERIES3_13).to_vec(),
        ),
        (
            "31cd97cce4b5bfafc67bb490a1b3d2f2cf0d573c03e811181e2e7b424490361b",
            report_descriptor_spi_umapp(TlcUsage::TouchPad, SPI_SERIES3_15).to_vec(),
        ),
        (
            "976e5046059319cf83acb73000237753a9d2e74e52f5f73da65b5f74b16359d7",
            report_descriptor_spi_umapp(TlcUsage::TouchScreen, SPI_SERIES3_15).to_vec(),
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
