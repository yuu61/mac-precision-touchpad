//! Replays `fixtures/descriptors.txt`: static facts, struct layouts, the HQA reference blob, and the
//! descriptor / attribute IOCTLs for 8 device configurations x 4 buffer sizes.

#![cfg(test)]

mod common;

use amtptp_hidfilter_core as core_;
use common::*;
use ptp_hid::feature::{
    DEVICE_CAPS_REPORT_LEN, HQA_BLOB, HQA_REPORT_LEN, INPUT_MODE_REPORT_LEN, SELECTIVE_REPORT_LEN,
};
use ptp_hid::hidport::{HID_DEVICE_ATTRIBUTES_LEN, HidDeviceAttributes};
use ptp_hid::input::{PTP_CONTACT_LEN, PTP_REPORT_LEN, PtpContact, PtpReport};
use ptp_hid::ioctl;

const TOTAL: usize = 99;

/// Value of a `facts` / `layout` key in the Rust port, or `None` for keys that describe private C
/// layout the port does not mirror.
fn port_value(key: &str) -> Option<i128> {
    // Distinct field values to locate offsets in the encoders.
    let attrs = HidDeviceAttributes {
        vendor_id: 0x1111,
        product_id: 0x2222,
        version_number: 0x3333,
    }
    .to_bytes();
    let find = |bytes: &[u8], needle: &[u8]| {
        bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p as i128)
    };
    let mut report = PtpReport {
        scan_time: 0x4455,
        contact_count: 0x66,
        is_button_clicked: 0x77,
        ..PtpReport::EMPTY
    };
    report.contacts[0] = PtpContact {
        confidence: true,
        tip_switch: false,
        contact_id: 0x8899_AABB,
        x: 0xCCDD,
        y: 0xEEF0,
    };
    let rb = report.to_bytes();
    let contact = &rb[1..=PTP_CONTACT_LEN];
    let v: i128 = match key {
        "sizeof_PTP_REPORT" => PTP_REPORT_LEN as i128,
        "sizeof_PTP_CONTACT" => PTP_CONTACT_LEN as i128,
        "sizeof_HID_XFER_PACKET" => core_::HID_XFER_PACKET_LEN as i128,
        "sizeof_HID_DESCRIPTOR" => core_::HID_DESCRIPTOR.len() as i128,
        "sizeof_HID_DEVICE_ATTRIBUTES" => HID_DEVICE_ATTRIBUTES_LEN as i128,
        "sizeof_report_descriptor" => core_::REPORT_DESCRIPTOR.len() as i128,
        "PTP_LIST_POOL_TAG" => i128::from(core_::config::READ_BUFFER_POOL_TAG),
        "REPORT_BUFFER_SIZE" => core_::REPORT_BUFFER_SIZE as i128,
        "HID_XFER_PACKET_SIZE" => core_::HID_XFER_PACKET_SIZE as i128,
        "IOCTL_HID_GET_DEVICE_DESCRIPTOR" => i128::from(ioctl::IOCTL_HID_GET_DEVICE_DESCRIPTOR),
        "IOCTL_HID_GET_REPORT_DESCRIPTOR" => i128::from(ioctl::IOCTL_HID_GET_REPORT_DESCRIPTOR),
        "IOCTL_HID_READ_REPORT" => i128::from(ioctl::IOCTL_HID_READ_REPORT),
        "IOCTL_HID_WRITE_REPORT" => i128::from(ioctl::IOCTL_HID_WRITE_REPORT),
        "IOCTL_HID_GET_STRING" => i128::from(ioctl::IOCTL_HID_GET_STRING),
        "IOCTL_HID_ACTIVATE_DEVICE" => i128::from(ioctl::IOCTL_HID_ACTIVATE_DEVICE),
        "IOCTL_HID_DEACTIVATE_DEVICE" => i128::from(ioctl::IOCTL_HID_DEACTIVATE_DEVICE),
        "IOCTL_HID_GET_DEVICE_ATTRIBUTES" => i128::from(ioctl::IOCTL_HID_GET_DEVICE_ATTRIBUTES),
        "IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST" => {
            i128::from(ioctl::IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST)
        }
        "IOCTL_HID_SET_FEATURE" => i128::from(ioctl::IOCTL_HID_SET_FEATURE),
        "IOCTL_HID_GET_FEATURE" => i128::from(ioctl::IOCTL_HID_GET_FEATURE),
        "IOCTL_UMDF_HID_SET_OUTPUT_REPORT" => i128::from(ioctl::IOCTL_UMDF_HID_SET_OUTPUT_REPORT),
        "IOCTL_UMDF_HID_GET_INPUT_REPORT" => i128::from(ioctl::IOCTL_UMDF_HID_GET_INPUT_REPORT),
        "IOCTL_UMDF_HID_GET_FEATURE" => i128::from(ioctl::IOCTL_UMDF_HID_GET_FEATURE),
        "IOCTL_UMDF_HID_SET_FEATURE" => i128::from(ioctl::IOCTL_UMDF_HID_SET_FEATURE),
        "IOCTL_HID_GET_INPUT_REPORT" => i128::from(ioctl::IOCTL_HID_GET_INPUT_REPORT),
        "IOCTL_HID_SET_OUTPUT_REPORT" => i128::from(ioctl::IOCTL_HID_SET_OUTPUT_REPORT),
        "IOCTL_HID_GET_INDEXED_STRING" => i128::from(ioctl::IOCTL_HID_GET_INDEXED_STRING),
        // HID_OUT_CTL_CODE(102); not in ptp-hid because no driver handles it.
        "IOCTL_GET_PHYSICAL_DESCRIPTOR" => i128::from(ioctl::hid_out_ctl_code(102)),
        "off_HID_XFER_PACKET_reportBuffer" => core_::XFER_REPORT_BUFFER_OFFSET as i128,
        "off_HID_XFER_PACKET_reportBufferLen" => core_::XFER_REPORT_BUFFER_LEN_OFFSET as i128,
        "off_HID_XFER_PACKET_reportId" => core_::XFER_REPORT_ID_OFFSET as i128,
        "off_HID_DEVICE_ATTRIBUTES_Size" => find(&attrs, &32_u32.to_le_bytes())?,
        "off_HID_DEVICE_ATTRIBUTES_VendorID" => find(&attrs, &[0x11, 0x11])?,
        "off_HID_DEVICE_ATTRIBUTES_ProductID" => find(&attrs, &[0x22, 0x22])?,
        "off_HID_DEVICE_ATTRIBUTES_VersionNumber" => find(&attrs, &[0x33, 0x33])?,
        "off_HID_DEVICE_ATTRIBUTES_Reserved" => {
            ptp_hid::hidport::HID_DEVICE_ATTRIBUTES_WRITTEN_LEN as i128
        }
        "off_PTP_REPORT_ReportID" => find(&rb, &[0x05])?,
        "off_PTP_REPORT_Contacts" => find(&rb, &[0x01, 0xBB])?,
        "off_PTP_REPORT_ScanTime" => find(&rb, &[0x55, 0x44])?,
        "off_PTP_REPORT_ContactCount" => find(&rb, &[0x66])?,
        "off_PTP_REPORT_IsButtonClicked" => find(&rb, &[0x77])?,
        "off_PTP_CONTACT_ContactID" => find(contact, &[0xBB, 0xAA, 0x99, 0x88])?,
        "off_PTP_CONTACT_X" => find(contact, &[0xDD, 0xCC])?,
        "off_PTP_CONTACT_Y" => find(contact, &[0xF0, 0xEE])?,
        "sizeof_TRACKPAD_FINGER_TYPE5" => core_::FINGER_RECORD_LEN as i128,
        "sizeof_PTP_DEVICE_CAPS_FEATURE_REPORT" => DEVICE_CAPS_REPORT_LEN as i128,
        "sizeof_PTP_DEVICE_HQA_CERTIFICATION_REPORT" => HQA_REPORT_LEN as i128,
        "sizeof_PTP_DEVICE_INPUT_MODE_REPORT" => INPUT_MODE_REPORT_LEN as i128,
        "sizeof_PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT" => SELECTIVE_REPORT_LEN as i128,
        _ => return None,
    };
    Some(v)
}

