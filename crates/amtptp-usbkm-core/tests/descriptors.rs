//! Replays `tests/fixtures/descriptors.txt` (173 records): descriptor bytes, layouts, constants, IOCTL
//! codes, the config table, PID → row selection and the descriptor/attribute IOCTLs.

#![cfg(test)]

mod common;

use amtptp_usbkm_core::config::{CONFIG_TABLE, T2Device, config_for_pid, reader_transfer_length};
use amtptp_usbkm_core::feature::HID_XFER_PACKET_LEN;
use amtptp_usbkm_core::hid::{
    HID_DESCRIPTOR, HID_DEVICE_ATTRIBUTES_LEN, InternalIoctl, REPORT_DESCRIPTOR,
    REPORT_DESCRIPTOR_LEN, route_internal_ioctl,
};
use amtptp_usbkm_core::input::TRACKPAD_FINGER_LEN;
use amtptp_usbkm_core::lifecycle::DEVICE_INTERFACE_GUID;
use amtptp_usbkm_core::wellspring::{POOL_TAG_PTP_CONTROL, TRANSFER_BUFFER_POOL_TYPE};
use common::{Record, Tally, hex, load, num, replay_descriptor_ioctl};
use ptp_hid::feature::{
    DEVICE_CAPS_REPORT_LEN, HQA_REPORT_LEN, INPUT_MODE_REPORT_LEN,
    SELECTIVE_REPORT_DEVICE_MODE_STRUCT_LEN,
};
use ptp_hid::hidport::HID_DESCRIPTOR_LEN;
use ptp_hid::input::{PTP_CONTACT_LEN, PTP_REPORT_LEN};

fn check_row(r: &Record, row: &T2Device) {
    let id = r.id();
    let int = |k: &str| num(r.req(k)) as i32;
    let format = row.profile.tp_type;
    let setup = amtptp_usbkm_core::wellspring::wellspring_read_setup(row);
    assert_eq!(i32::from(row.identification), int("identification"), "{id}");
    assert_eq!(format as i32, int("tp_type"), "{id}");
    assert_eq!(format.header(), int("tp_header"), "{id}");
    assert_eq!(format.button(), int("tp_button"), "{id}");
    assert_eq!(format.finger_size(), int("tp_fsize"), "{id}");
    assert_eq!(format.delta(), int("tp_delta"), "{id}");
    assert_eq!(i32::from(setup.w_value), int("um_req_val"), "{id}");
    assert_eq!(i32::from(setup.w_index), int("um_req_idx"), "{id}");
    for (axis, key) in [(row.profile.x, "x"), (row.profile.y, "y")] {
        let (_, bounds) = r.req(key).split_once(',').expect("axis limits");
        assert_eq!(bounds, format!("{},{}", axis.min, axis.max), "{id} {key}");
    }
}

