//! Shared test support: fixture record parser, a fake UMDF framework (requests, the `READ_REPORT` queue)
//! and a fake USB device, modelled exactly as described in `tests/fixtures/README.md`.

#![allow(
    dead_code,
    reason = "Each integration test target uses a different subset of these helpers"
)]

use core::fmt::Write as _;
use std::collections::VecDeque;

use amtptp_usbum_core::io::{
    ControlPipe, HidRequest, InterruptPipeTarget, PipeInfo, PipeType, ReadQueue, UsbStringSource,
    UsbTargetSetup,
};
use amtptp_usbum_core::state::{StateSnapshot, UsbDeviceDescriptor};
use amtptp_usbum_core::status::{
    NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_BUFFER_SIZE, STATUS_NO_MORE_ENTRIES,
    STATUS_UNSUCCESSFUL, nt_success,
};
use amtptp_usbum_core::usb::SetupPacket;

// ------------------------------------------------------------------------------------------------
// Fixture records
// ------------------------------------------------------------------------------------------------

/// Reads a fixture file (LF lines, one record per line).
pub fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// One fixture record: `key=value` fields separated by single spaces, split at the first `=`.
pub struct Rec<'a> {
    pub fields: Vec<(&'a str, &'a str)>,
}

impl<'a> Rec<'a> {
    pub fn parse(line: &'a str) -> Self {
        let fields = line
            .split(' ')
            .map(|field| {
                field
                    .split_once('=')
                    .unwrap_or_else(|| panic!("field without '=': {field:?} in {line}"))
            })
            .collect::<Vec<_>>();
        for (i, (k, _)) in fields.iter().enumerate() {
            assert!(
                !fields[..i].iter().any(|(other, _)| other == k),
                "duplicate key {k} in {line}"
            );
        }
        Self { fields }
    }

    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn s(&self, key: &str) -> &'a str {
        self.get(key)
            .unwrap_or_else(|| panic!("{}: missing field {key}", self.case()))
    }

    pub fn case(&self) -> &'a str {
        self.get("case")
            .or_else(|| self.get("pkt"))
            .unwrap_or("<no case>")
    }

    /// Hex byte string (`-` = empty).
    pub fn hex(&self, key: &str) -> Vec<u8> {
        hex(self.s(key))
    }

    /// Decimal or `0x` hexadecimal unsigned integer.
    pub fn u(&self, key: &str) -> u64 {
        parse_u(self.s(key))
    }

    /// Signed decimal integer.
    pub fn i(&self, key: &str) -> i64 {
        let v = self.s(key);
        v.parse()
            .unwrap_or_else(|e| panic!("{}: bad integer {key}={v}: {e}", self.case()))
    }

    /// `0x%08x` NTSTATUS.
    pub fn status(&self, key: &str) -> NtStatus {
        parse_status(self.s(key))
    }

    /// `0`/`1` flag.
    pub fn flag(&self, key: &str) -> bool {
        match self.s(key) {
            "0" => false,
            "1" => true,
            other => panic!("{}: bad flag {key}={other}", self.case()),
        }
    }

    /// State before the call (`wsm surf btn pq sq mq perf`); missing `perf` reads as 0.
    pub fn state_before(&self) -> StateSnapshot {
        self.state_with_suffix("")
    }

    /// State after the call (`wsm2 ... perf2`); missing `perf2` reads as the `perf` before.
    pub fn state_after(&self) -> StateSnapshot {
        self.state_with_suffix("2")
    }

    fn state_with_suffix(&self, suffix: &str) -> StateSnapshot {
        let key = |k: &str| format!("{k}{suffix}");
        let perf = self
            .get(&key("perf"))
            .or_else(|| self.get("perf"))
            .map_or(0, |v| v.parse().expect("perf"));
        StateSnapshot {
            wellspring_mode_on: self.flag(&key("wsm")),
            surface_report_on: self.flag(&key("surf")),
            button_report_on: self.flag(&key("btn")),
            pressure_qual_level: self.u(&key("pq")) as u8,
            sg_contact_size_qual_level: self.u(&key("sq")) as u8,
            mu_contact_size_qual_level: self.u(&key("mq")) as u8,
            qpc_frequency: perf,
        }
    }
}