/// Keys that describe private C layout (the Rust context layout is free, spec 3.1), or the undocumented
/// structures of the HIDCLASS detour, which the port removed (the data source replaces it).
fn is_unmirrored(key: &str) -> bool {
    is_removed_detour_layout(key) || is_private_layout(key)
}

/// The detour's layout: `DRIVER_EXTENSION`, `IO_CLIENT_EXTENSION`, `HIDCLASS_DRIVER_EXTENSION` and
/// the major-function indexes it patched.
fn is_removed_detour_layout(key: &str) -> bool {
    key.starts_with("off_HIDCLASS_DRIVER_EXTENSION_")
        || matches!(
            key,
            "off_DRIVER_EXTENSION_EXT_IoClientExtension"
                | "off_IO_CLIENT_EXTENSION_ClientIdentificationAddress"
                | "sizeof_HIDCLASS_DRIVER_EXTENSION"
                | "IRP_MJ_INTERNAL_DEVICE_CONTROL"
                | "IRP_MJ_MAXIMUM_FUNCTION"
        )
}

fn is_private_layout(key: &str) -> bool {
    key.starts_with("off_DEVICE_CONTEXT_")
        || key.starts_with("off_WORKER_REQUEST_CONTEXT_")
        || key.starts_with("off_QUEUE_CONTEXT_")
        || matches!(
            key,
            "sizeof_DEVICE_CONTEXT"
                | "sizeof_WORKER_REQUEST_CONTEXT"
                | "sizeof_QUEUE_CONTEXT"
                | "sizeof_BCM5974_PARAM"
        )
}

