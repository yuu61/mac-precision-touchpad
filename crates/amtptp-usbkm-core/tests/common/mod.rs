//! Shared fixture parsing and accounting for the oracle-fixture replay tests.
//!
//! Fixture format (tests/fixtures/README.md): one record per line, fields separated by one space, each
//! field split at its first `=`; byte strings are lowercase hex, `-` = empty/none.
#![allow(dead_code)]

pub mod fake;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use amtptp_usbkm_core::config::config_index_for_pid;
use amtptp_usbkm_core::hid::{
    self, HID_DEVICE_ATTRIBUTES_LEN, InternalIoctl, route_internal_ioctl,
};
use amtptp_usbkm_core::lifecycle;
use amtptp_usbkm_core::state::{DeviceState, UsbDeviceIds};
use amtptp_usbkm_core::status::{Completion, STATUS_BUFFER_TOO_SMALL};

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
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
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

/// `k:v,k:v` → map (st_in / st_out / ctx_out / req fields).
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
        println!("{file}: {:?}", self.buckets);
        assert_eq!(self.total(), total, "{file}: records accounted");
        let expected: BTreeMap<String, usize> = expected
            .iter()
            .map(|(k, v)| ((*k).to_owned(), *v))
            .collect();
        assert_eq!(self.buckets, expected, "{file}: bucket counts");
    }
}

/// Replays one descriptor/attributes IOCTL through the dispatcher. Returns the bucket.
///
/// The output-buffer retrieval is the framework's: `WdfRequestRetrieveOutputMemory` fails with
/// `STATUS_BUFFER_TOO_SMALL` for a zero-length buffer, `WdfRequestRetrieveOutputBuffer(.., 32, ..)` below
/// 32 bytes (README "Fake framework"). Those records are replayed with that modelled status.
pub fn replay_descriptor_ioctl(r: &Record) -> &'static str {
    let id = r.id();
    let code = num(r.req("ioctl")) as u32;
    let outlen = num(r.req("outlen")) as usize;
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
    let completion = match route_internal_ioctl(code) {
        InternalIoctl::GetDeviceDescriptor | InternalIoctl::GetReportDescriptor => {
            let output = if outlen == 0 {
                framework = true;
                Err(STATUS_BUFFER_TOO_SMALL)
            } else {
                Ok(&mut out[..])
            };
            if route_internal_ioctl(code) == InternalIoctl::GetDeviceDescriptor {
                hid::get_hid_descriptor(output)
            } else {
                hid::get_report_descriptor(output)
            }
        }
        InternalIoctl::GetDeviceAttributes => {
            let output = if outlen < HID_DEVICE_ATTRIBUTES_LEN {
                framework = true;
                Err(STATUS_BUFFER_TOO_SMALL)
            } else {
                Ok(&mut out[..])
            };
            hid::get_device_attributes(output, state.usb_device_ids())
        }
        other => panic!("{id}: unexpected route {other:?}"),
    };
    assert_eq!(r.req("completed"), "1", "{id}");
    assert_eq!(
        completion,
        Completion {
            status: status(r.req("status")),
            information: num(r.req("info")) as usize
        },
        "{id}"
    );
    assert_eq!(to_hex(&out), r.req("out"), "{id}: output buffer");
    assert_eq!(r.req("qdepth_before"), "0", "{id}");
    assert_eq!(r.req("qdepth_after"), "0", "{id}");
    if framework {
        "ioctl/framework_retrieval_status"
    } else {
        "ioctl/driver"
    }
}
