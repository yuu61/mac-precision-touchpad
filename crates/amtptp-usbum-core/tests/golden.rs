//! Focused unit tests for the golden vectors of the porting spec (`specs/usbum.md`) and for the P2
//! decisions that no fixture record covers.

#![cfg(test)]

use core::fmt::Write as _;
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
use amtptp_usbum_core::input::{ReadCompleteOutcome, build_report, mt2_contact, on_read_complete, wellspring_contact};
use amtptp_usbum_core::pnp::{d0_entry, d0_exit, prepare_hardware};
use amtptp_usbum_core::state::{DeviceState, UsbDeviceDescriptor};
use amtptp_usbum_core::status::{STATUS_INVALID_DEVICE_STATE, STATUS_SUCCESS};
use amtptp_usbum_core::usb::{wellspring_mode_read_setup, wellspring_mode_write_setup};
use amtptp_usbum_core::io::{PipeInfo, PipeType};
use common::{FakeQueue, FakeReq, FakeUsb, hex, to_hex};
use ptp_hid::input::PtpContact;

fn cfg(family: Family) -> &'static amtptp_usbum_core::config::WellspringDevice {
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
    // Spec 8.3: not every config row has a descriptor.
    for pid in [0x0223_u16, 0x0230, 0x023f, 0x0242, 0x027d, 0x0000, 0x0266] {
        assert_eq!(HidRegistry::for_pid(pid), None, "{pid:04x}");
    }
    assert_eq!(HidRegistry::for_pid(0x0254), Some(HidRegistry::Ws5)); // WS5A -> AmtPtp5 (B18)
    assert_eq!(HidRegistry::for_pid(0x0272), Some(HidRegistry::Ws8)); // WS9 -> AmtPtp8 (B18)
    assert_eq!(HidRegistry::for_pid(0x0292), Some(HidRegistry::Ws8)); // WS8 JIS
    assert_eq!(HidRegistry::for_pid(0x024b), Some(HidRegistry::Ws6)); // WS6A
    assert_eq!(HidRegistry::for_pid(0x025b), Some(HidRegistry::Ws7a)); // WS7A
    assert_eq!(HidRegistry::for_pid(0x0265), Some(HidRegistry::Mt2));
}

/// The C PID switch of `AmtPtpGetHidDescriptor` / `AmtPtpGetReportDescriptor`, kept as the oracle of the
/// family-based selection.
const fn c_descriptor_switch(pid: u16) -> Option<HidRegistry> {
    match pid {
        0x0236..=0x0238 => Some(HidRegistry::Ws3),
        0x0245..=0x0247 | 0x0252..=0x0254 => Some(HidRegistry::Ws5),
        0x0249..=0x024e => Some(HidRegistry::Ws6),
        0x0262..=0x0264 | 0x0259..=0x025b => Some(HidRegistry::Ws7a),
        0x0290..=0x0292 | 0x0272..=0x0274 => Some(HidRegistry::Ws8),
        0x0265 => Some(HidRegistry::Mt2),
        _ => None,
    }
}

#[test]
fn family_based_descriptor_selection_equals_the_c_switch() {
    for pid in 0..=u16::MAX {
        assert_eq!(HidRegistry::for_pid(pid), c_descriptor_switch(pid), "{pid:04x}");
    }
}

// ------------------------------------------------------------------------------------------------
// Config table (spec 8.1) and the bounded lookup (B3)
// ------------------------------------------------------------------------------------------------

