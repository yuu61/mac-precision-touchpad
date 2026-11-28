//! Shared fixture parsing and accounting for the oracle-fixture replay tests.
//!
//! Fixture format (tests/fixtures/README.md): one record per line, fields separated by one space, each
//! field split at its first `=`; byte strings are lowercase hex, `-` = empty/none.
#![allow(
    dead_code,
    reason = "Each integration test target uses a different subset of these helpers"
)]

pub mod fake;

use core::fmt::Write as _;
use std::collections::BTreeMap;

use amtptp_usbkm_core::config::config_index_for_pid;
use amtptp_usbkm_core::hid::{
    self, HID_DESCRIPTOR, HID_DEVICE_ATTRIBUTES_LEN, InternalIoctl, REPORT_DESCRIPTOR_LEN,
    route_internal_ioctl,
};
use amtptp_usbkm_core::lifecycle;
use amtptp_usbkm_core::state::{DeviceState, UsbDeviceIds};
use amtptp_usbkm_core::status::{Completion, STATUS_BUFFER_TOO_SMALL};
use ptp_hid::common::{MAX_FINGERS, PTP_COLLECTION_WINDOWS};
use ptp_hid::descriptor::{
    FINGER_COLLECTION_U32_ID_LEN, FINGER_HEAD_U32_ID_LEN, FINGER_SEPARATOR_LEN, GLOBAL_RESET,
    TOUCHPAD_TLC_HEADER_LEN,
};

use fake::FakeHw;

/// One fixture line.
pub struct Record {
    pub line_no: usize,
    pub fields: Vec<(String, String)>,
}

impl Record {
    pub fn parse(line_no: usize, line: &str) -> Self {
        let fields = line
            .split(' ')
            .map(|f| match f.split_once('=') {
                Some((k, v)) => (k.to_owned(), v.to_owned()),
                None => panic!("line {line_no}: field without '=': {f:?}"),
            })
            .collect();
        Self { line_no, fields }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn req(&self, key: &str) -> &str {
        self.get(key)
            .unwrap_or_else(|| panic!("line {}: missing field {key}", self.line_no))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.fields.iter().map(|(k, _)| k.as_str())
    }

    /// The `tags` field as a list (`-` = none).
    pub fn tags(&self) -> Vec<&str> {
        match self.get("tags") {
            None | Some("-") => Vec::new(),
            Some(t) => t.split(',').collect(),
        }
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags().contains(&tag)
    }

    /// `case=` or `scenario=`+`step=`, for messages.
    pub fn id(&self) -> String {
        match (self.get("case"), self.get("scenario"), self.get("step")) {
            (Some(c), _, _) => c.to_owned(),
            (None, Some(s), Some(st)) => format!("{s}#{st}"),
            _ => format!("line {}", self.line_no),
        }
    }
}

/// Loads `tests/fixtures/<name>` (read-only).
pub fn load(name: &str) -> Vec<Record> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.is_empty())
        .map(|(i, l)| Record::parse(i + 1, l))
        .collect()
}

/// Lowercase hex → bytes; `-` → empty.
pub fn hex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    assert!(s.len().is_multiple_of(2), "odd hex length: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).expect("ASCII hex bytes"), 16).expect("hex"))
        .collect()
}

/// Bytes → lowercase hex; empty → `-`.
pub fn to_hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "-".to_owned();
    }
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        write!(s, "{x:02x}").expect("write");
    }
    s
}

/// `0x...` hex or decimal, as u64.
pub fn num(s: &str) -> u64 {
    match s.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16).expect("hex number"),
        None => s.parse().expect("decimal number"),
    }
}

/// `0x%08x` NTSTATUS → i32.
pub fn status(s: &str) -> i32 {
    num(s) as u32 as i32
}

/// NTSTATUS → `0x%08x`.
pub fn status_str(s: i32) -> String {
    format!("0x{:08x}", s as u32)
}

/// `k:v,k:v` → map (`st_in` / `st_out` / `ctx_out` / req fields).
pub fn kv(s: &str) -> BTreeMap<String, String> {
    s.split(',')
        .map(|p| {
            let (k, v) = p
                .split_once(':')
                .unwrap_or_else(|| panic!("bad kv item {p:?}"));
            (k.to_owned(), v.to_owned())
        })
        .collect()
}

/// Per-file accounting: every record lands in exactly one named bucket, and the sum is asserted against
/// the file's record count.
#[derive(Default)]
pub struct Tally {
    pub buckets: BTreeMap<String, usize>,
}

impl Tally {
    pub fn add(&mut self, bucket: &str) {
        *self.buckets.entry(bucket.to_owned()).or_default() += 1;
    }

    pub fn get(&self, bucket: &str) -> usize {
        self.buckets.get(bucket).copied().unwrap_or(0)
    }

    pub fn total(&self) -> usize {
        self.buckets.values().sum()
    }

