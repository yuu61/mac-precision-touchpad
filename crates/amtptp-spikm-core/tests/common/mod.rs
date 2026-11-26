//! Shared fixture parsing for the C-oracle replay tests (see `tests/fixtures/README.md`).
#![allow(
    dead_code,
    reason = "Each integration test target uses a different subset of these helpers"
)]

use core::fmt::Write as _;
use std::collections::BTreeMap;

/// One fixture line: `key=value` fields, split at the first `=`.
#[derive(Debug, Clone)]
pub struct Record {
    pub line: usize,
    pub fields: BTreeMap<String, String>,
}

impl Record {
    pub fn get(&self, key: &str) -> &str {
        match self.fields.get(key) {
            Some(v) => v.as_str(),
            None => panic!("line {}: missing field `{key}`", self.line),
        }
    }

    pub fn opt(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    pub fn case(&self) -> &str {
        self.opt("case").or_else(|| self.opt("seq")).unwrap_or("?")
    }

    /// 8-hex-digit status/code field as the raw 32 bits.
    pub fn hex32(&self, key: &str) -> u32 {
        u32::from_str_radix(self.get(key), 16)
            .unwrap_or_else(|e| panic!("line {}: {key}: {e}", self.line))
    }

    /// Status field as NTSTATUS (i32 bit pattern).
    pub fn status(&self, key: &str) -> i32 {
        self.hex32(key) as i32
    }

    pub fn hex16(&self, key: &str) -> u16 {
        u16::from_str_radix(self.get(key), 16)
            .unwrap_or_else(|e| panic!("line {}: {key}: {e}", self.line))
    }

    pub fn dec<T: core::str::FromStr>(&self, key: &str) -> T
    where
        T::Err: core::fmt::Debug,
    {
        self.get(key)
            .parse()
            .unwrap_or_else(|e| panic!("line {}: {key}: {e:?}", self.line))
    }

    /// Hex byte string; `-` is empty.
    pub fn bytes(&self, key: &str) -> Vec<u8> {
        hex(self.get(key))
    }

    pub fn has(&self, key: &str) -> bool {
        self.fields.contains_key(key)
    }
}

pub fn hex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).expect("ASCII hex bytes"), 16).expect("hex"))
        .collect()
}

pub fn to_hex(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "-".to_owned();
    }
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(hex, "{byte:02x}").expect("formatting hex into a String");
    }
    hex
}

/// Loads `tests/fixtures/<name>` and parses every non-empty line.
pub fn load(name: &str) -> Vec<Record> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            let mut fields = BTreeMap::new();
            for tok in l.split(' ') {
                let (k, v) = tok
                    .split_once('=')
                    .unwrap_or_else(|| panic!("{name}:{}: bad token `{tok}`", i + 1));
                let prev = fields.insert(k.to_owned(), v.to_owned());
                assert!(prev.is_none(), "{name}:{}: duplicate key {k}", i + 1);
            }
            Record {
                line: i + 1,
                fields,
            }
        })
        .collect()
}

/// `PRE(i) = (0xA0 + i) mod 256`: the prefill of every caller-owned buffer.
pub fn prefill(len: usize) -> Vec<u8> {
    (0..len).map(|i| (0xA0_usize + i) as u8).collect()
}

/// `offset:value` list of bytes that differ between `before` and `after`, or `-`.
pub fn chg(before: &[u8], after: &[u8]) -> String {
    let parts: Vec<String> = before
        .iter()
        .zip(after)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (_, b))| format!("{i}:{b:02x}"))
        .collect();
    if parts.is_empty() {
        "-".to_owned()
    } else {
        parts.join(",")
    }
}

/// Splits the `wdf` field into `(function, status)` pairs.
pub fn wdf_failures(r: &Record) -> Vec<(String, String)> {
    let w = r.get("wdf");
    if w == "-" {
        return Vec::new();
    }
    w.split(',')
        .map(|e| {
            let (f, s) = e.split_once(':').expect("wdf entry");
            (f.to_owned(), s.to_owned())
        })
        .collect()
}

/// Counts records by a named outcome and checks the totals at the end.
#[derive(Default, Debug)]
pub struct Tally {
    pub counts: BTreeMap<&'static str, usize>,
}

impl Tally {
    pub fn add(&mut self, bucket: &'static str) {
        *self.counts.entry(bucket).or_default() += 1;
    }

    pub fn get(&self, bucket: &str) -> usize {
        self.counts.get(bucket).copied().unwrap_or(0)
    }

    pub fn total(&self) -> usize {
        self.counts.values().sum()
    }
}
