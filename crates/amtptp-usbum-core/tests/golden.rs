//! Focused unit tests for the golden vectors of the porting spec (`specs/usbum.md`) and for the P2
//! decisions that no fixture record covers.

mod common;

use amtptp_usbum_core::config::{
    CONFIG_TABLE, Family, TrackpadType, lookup_config, lookup_config_row,
};
use amtptp_usbum_core::descriptors::{
    HID_DESCRIPTOR, HidRegistry, REPORT_DESCRIPTOR_MT2, REPORT_DESCRIPTOR_WS3,
    REPORT_DESCRIPTOR_WS5, REPORT_DESCRIPTOR_WS6, REPORT_DESCRIPTOR_WS7A, REPORT_DESCRIPTOR_WS8,
};
use amtptp_usbum_core::hid::{
    IoctlOutcome, IoctlRoute, TraceEvent, get_string, route_ioctl, set_feature,
};
use amtptp_usbum_core::input::{build_report, mt2_contact, scan_time, wellspring_contact};
use amtptp_usbum_core::pnp::{d0_entry, d0_exit, prepare_hardware};
use amtptp_usbum_core::state::{DeviceState, UsbDeviceDescriptor};
use amtptp_usbum_core::status::{STATUS_INVALID_DEVICE_STATE, STATUS_SUCCESS};
use amtptp_usbum_core::usb::{wellspring_mode_read_setup, wellspring_mode_write_setup};
use common::{FakeReq, FakeUsb, hex, to_hex};
use ptp_hid::input::PtpContact;

fn cfg(family: Family) -> &'static amtptp_usbum_core::config::Bcm5974Config {
    CONFIG_TABLE
        .iter()
        .find(|c| c.family == family)
        .expect("family")
}

// ------------------------------------------------------------------------------------------------
// Descriptors (spec 5.1): SHA-256 of the six report descriptors and the WS3 byte dump
// ------------------------------------------------------------------------------------------------

/// Minimal SHA-256 (FIPS 180-4) for the spec's descriptor hashes.
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
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
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
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn report_descriptor_hashes() {
    for (bytes, sha) in [
        (
            &REPORT_DESCRIPTOR_WS3,
            "a94733137748c90e10982b8e472b2f82f4e1fac862b32b83ee67e7629aed214c",
        ),
        (
            &REPORT_DESCRIPTOR_WS5,
            "3b57036a6f16393ef1be451395fc4eec774f0630d798288a762b5e38133285ca",
        ),
        (
            &REPORT_DESCRIPTOR_WS6,
            "83e1b906e55b42957a3a504d36219004ffcb83065ed2fd2d6d035bb14485d007",
        ),
        (
            &REPORT_DESCRIPTOR_WS7A,
            "9a509e4407b52d68371a8e49b999cc7c55e268c5e4e5d75437c8ba4c34e21703",
        ),
        (
            &REPORT_DESCRIPTOR_WS8,
            "13d94c5f486b201ef4d7919d2f955415560799cc78b12524b874bce6dee7724a",
        ),
        (
            &REPORT_DESCRIPTOR_MT2,
            "c803f5c305a07a33d24e518be786ade6338bad844105657e64c582ba7fa13f25",
        ),
    ] {
        assert_eq!(bytes.len(), 516);
        assert_eq!(sha256(bytes), sha);
    }
    assert_eq!(
        HID_DESCRIPTOR,
        [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0x04, 0x02]
    );
}