/// The IOCTL code each named constant must have, from ptp-hid (None = not defined there; the record is
/// then checked for routing only).
fn ptp_hid_code(name: &str) -> Option<u32> {
    use ptp_hid::ioctl::*;
    Some(match name {
        "IOCTL_HID_GET_DEVICE_DESCRIPTOR" => IOCTL_HID_GET_DEVICE_DESCRIPTOR,
        "IOCTL_HID_GET_REPORT_DESCRIPTOR" => IOCTL_HID_GET_REPORT_DESCRIPTOR,
        "IOCTL_HID_READ_REPORT" => IOCTL_HID_READ_REPORT,
        "IOCTL_HID_WRITE_REPORT" => IOCTL_HID_WRITE_REPORT,
        "IOCTL_HID_GET_STRING" => IOCTL_HID_GET_STRING,
        "IOCTL_HID_ACTIVATE_DEVICE" => IOCTL_HID_ACTIVATE_DEVICE,
        "IOCTL_HID_DEACTIVATE_DEVICE" => IOCTL_HID_DEACTIVATE_DEVICE,
        "IOCTL_HID_GET_DEVICE_ATTRIBUTES" => IOCTL_HID_GET_DEVICE_ATTRIBUTES,
        "IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST" => IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST,
        "IOCTL_UMDF_HID_SET_FEATURE" => IOCTL_UMDF_HID_SET_FEATURE,
        "IOCTL_UMDF_HID_GET_FEATURE" => IOCTL_UMDF_HID_GET_FEATURE,
        "IOCTL_UMDF_HID_SET_OUTPUT_REPORT" => IOCTL_UMDF_HID_SET_OUTPUT_REPORT,
        "IOCTL_UMDF_HID_GET_INPUT_REPORT" => IOCTL_UMDF_HID_GET_INPUT_REPORT,
        "IOCTL_HID_SET_FEATURE" => IOCTL_HID_SET_FEATURE,
        "IOCTL_HID_GET_FEATURE" => IOCTL_HID_GET_FEATURE,
        "IOCTL_HID_SET_OUTPUT_REPORT" => IOCTL_HID_SET_OUTPUT_REPORT,
        "IOCTL_HID_GET_INPUT_REPORT" => IOCTL_HID_GET_INPUT_REPORT,
        "IOCTL_HID_GET_INDEXED_STRING" => IOCTL_HID_GET_INDEXED_STRING,
        _ => return None,
    })
}

fn expected_route(name: &str) -> InternalIoctl {
    match name {
        "IOCTL_HID_GET_DEVICE_DESCRIPTOR" => InternalIoctl::GetDeviceDescriptor,
        "IOCTL_HID_GET_DEVICE_ATTRIBUTES" => InternalIoctl::GetDeviceAttributes,
        "IOCTL_HID_GET_REPORT_DESCRIPTOR" => InternalIoctl::GetReportDescriptor,
        "IOCTL_HID_READ_REPORT" => InternalIoctl::ReadReport,
        "IOCTL_HID_GET_FEATURE" => InternalIoctl::GetFeature,
        "IOCTL_HID_SET_FEATURE" => InternalIoctl::SetFeature,
        _ => InternalIoctl::NotSupported,
    }
}