pub fn records(text: &str) -> Vec<Rec<'_>> {
    text.lines()
        .filter(|l| !l.is_empty())
        .map(Rec::parse)
        .collect()
}

pub fn hex(s: &str) -> Vec<u8> {
    if s == "-" || s.is_empty() {
        return Vec::new();
    }
    assert!(s.len().is_multiple_of(2), "odd hex string {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(s.get(i..i + 2).expect("ASCII hex bytes"), 16)
                .unwrap_or_else(|e| panic!("bad hex {s}: {e}"))
        })
        .collect()
}

pub fn to_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(hex, "{byte:02x}").expect("formatting hex into a String");
    }
    hex
}

pub fn parse_u(v: &str) -> u64 {
    match v.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16),
        None => v.parse(),
    }
    .unwrap_or_else(|e| panic!("bad unsigned {v}: {e}"))
}

pub fn parse_status(v: &str) -> NtStatus {
    let raw = v.strip_prefix("0x").unwrap_or(v);
    u32::from_str_radix(raw, 16).unwrap_or_else(|e| panic!("bad status {v}: {e}")) as i32
}

pub fn status_hex(s: NtStatus) -> String {
    format!("{:08x}", s as u32)
}

// ------------------------------------------------------------------------------------------------
// Fake WDF request / READ_REPORT queue
// ------------------------------------------------------------------------------------------------

/// A WDFREQUEST with an input buffer and an output buffer, following the README framework model:
/// zero-length buffers fail retrieval with `STATUS_BUFFER_TOO_SMALL`, `WdfMemoryCopyFromBuffer` of more
/// bytes than the buffer fails with `STATUS_INVALID_BUFFER_SIZE`, `WdfRequestRetrieveOutputBuffer` below
/// the minimum fails with `STATUS_BUFFER_TOO_SMALL`.
#[derive(Debug, Clone)]
pub struct FakeReq {
    pub input: Vec<u8>,
    pub out: Vec<u8>,
    /// Set when the output bytes were handed out writable or copied into.
    pub out_accessed: bool,
    /// Failure statuses this fake framework returned (framework-decided outcomes).
    pub failures: Vec<NtStatus>,
}

impl FakeReq {
    pub fn new(input: Vec<u8>, outlen: usize, fill: u8) -> Self {
        Self {
            input,
            out: vec![fill; outlen],
            out_accessed: false,
            failures: Vec::new(),
        }
    }

    fn fail<T>(&mut self, status: NtStatus) -> Result<T, NtStatus> {
        self.failures.push(status);
        Err(status)
    }
}

impl HidRequest for FakeReq {
    fn input_memory(&mut self) -> Result<&[u8], NtStatus> {
        if self.input.is_empty() {
            self.fail(STATUS_BUFFER_TOO_SMALL)
        } else {
            Ok(&self.input)
        }
    }

    fn output_memory_len(&mut self) -> Result<usize, NtStatus> {
        if self.out.is_empty() {
            self.fail(STATUS_BUFFER_TOO_SMALL)
        } else {
            Ok(self.out.len())
        }
    }

    fn output_memory(&mut self) -> Result<&mut [u8], NtStatus> {
        if self.out.is_empty() {
            self.fail(STATUS_BUFFER_TOO_SMALL)
        } else {
            self.out_accessed = true;
            Ok(&mut self.out)
        }
    }

    fn copy_to_output_memory(&mut self, src: &[u8]) -> Result<(), NtStatus> {
        if self.out.is_empty() {
            return self.fail(STATUS_BUFFER_TOO_SMALL);
        }
        if src.len() > self.out.len() {
            return self.fail(STATUS_INVALID_BUFFER_SIZE);
        }
        self.out_accessed = true;
        self.out[..src.len()].copy_from_slice(src);
        Ok(())
    }

    fn output_buffer(&mut self, minimum_len: usize) -> Result<&mut [u8], NtStatus> {
        if self.out.is_empty() || self.out.len() < minimum_len {
            self.fail(STATUS_BUFFER_TOO_SMALL)
        } else {
            self.out_accessed = true;
            Ok(&mut self.out)
        }
    }
}