    /// Asserts the total and the exact per-bucket counts, and prints the table.
    pub fn assert_exact(&self, file: &str, total: usize, expected: &[(&str, usize)]) {
        for (bucket, count) in &self.buckets {
            println!("{file}: {bucket} = {count}");
        }
        assert_eq!(self.total(), total, "{file}: records accounted");
        let expected: BTreeMap<String, usize> = expected
            .iter()
            .map(|(k, v)| ((*k).to_owned(), *v))
            .collect();
        assert_eq!(self.buckets, expected, "{file}: bucket counts");
    }
}

pub const C_REPORT_DESCRIPTOR_LEN: usize = 493;
pub const REPORT_DESCRIPTOR_DELTA: usize = REPORT_DESCRIPTOR_LEN - C_REPORT_DESCRIPTOR_LEN;

/// Projects a clean 511-byte T2 report descriptor back to the 493-byte C descriptor.
pub fn to_legacy_c_u32(bytes: &[u8], is_t2_13: bool) -> Vec<u8> {
    let mut patched = bytes.to_vec();
    let step = FINGER_COLLECTION_U32_ID_LEN + FINGER_SEPARATOR_LEN;
    if is_t2_13 {
        // Step 2 (A-2.4): 13-inch T2 (0x027a/0x027b) uses 12992x7855 (1352x836); C used 20000x12000.
        for finger in 0..5 {
            let base = TOUCHPAD_TLC_HEADER_LEN + finger * step + FINGER_HEAD_U32_ID_LEN;
            assert_eq!(&patched[base + 3..base + 5], &12992_u16.to_le_bytes());
            patched[base + 3..base + 5].copy_from_slice(&20000_u16.to_le_bytes());
            assert_eq!(&patched[base + 14..base + 16], &1352_u16.to_le_bytes());
            patched[base + 14..base + 16].copy_from_slice(&1300_u16.to_le_bytes());
            assert_eq!(&patched[base + 21..base + 23], &836_u16.to_le_bytes());
            patched[base + 21..base + 23].copy_from_slice(&850_u16.to_le_bytes());
            assert_eq!(&patched[base + 24..base + 26], &7855_u16.to_le_bytes());
            patched[base + 24..base + 26].copy_from_slice(&12000_u16.to_le_bytes());
        }
    }
    let f3_reset = TOUCHPAD_TLC_HEADER_LEN
        + 2 * step
        + (FINGER_COLLECTION_U32_ID_LEN - 1 - GLOBAL_RESET.len());
    let f5_reset = TOUCHPAD_TLC_HEADER_LEN
        + 4 * step
        + (FINGER_COLLECTION_U32_ID_LEN - 1 - GLOBAL_RESET.len());
    let tail_start =
        TOUCHPAD_TLC_HEADER_LEN + 5 * FINGER_COLLECTION_U32_ID_LEN + 4 * FINGER_SEPARATOR_LEN;
    let scan_time_reset = tail_start + 21;
    let input_mode_max_offset = tail_start + 88 + 17;

    assert_eq!(&patched[f3_reset..f3_reset + 6], &GLOBAL_RESET);
    assert_eq!(&patched[f5_reset..f5_reset + 6], &GLOBAL_RESET);
    assert_eq!(
        &patched[scan_time_reset..scan_time_reset + 6],
        &GLOBAL_RESET
    );
    assert_eq!(patched[input_mode_max_offset], PTP_COLLECTION_WINDOWS);

    let mut out = Vec::with_capacity(C_REPORT_DESCRIPTOR_LEN);
    out.extend_from_slice(&patched[..f3_reset]);
    out.extend_from_slice(&patched[f3_reset + 6..f5_reset]);
    out.extend_from_slice(&patched[f5_reset + 6..scan_time_reset]);
    out.extend_from_slice(&patched[scan_time_reset + 6..input_mode_max_offset]);
    out.push(MAX_FINGERS);
    out.extend_from_slice(&patched[input_mode_max_offset + 1..]);
    out
}