fn check_keys(r: &Rec, checked: &mut usize, unmirrored: &mut usize) {
    for (k, v) in r.fields.iter().skip(1) {
        if let Some(p) = port_value(k) {
            assert_eq!(p, num(v), "{}: {k}", r.case());
            *checked += 1;
        } else {
            assert!(is_unmirrored(k), "{}: unexpected key {k}", r.case());
            *unmirrored += 1;
        }
    }
}

#[test]
fn replay_descriptors() {
    let recs = load("descriptors.txt");
    assert_eq!(recs.len(), TOTAL);
    let mut tally = Tally::default();
    let (mut checked, mut unmirrored) = (0, 0);
    for r in &recs {
        match r.case() {
            "facts" | "layout" => {
                check_keys(r, &mut checked, &mut unmirrored);
                tally.add("static facts/layout");
            }
            "hqa_blob_reference" => {
                assert_eq!(r.bytes("bytes"), HQA_BLOB);
                tally.add("static facts/layout");
            }
            _ => {
                replay_ioctl(r);
                let s = status(r.get("status"));
                tally.add(if s == 0xC000_0023_u32 as i32 {
                    "framework status (stub model, README 5)"
                } else {
                    "driver-decided"
                });
            }
        }
    }
    // Private C layout: 22 DEVICE_CONTEXT offsets + 2 + 2 context offsets + 4 private sizes + 5
    // HIDCLASS members. The removed detour: 4 offsets and sizes + 2 major-function indexes.
    assert_eq!(unmirrored, 35 + 6, "keys not mirrored (private C layout, removed detour)");
    assert_eq!(
        checked,
        28 + 27 - 6,
        "keys checked against the port (facts + layout)"
    );
    let total = tally.assert(&[
        ("static facts/layout", 3),
        ("driver-decided", 58),
        ("framework status (stub model, README 5)", 38),
    ]);
    assert_eq!(total, TOTAL);
}

fn replay_ioctl(r: &Rec) {
    let state = core_::DeviceState::with(&parse_state(r.get("st_in")));
    let outlen = r.num("outlen") as usize;
    let mut m = Mock::new();
    let req = m.add_upper(Upper::with_output(outlen, 0xCC));
    core_::dispatch_internal_ioctl(&mut m, &state, req, outlen, 0, r.num("ioctl") as u32);
    let (s, info) = m.uppers[req].completed.expect("completed");
    assert_eq!(st(s), r.get("status"), "{}", r.case());
    assert_eq!(info as i128, r.num("info"), "{}", r.case());
    let mut expected = r.bytes("out");
    if r.num("ioctl") as u32 == ioctl::IOCTL_HID_GET_DEVICE_ATTRIBUTES && info == 32 {
        expected[10..32].fill(0);
    }
    assert_eq!(m.uppers[req].buf, expected, "{}", r.case());
    assert_eq!(m.ev, events(r), "{}", r.case());
    assert_eq!(
        state.snapshot(),
        parse_state(r.get("st_in")),
        "state unchanged"
    );
}