/// The manual `InputQueue`.
#[derive(Default)]
pub struct FakeQueue {
    pub pending: VecDeque<FakeReq>,
    pub completed: Vec<(FakeReq, NtStatus, usize)>,
}

impl FakeQueue {
    /// Queue of `READ_REPORT` requests with the given output lengths, pre-filled with 0xee.
    pub fn with_lengths(lengths: &[usize]) -> Self {
        Self {
            pending: lengths
                .iter()
                .map(|&n| FakeReq::new(Vec::new(), n, 0xee))
                .collect(),
            completed: Vec::new(),
        }
    }
}

impl ReadQueue for FakeQueue {
    type Request = FakeReq;

    fn retrieve_next_request(&mut self) -> Result<FakeReq, NtStatus> {
        self.pending.pop_front().ok_or(STATUS_NO_MORE_ENTRIES)
    }

    fn complete_request(&mut self, request: FakeReq, status: NtStatus, information: usize) {
        self.completed.push((request, status, information));
    }
}

// ------------------------------------------------------------------------------------------------
// Fake USB device
// ------------------------------------------------------------------------------------------------

/// An interrupt IN pipe at `endpoint_address` (bit 7 set).
pub const fn interrupt_in(endpoint_address: u8) -> PipeInfo {
    PipeInfo {
        pipe_type: PipeType::Interrupt,
        endpoint_address,
    }
}

/// A pipe of `pipe_type` at `endpoint_address`.
pub const fn pipe(pipe_type: PipeType, endpoint_address: u8) -> PipeInfo {
    PipeInfo {
        pipe_type,
        endpoint_address,
    }
}

/// The README device model plus the PnP-side framework objects.
pub struct FakeUsb {
    /// 8-byte mode register.
    pub reg: [u8; 8],
    /// Bytes a GET returns at most.
    pub resp: usize,
    /// Scripted statuses of the successive control transfers (`None` = success).
    pub xs: Vec<Option<NtStatus>>,
    pub transfers: usize,
    /// `in/<setup>/<data>/<status>` and `out/<setup>/<payload>/<status>` entries.
    pub wire: Vec<String>,
    /// Framework calls (`WdfUsbTargetDeviceCreate`, `GetInterface(0)`, ... `IoTargetStart(pipe0)=...`).
    pub calls: Vec<String>,
    // PrepareHardware model
    pub usb_created: bool,
    pub create_status: NtStatus,
    pub descriptor: UsbDeviceDescriptor,
    pub traits: Result<u32, NtStatus>,
    /// `None` = interface 0 is NULL.
    pub pipes: Option<Vec<PipeInfo>>,
    pub visited: Vec<bool>,
    pub interrupt_pipe: Option<u8>,
    pub reader_status: NtStatus,
    pub reader_len: Option<usize>,
    // D0 model
    pub start_status: NtStatus,
    pub pipe_name: String,
    /// Failure statuses this fake framework/device returned.
    pub failures: Vec<NtStatus>,
    /// How many `serialized` calls are running: every control transfer must run inside exactly one
    /// (the mode-switch lock is not recursive).
    pub serialized_depth: usize,
}

impl Default for FakeUsb {
    fn default() -> Self {
        Self {
            reg: [0; 8],
            resp: 4,
            xs: Vec::new(),
            transfers: 0,
            wire: Vec::new(),
            calls: Vec::new(),
            usb_created: false,
            create_status: 0,
            descriptor: UsbDeviceDescriptor::default(),
            traits: Ok(2),
            pipes: Some(vec![interrupt_in(0x81)]),
            visited: vec![false],
            interrupt_pipe: None,
            reader_status: 0,
            reader_len: None,
            start_status: 0,
            pipe_name: "pipe0".to_owned(),
            failures: Vec::new(),
            serialized_depth: 0,
        }
    }
}

impl FakeUsb {
    /// Device model from `dev=<reg>/<resp>` and `xs=` (both optional).
    pub fn from_rec(rec: &Rec<'_>) -> Self {
        let mut usb = Self::default();
        if let Some(dev) = rec.get("dev") {
            let (reg, resp) = dev.split_once('/').expect("dev=reg/resp");
            usb.reg.copy_from_slice(&hex(reg));
            usb.resp = resp.parse().expect("resp");
        }
        usb.set_script(rec.get("xs"));
        usb
    }