/// Replays one descriptor/attributes IOCTL through the dispatcher. Returns the bucket.
///
/// The output-buffer retrieval is the framework's: `WdfRequestRetrieveOutputMemory` fails with
/// `STATUS_BUFFER_TOO_SMALL` for a zero-length buffer, `WdfRequestRetrieveOutputBuffer(.., 32, ..)` below
/// 32 bytes (README "Fake framework"). Those records are replayed with that modelled status.
pub fn replay_descriptor_ioctl(r: &Record) -> &'static str {
    let id = r.id();
    let code = num(r.req("ioctl")) as u32;
    let route = route_internal_ioctl(code);
    let c_outlen = num(r.req("outlen")) as usize;
    let outlen = if route == InternalIoctl::GetReportDescriptor
        && (C_REPORT_DESCRIPTOR_LEN - 1..=C_REPORT_DESCRIPTOR_LEN + 1).contains(&c_outlen)
    {
        c_outlen + REPORT_DESCRIPTOR_DELTA
    } else {
        c_outlen
    };
    let prefill = hex(r.req("prefill"))[0];
    let mut out = vec![prefill; outlen];
    let state = DeviceState::new();
    state.on_device_create();
    if r.req("prepared") == "1" {
        // Bring-up to D0 with the record's device descriptor, through the core lifecycle.
        let mut hw = FakeHw::new(0);
        hw.ids = UsbDeviceIds {
            vendor_id: num(r.req("vid")) as u16,
            product_id: num(r.req("pid")) as u16,
            bcd_device: num(r.req("bcd")) as u16,
        };
        assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0, "{id}");
        assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0, "{id}");
        assert_eq!(state.usb_device_ids(), hw.ids, "{id}");
        assert_eq!(
            state.config_index(),
            Some(config_index_for_pid(hw.ids.product_id)),
            "{id}"
        );
    }
    let mut framework = false;
    let completion = match route {
        InternalIoctl::GetDeviceDescriptor | InternalIoctl::GetReportDescriptor => {
            let output = if outlen == 0 {
                framework = true;
                Err(STATUS_BUFFER_TOO_SMALL)
            } else {
                Ok(&mut *out)
            };
            if route == InternalIoctl::GetDeviceDescriptor {
                hid::get_hid_descriptor(output)
            } else {
                hid::get_report_descriptor_for(state.config(), output)
            }
        }
        InternalIoctl::GetDeviceAttributes => {
            let output = if outlen < HID_DEVICE_ATTRIBUTES_LEN {
                framework = true;
                Err(STATUS_BUFFER_TOO_SMALL)
            } else {
                Ok(&mut *out)
            };
            hid::get_device_attributes(output, state.usb_device_ids())
        }
        other @ (InternalIoctl::ReadReport
        | InternalIoctl::GetFeature
        | InternalIoctl::GetString
        | InternalIoctl::SetFeature
        | InternalIoctl::NotSupported) => {
            panic!("{id}: unexpected route {other:?}");
        }
    };
    assert_eq!(r.req("completed"), "1", "{id}");
    let expected_info = if completion.status == 0 && route == InternalIoctl::GetReportDescriptor {
        num(r.req("info")) as usize + REPORT_DESCRIPTOR_DELTA
    } else {
        num(r.req("info")) as usize
    };
    assert_eq!(
        completion,
        Completion {
            status: status(r.req("status")),
            information: expected_info
        },
        "{id}"
    );
    let mut expected = hex(r.req("out"));
    match (completion.status, route) {
        (0, InternalIoctl::GetDeviceAttributes) => {
            expected[10..32].fill(0);
            assert_eq!(out, expected, "{id}: output buffer");
        }
        (0, InternalIoctl::GetDeviceDescriptor) => {
            assert_eq!(&out[..9], &HID_DESCRIPTOR, "{id}: HID descriptor");
            expected[7..9].copy_from_slice(&(REPORT_DESCRIPTOR_LEN as u16).to_le_bytes());
            assert_eq!(out, expected, "{id}: output buffer");
        }
        (0, InternalIoctl::GetReportDescriptor) => {
            assert_eq!(
                &out[..REPORT_DESCRIPTOR_LEN],
                hid::report_descriptor_for(state.config()),
                "{id}: report descriptor"
            );
            let is_t2_13 = matches!(state.config().identification, 0x027a | 0x027b);
            if state.config().firmware == amtptp_usbkm_core::config::Firmware::T2 {
                assert_eq!(
                    to_legacy_c_u32(&out[..REPORT_DESCRIPTOR_LEN], is_t2_13),
                    expected[..C_REPORT_DESCRIPTOR_LEN],
                    "{id}: legacy C projection"
                );
            } else {
                assert_eq!(&out[..REPORT_DESCRIPTOR_LEN], &state.config().descriptor);
            }

            assert!(
                out[REPORT_DESCRIPTOR_LEN..].iter().all(|&b| b == prefill)
                    && expected[C_REPORT_DESCRIPTOR_LEN..]
                        .iter()
                        .all(|&b| b == prefill),
                "{id}: trailing prefill"
            );
        }
        (_, InternalIoctl::GetReportDescriptor) => {
            assert!(
                out.iter().all(|&b| b == prefill) && expected.iter().all(|&b| b == prefill),
                "{id}: untouched prefill"
            );
        }
        _ => {
            assert_eq!(out, expected, "{id}: output buffer");
        }
    }
    assert_eq!(r.req("qdepth_before"), "0", "{id}");
    assert_eq!(r.req("qdepth_after"), "0", "{id}");
    if framework {
        "ioctl/framework_retrieval_status"
    } else {
        "ioctl/driver"
    }
}