#[test]
fn config_lookup_is_bounded() {
    assert_eq!(CONFIG_TABLE.len(), 15);
    assert_eq!(lookup_config_row(0x0265), Some(13));
    assert_eq!(lookup_config_row(0x027d), Some(14));
    for pid in [0_u16, 1, 0x0222, 0x0275, 0x05ac, 0x8910, 0xffff] {
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

/// A Magic Trackpad 2 whose interface 0 has the given pipes.
fn mt2_with_pipes(pipes: Vec<PipeInfo>) -> (DeviceState, FakeUsb) {
    let mut usb = FakeUsb::default();
    usb.descriptor.id_product = 0x0265;
    usb.set_pipes(Some(pipes));
    (DeviceState::new(), usb)
}

#[test]
fn reader_goes_on_the_interrupt_in_pipe_after_an_out_pipe() {
    // B21 (P3): interrupt OUT 0x03, then interrupt IN 0x83.
    let (state, mut usb) = mt2_with_pipes(vec![
        common::pipe(PipeType::Interrupt, 0x03),
        common::interrupt_in(0x83),
    ]);
    let outcome = prepare_hardware(&state, &mut usb);
    assert_eq!(outcome.status, STATUS_SUCCESS);
    assert_eq!(usb.interrupt_pipe, Some(1));
    assert_eq!(usb.calls.last().map(String::as_str), Some("ConfigContinuousReader(1)=0x00000000"));
}

#[test]
fn interrupt_out_pipe_alone_fails_prepare_hardware() {
    let (state, mut usb) = mt2_with_pipes(vec![
        common::pipe(PipeType::Bulk, 0x81),
        common::pipe(PipeType::Interrupt, 0x03),
    ]);
    let outcome = prepare_hardware(&state, &mut usb);
    assert_eq!(outcome.status, STATUS_INVALID_DEVICE_STATE);
    assert_eq!(usb.interrupt_pipe, None);
    assert!(!usb.calls.iter().any(|c| c.starts_with("ConfigContinuousReader")));
    assert!(!state.reader_configured());
}

#[test]
fn reader_failure_after_the_in_pipe_fails_prepare_hardware() {
    const STATUS_INSUFFICIENT_RESOURCES: i32 = 0xC000_009A_u32 as i32;
    let (state, mut usb) = mt2_with_pipes(vec![
        common::pipe(PipeType::Interrupt, 0x03),
        common::interrupt_in(0x83),
    ]);
    usb.reader_status = STATUS_INSUFFICIENT_RESOURCES;
    let outcome = prepare_hardware(&state, &mut usb);
    assert_eq!(outcome.status, STATUS_INSUFFICIENT_RESOURCES);
    assert_eq!(usb.interrupt_pipe, Some(1));
    assert!(!state.reader_configured());
    assert!(!state.is_surface_report_on());
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
    state.set_button_report_on(true); // as after PrepareHardware
    let mut usb = FakeUsb {
        reg: [0x02, 0x00, 0x5a, 0xa5, 0, 0, 0, 0],
        resp: 1,
        ..FakeUsb::default()
    };
    let outcome = d0_entry(&state, &mut usb, || 5);
    assert_eq!(outcome.status, STATUS_SUCCESS);
    assert_eq!(outcome.mode_switch_status, STATUS_SUCCESS);
    assert_eq!(
        usb.wire,
        [
            "in/a101020302000400/02/00000000",
            "out/2109020302000400/02010000/00000000"
        ]
    );
    assert_eq!(state.qpc_frequency(), 5);
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
        (0_i16, 0_i16, 100_i16, 1_i16, 4460_u16, 6700_u16, true, true),
        (-4460, 6700, 99, 0, 0, 0, false, false),
        (-5000, 7000, -5, -1, 0, 0, false, false),
        (5166, -75, 150, 30, 9626, 6775, true, true),
        (0x7FFF, -0x8000, 0x4000, 2, 37227, 39468, true, true),
    ] {
        let mut finger = [0_u8; 28];
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
    let mut packet = [0_u8; 106];
    packet[31] = 1;
    for (offset, value) in [
        (50, 100_i16),
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
    let packet = [0xff_u8; 30 + 28];
    let report = build_report(cfg(Family::Ws3), &packet, false, false, 7);
    let mut expected = [0_u8; 50];
    expected[0] = 0x05;
    expected[46] = 7;
    assert_eq!(report.to_bytes(), expected);
}

#[test]
fn input_scan_time_uses_the_cached_qpc_frequency() {
    let state = started(0x0272);
    let mut usb = FakeUsb::default();
    let frequency = 24_000_000;
    assert_eq!(d0_entry(&state, &mut usb, || frequency).status, STATUS_SUCCESS);
    state.set_wellspring_mode_on(true);
    let packet = [0_u8; 46];
    let mut queue = FakeQueue::with_lengths(&[50; 3]);

    for (counter, expected) in [(200_000, 0_u16), (202_400, 1), (819_200, 258)] {
        assert_eq!(
            on_read_complete(&state, &packet, &mut queue, &mut usb, || counter),
            ReadCompleteOutcome::Completed
        );
        let (request, _, _) = queue.completed.last().expect("report completed");
        assert_eq!(request.out[46..48], expected.to_le_bytes());
    }
}

/// `SET_FEATURE` 0x06 `[06, bits]` (bit 0 Surface Switch, bit 1 Button Switch) for each combination,
/// then a WS3 frame with one contact and the button down.
#[test]
fn selective_reporting_shapes_the_input_report() {
    let state = started(0x0236);
    let mut usb = FakeUsb::default();
    assert_eq!(d0_entry(&state, &mut usb, || 10_000_000).status, STATUS_SUCCESS);
    state.set_wellspring_mode_on(true);
    let mut packet = vec![0_u8; 30 + 28];
    packet[15] = 1; // button
    packet[30 + 2..30 + 4].copy_from_slice(&1000_i16.to_le_bytes()); // X
    packet[30 + 16..30 + 18].copy_from_slice(&200_i16.to_le_bytes()); // touch major: tip
    for (bits, contacts, button) in [(0_u8, 0_u8, 0_u8), (1, 1, 0), (2, 0, 1), (3, 1, 1)] {
        let mut req = FakeReq::new(vec![0x06, bits], 6, 0xa5);
        assert_eq!(
            set_feature(&state, &mut req, &mut usb),
            IoctlOutcome::success(2)
        );
        let mut queue = FakeQueue::with_lengths(&[50]);
        assert_eq!(
            on_read_complete(&state, &packet, &mut queue, &mut usb, || 0),
            ReadCompleteOutcome::Completed
        );
        let (request, _, _) = queue.completed.last().expect("report completed");
        assert_eq!(
            (request.out[48], request.out[49]),
            (contacts, button),
            "switch byte {bits}"
        );
    }
}

/// UM-B10 (P3): Windows turns both selective-reporting switches off, the device is suspended and resumed.
/// `D0Entry` still switches the trackpad into multitouch (Wellspring) mode, so turning the switches back
/// on brings the contacts back without an input-mode report.
#[test]
fn d0_entry_switches_the_mode_after_selective_reporting_off() {
    let state = started(0x0236);
    let mut usb = FakeUsb {
        reg: [0x01, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07],
        resp: 4,
        ..FakeUsb::default()
    };
    state.set_wellspring_mode_on(true);
    let mut req = FakeReq::new(vec![0x06, 0x00], 6, 0xa5);
    assert_eq!(set_feature(&state, &mut req, &mut usb), IoctlOutcome::success(2));
    assert!(!state.is_button_report_on() && !state.is_surface_report_on());

    assert_eq!(d0_exit(&state, &mut usb), STATUS_SUCCESS);
    assert!(!state.is_wellspring_mode_on());
    usb.wire.clear();
    let outcome = d0_entry(&state, &mut usb, || 10_000_000);
    assert_eq!(outcome.mode_switch_status, STATUS_SUCCESS);
    assert!(state.is_wellspring_mode_on());
    assert_eq!(
        usb.wire_string(),
        "in/a101000300000400/08010203/00000000,out/2109000300000400/01010203/00000000"
    );

    let mut req = FakeReq::new(vec![0x06, 0x03], 6, 0xa5);
    assert_eq!(set_feature(&state, &mut req, &mut usb), IoctlOutcome::success(2));
    let mut packet = vec![0_u8; 30 + 28];
    packet[30 + 16..30 + 18].copy_from_slice(&200_i16.to_le_bytes());
    let mut queue = FakeQueue::with_lengths(&[50]);
    assert_eq!(
        on_read_complete(&state, &packet, &mut queue, &mut usb, || 0),
        ReadCompleteOutcome::Completed
    );
    let (request, _, _) = queue.completed.last().expect("report completed");
    assert_eq!(request.out[48], 1, "one contact");
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
    // ID 6 with a 1-byte report: no switch byte, so both selective-report flags stay as they are (C read
    // the missing byte as 0 and turned both off).
    let state = started(0x0236);
    state.set_button_report_on(true);
    state.set_surface_report_on(true);
    let mut usb = FakeUsb::default();
    let mut req = FakeReq::new(vec![0x06], 6, 0xa5);
    let outcome = set_feature(&state, &mut req, &mut usb);
    assert_eq!(outcome, IoctlOutcome::success(2));
    assert!(state.is_button_report_on() && state.is_surface_report_on());

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
    for mode in [1_u8, 2, 4, 16, 255] {
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
        (0x000b_0003_u32, IoctlRoute::GetDeviceDescriptor),
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
