//! Replays every record of every oracle fixture file (`tests/fixtures/`, produced by executing the C
//! driver against a recording fake of UMDF 2.15) through the core crate, with the fake framework and
//! device of `common`.
//!
//! Each test asserts the exact number of records per file and the exact number of records in every
//! named group where the Rust port deliberately differs from C (P2 fixes) or where the record is
//! glue-only. Nothing is skipped silently. Records whose failing status was produced by the fake
//! framework/device model (and only passed through by the core) are counted per file as well.
//!
//! Fields that are not observable outside the driver and are not asserted:
//! - `calls=mc<n>,del` (feature/wire): the C `WdfMemoryCreate(um_size)` + `WdfObjectDelete` around each
//!   mode switch. The port uses a 4-byte stack buffer (UM-B1/B5 P2), so there is no allocation.
//! - `prev`/`target` (D0 records): only traced.

#![cfg(test)]

mod common;

use std::collections::{BTreeMap, HashMap};

use amtptp_usbum_core::NtStatus;
use amtptp_usbum_core::config::{CONFIG_TABLE, Family, lookup_config_row};
use amtptp_usbum_core::hid::{
    IoctlOutcome, IoctlRoute, get_device_attributes, get_feature, get_hid_descriptor,
    get_report_descriptor, get_string, route_ioctl, set_feature,
};
use amtptp_usbum_core::input::on_read_complete;
use amtptp_usbum_core::io::{PipeInfo, PipeType};
use amtptp_usbum_core::pnp::{
    CONTINUOUS_READER_NUM_PENDING_READS, GUID_DEVINTERFACE_AMTPTP_DEVICE, READERS_FAILED_RESULT,
    d0_entry, d0_exit, prepare_hardware,
};
use amtptp_usbum_core::state::{DeviceState, StateSnapshot, UsbDeviceDescriptor};
use amtptp_usbum_core::status::{STATUS_INVALID_DEVICE_STATE, STATUS_SUCCESS, nt_success};
use common::{
    FAKE_DECIDED, FakeQueue, FakeReq, FakeStrings, FakeUsb, Rec, check_dev2, fake_decided, fixture,
    hex, normalize_wire, records, status_hex, to_hex,
};

const B1_UMASK: &str = "B1 TYPE4/5 SET payload bytes past a short GET are 00 (umask)";
const B6_LEAK: &str = "B6 failed READ_REPORT is completed with the error instead of leaked";
const B7_STRING: &str = "B7 GET_STRING copies the string to the caller's buffer";
const B16_RESTART: &str = "B16 restart does not reconfigure the continuous reader";
const B21_INTERRUPT_IN: &str = "B21 the reader goes on the first interrupt IN pipe, not the OUT one";
const B10_GATE: &str = "B10 D0Entry switches the mode on whatever the button switch (= the wsm1 record)";
const B10_SEQUENCES: &str =
    "B10 sequences recorded the mode-switch gate; see d0_entry_switches_the_mode_after_selective_reporting_off";
const B9_D0_EXIT: &str =
    "B9 D0Exit returns SUCCESS when SetWellspringMode(FALSE) fails (= the C return value)";
const GLUE_ONLY: &str = "glue-only (WDF driver object / tracing / no-op EvtIoStop)";

/// Per-file bookkeeping: total records, records that assert P2-fixed behavior (by reason), records
/// skipped (by reason) and informational counts.
#[derive(Default, Debug)]
struct Tally {
    total: usize,
    p2: BTreeMap<&'static str, usize>,
    skipped: BTreeMap<&'static str, usize>,
    notes: BTreeMap<&'static str, usize>,
}

impl Tally {
    fn p2(&mut self, reason: &'static str) {
        *self.p2.entry(reason).or_default() += 1;
    }

    fn skip(&mut self, reason: &'static str) {
        *self.skipped.entry(reason).or_default() += 1;
    }

    fn note(&mut self, what: &'static str) {
        *self.notes.entry(what).or_default() += 1;
    }

    /// Counts the record as framework/device-decided when its failing status came from a fake.
    fn fake_status(&mut self, status: NtStatus, sources: &[&[NtStatus]]) {
        if fake_decided(status, sources) {
            self.note(FAKE_DECIDED);
        }
    }