#[test]
fn ws3_report_descriptor_dump() {
    // Spec 5.1, full byte dump of AmtPtp3ReportDescriptor.
    let dump = [
        "05 0d 09 05 a1 01 85 05 09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01",
        "75 20 27 ff ff ff ff 09 51 81 02 05 01 26 9a 25 75 10 55 0e 65 11 09 30 46 1a 04 95 01 81 02 46",
        "f8 02 26 77 1a 09 31 81 02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02 25 01 09 47 09 42 95 02 75 01",
        "81 02 75 01 95 06 81 03 95 01 75 20 27 ff ff ff ff 09 51 81 02 05 01 26 9a 25 75 10 55 0e 65 11",
        "09 30 46 1a 04 95 01 81 02 46 f8 02 26 77 1a 09 31 81 02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02",
        "25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01 75 20 27 ff ff ff ff 09 51 81 02 05",
        "01 26 9a 25 75 10 55 0e 65 11 09 30 46 1a 04 95 01 81 02 46 f8 02 26 77 1a 09 31 81 02 c0 05 0d",
        "09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01 75 20 27 ff ff ff ff 09",
        "51 81 02 05 01 26 9a 25 75 10 55 0e 65 11 09 30 46 1a 04 95 01 81 02 46 f8 02 26 77 1a 09 31 81",
        "02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03",
        "95 01 75 20 27 ff ff ff ff 09 51 81 02 05 01 26 9a 25 75 10 55 0e 65 11 09 30 46 1a 04 95 01 81",
        "02 46 f8 02 26 77 1a 09 31 81 02 c0 05 0d 55 0c 66 01 10 47 ff ff 00 00 27 ff ff 00 00 09 56 81",
        "02 09 54 25 7f 75 08 81 02 05 09 09 01 25 01 75 01 81 02 95 07 81 03 05 0d 85 07 09 55 09 59 15",
        "00 26 ff 00 75 08 95 02 b1 02 06 00 ff 85 08 09 c5 15 00 26 ff 00 75 08 96 00 01 b1 02 c0 05 0d",
        "09 0e a1 01 85 04 09 22 a1 02 09 52 15 00 25 10 75 08 95 01 b1 02 c0 a1 00 85 06 09 57 09 58 75",
        "01 95 02 25 01 b1 02 95 06 b1 03 c0 c0 06 00 ff 09 01 a1 01 85 09 09 01 15 00 26 ff 00 75 08 95",
        "03 b1 02 c0",
    ]
    .concat();
    let compact: String = dump.chars().filter(char::is_ascii_hexdigit).collect();
    assert_eq!(to_hex(&REPORT_DESCRIPTOR_WS3), compact);
}

#[test]
fn pid_to_descriptor_switch() {
    // Spec 8.3: the descriptor switch is not the config table.
    for pid in [0x0223u16, 0x0230, 0x023f, 0x0242, 0x027d, 0x0000, 0x0266] {
        assert_eq!(HidRegistry::for_pid(pid), None, "{pid:04x}");
    }
    assert_eq!(HidRegistry::for_pid(0x0254), Some(HidRegistry::Ws5)); // WS5A -> AmtPtp5 (B18)
    assert_eq!(HidRegistry::for_pid(0x0272), Some(HidRegistry::Ws8)); // WS9 -> AmtPtp8 (B18)
    assert_eq!(HidRegistry::for_pid(0x0292), Some(HidRegistry::Ws8)); // WS8 JIS
    assert_eq!(HidRegistry::for_pid(0x024b), Some(HidRegistry::Ws6)); // WS6A
    assert_eq!(HidRegistry::for_pid(0x025b), Some(HidRegistry::Ws7a)); // WS7A
    assert_eq!(HidRegistry::for_pid(0x0265), Some(HidRegistry::Mt2));
}

// ------------------------------------------------------------------------------------------------
// Config table (spec 8.1) and the bounded lookup (B3)
// ------------------------------------------------------------------------------------------------

#[test]
fn config_lookup_is_bounded() {
    assert_eq!(CONFIG_TABLE.len(), 15);
    assert_eq!(lookup_config_row(0x0265), Some(13));
    assert_eq!(lookup_config_row(0x027d), Some(14));
    for pid in [0u16, 1, 0x0222, 0x0275, 0x05ac, 0x8910, 0xffff] {
        assert_eq!(lookup_config(pid), None, "{pid:04x}");
    }
    let lengths: Vec<usize> = [
        TrackpadType::Type1,
        TrackpadType::Type2,
        TrackpadType::Type3,
        TrackpadType::Type4,
        TrackpadType::Type5,
    ]
    .iter()
    .map(|t| t.reader_transfer_length())
    .collect();
    assert_eq!(lengths, [474, 478, 486, 526, 156]);
}

#[test]
fn unknown_pid_fails_prepare_hardware() {
    // Excluded fixture case 1 (B3): the C lookup walks past the table. The port returns
    // STATUS_INVALID_DEVICE_STATE with the DeviceNotFoundInRegistry event and DeviceInfo = NULL.
    let state = DeviceState::new();
    let mut usb = FakeUsb {
        descriptor: UsbDeviceDescriptor {
            id_vendor: 0x05ac,
            id_product: 0x0275,
            ..UsbDeviceDescriptor::default()
        },
        ..FakeUsb::default()
    };
    let outcome = prepare_hardware(&state, &mut usb);
    assert_eq!(outcome.status, STATUS_INVALID_DEVICE_STATE);
    assert_eq!(
        outcome.event,
        Some(TraceEvent::DeviceNotFound {
            id_product: 0x0275,
            id_vendor: 0x05ac
        })
    );
    assert_eq!(state.config(), None);
    assert_eq!(usb.calls, ["WdfUsbTargetDeviceCreate"]);
}

