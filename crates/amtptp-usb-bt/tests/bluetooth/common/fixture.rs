//! Parsing and accounting for Bluetooth C-oracle fixture records.

use core::fmt::Write as _;

use amtptp_usb_bt::bluetooth::{Bcm5974Param, DeviceSnapshot, NtStatus};

/// One fixture record: the `key=value` tokens in file order.
#[derive(Debug, Clone)]
pub struct Rec {
    pub fields: Vec<(String, String)>,
}

impl Rec {
    pub fn opt(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn get(&self, key: &str) -> &str {
        self.opt(key)
            .unwrap_or_else(|| panic!("{}: missing key {key}", self.case()))
    }

    pub fn case(&self) -> &str {
        self.fields.first().map_or("?", |(_, v)| v.as_str())
    }

    pub fn num(&self, key: &str) -> i128 {
        num(self.get(key))
    }

    pub fn bytes(&self, key: &str) -> Vec<u8> {
        hex(self.get(key))
    }
}

/// Loads all records of `tests/bluetooth/fixtures/<name>` (comment lines skipped).
pub fn load(name: &str) -> Vec<Rec> {
    let path = format!(
        "{}/tests/bluetooth/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|line| Rec {
            fields: line
                .split(' ')
                .map(|tok| {
                    let (k, v) = tok
                        .split_once('=')
                        .unwrap_or_else(|| panic!("bad token {tok}"));
                    (k.to_owned(), v.to_owned())
                })
                .collect(),
        })
        .collect()
}

/// `0x..` hex or signed decimal.
pub fn num(s: &str) -> i128 {
    if let Some(h) = s.strip_prefix("0x") {
        i128::from_str_radix(h, 16).unwrap_or_else(|_| panic!("bad hex {s}"))
    } else {
        s.parse().unwrap_or_else(|_| panic!("bad number {s}"))
    }
}

/// Lowercase hex bytes, `-` = empty.
pub fn hex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    assert!(s.len().is_multiple_of(2), "odd hex {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).expect("ASCII hex bytes"), 16).unwrap())
        .collect()
}

pub fn to_hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "-".into();
    }
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        write!(s, "{x:02x}").unwrap();
    }
    s
}

pub fn st(status: NtStatus) -> String {
    format!("0x{:08x}", status as u32)
}

pub fn status(s: &str) -> NtStatus {
    num(s) as u32 as i32
}

/// Parses an `st_in` / `st_out` value (README 2.1).
pub fn parse_state(text: &str) -> DeviceSnapshot {
    let mut state = DeviceSnapshot::default();
    // C's `IsHidIoDetourCompleted` and `HidIoTarget != NULL` together are the port's open data source.
    let (mut detour, mut target) = (false, false);
    for pair in text.split(',') {
        let (key, value) = pair
            .split_once(':')
            .unwrap_or_else(|| panic!("bad state {pair}"));
        let axis = |value: &str| {
            let bounds: Vec<i32> = value.split('/').map(|item| num(item) as i32).collect();
            Bcm5974Param {
                min: bounds[1],
                max: bounds[2],
            }
        };
        match key {
            "vid" => state.vendor_id = num(value) as u16,
            "pid" => state.product_id = num(value) as u16,
            "ver" => state.version_number = num(value) as u16,
            "hdr" => state.input_header_size = num(value) as usize,
            "fsz" => state.input_finger_size = num(value) as usize,
            "fdelta" => state.input_finger_delta = num(value) as usize,
            "bdelta" => state.input_button_delta = num(value) as usize,
            "x" => state.x = axis(value),
            "y" => state.y = axis(value),
            "cfg" => state.device_configured = value == "1",
            "detour" => detour = value == "1",
            "target" => target = value == "1",
            "inp_on" => state.ptp_input_on = value == "1",
            "touch" => state.ptp_report_touch = value == "1",
            "btn" => state.ptp_report_button = value == "1",
            "last" => state.qpc_frequency = num(value) as i64,
            _ => panic!("unknown state key {key}"),
        }
    }
    state.source_open = detour && target;
    state
}

/// The `ev` list of a record (`-` = no events).
pub fn events(r: &Rec) -> Vec<String> {
    match r.opt("ev") {
        None | Some("-") => Vec::new(),
        Some(v) => v.split('|').map(str::to_owned).collect(),
    }
}

/// Splits an event into (name, result) — `Name(args)=result` or `Name(args)`.
pub fn ev_name_result(e: &str) -> (String, Option<String>) {
    let name = e.split('(').next().unwrap_or(e).to_owned();
    let result = e.rsplit_once(")=").map(|(_, value)| value.to_owned());
    (name, result)
}

/// The arguments of an event as (key, value) pairs.
pub fn ev_args(e: &str) -> Vec<(String, String)> {
    let start = e.find('(').map_or(e.len(), |i| i + 1);
    let end = e.rfind(')').unwrap_or(e.len());
    e.get(start..end)
        .expect("valid event argument delimiters")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k.to_owned(), v.to_owned())
        })
        .collect()
}

/// The state of a started device on the given transport (what `SelfManagedIoInit` leaves).
pub fn started(vid: u16, hdr: usize) -> DeviceSnapshot {
    DeviceSnapshot {
        vendor_id: vid,
        product_id: 0x0265,
        version_number: 0x0100,
        input_header_size: hdr,
        input_finger_size: 9,
        input_finger_delta: 0,
        input_button_delta: 1,
        x: Bcm5974Param {
            min: -3678,
            max: 3934,
        },
        y: Bcm5974Param {
            min: -2479,
            max: 2586,
        },
        device_configured: true,
        source_open: true,
        ptp_input_on: true,
        // Both selective-reporting switches on: what the C input path behaved like (it ignored them).
        ptp_report_touch: true,
        ptp_report_button: true,
        qpc_frequency: 10_000_000,
    }
}

/// Counts records per named category and checks the totals at the end.
#[derive(Debug, Default)]
pub struct Tally {
    pub counts: Vec<(String, usize)>,
}

impl Tally {
    pub fn add(&mut self, category: &str) {
        match self.counts.iter_mut().find(|(c, _)| c == category) {
            Some((_, n)) => *n += 1,
            None => self.counts.push((category.to_owned(), 1)),
        }
    }

    pub fn get(&self, category: &str) -> usize {
        self.counts
            .iter()
            .find(|(c, _)| c == category)
            .map_or(0, |(_, n)| *n)
    }

    /// Asserts the exact per-category counts (order-insensitive) and returns the total.
    pub fn assert(&self, expected: &[(&str, usize)]) -> usize {
        let mut got: Vec<(String, usize)> = self.counts.clone();
        got.sort();
        let mut want: Vec<(String, usize)> = expected
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(c, n)| ((*c).to_owned(), *n))
            .collect();
        want.sort();
        assert_eq!(got, want, "per-category record counts");
        got.iter().map(|(_, n)| n).sum()
    }
}