    fn assert(
        &self,
        file: &str,
        total: usize,
        p2: &[(&'static str, usize)],
        skipped: &[(&'static str, usize)],
        fake_decided: usize,
    ) {
        println!("{file}: {} records", self.total);
        for (label, counts) in [
            ("P2", &self.p2),
            ("skipped", &self.skipped),
            ("notes", &self.notes),
        ] {
            for (reason, count) in counts {
                println!("  {label}: {reason} = {count}");
            }
        }
        assert_eq!(self.total, total, "{file}: record count");
        let p2: BTreeMap<_, _> = p2.iter().copied().collect();
        assert_eq!(self.p2, p2, "{file}: P2-fixed groups");
        let skipped: BTreeMap<_, _> = skipped.iter().copied().collect();
        assert_eq!(self.skipped, skipped, "{file}: skipped groups");
        assert_eq!(
            self.notes.get(FAKE_DECIDED).copied().unwrap_or(0),
            fake_decided,
            "{file}: records with a framework/device-decided status"
        );
    }
}

/// A started device for `pid` (config row looked up, descriptor cached with string indices
/// `strings`).
fn device(pid: u16, vid: u16, bcd: u16, strings: (u8, u8, u8)) -> DeviceState {
    let state = DeviceState::new();
    state.set_device_descriptor(&UsbDeviceDescriptor {
        id_vendor: vid,
        id_product: pid,
        bcd_device: bcd,
        i_manufacturer: strings.0,
        i_product: strings.1,
        i_serial_number: strings.2,
    });
    state.set_config_row(lookup_config_row(pid));
    state
}

fn pid_of(rec: &Rec<'_>) -> u16 {
    rec.u("pid") as u16
}

/// The glue's `EvtIoDeviceControl` for everything but `READ_REPORT`. `None` = forwarded to the
/// `InputQueue` (pending).
fn dispatch(
    state: &DeviceState,
    code: u32,
    req: &mut FakeReq,
    usb: &mut FakeUsb,
    strings: &mut FakeStrings,
) -> Option<IoctlOutcome> {
    Some(match route_ioctl(code) {
        IoctlRoute::GetDeviceDescriptor => get_hid_descriptor(state, req),
        IoctlRoute::GetReportDescriptor => get_report_descriptor(state, req),
        IoctlRoute::ReadReport => return None,
        IoctlRoute::GetString => get_string(state, req, strings),
        IoctlRoute::GetDeviceAttributes => get_device_attributes(state, req),
        IoctlRoute::SetFeature => set_feature(state, req, usb),
        IoctlRoute::GetFeature => get_feature(state, req),
        IoctlRoute::NotSupported => IoctlOutcome::NOT_SUPPORTED,
    })
}

/// Builds the request of an IOCTL record (`inlen/in`, `outlen`, output pre-filled with 0xa5).
fn ioctl_request(rec: &Rec<'_>) -> FakeReq {
    let mut input = rec.hex("in");
    assert_eq!(input.len() as u64, rec.u("inlen"), "{}: inlen", rec.case());
    let outlen = rec.u("outlen") as usize;
    // UM-B14 (P3): the C decoded SET_FEATURE 0x06 with bit 0 as the button switch and bit 1 as the
    // surface switch; the port reads bit 0 as Surface (usage 0x57) and bit 1 as Button (0x58). The port
    // reaches the recorded state from the byte with bits 0 and 1 exchanged, so the C expectations (state,
    // the D0Entry mode-switch gate, input gating) apply unchanged.
    let is_set = match rec.get("ioctl") {
        Some(code) => code == "0x000b0053",
        None => rec.get("op") == Some("SetFeature"),
    };
    if is_set
        && outlen == usize::from(ptp_hid::common::REPORTID_FUNCSWITCH)
        && let Some(byte) = input.get_mut(1)
    {
        *byte = (*byte & !0x03) | ((*byte & 0x01) << 1) | ((*byte >> 1) & 0x01);
        SELECTIVE_SWAPS.with(|swaps| swaps.set(swaps.get() + 1));
    }
    FakeReq::new(input, outlen, 0xa5)
}

thread_local! {
    /// SET 0x06 switch bytes exchanged by [`ioctl_request`] in the current test.
    static SELECTIVE_SWAPS: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
}

/// Compares an IOCTL completion with `cpl/status/info` and the output buffer with `out`.
fn check_ioctl(
    rec: &Rec<'_>,
    outcome: &IoctlOutcome,
    req: &FakeReq,
    expected_out: Option<Vec<u8>>,
) {
    let case = rec.case();
    assert_eq!(rec.s("cpl"), "1", "{case}: completed once");
    assert_eq!(
        status_hex(outcome.status),
        status_hex(rec.status("status")),
        "{case}: status"
    );
    assert_eq!(
        outcome.information as u64,
        rec.u("info"),
        "{case}: information"
    );
    if let Some(mut expected) = expected_out {
        // The C recorded only the first blob byte for successful HQA reads.
        use ptp_hid::feature::{HQA_REPORT, HQA_REPORT_LEN};
        if rec.get("in").is_some_and(|input| input.starts_with("08"))
            && outcome.status == STATUS_SUCCESS
            && outcome.information == HQA_REPORT_LEN
        {
            expected[..HQA_REPORT_LEN].copy_from_slice(&HQA_REPORT);
        }
        assert_eq!(to_hex(&req.out), to_hex(&expected), "{case}: output buffer");
    }
}

/// `out=` of an IOCTL record as a full buffer (`-` = untouched 0xa5 fill).
fn expected_ioctl_out(rec: &Rec<'_>) -> Vec<u8> {
    match rec.s("out") {
        "-" => vec![0xa5; rec.u("outlen") as usize],
        out => hex(out),
    }
}

// =================================================================================================
// devices.txt
// =================================================================================================

#[test]
fn devices() {
    let text = fixture("devices.txt");
    let mut tally = Tally::default();
    for rec in records(&text) {
        tally.total += 1;
        let case = rec.case();
        let pid = pid_of(&rec);
        let row = lookup_config_row(pid).unwrap_or_else(|| panic!("{case}: PID not in table"));
        assert_eq!(u64::from(row), rec.u("row"), "{case}: row");
        let cfg = &CONFIG_TABLE[usize::from(row)];
        assert_eq!(cfg.family.name(), rec.s("fam"), "{case}: family");
        let int = |k: &str| -> i64 {
            let v = rec.s(k);
            if v.starts_with("0x") {
                common::parse_u(v) as i64
            } else {
                rec.i(k)
            }
        };
        let (ansi, iso, jis) = cfg.product_ids;
        let format = cfg.profile.tp_type;
        let read = amtptp_usbum_core::usb::wellspring_mode_read_setup(cfg);
        for (key, value) in [
            ("ansi", i32::from(ansi)),
            ("iso", i32::from(iso)),
            ("jis", i32::from(jis)),
            ("header", format.header()),
            ("button", format.button()),
            ("fsize", format.finger_size()),
            ("delta", format.delta()),
            ("um_req_val", i32::from(read.w_value)),
            ("um_req_idx", i32::from(read.w_index)),
        ] {
            assert_eq!(int(key), i64::from(value), "{case}: {key}");
        }
        assert_eq!(format.name(), rec.s("type"), "{case}: type");
        for (key, axis) in [("x", cfg.profile.x), ("y", cfg.profile.y)] {
            let (_, bounds) = rec.s(key).split_once(',').expect("axis limits");
            assert_eq!(
                bounds,
                format!("{},{}", axis.min, axis.max),
                "{case}: {key}"
            );
        }
        assert_eq!(
            format.reader_transfer_length() as u64,
            rec.u("reader_len"),
            "{case}: reader length"
        );
        assert_eq!(
            rec.u("reader_npending"),
            u64::from(CONTINUOUS_READER_NUM_PENDING_READS)
        );
        assert_eq!(
            (rec.s("reader_hdr"), rec.s("reader_trl")),
            ("0", "0"),
            "{case}"
        );
        assert_eq!(
            (rec.s("reader_failed_cb"), rec.s("reader_bufattr")),
            ("1", "0"),
            "{case}"
        );

        // EvtDevicePrepareHardware on a fresh (zeroed) context.
        let state = DeviceState::new();
        let mut usb = FakeUsb {
            descriptor: UsbDeviceDescriptor {
                id_vendor: rec.u("vid") as u16,
                id_product: pid,
                bcd_device: rec.u("bcd") as u16,
                i_manufacturer: 1,
                i_product: 2,
                i_serial_number: 3,
            },
            ..FakeUsb::default()
        };
        let outcome = prepare_hardware(&state, &mut usb);
        assert_eq!(outcome.status, rec.status("status"), "{case}: status");
        assert_eq!(outcome.event, None, "{case}");
        assert_eq!(state.config_row(), Some(row), "{case}: DeviceInfo");
        assert_eq!(
            usb.reader_len,
            Some(cfg.profile.tp_type.reader_transfer_length()),
            "{case}"
        );
        assert_eq!(state.snapshot(), rec.state_after(), "{case}: state after");
    }
    tally.assert("devices.txt", 44, &[], &[], 0);
}

// =================================================================================================
// descriptors.txt
// =================================================================================================

#[test]
fn descriptors() {
    let text = fixture("descriptors.txt");
    let mut tally = Tally::default();
    for rec in records(&text) {
        tally.total += 1;
        let case = rec.case();
        let strings_idx = if case.starts_with("str-d1-") {
            (1, 2, 4)
        } else if case.starts_with("str-d2-") {
            (0, 0, 0)
        } else {
            (1, 2, 3)
        };
        let state = device(
            pid_of(&rec),
            rec.u("vid") as u16,
            rec.u("bcd") as u16,
            strings_idx,
        );
        if rec.flag("inj") {
            // `inj=1`: an idProduct that cannot pass PrepareHardware, written into the context.
            assert_eq!(state.config(), None, "{case}: injected PID must be unknown");
            tally.note("inj=1 (unknown PID via context injection, B3 exclusion)");
        }
        let mut req = ioctl_request(&rec);
        let mut usb = FakeUsb::default();
        let mut strings = FakeStrings::new();
        if case == "str-queryfail" {
            strings.fail_all = Some(rec.status("status"));
        }
        let code = rec.u("ioctl") as u32;
        let outcome = dispatch(&state, code, &mut req, &mut usb, &mut strings)
            .unwrap_or_else(|| panic!("{case}: not completed"));

        // B7 (P2): C copies the string into its own stack variable; the port writes it to the caller.
        let mut expected_out = expected_ioctl_out(&rec);
        if route_ioctl(code) == IoctlRoute::GetString && rec.s("calls").starts_with("ctb") {
            let index = rec
                .s("strq")
                .split('/')
                .next()
                .expect("strq")
                .parse::<u8>()
                .expect("index");
            let string = FakeStrings::utf16(FakeStrings::string(index).expect("known string"));
            expected_out[..string.len()].copy_from_slice(&string);
            tally.p2(B7_STRING);
        }
        check_ioctl(&rec, &outcome, &req, Some(expected_out));
        tally.fake_status(outcome.status, &[&req.failures, &strings.failures]);
        let events = outcome.event.map_or("-", |e| e.event_name());
        assert_eq!(events, rec.s("tl"), "{case}: TraceLogging");
        assert_eq!(
            strings.queries_string(),
            rec.s("strq"),
            "{case}: string queries"
        );
    }
    tally.assert("descriptors.txt", 898, &[(B7_STRING, 35)], &[], 372);
    assert_eq!(
        tally
            .notes
            .get("inj=1 (unknown PID via context injection, B3 exclusion)"),
        Some(&24)
    );
}

// =================================================================================================
// ioctl.txt
// =================================================================================================

#[test]
fn ioctl_routing() {
    let text = fixture("ioctl.txt");
    let mut tally = Tally::default();
    for rec in records(&text) {
        tally.total += 1;
        let case = rec.case();
        let state = device(pid_of(&rec), 0x05ac, 0x0105, (1, 2, 3));
        state.restore(&StateSnapshot::default());
        let mut req = ioctl_request(&rec);
        let mut usb = FakeUsb::default();
        let mut strings = FakeStrings::new();
        let code = rec.u("ioctl") as u32;
        match dispatch(&state, code, &mut req, &mut usb, &mut strings) {
            None => {
                // READ_REPORT: forwarded to the InputQueue (the fake forward always succeeds).
                assert_eq!(
                    (
                        rec.s("cpl"),
                        rec.s("status"),
                        rec.s("queued"),
                        rec.s("inputq")
                    ),
                    ("0", "-", "1", "1"),
                    "{case}"
                );
                assert_eq!(rec.u("info"), 0, "{case}");
            }
            Some(outcome) => {
                assert_eq!((rec.s("queued"), rec.s("inputq")), ("0", "0"), "{case}");
                check_ioctl(&rec, &outcome, &req, Some(expected_ioctl_out(&rec)));
                tally.fake_status(outcome.status, &[&req.failures]);
            }
        }
        assert_eq!(usb.wire_string(), "-", "{case}: no device I/O");
    }
    tally.assert("ioctl.txt", 24, &[], &[], 3);
}

// =================================================================================================
// feature.txt
// =================================================================================================

#[test]
fn features() {
    let text = fixture("feature.txt");
    let mut tally = Tally::default();
    for rec in records(&text) {
        tally.total += 1;
        let case = rec.case();
        let state = device(pid_of(&rec), 0x05ac, 0x0105, (1, 2, 3));
        assert_eq!(
            state.config().map(|c| c.family.name()),
            Some(rec.s("fam")),
            "{case}: family"
        );
        state.restore(&rec.state_before());
        let mut usb = FakeUsb::from_rec(&rec);
        let mut strings = FakeStrings::new();
        let mut req = ioctl_request(&rec);
        let code = rec.u("ioctl") as u32;
        let outcome = dispatch(&state, code, &mut req, &mut usb, &mut strings)
            .unwrap_or_else(|| panic!("{case}: not completed"));
        let expected_out = if rec.has("out") {
            Some(expected_ioctl_out(&rec))
        } else {
            // SET_FEATURE: the output buffer is never written (only its length is used).
            assert!(!req.out_accessed, "{case}: SET touched the output buffer");
            None
        };
        check_ioctl(&rec, &outcome, &req, expected_out);
        tally.fake_status(outcome.status, &[&req.failures, &usb.failures]);
        let (wire, masked) = normalize_wire(case, rec.s("wire"));
        if masked > 0 {
            tally.p2(B1_UMASK);
        }
        assert_eq!(usb.wire_string(), wire, "{case}: wire");
        check_dev2(&rec, &usb);
        assert_eq!(state.snapshot(), rec.state_after(), "{case}: state after");
    }
    tally.assert("feature.txt", 1050, &[(B1_UMASK, 8)], &[], 20);
    assert_eq!(SELECTIVE_SWAPS.with(core::cell::Cell::get), 258, "SET 0x06 bytes exchanged");
}

// =================================================================================================
// input_<fam>.txt + packets_t<N>.txt
// =================================================================================================

fn packets() -> HashMap<String, Vec<u8>> {
    let mut map = HashMap::new();
    for (n, count) in [(1, 300), (2, 1000), (3, 1000), (4, 1000), (5, 1000)] {
        let text = fixture(&format!("packets_t{n}.txt"));
        let recs = records(&text);
        assert_eq!(recs.len(), count, "packets_t{n}.txt: record count");
        for (i, rec) in recs.iter().enumerate() {
            let name = rec.s("pkt");
            assert_eq!(name, format!("t{n}:{i}"), "packets_t{n}.txt: index");
            let bytes = rec.hex("in");
            assert_eq!(bytes.len() as u64, rec.u("len"), "{name}: len");
            map.insert(name.to_owned(), bytes);
        }
    }
    map
}

/// Runs one `Input` record (input files and sequences) on `state`/`usb` and checks everything it
/// records. `lengths` are the pending `READ_REPORT` output lengths.
fn run_input(
    rec: &Rec<'_>,
    state: &DeviceState,
    usb: &mut FakeUsb,
    packet: &[u8],
    lengths: &[usize],
    tally: &mut Tally,
) {
    let case = rec.case();
    let mut queue = FakeQueue::with_lengths(lengths);
    let qpc = rec.i("qpc");
    let mut qpc_calls = 0;
    let frequency = state.qpc_frequency();
    on_read_complete(state, packet, &mut queue, usb, || {
        qpc_calls += 1;
        qpc
    });
    assert_eq!(
        state.qpc_frequency(),
        frequency,
        "{case}: cached QPC frequency never changes"
    );
    assert_eq!(
        qpc_calls,
        rec.u("qpcn"),
        "{case}: QueryPerformanceCounter calls"
    );
    let (wire, masked) = normalize_wire(case, rec.s("wire"));
    if masked > 0 {
        tally.p2(B1_UMASK);
    }
    assert_eq!(usb.wire_string(), wire, "{case}: wire");
    check_dev2(rec, usb);
    if rec.has("undef") {
        // B13: `out` is the run with the C stack poisoned with 0x00 = the zero-initialized report.
        tally.note("undef (B13 zero-initialized report compared directly)");
    }
    match rec.s("done") {
        "c" => {
            assert_eq!(queue.completed.len(), 1, "{case}: one completion");
            let (req, status, info) = &queue.completed[0];
            assert_eq!((*status, *info), (STATUS_SUCCESS, 50), "{case}: completion");
            let mut expected = rec.hex("out");
            // Replays are single frames or use the same QPC sample throughout a sequence.
            let offset = ptp_hid::input::PTP_REPORT_SCAN_TIME_OFFSET;
            expected[offset..offset + 2].copy_from_slice(&0_u16.to_le_bytes());
            assert_eq!(&req.out[..50], expected, "{case}: report");
            assert!(
                req.out[50..].iter().all(|&b| b == 0xee),
                "{case}: bytes past 50 written"
            );
        }
        "l" => {
            // B6 (P2): C leaks the retrieved request; the port completes it with the failing status.
            assert_eq!(queue.completed.len(), 1, "{case}: one completion");
            let (req, status, info) = &queue.completed[0];
            assert_eq!(
                status_hex(*status),
                status_hex(rec.status("err")),
                "{case}: status"
            );
            assert_eq!(*info, 0, "{case}: information");
            assert!(
                req.out.iter().all(|&b| b == 0xee),
                "{case}: nothing written"
            );
            tally.fake_status(*status, &[&req.failures]);
            tally.p2(B6_LEAK);
        }
        "q" => {
            assert!(queue.completed.is_empty(), "{case}: nothing completed");
            assert_eq!(
                queue.pending.len(),
                lengths.len(),
                "{case}: head stays queued"
            );
        }
        "-" => {
            assert!(lengths.is_empty(), "{case}: no request was pending");
            assert!(queue.completed.is_empty(), "{case}");
        }
        other => panic!("{case}: bad done={other}"),
    }
    // Requests other than the head are never touched.
    for req in &queue.pending {
        assert!(
            !req.out_accessed && req.failures.is_empty() && req.out.iter().all(|&b| b == 0xee),
            "{case}: a non-head request was touched"
        );
    }
    if let Some(rem) = rec.get("rem") {
        assert_eq!(
            queue.pending.len().to_string(),
            rem,
            "{case}: requests left"
        );
    }
}

fn st_flags(st: &str) -> (bool, bool, bool) {
    let b = st.as_bytes();
    assert_eq!(b.len(), 3, "st={st}");
    (b[0] == b'1', b[1] == b'1', b[2] == b'1')
}

#[test]
fn input_reports() {
    let packets = packets();
    let mut used_packets = 0;
    // (file, family, records, done=l (B6), records with a umask transfer (B1))
    let files: [(&str, Family, usize, usize, usize); 15] = [
        ("input_ws1.txt", Family::Ws1, 340, 0, 0),
        ("input_ws2.txt", Family::Ws2, 70, 0, 0),
        ("input_ws3.txt", Family::Ws3, 1128, 14, 0),
        ("input_ws4.txt", Family::Ws4, 287, 5, 0),
        ("input_ws4a.txt", Family::Ws4a, 287, 5, 0),
        ("input_ws5.txt", Family::Ws5, 287, 5, 0),
        ("input_ws5a.txt", Family::Ws5a, 287, 5, 0),
        ("input_ws6.txt", Family::Ws6, 120, 1, 0),
        ("input_ws6a.txt", Family::Ws6a, 120, 1, 0),
        ("input_ws7.txt", Family::Ws7, 120, 1, 0),
        ("input_ws7a.txt", Family::Ws7a, 120, 1, 0),
        ("input_ws8.txt", Family::Ws8, 1118, 20, 0),
        ("input_ws9.txt", Family::Ws9, 1129, 16, 6),
        ("input_mt2.txt", Family::Mt2, 1135, 15, 8),
        ("input_t2.txt", Family::T2, 121, 1, 0),
    ];
    let mut total = 0;
    for (file, family, count, leaked, masked) in files {
        let text = fixture(file);
        let row = CONFIG_TABLE
            .iter()
            .position(|c| c.family == family)
            .expect("family row");
        let cfg = &CONFIG_TABLE[row];
        let mut tally = Tally::default();
        for rec in records(&text) {
            tally.total += 1;
            let case = rec.case();
            let packet = match rec.get("pkt") {
                Some(name) => {
                    used_packets += 1;
                    packets
                        .get(name)
                        .unwrap_or_else(|| panic!("{case}: unknown packet {name}"))
                        .clone()
                }
                None => rec.hex("in"),
            };
            assert_eq!(packet.len() as u64, rec.u("len"), "{case}: len");
            // Fresh device for the row's ANSI PID, then the record's state.
            let state = device(cfg.ansi_pid(), 0x05ac, 0x0105, (1, 2, 3));
            let (wsm, surf, btn) = st_flags(rec.s("st"));
            state.set_wellspring_mode_on(wsm);
            state.set_surface_report_on(surf);
            state.set_button_report_on(btn);
            state.set_qpc_frequency(rec.i("perf"));
            let mut usb = FakeUsb::from_rec(&rec);
            let lengths: Vec<usize> = match rec.s("req") {
                "-" => Vec::new(),
                list => list.split(',').map(|n| n.parse().expect("req")).collect(),
            };
            run_input(&rec, &state, &mut usb, &packet, &lengths, &mut tally);
            let (wsm2, surf2, btn2) = st_flags(rec.s("st2"));
            assert_eq!(
                (
                    state.is_wellspring_mode_on(),
                    state.is_surface_report_on(),
                    state.is_button_report_on()
                ),
                (wsm2, surf2, btn2),
                "{case}: flags after"
            );
        }
        let mut p2 = Vec::new();
        if leaked > 0 {
            p2.push((B6_LEAK, leaked));
        }
        if masked > 0 {
            p2.push((B1_UMASK, masked));
        }
        // Every B6 completion status is a framework status (RetrieveOutputMemory / CopyFromBuffer).
        tally.assert(file, count, &p2, &[], leaked);
        total += tally.total;
    }
    assert_eq!(total, 6669, "input_* record total");
    assert_eq!(used_packets, 5400, "pkt= references");
}

// =================================================================================================
// sequences.txt
// =================================================================================================

#[test]
fn sequences() {
    let text = fixture("sequences.txt");
    let mut tally = Tally::default();
    let mut current: Option<(String, DeviceState, FakeUsb)> = None;
    let mut sequences = 0;
    for rec in records(&text) {
        tally.total += 1;
        let case = rec.case();
        let seq = rec.s("seq");
        if seq.starts_with("b10") {
            // UM-B10 (P3): these sequences show the trackpad left in mouse mode after a D0Entry with the
            // button switch off; the port switches the mode on in every D0Entry.
            tally.skip(B10_SEQUENCES);
            continue;
        }
        let pid = pid_of(&rec);
        if current.as_ref().is_none_or(|(name, _, _)| name != seq) {
            assert_eq!(rec.s("step"), "0", "{case}: sequence starts at step 0");
            let state = device(pid, 0x05ac, 0x0105, (1, 2, 3));
            state.restore(&rec.state_before());
            current = Some((seq.to_owned(), state, FakeUsb::from_rec(&rec)));
            sequences += 1;
        }
        let (_, state, usb) = current.as_mut().expect("sequence");
        // The state and the device register carry over between steps.
        assert_eq!(
            state.snapshot(),
            rec.state_before(),
            "{case}: carried state"
        );
        let (reg, resp) = rec.s("dev").split_once('/').expect("dev");
        assert_eq!(to_hex(&usb.reg), reg, "{case}: carried device register");
        usb.resp = resp.parse().expect("resp");
        usb.set_script(rec.get("xs"));
        usb.wire.clear();
        usb.failures.clear();
        match rec.s("op") {
            "Input" => {
                let packet = rec.hex("in");
                run_input(&rec, state, usb, &packet, &[50], &mut tally);
            }
            "SetFeature" | "GetFeature" => {
                let mut req = ioctl_request(&rec);
                let mut strings = FakeStrings::new();
                let code = if rec.s("op") == "SetFeature" {
                    0x000b_0053
                } else {
                    0x000b_0057
                };
                let outcome =
                    dispatch(state, code, &mut req, usb, &mut strings).expect("completed");
                let expected_out = rec.has("out").then(|| expected_ioctl_out(&rec));
                check_ioctl(&rec, &outcome, &req, expected_out);
                tally.fake_status(outcome.status, &[&req.failures, &usb.failures]);
                assert_eq!(
                    usb.wire_string(),
                    normalize_wire(case, rec.s("wire")).0,
                    "{case}"
                );
            }
            "D0Entry" => {
                let qpc = rec.i("qpc");
                let outcome = d0_entry(state, usb, || qpc);
                assert_eq!(outcome.status, rec.status("status"), "{case}: status");
                tally.fake_status(outcome.status, &[&usb.failures]);
                assert_eq!(
                    usb.wire_string(),
                    normalize_wire(case, rec.s("wire")).0,
                    "{case}"
                );
            }
            "D0Exit" => {
                let outcome = d0_exit(state, usb);
                assert_eq!(outcome.status, STATUS_SUCCESS, "{case}: D0Exit always succeeds");
                // C returned the mode-switch status (UM-B9); compare it with that.
                let status = outcome.mode_switch_status;
                if !nt_success(status) {
                    tally.p2(B9_D0_EXIT);
                }
                assert_eq!(
                    status_hex(status),
                    status_hex(rec.status("status")),
                    "{case}"
                );
                tally.fake_status(status, &[&usb.failures]);
                assert_eq!(
                    usb.wire_string(),
                    normalize_wire(case, rec.s("wire")).0,
                    "{case}"
                );
            }
            other => panic!("{case}: unknown op {other}"),
        }
        check_dev2(&rec, usb);
        assert_eq!(state.snapshot(), rec.state_after(), "{case}: state after");
    }
    assert_eq!(sequences, 5, "sequence count");
    tally.assert("sequences.txt", 48, &[], &[(B10_SEQUENCES, 12)], 0);
    // The only SET 0x06 records of this file are in the B10 sequences.
    assert_eq!(SELECTIVE_SWAPS.with(core::cell::Cell::get), 0, "SET 0x06 bytes exchanged");
}

// =================================================================================================
// wire.txt
// =================================================================================================

/// The fake USB configuration of a `PrepareHardware` record (pipe layouts by case name, README
/// "pipe-selection variants").
fn prepare_usb(rec: &Rec<'_>, restart: bool) -> FakeUsb {
    use PipeType::{Bulk, Control, Interrupt, Isochronous};
    let case = rec.case();
    let mut usb = FakeUsb {
        descriptor: UsbDeviceDescriptor {
            id_vendor: 0x05ac,
            id_product: pid_of(rec),
            bcd_device: 0x0105,
            i_manufacturer: 1,
            i_product: 2,
            i_serial_number: 3,
        },
        ..FakeUsb::default()
    };
    // Endpoints 0x81, 0x82, ... (IN), except the OUT endpoint 0x03 of `prep-intout-then-intin`.
    let ins = |types: &[PipeType]| -> Vec<PipeInfo> {
        (0_u8..)
            .zip(types)
            .map(|(i, &t)| common::pipe(t, 0x81 + i))
            .collect()
    };
    let pipes = match case {
        "prep-iface-null" => None,
        "prep-no-pipes" => Some(vec![]),
        "prep-bulk-only" => Some(ins(&[Bulk, Bulk])),
        "prep-bulk-then-int" => Some(ins(&[Bulk, Interrupt])),
        "prep-intout-then-intin" => Some(vec![
            common::pipe(Interrupt, 0x03),
            common::pipe(Interrupt, 0x83),
        ]),
        "prep-int-then-bulk" => Some(ins(&[Interrupt, Bulk, Bulk])),
        "prep-control-iso-int" => Some(ins(&[Control, Isochronous, Interrupt])),
        _ => Some(ins(&[Interrupt])),
    };
    usb.set_pipes(pipes);
    if case == "prep-info-fail" {
        usb.traits = Err(amtptp_usbum_core::status::STATUS_UNSUCCESSFUL);
    }
    if case == "prep-usbdev-create-fail" {
        usb.create_status = rec.status("status");
    }
    if restart {
        // PnP stop/start: the USB target device and the interrupt pipe handle survive.
        usb.usb_created = true;
        usb.interrupt_pipe = Some(0);
    }
    usb
}

#[test]
fn lifecycle() {
    let text = fixture("wire.txt");
    let all = records(&text);
    let mut tally = Tally::default();
    for rec in records(&text) {
        tally.total += 1;
        let case = rec.case();
        // UM-B10 (P3): with the button switch off and the mode off, C skipped the D0Entry mode switch. The
        // port switches anyway, which is what the record with the mode on (same device register) shows.
        let gate_companion = case
            .strip_suffix("-d0entry-gate-btn0-wsm0")
            .map(|family| format!("{family}-d0entry-gate-btn0-wsm1"))
            .map(|companion| {
                all.iter()
                    .find(|r| r.case() == companion)
                    .unwrap_or_else(|| panic!("{case}: {companion}"))
            });
        let expect = gate_companion.unwrap_or(&rec);
        match rec.s("op") {
            "EvtDeviceAdd" if case == "deviceadd" => {
                let (d1, d2, d3, d4) = GUID_DEVINTERFACE_AMTPTP_DEVICE;
                let guid = format!(
                    "{d1:08x}-{d2:04x}-{d3:04x}-{}-{}",
                    to_hex(&d4[..2]),
                    to_hex(&d4[2..])
                );
                assert_eq!(guid, rec.s("devif"), "{case}: device interface GUID");
            }
            "DriverEntry" | "EvtDriverContextCleanup" | "EvtIoStop" | "EvtDeviceAdd" => {
                tally.skip(GLUE_ONLY);
            }
            "EvtUsbTargetPipeReadersFailed" => {
                assert_eq!(rec.flag("ret"), READERS_FAILED_RESULT, "{case}");
            }
            "PrepareHardware" => {
                let restart = case.ends_with("prep2");
                let state = device(pid_of(&rec), 0x05ac, 0x0105, (1, 2, 3));
                state.set_config_row(None);
                state.restore(&rec.state_before());
                state.set_reader_configured(restart);
                let mut usb = prepare_usb(&rec, restart);
                let outcome = prepare_hardware(&state, &mut usb);
                assert_eq!(outcome.event, None, "{case}");
                let calls = usb.calls.join(",");
                if restart {
                    // B16 (P2): the reader is configured once; the restart succeeds. C fails with
                    // STATUS_INVALID_DEVICE_STATE from the second ConfigContinuousReader.
                    assert_eq!(rec.status("status"), STATUS_INVALID_DEVICE_STATE, "{case}");
                    let c_calls = rec.s("calls");
                    let expected = c_calls
                        .strip_suffix(",ConfigContinuousReader(0)=0xc0000184")
                        .unwrap_or_else(|| panic!("{case}: unexpected C calls {c_calls}"));
                    assert_eq!(calls, expected, "{case}: calls");
                    assert_eq!(outcome.status, STATUS_SUCCESS, "{case}: restart succeeds");
                    let mut after = rec.state_before();
                    after.surface_report_on = true;
                    after.button_report_on = true;
                    assert_eq!(state.snapshot(), after, "{case}: state after");
                    tally.p2(B16_RESTART);
                } else if case == "prep-intout-then-intin" {
                    // B21 (P3): C configured the reader on the interrupt OUT pipe 0.
                    assert_eq!(rec.i("pipe"), 0, "{case}: C pipe");
                    assert_eq!(rec.s("nomaxpkt"), "1,0", "{case}: C visits");
                    assert_eq!(outcome.status, rec.status("status"), "{case}: status");
                    let expected = rec.s("calls").replace(
                        "SetNoMaxPktCheck(0),ConfigContinuousReader(0)",
                        "SetNoMaxPktCheck(0),GetConfiguredPipe(1),SetNoMaxPktCheck(1),ConfigContinuousReader(1)",
                    );
                    assert_eq!(calls, expected, "{case}: calls");
                    assert_eq!(state.snapshot(), rec.state_after(), "{case}: state after");
                    assert_eq!(usb.interrupt_pipe, Some(1), "{case}: interrupt pipe");
                    assert_eq!(usb.visited, [true, true], "{case}: visits");
                    tally.p2(B21_INTERRUPT_IN);
                    continue;
                } else {
                    assert_eq!(
                        status_hex(outcome.status),
                        status_hex(rec.status("status")),
                        "{case}: status"
                    );
                    tally.fake_status(outcome.status, &[&usb.failures]);
                    assert_eq!(calls, rec.s("calls"), "{case}: calls");
                    assert_eq!(state.snapshot(), rec.state_after(), "{case}: state after");
                }
                let pipe = usb.interrupt_pipe.map_or(-1, i64::from);
                assert_eq!(pipe, rec.i("pipe"), "{case}: interrupt pipe");
                assert_eq!(
                    u64::from(state.usb_device_traits()),
                    rec.u("traits"),
                    "{case}: traits"
                );
                assert_eq!(rec.flag("usbdev"), usb.usb_created, "{case}: UsbDevice");
                let nomaxpkt = if usb.pipes.as_ref().is_some_and(std::vec::Vec::is_empty) {
                    "-".to_owned()
                } else {
                    usb.visited
                        .iter()
                        .map(|&v| if v { "1" } else { "0" })
                        .collect::<Vec<_>>()
                        .join(",")
                };
                assert_eq!(
                    nomaxpkt,
                    rec.s("nomaxpkt"),
                    "{case}: SetNoMaximumPacketSizeCheck"
                );
                assert_eq!(rec.s("tl"), "-", "{case}");
                assert_eq!(rec.u("qpcn"), 0, "{case}");
            }
            op @ ("D0Entry" | "D0Exit") => {
                let state = device(pid_of(&rec), 0x05ac, 0x0105, (1, 2, 3));
                state.restore(&rec.state_before());
                let mut usb = FakeUsb::from_rec(&rec);
                if let Some(companion) = gate_companion {
                    assert_eq!(rec.s("dev"), companion.s("dev"), "{case}: same device register");
                    assert_eq!(rec.s("wire"), "-", "{case}: C skipped the switch");
                    tally.p2(B10_GATE);
                }
                // The framework's WdfIoTargetStart result is part of the record.
                if let Some(start) = expect
                    .s("calls")
                    .split(',')
                    .find_map(|c| c.strip_prefix("IoTargetStart(pipe0)="))
                {
                    usb.start_status = common::parse_status(start);
                }
                let mut qpc_calls = 0;
                let status = if op == "D0Entry" {
                    let qpc = rec.i("qpc");
                    d0_entry(&state, &mut usb, || {
                        qpc_calls += 1;
                        qpc
                    })
                    .status
                } else {
                    let outcome = d0_exit(&state, &mut usb);
                    assert_eq!(outcome.status, STATUS_SUCCESS, "{case}: D0Exit always succeeds");
                    // C returned the mode-switch status (UM-B9); compare it with that.
                    if !nt_success(outcome.mode_switch_status) {
                        tally.p2(B9_D0_EXIT);
                    }
                    outcome.mode_switch_status
                };
                assert_eq!(
                    status_hex(status),
                    status_hex(rec.status("status")),
                    "{case}"
                );
                tally.fake_status(status, &[&usb.failures]);
                assert_eq!(qpc_calls, rec.u("qpcn"), "{case}: QPC calls");
                let (wire, masked) = normalize_wire(case, expect.s("wire"));
                if masked > 0 {
                    tally.p2(B1_UMASK);
                }
                assert_eq!(usb.wire_string(), wire, "{case}: wire");
                check_dev2(expect, &usb);
                // Framework calls without the C WdfMemoryCreate/WdfObjectDelete pair (no allocation).
                let expected_calls = expect
                    .s("calls")
                    .split(',')
                    .filter(|c| *c != "del" && !c.starts_with("mc"))
                    .collect::<Vec<_>>()
                    .join(",");
                assert_eq!(usb.calls.join(","), expected_calls, "{case}: calls");
                let mut after = expect.state_after();
                // The reporting clock comes from this record's own QPC sample.
                after.qpc_frequency = rec.state_after().qpc_frequency;
                assert_eq!(state.snapshot(), after, "{case}: state after");
                assert_eq!(rec.s("tl"), "-", "{case}");
            }
            other => panic!("{case}: unknown op {other}"),
        }
    }
    tally.assert(
        "wire.txt",
        259,
        &[
            (B1_UMASK, 16),
            (B10_GATE, 5),
            (B16_RESTART, 16),
            (B21_INTERRUPT_IN, 1),
            (B9_D0_EXIT, 18),
        ],
        &[(GLUE_ONLY, 5)],
        20,
    );
}