#[test]
fn descriptors_fixture() {
    let records = load("descriptors.txt");
    let mut tally = Tally::default();
    for r in &records {
        let id = r.id();
        match r.req("kind") {
            "report_descriptor" => {
                assert_eq!(num(r.req("len")) as usize, REPORT_DESCRIPTOR_LEN);
                assert_eq!(hex(r.req("bytes")), REPORT_DESCRIPTOR.to_vec());
                tally.add("report_descriptor");
            }
            "hid_descriptor" => {
                assert_eq!(num(r.req("len")) as usize, HID_DESCRIPTOR_LEN);
                assert_eq!(hex(r.req("bytes")), HID_DESCRIPTOR.to_vec());
                tally.add("hid_descriptor");
            }
            "layout" if r.get("device_context").is_some() => {
                // DEVICE_CONTEXT size and field offsets of the C build. The Rust context is a different
                // type (WDF handles in the glue + core DeviceState); nothing outside the driver sees it.
                tally.add("skipped/c_memory_layout");
            }
            "layout" => {
                for key in r.keys() {
                    let v = || num(r.req(key)) as usize;
                    match key {
                        // The C configuration layout is internal to the original driver.
                        "kind" | "BCM5974_CONFIG" => {}
                        "PTP_REPORT" => assert_eq!(v(), PTP_REPORT_LEN),
                        "PTP_CONTACT" => assert_eq!(v(), PTP_CONTACT_LEN),
                        "HID_XFER_PACKET" => assert_eq!(v(), HID_XFER_PACKET_LEN),
                        "HID_DESCRIPTOR" => assert_eq!(v(), HID_DESCRIPTOR_LEN),
                        "HID_DEVICE_ATTRIBUTES" => assert_eq!(v(), HID_DEVICE_ATTRIBUTES_LEN),
                        "PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT" => {
                            assert_eq!(v(), SELECTIVE_REPORT_DEVICE_MODE_STRUCT_LEN);
                        }
                        "TRACKPAD_FINGER" => assert_eq!(v(), TRACKPAD_FINGER_LEN),
                        "PTP_DEVICE_CAPS_FEATURE_REPORT" => assert_eq!(v(), DEVICE_CAPS_REPORT_LEN),
                        "PTP_DEVICE_HQA_CERTIFICATION_REPORT" => assert_eq!(v(), HQA_REPORT_LEN),
                        "PTP_DEVICE_INPUT_MODE_REPORT" => assert_eq!(v(), INPUT_MODE_REPORT_LEN),
                        "wReportLength" => assert_eq!(v(), REPORT_DESCRIPTOR_LEN),
                        // HID_XFER_PACKET field offsets: the glue reads the struct (glue contract,
                        // documented in feature.rs); pinned here so a change is noticed.
                        "xfer_off_reportBuffer" => assert_eq!(v(), 0),
                        "xfer_off_reportBufferLen" => assert_eq!(v(), 8),
                        "xfer_off_reportId" => assert_eq!(v(), 12),
                        other => panic!("{id}: unhandled layout key {other}"),
                    }
                }
                tally.add("layout");
            }
            "consts" => {
                assert_eq!(num(r.req("pool_tag")) as u32, POOL_TAG_PTP_CONTROL);
                // C: PagedPool (1) for a URB transfer buffer. P2 (B27): NonPagedPoolNx (512).
                assert_eq!(num(r.req("paged_pool")), 1);
                assert_eq!(TRANSFER_BUFFER_POOL_TYPE, 512);
                assert_eq!(
                    num(r.req("reader_transfer_len")) as usize,
                    reader_transfer_length(CONFIG_TABLE[0].profile.tp_type)
                );
                assert_eq!(
                    hex(r.req("devif_guid_mem")),
                    DEVICE_INTERFACE_GUID.to_bytes().to_vec()
                );
                tally.add("consts");
            }
            "ioctl_code" => {
                let name = r.req("name");
                let code = num(r.req("code")) as u32;
                if let Some(c) = ptp_hid_code(name) {
                    assert_eq!(c, code, "{name}");
                }
                assert_eq!(route_internal_ioctl(code), expected_route(name), "{name}");
                tally.add("ioctl_code");
            }
            "config_row" => {
                let idx = num(r.req("idx")) as usize;
                if let Some(row) = CONFIG_TABLE.get(idx) {
                    check_row(r, row);
                } else {
                    // The `{0}` sentinel: not represented (the Rust lookup is bounds-checked).
                    assert_eq!(idx, CONFIG_TABLE.len(), "{id}");
                    assert_eq!(num(r.req("identification")), 0);
                    for k in ["caps", "tp_type", "tp_header", "tp_fsize", "um_size"] {
                        assert_eq!(num(r.req(k)), 0, "{id} {k}");
                    }
                }
                tally.add("config_row");
            }
            "config_select" => {
                let pid = num(r.req("pid")) as u16;
                let row = config_for_pid(pid);
                assert_eq!(u64::from(row.identification), num(r.req("cfg")), "{id}");
                assert_eq!(
                    row.profile.x.min,
                    r.req("x_min").parse::<i32>().unwrap(),
                    "{id}"
                );
                assert_eq!(
                    row.profile.y.max,
                    r.req("y_max").parse::<i32>().unwrap(),
                    "{id}"
                );
                tally.add("config_select");
            }
            "ioctl" => tally.add(replay_descriptor_ioctl(r)),
            other => panic!("{id}: unknown kind {other}"),
        }
    }
    tally.assert_exact(
        "descriptors.txt",
        173,
        &[
            ("config_row", 6),
            ("config_select", 18),
            ("consts", 1),
            ("hid_descriptor", 1),
            ("ioctl/driver", 92),
            ("ioctl/framework_retrieval_status", 18),
            ("ioctl_code", 34),
            ("layout", 1),
            ("report_descriptor", 1),
            ("skipped/c_memory_layout", 1),
        ],
    );
}