    pub fn set_script(&mut self, xs: Option<&str>) {
        self.transfers = 0;
        self.xs = match xs {
            None | Some("-") => Vec::new(),
            Some(list) => list
                .split(',')
                .map(|s| {
                    if s == "-" {
                        None
                    } else {
                        Some(parse_status(s))
                    }
                })
                .collect(),
        };
    }

    pub fn wire_string(&self) -> String {
        if self.wire.is_empty() {
            "-".to_owned()
        } else {
            self.wire.join(",")
        }
    }

    pub fn set_pipes(&mut self, pipes: Option<Vec<PipeInfo>>) {
        self.visited = vec![false; pipes.as_ref().map_or(1, std::vec::Vec::len)];
        self.pipes = pipes;
    }
}

impl ControlPipe for FakeUsb {
    fn serialized<R, F: FnOnce(&mut Self) -> R>(&mut self, f: F) -> R {
        assert_eq!(self.serialized_depth, 0, "mode-switch lock taken twice");
        self.serialized_depth += 1;
        let result = f(self);
        self.serialized_depth -= 1;
        result
    }

    fn send_control_transfer(&mut self, setup: &SetupPacket, data: &mut [u8; 4]) -> NtStatus {
        assert_eq!(self.serialized_depth, 1, "control transfer outside the mode-switch lock");
        let status = self.xs.get(self.transfers).copied().flatten().unwrap_or(0);
        self.transfers += 1;
        if !nt_success(status) {
            self.failures.push(status);
        }
        assert_eq!(
            usize::from(setup.w_length),
            data.len(),
            "wLength = descriptor length"
        );
        let setup_hex = to_hex(&setup.to_bytes());
        if setup.is_device_to_host() {
            let mut returned = Vec::new();
            if nt_success(status) {
                let n = self.resp.min(usize::from(setup.w_length)).min(8);
                data[..n].copy_from_slice(&self.reg[..n]);
                returned.extend_from_slice(&self.reg[..n]);
            }
            self.wire.push(format!(
                "in/{setup_hex}/{}/{}",
                to_hex(&returned),
                status_hex(status)
            ));
        } else {
            if nt_success(status) {
                self.reg[..4].copy_from_slice(data);
            }
            self.wire.push(format!(
                "out/{setup_hex}/{}/{}",
                to_hex(data),
                status_hex(status)
            ));
        }
        status
    }
}

impl InterruptPipeTarget for FakeUsb {
    fn start(&mut self) -> NtStatus {
        self.calls.push(format!(
            "IoTargetStart({})=0x{}",
            self.pipe_name,
            status_hex(self.start_status)
        ));
        if !nt_success(self.start_status) {
            self.failures.push(self.start_status);
        }
        self.start_status
    }

    fn stop_cancel_sent_io(&mut self) {
        self.calls
            .push(format!("IoTargetStop({},action=1)", self.pipe_name));
    }
}

impl UsbTargetSetup for FakeUsb {
    fn create_usb_device_if_needed(&mut self) -> Result<(), NtStatus> {
        if self.usb_created {
            return Ok(());
        }
        self.calls.push("WdfUsbTargetDeviceCreate".to_owned());
        if nt_success(self.create_status) {
            self.usb_created = true;
            Ok(())
        } else {
            self.failures.push(self.create_status);
            Err(self.create_status)
        }
    }

    fn device_descriptor(&mut self) -> UsbDeviceDescriptor {
        self.descriptor
    }

    fn retrieve_device_traits(&mut self) -> Result<u32, NtStatus> {
        if let Err(status) = self.traits {
            self.failures.push(status);
        }
        self.traits
    }

    fn interface0_configured_pipes(&mut self) -> Option<u8> {
        self.calls.push("GetInterface(0)".to_owned());
        self.pipes.as_ref().map(|p| p.len() as u8)
    }

    fn visit_configured_pipe(&mut self, index: u8) -> PipeInfo {
        self.calls.push(format!("GetConfiguredPipe({index})"));
        self.calls.push(format!("SetNoMaxPktCheck({index})"));
        self.visited[usize::from(index)] = true;
        self.pipes.as_ref().expect("interface")[usize::from(index)]
    }