#[test]
fn reader_is_configured_once() {
    // B16 (P2): a PnP stop/start re-runs PrepareHardware; the reader is configured only the first time.
    let state = DeviceState::new();
    let mut usb = FakeUsb::default();
    usb.descriptor.id_product = 0x0265;
    let first = prepare_hardware(&state, &mut usb);
    assert_eq!(
        (first.status, first.configured_reader),
        (STATUS_SUCCESS, true)
    );
    state.set_surface_report_on(false);
    usb.calls.clear();
    usb.reader_status = STATUS_INVALID_DEVICE_STATE; // what WDF would answer to a second configuration
    let second = prepare_hardware(&state, &mut usb);
    assert_eq!(
        (second.status, second.configured_reader),
        (STATUS_SUCCESS, false)
    );
    assert_eq!(
        usb.calls,
        [
            "GetInterface(0)",
            "GetConfiguredPipe(0)",
            "SetNoMaxPktCheck(0)"
        ]
    );
    assert!(state.is_surface_report_on());
}

// ------------------------------------------------------------------------------------------------
// Setup packets (spec 6.3)
// ------------------------------------------------------------------------------------------------

#[test]
fn setup_packets() {
    for (family, get, set) in [
        (Family::Ws3, "a101000300000400", "2109000300000400"),
        (Family::Ws1, "a101000300000400", "2109000300000400"),
        (Family::Ws9, "a101020302000400", "2109020302000400"),
        (Family::T2, "a101020302000400", "2109020302000400"),
        (Family::Mt2, "a101020301000400", "2109020301000400"),
    ] {
        let c = cfg(family);
        assert_eq!(
            to_hex(&wellspring_mode_read_setup(c).to_bytes()),
            get,
            "{family:?}"
        );
        assert_eq!(
            to_hex(&wellspring_mode_write_setup(c).to_bytes()),
            set,
            "{family:?}"
        );
    }
}

#[test]
fn type4_short_response_sends_zero_padding() {
    // B1 (P2): the TYPE4/5 buffer is 4 zeroed bytes, so a 1-byte GET answer leaves bytes 1..3 zero
    // before the mode byte is patched in.
    let state = DeviceState::new();
    state.set_config_row(lookup_config_row(0x0272));
    state.set_button_report_on(true); // as after PrepareHardware: D0Entry switches the mode on
    let mut usb = FakeUsb {
        reg: [0x02, 0x00, 0x5a, 0xa5, 0, 0, 0, 0],
        resp: 1,
        ..FakeUsb::default()
    };
    let outcome = d0_entry(&state, &mut usb, || 5);
    assert_eq!(outcome.status, STATUS_SUCCESS);
    assert_eq!(outcome.mode_switch_status, Some(STATUS_SUCCESS));
    assert_eq!(
        usb.wire,
        [
            "in/a101020302000400/02/00000000",
            "out/2109020302000400/02010000/00000000"
        ]
    );
    assert_eq!(state.perf_counter(), 5);
    assert_eq!(usb.calls, ["IoTargetStart(pipe0)=0x00000000"]);
    assert_eq!(d0_exit(&state, &mut usb), STATUS_SUCCESS);
    assert!(!state.is_wellspring_mode_on());
}

// ------------------------------------------------------------------------------------------------
// Input decoding (spec 7.5, 7.6) and ScanTime (B12)
// ------------------------------------------------------------------------------------------------

#[test]
fn mt2_golden_vectors() {
    let c = cfg(Family::Mt2);
    // finger bytes, X, Y, ID, Tip, Conf
    for (bytes, x, y, id, tip, conf) in [
        ("000000001020000013", 3678, 2479, 3, true, true),
        ("01000000 10ac000002", 3679, 2478, 2, true, true),
        ("34125600 00ad00005f", 146, 1790, 15, false, false),
        ("ffffff03 304000 0001", 3677, 2479, 1, true, true),
        ("62f19a02 2030000004", 0, 5335, 4, true, true),
        ("00e0ff01 0101000000", 3678, 0, 0, true, true),
        // INT_MIN negation: the literal C expression gives y = -4096 -> Y = 0 (closed form: 6575).
        ("00000002 0101000000", 3678, 0, 0, true, true),
    ] {
        let raw = hex(&bytes.replace(' ', ""));
        let finger: [u8; 9] = raw.try_into().expect("9 bytes");
        let contact = mt2_contact(c, &finger);
        assert_eq!(
            contact,
            PtpContact {
                confidence: conf,
                tip_switch: tip,
                contact_id: id,
                x,
                y
            },
            "{bytes}"
        );
    }
}