    fn select_interrupt_pipe(&mut self, index: u8) {
        self.interrupt_pipe = Some(index);
    }

    fn configure_continuous_reader(&mut self, transfer_length: usize) -> NtStatus {
        let pipe = self.interrupt_pipe.expect("reader on the interrupt pipe");
        self.calls.push(format!(
            "ConfigContinuousReader({pipe})=0x{}",
            status_hex(self.reader_status)
        ));
        self.reader_len = Some(transfer_length);
        if !nt_success(self.reader_status) {
            self.failures.push(self.reader_status);
        }
        self.reader_status
    }
}

/// `GET_STRING` device strings (README "Device model").
pub struct FakeStrings {
    pub fail_all: Option<NtStatus>,
    pub queries: Vec<String>,
    /// Failure statuses this fake device returned.
    pub failures: Vec<NtStatus>,
}

impl FakeStrings {
    pub const fn new() -> Self {
        Self {
            fail_all: None,
            queries: Vec::new(),
            failures: Vec::new(),
        }
    }

    pub const fn string(index: u8) -> Option<&'static str> {
        match index {
            1 => Some("Apple Inc."),
            2 => Some("Apple Internal Keyboard / Trackpad"),
            3 => Some("ABCD"),
            4 => Some("AB"),
            _ => None,
        }
    }

    pub fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    pub fn queries_string(&self) -> String {
        if self.queries.is_empty() {
            "-".to_owned()
        } else {
            self.queries.join(",")
        }
    }
}

impl UsbStringSource for FakeStrings {
    type String = Vec<u8>;

    fn alloc_and_query_string(
        &mut self,
        string_index: u8,
        language_id: u16,
    ) -> Result<Vec<u8>, NtStatus> {
        self.queries
            .push(format!("{string_index}/0x{language_id:04x}"));
        let result = match self.fail_all {
            Some(status) => Err(status),
            None => Self::string(string_index)
                .map(Self::utf16)
                .ok_or(STATUS_UNSUCCESSFUL),
        };
        if let Err(status) = result {
            self.failures.push(status);
        }
        result
    }
}

// ------------------------------------------------------------------------------------------------
// Wire comparison
// ------------------------------------------------------------------------------------------------

/// Normalizes an expected `wire=` value: drops the optional 5th `/umask` field of `out` entries after
/// asserting that every byte it marks undefined is `00` in the payload (the P2 4-byte zeroed buffer of
/// UM-B1). Returns the normalized string and the number of entries that carried a umask.
pub fn normalize_wire(case: &str, wire: &str) -> (String, usize) {
    if wire == "-" {
        return ("-".to_owned(), 0);
    }
    let mut masked = 0;
    let entries = wire
        .split(',')
        .map(|entry| {
            let parts: Vec<&str> = entry.split('/').collect();
            match parts.len() {
                4 => entry.to_owned(),
                5 => {
                    masked += 1;
                    let payload = hex(parts[2]);
                    let mask = hex(parts[4]);
                    assert_eq!(payload.len(), mask.len(), "{case}: umask length");
                    for (b, m) in payload.iter().zip(&mask) {
                        assert_eq!(
                            b & m,
                            0,
                            "{case}: undefined payload byte not zero ({entry})"
                        );
                    }
                    parts[..4].join("/")
                }
                _ => panic!("{case}: bad wire entry {entry}"),
            }
        })
        .collect::<Vec<_>>();
    (entries.join(","), masked)
}

/// Asserts the device register after the call against `dev2` (when present).
pub fn check_dev2(rec: &Rec<'_>, usb: &FakeUsb) {
    if let Some(dev2) = rec.get("dev2") {
        assert_eq!(
            to_hex(&usb.reg),
            dev2,
            "{}: device register after",
            rec.case()
        );
    }
}

/// Whether a failing `status` was produced by one of the fakes (framework or device model) and only
/// passed through by the driver logic.
pub fn fake_decided(status: NtStatus, sources: &[&[NtStatus]]) -> bool {
    !nt_success(status) && sources.iter().any(|failures| failures.contains(&status))
}

/// Tally label for records whose final status came from the fake framework/device model.
pub const FAKE_DECIDED: &str = "framework/device-decided status (fake model, passed through)";