#[test]
fn wellspring_golden_vectors() {
    let c = cfg(Family::Ws3);
    for (abs_x, abs_y, major, minor, x, y, tip, conf) in [
        (0i16, 0i16, 100i16, 1i16, 4460u16, 6700u16, true, true),
        (-4460, 6700, 99, 0, 0, 0, false, false),
        (-5000, 7000, -5, -1, 0, 0, false, false),
        (5166, -75, 150, 30, 9626, 6775, true, true),
        (32767, -32768, 16384, 2, 37227, 39468, true, true),
    ] {
        let mut finger = [0u8; 28];
        finger[2..4].copy_from_slice(&abs_x.to_le_bytes());
        finger[4..6].copy_from_slice(&abs_y.to_le_bytes());
        finger[16..18].copy_from_slice(&major.to_le_bytes());
        finger[18..20].copy_from_slice(&minor.to_le_bytes());
        let contact = wellspring_contact(c, &finger, 3);
        assert_eq!(
            contact,
            PtpContact {
                confidence: conf,
                tip_switch: tip,
                contact_id: 3,
                x,
                y
            },
            "{abs_x} {abs_y} {major} {minor}"
        );
    }
}

#[test]
fn type4_frame_vector() {
    // Spec 7.6 frame-level vector (WS9, 106 bytes, two fingers, button), ScanTime forced to 0xFF.
    let mut packet = [0u8; 106];
    packet[31] = 1;
    for (offset, value) in [
        (50, 100i16),
        (52, 200),
        (64, 120),
        (66, 40),
        (80, -5000),
        (82, 7000),
        (94, 50),
        (96, 0),
    ] {
        packet[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    let report = build_report(cfg(Family::Ws9), &packet, true, true, 0xff);
    let expected = concat!(
        "05 03 00 00 00 00 40 13 cb 19 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00",
        "00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 ff 00 02 01"
    )
    .replace(' ', "");
    assert_eq!(to_hex(&report.to_bytes()), expected);
}

#[test]
fn report_with_surface_off_is_zero() {
    // B13 (P2): with the surface off, C sends garbage contacts; the port sends zeros.
    let packet = [0xffu8; 30 + 28];
    let report = build_report(cfg(Family::Ws3), &packet, false, false, 7);
    let mut expected = [0u8; 50];
    expected[0] = 0x05;
    expected[46] = 7;
    assert_eq!(report.to_bytes(), expected);
}

#[test]
fn scan_time_matches_c() {
    for (now, base, expected) in [
        (0i64, 0i64, 0u16),
        (99, 0, 0),
        (100, 0, 1),
        (25599, 0, 0xff),
        (25600, 0, 0xff),
        (900, 1000, 0xffff),
        (901, 1000, 0),
        (0, 1000, 0xfff6),
        (-25700, 0, 0xfeff),
        (-25500, 0, 0xff01),
        (0, i64::MAX, 0xb852),
        (123_469_134, 123_456_789, 0x7b),
        // Overflowing difference wraps instead of trapping (excluded fixture case 7).
        (i64::MAX, i64::MIN, 0),
        (i64::MIN, i64::MAX, 0),
    ] {
        assert_eq!(scan_time(now, base), expected, "{now} {base}");
    }
}

// ------------------------------------------------------------------------------------------------
// SET_FEATURE with short reports (B8, excluded fixture case 2) and GET_STRING edge cases (B7)
// ------------------------------------------------------------------------------------------------

fn started(pid: u16) -> DeviceState {
    let state = DeviceState::new();
    state.set_device_descriptor(&UsbDeviceDescriptor {
        id_vendor: 0x05ac,
        id_product: pid,
        bcd_device: 0x0105,
        i_manufacturer: 1,
        i_product: 2,
        i_serial_number: 3,
    });
    state.set_config_row(lookup_config_row(pid));
    state
}

#[test]
fn short_set_feature_reads_missing_bytes_as_zero() {
    // ID 6 with a 1-byte report: byte 1 is read as 0 -> both selective-report flags off.
    let state = started(0x0236);
    state.set_button_report_on(true);
    state.set_surface_report_on(true);
    let mut usb = FakeUsb::default();
    let mut req = FakeReq::new(vec![0x06], 6, 0xa5);
    let outcome = set_feature(&state, &mut req, &mut usb);
    assert_eq!(outcome, IoctlOutcome::success(2));
    assert!(!state.is_button_report_on() && !state.is_surface_report_on());

    // ID 9 with 2 bytes: (byte1, 0, 0).
    let mut req = FakeReq::new(vec![0x09, 7], 9, 0xa5);
    assert_eq!(
        set_feature(&state, &mut req, &mut usb),
        IoctlOutcome::success(4)
    );
    assert_eq!(state.qual_levels(), [7, 0, 0]);
    // ID 9 with 3 bytes: (byte1, byte2, 0).
    let mut req = FakeReq::new(vec![0x09, 7, 8], 9, 0xa5);
    assert_eq!(
        set_feature(&state, &mut req, &mut usb),
        IoctlOutcome::success(4)
    );
    assert_eq!(state.qual_levels(), [7, 8, 0]);

    // ID 4 with a 1-byte report: mode 0 (mouse) -> Wellspring mode is switched off.
    usb.reg = [0x01, 1, 2, 3, 4, 5, 6, 7];
    state.set_wellspring_mode_on(true);
    let mut req = FakeReq::new(vec![0x04], 4, 0xa5);
    assert_eq!(
        set_feature(&state, &mut req, &mut usb),
        IoctlOutcome::success(2)
    );
    assert!(!state.is_wellspring_mode_on());
    assert_eq!(usb.reg[0], 0x08);
    assert!(!req.out_accessed);
}

#[test]
fn set_feature_input_mode_other_values_only_read() {
    // Modes other than 0/3 still issue the GET, then report Information 2.
    let state = started(0x0265);
    let mut usb = FakeUsb {
        reg: [0x02, 0x01, 0x5a, 0xa5, 0, 0, 0, 0],
        ..FakeUsb::default()
    };
    for mode in [1u8, 2, 4, 16, 255] {
        usb.wire.clear();
        let mut req = FakeReq::new(vec![0x04, mode], 4, 0xa5);
        assert_eq!(
            set_feature(&state, &mut req, &mut usb),
            IoctlOutcome::success(2)
        );
        assert_eq!(usb.wire, ["in/a101020301000400/02015aa5/00000000"]);
    }
}

#[test]
fn get_string_edge_cases() {
    struct Strings(Vec<u8>);
    impl amtptp_usbum_core::io::UsbStringSource for Strings {
        type String = Vec<u8>;
        fn alloc_and_query_string(&mut self, _: u8, _: u16) -> Result<Vec<u8>, i32> {
            Ok(self.0.clone())
        }
    }
    let state = started(0x0265);
    // Empty device string (excluded fixture case 4): nothing is written, no crash.
    let mut req = FakeReq::new(vec![0x0e, 0, 0x09, 0x04], 16, 0xa5);
    let outcome = get_string(&state, &mut req, &mut Strings(Vec::new()));
    assert_eq!(outcome, IoctlOutcome::success(16));
    assert!(req.out.iter().all(|&b| b == 0xa5));
    // A string as long as the buffer and longer than 8 bytes (excluded fixture case 3): copied whole.
    let text = common::FakeStrings::utf16("Apple Inc.");
    let mut req = FakeReq::new(vec![0x0f, 0, 0x09, 0x04], text.len(), 0xa5);
    let outcome = get_string(&state, &mut req, &mut Strings(text.clone()));
    assert_eq!(outcome, IoctlOutcome::success(text.len()));
    assert_eq!(req.out, text);
}

#[test]
fn ioctl_routes() {
    for (code, route) in [
        (0x000b_0003u32, IoctlRoute::GetDeviceDescriptor),
        (0x000b_0007, IoctlRoute::GetReportDescriptor),
        (0x000b_000b, IoctlRoute::ReadReport),
        (0x000b_000f, IoctlRoute::NotSupported),
        (0x000b_0013, IoctlRoute::GetString),
        (0x000b_001f, IoctlRoute::NotSupported),
        (0x000b_0023, IoctlRoute::NotSupported),
        (0x000b_0027, IoctlRoute::GetDeviceAttributes),
        (0x000b_002b, IoctlRoute::NotSupported),
        (0x000b_0053, IoctlRoute::SetFeature),
        (0x000b_0057, IoctlRoute::GetFeature),
        (0x000b_005b, IoctlRoute::NotSupported),
        (0x000b_005f, IoctlRoute::NotSupported),
        (0x000b_0063, IoctlRoute::NotSupported),
    ] {
        assert_eq!(route_ioctl(code), route, "{code:08x}");
    }
}
