//! Shared fixture parsing and a mock of the framework that follows the C oracle's stub model
//! (`tests/bluetooth/fixtures/README.md` section 5). Every trait call is recorded in the fixtures' `ev` format.
#![allow(
    dead_code,
    reason = "Each integration test target uses a different subset of these helpers"
)]

use amtptp_core::hid::hidport::with_xfer_packet;
use core::fmt::Write as _;
use std::collections::VecDeque;

use amtptp_usb_bt::bluetooth::{
    Bcm5974Param, CompletionQueue, ContactTracker, DeviceSnapshot, DeviceState, EnqueueOutcome,
    NtStatus, PostSendAction, ReadTicket, ReadTracker, XferPacket, XferPacketMut,
};

// ------------------------------------------------------------------------------------------------
// Fixture files
// ------------------------------------------------------------------------------------------------

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

/// Information preset on every upper request (README 2.2).
pub const INFO_IN: usize = 23130;

// Fault statuses (README 5).
pub const RESOURCE_FAIL: NtStatus = 0xC000_009A_u32 as i32;
pub const FORMAT_FAIL: NtStatus = 0xC000_0184_u32 as i32;
pub const SEND_FAIL: NtStatus = 0xC000_009D_u32 as i32;
pub const EXEC_LEVEL_FAIL: NtStatus = 0xC020_0225_u32 as i32;
pub const TRUE_STATUS_FAIL: NtStatus = 0xC000_0001_u32 as i32;
const BUFFER_TOO_SMALL: NtStatus = 0xC000_0023_u32 as i32;
const NO_MORE_ENTRIES: NtStatus = 0x8000_001A_u32 as i32;

// ------------------------------------------------------------------------------------------------
// Mock framework
// ------------------------------------------------------------------------------------------------

/// The `HID_XFER_PACKET` behind `Irp->UserBuffer` of a feature request.
#[derive(Debug, Clone)]
pub struct Feature {
    pub rid: u8,
    /// Physical buffer (at least `rblen` bytes).
    pub buf: Vec<u8>,
    pub rblen: usize,
}

/// An upper request presented to the dispatcher.
#[derive(Debug, Clone)]
pub struct Upper {
    /// The caller's output buffer (`UserBuffer`), possibly longer than `out_len`.
    pub buf: Vec<u8>,
    /// `OutputBufferLength`.
    pub out_len: usize,
    pub info: usize,
    pub completed: Option<(NtStatus, usize)>,
    /// `None` models `Irp->UserBuffer == NULL` for feature requests.
    pub feature: Option<Feature>,
}

impl Upper {
    pub fn with_output(out_len: usize, fill: u8) -> Self {
        Self {
            buf: vec![fill; out_len],
            out_len,
            info: INFO_IN,
            completed: None,
            feature: None,
        }
    }
}

/// Fault injection (README 5 statuses).
#[derive(Debug, Default, Clone)]
pub struct Faults {
    pub forward: Option<NtStatus>,
    /// `copy_string`'s result: `Ok(bytes)` or the failure status (default: no strings yet).
    pub string: Option<Result<usize, NtStatus>>,
    pub lower_create: Option<NtStatus>,
    pub lookaside: Option<NtStatus>,
    pub lower_format: Option<NtStatus>,
    pub lower_send_false: bool,
    pub cfg_create: Option<NtStatus>,
    pub cfg_mem: Option<NtStatus>,
    pub cfg_format: Option<NtStatus>,
    pub irp_null: bool,
    pub cfg_send_false: Option<NtStatus>,
    pub cfg_send_true_status: Option<NtStatus>,
    /// `AddDevice`: (event name of the failing create, status).
    pub add: Option<(&'static str, NtStatus)>,
    pub wdm_null: bool,
}

/// One recorded mode-switch send.
#[derive(Debug, Clone)]
pub struct Send {
    pub in_is_userbuffer: bool,
    pub xfer_id: u8,
    pub xfer_len: u32,
    pub rb_off: u64,
    pub payload: Vec<u8>,
    pub raw32: Vec<u8>,
}

#[derive(Debug, Default)]
#[expect(
    clippy::partial_pub_fields,
    reason = "Fixtures set mock inputs directly while transfer bookkeeping stays private"
)]
pub struct Mock {
    pub ev: Vec<String>,
    pub uppers: Vec<Upper>,
    pub read_queue: VecDeque<usize>,
    pub faults: Faults,
    pub freq: i64,
    pub counter: i64,
    pub lower_buf: Vec<u8>,
    pub lower_reads_pending: i64,
    pub sends: Vec<Send>,
    /// The lower-read tracker (not in the C oracle, so its calls are not recorded in `ev`).
    pub tracker: ReadTracker<usize>,
    /// Per-device contact lifecycle tracker (not in the C oracle).
    pub contacts: ContactTracker,
    /// FIFO serializer for upper `READ_REPORT` completions (not in the C oracle).
    pub completions: CompletionQueue<(usize, NtStatus)>,
    /// `WdfTimerStop` calls (not in the C oracle).
    pub timer_stops: usize,
    /// Reads `cancel_in_flight_reads` cancelled (not in the C oracle).
    pub cancelled_reads: usize,
    /// Every lower read sent (its id) with its ticket, in order.
    pub tickets: Vec<(usize, ReadTicket)>,
    /// The id the next lower read gets (ids keep the reads apart in the tracker; the recorded events
    /// name every lower read `lower_read`, as the C oracle does).
    next_lower_read: usize,
    cfg_mem_addr: Option<usize>,
    cfg_user_buffer: Option<(usize, [u8; 32])>,
}

impl Mock {
    fn fdo_init_set_filter(&mut self) {
        self.rec("WdfFdoInitSetFilter".into());
    }

    fn set_pnp_power_event_callbacks(&mut self) {
        self.rec("WdfDeviceInitSetPnpPowerEventCallbacks".into());
    }

    fn create_device(&mut self) -> NtStatus {
        self.add_step("WdfDeviceCreate")
    }

    fn store_wdm_device_object(&mut self) -> bool {
        self.rec("WdfDeviceWdmGetDeviceObject".into());
        !self.faults.wdm_null
    }

    fn create_read_buffer_lookaside(&mut self) -> NtStatus {
        self.add_step("WdfLookasideListCreate")
    }

    fn create_recovery_timer(&mut self) -> NtStatus {
        self.add_step("WdfTimerCreate")
    }

    fn create_recovery_work_item(&mut self) -> NtStatus {
        self.add_step("WdfWorkItemCreate")
    }

    fn create_default_queue(&mut self) -> NtStatus {
        self.add_step("WdfIoQueueCreate")
    }

    fn create_read_queue(&mut self) -> NtStatus {
        let s = match self.faults.add {
            Some(("ReadQueue", s)) => s,
            _ => 0,
        };
        self.rec(format!("WdfIoQueueCreate={}", st(s)));
        s
    }

    fn add_step(&mut self, name: &'static str) -> NtStatus {
        let s = match self.faults.add {
            Some((n, s)) if n == name => s,
            _ => 0,
        };
        self.rec(format!("{name}={}", st(s)));
        s
    }

    pub fn new() -> Self {
        Self {
            freq: 10_000_000,
            counter: 10_000_000,
            lower_buf: vec![0; 1024],
            ..Self::default()
        }
    }

    /// Lower reads of the current epoch in flight.
    pub fn in_flight(&self) -> usize {
        self.tracker.in_flight()
    }

    pub fn add_upper(&mut self, u: Upper) -> usize {
        self.uppers.push(u);
        self.uppers.len() - 1
    }

    fn rec(&mut self, e: String) {
        self.ev.push(e);
    }

    fn up(req: usize) -> String {
        format!("upper{}", req + 1)
    }

    fn complete(&mut self, request: usize, status: NtStatus) {
        let info = self.uppers[request].info;
        assert!(
            self.uppers[request].completed.is_none(),
            "request completed twice"
        );
        self.uppers[request].completed = Some((status, info));
        self.rec(format!(
            "WdfRequestComplete(req={},status={},info={info})",
            Self::up(request),
            st(status)
        ));
    }

    fn set_information(&mut self, request: usize, information: usize) {
        self.uppers[request].info = information;
        self.rec(format!(
            "WdfRequestSetInformation(req={},info={information})",
            Self::up(request)
        ));
    }

    fn retrieve_output_memory(&mut self, request: usize) -> Result<usize, NtStatus> {
        let r = if self.uppers[request].out_len == 0 {
            Err(BUFFER_TOO_SMALL)
        } else {
            Ok(request)
        };
        let s = r.err().unwrap_or(0);
        self.rec(format!(
            "WdfRequestRetrieveOutputMemory(req={})={}",
            Self::up(request),
            st(s)
        ));
        r
    }

    fn copy_to_memory(&mut self, memory: usize, source: &[u8]) -> NtStatus {
        let u = &mut self.uppers[memory];
        let s = if source.len() > u.out_len {
            BUFFER_TOO_SMALL
        } else {
            u.buf[..source.len()].copy_from_slice(source);
            0
        };
        self.rec(format!(
            "WdfMemoryCopyFromBuffer(mem=upper_mem,off=0,len={})={}",
            source.len(),
            st(s)
        ));
        s
    }

    fn retrieve_output_buffer(
        &mut self,
        request: usize,
        minimum_length: usize,
    ) -> Result<&mut [u8], NtStatus> {
        let out_len = self.uppers[request].out_len;
        let s = if out_len == 0 || out_len < minimum_length {
            BUFFER_TOO_SMALL
        } else {
            0
        };
        self.rec(format!(
            "WdfRequestRetrieveOutputBuffer(req={},min={minimum_length},len_ptr=null)={}",
            Self::up(request),
            st(s)
        ));
        if s != 0 {
            return Err(s);
        }
        Ok(&mut self.uppers[request].buf[..out_len])
    }

    fn get_feature_packet(&mut self, request: usize) -> Option<XferPacketMut<'_>> {
        self.uppers[request]
            .feature
            .as_mut()
            .map(|f| XferPacketMut {
                report_id: f.rid,
                report_buffer: &mut f.buf[..f.rblen],
            })
    }

    fn set_feature_packet(&self, request: usize) -> Option<XferPacket<'_>> {
        self.uppers[request].feature.as_ref().map(|f| XferPacket {
            report_id: f.rid,
            report_buffer: &f.buf[..f.rblen],
        })
    }

    fn forward_to_read_queue(&mut self, request: usize) -> NtStatus {
        let s = self.faults.forward.unwrap_or(0);
        if s == 0 {
            self.read_queue.push_back(request);
        }
        self.rec(format!(
            "WdfRequestForwardToIoQueue(req={},queue=queue_read)={}",
            Self::up(request),
            st(s)
        ));
        s
    }

    fn retrieve_next_parked_read(&mut self) -> Result<usize, NtStatus> {
        if let Some(r) = self.read_queue.pop_front() {
            self.rec(format!(
                "WdfIoQueueRetrieveNextRequest(queue=queue_read)=0x00000000:{}",
                Self::up(r)
            ));
            Ok(r)
        } else {
            self.rec(format!(
                "WdfIoQueueRetrieveNextRequest(queue=queue_read)={}",
                st(NO_MORE_ENTRIES)
            ));
            Err(NO_MORE_ENTRIES)
        }
    }

    fn parked_read_count(&self) -> usize {
        // A pure query: the C oracle logs no getters.
        self.read_queue.len()
    }

    fn copy_string(&mut self, request: usize) -> Result<usize, NtStatus> {
        let result = self
            .faults
            .string
            .unwrap_or(Err(amtptp_usb_bt::bluetooth::STATUS_INVALID_DEVICE_STATE));
        self.rec(format!("CopyString(req={})", Self::up(request)));
        result
    }

    fn create_lower_read(&mut self) -> Result<usize, NtStatus> {
        let s = self.faults.lower_create.unwrap_or(0);
        self.rec(format!("WdfRequestCreate(ctx=WORKER_REQUEST_CONTEXT,parent=device,exec=inherit,sync=inherit,cleanup=null,destroy=null,target=target)={}", st(s)));
        if s != 0 {
            return Err(s);
        }
        let read = self.next_lower_read;
        self.next_lower_read += 1;
        Ok(read)
    }

    fn create_lower_read_memory(&mut self, _read: usize) -> Result<(), NtStatus> {
        let s = self.faults.lookaside.unwrap_or(0);
        self.rec(format!(
            "WdfMemoryCreateFromLookaside(list=lookaside)={}",
            st(s)
        ));
        if s == 0 { Ok(()) } else { Err(s) }
    }

    fn format_lower_read(&mut self, _read: usize, _memory: ()) -> NtStatus {
        let s = self.faults.lower_format.unwrap_or(0);
        self.rec(format!("WdfIoTargetFormatRequestForInternalIoctl(target=target,req=lower_read,ioctl=0x000b000b,in=null,in_len=0,in_off=null,out=lower_mem,out_len=1024,out_off=null)={}", st(s)));
        s
    }

    fn set_lower_read_completion(&mut self, _read: usize) {
        self.rec("WdfRequestSetCompletionRoutine(req=lower_read,routine=PtpFilterInputRequestCompletionCallback,ctx=req_context)".into());
    }

    fn send_lower_read(&mut self, read: usize, ticket: ReadTicket) -> bool {
        let ok = !self.faults.lower_send_false;
        if ok {
            self.lower_reads_pending += 1;
            self.tickets.push((read, ticket));
        }
        self.rec(format!("WdfRequestSend(req=lower_read,target=target,opts=null,ioctl=0x000b000b,out_len=1024,completion=PtpFilterInputRequestCompletionCallback)={}", if ok { "TRUE" } else { "FALSE" }));
        ok
    }

    fn lower_read_buffer(&self, _memory: ()) -> &[u8] {
        &self.lower_buf
    }

    fn delete_lower_read(&mut self, _read: usize) {
        self.rec("WdfObjectDelete(obj=lower_read)".into());
    }

    fn delete_lower_memory(&mut self, _memory: ()) {
        self.rec("WdfObjectDelete(obj=lower_mem)".into());
    }

    fn in_flight_reads(&self) -> usize {
        self.tracker.in_flight()
    }

    fn reset_contacts(&mut self) {
        self.contacts.reset();
    }

    fn prepare_lower_read(&mut self, state: &DeviceState, read: usize) -> Option<ReadTicket> {
        if state.device_configured() {
            self.tracker.prepare(read)
        } else {
            None
        }
    }

    fn complete_lower_read_send(
        &mut self,
        read: usize,
        ticket: ReadTicket,
        sent: bool,
    ) -> PostSendAction {
        let action = self.tracker.complete_send(read, ticket, sent);
        if action == PostSendAction::CancelSent {
            self.cancelled_reads += 1;
        }
        action
    }

    fn pack_completion(request: usize, status: NtStatus) -> (usize, NtStatus) {
        (request, status)
    }

    fn with_pipeline<R, F>(&mut self, f: F) -> R
    where
        F: FnOnce(
            &mut Self,
            &mut ReadTracker<usize>,
            &mut ContactTracker,
            &mut CompletionQueue<(usize, NtStatus)>,
        ) -> R,
    {
        let mut tracker = core::mem::take(&mut self.tracker);
        let mut contacts = core::mem::take(&mut self.contacts);
        let mut completions = core::mem::take(&mut self.completions);
        let out = f(self, &mut tracker, &mut contacts, &mut completions);
        self.tracker = tracker;
        self.contacts = contacts;
        self.completions = completions;
        out
    }

    fn drain_upper_completions(&mut self, outcome: EnqueueOutcome<(usize, NtStatus)>) {
        match outcome {
            EnqueueOutcome::Queued => {}
            EnqueueOutcome::Overflow((req, st)) => self.complete(req, st),
            EnqueueOutcome::StartDrain((first_req, first_st)) => {
                self.complete(first_req, first_st);
                while let Some((req, st)) = self.completions.next_to_complete() {
                    self.complete(req, st);
                }
            }
        }
    }

    fn cancel_in_flight_reads(&mut self) -> usize {
        self.contacts.reset();
        let cancelled = self.tracker.suspend().iter().count();
        self.cancelled_reads += cancelled;
        cancelled
    }

    fn set_failed(&mut self, action: FailedAction) {
        let a = match action {
            FailedAction::AttemptRestart => "attempt_restart",
            FailedAction::NoRestart => "no_restart",
        };
        self.rec(format!("WdfDeviceSetFailed(dev=device,action={a})"));
    }

    fn start_recovery_timer(&mut self, due_time: i64) {
        self.rec(format!("WdfTimerStart(timer=timer,due={due_time})"));
    }

    fn stop_recovery_timer(&mut self) {
        self.timer_stops += 1;
    }

    fn enqueue_recovery_work_item(&mut self) {
        self.rec("WdfWorkItemEnqueue(item=workitem)".into());
    }

    fn query_performance_counter(&mut self) -> i64 {
        self.rec("KeQueryPerformanceCounter(freq_ptr=null)".into());
        self.counter
    }

    fn query_performance_frequency(&mut self) -> i64 {
        self.rec("KeQueryPerformanceCounter(freq_ptr=nonnull)".into());
        self.freq
    }

    fn send_mode_report(&mut self, report: amtptp_usb_bt::bluetooth::ModeSwitch) -> NtStatus {
        // Project the class transfer onto the historical oracle's events.
        if let Err(status) = self.create_mode_switch_request() {
            return status;
        }
        let status = with_xfer_packet::<32, _, _>(report.report_id, report.payload, |bytes| {
            if let Err(status) = self.create_mode_switch_memory((), bytes) {
                return status;
            }
            let status = self.format_mode_switch((), ());
            if status < 0 {
                return status;
            }
            if !self.set_mode_switch_user_buffer((), bytes) {
                return amtptp_usb_bt::bluetooth::STATUS_UNSUCCESSFUL;
            }
            self.send_mode_switch_synchronously(());
            self.mode_switch_status(())
        })
        .unwrap();
        self.delete_mode_switch_request(());
        status
    }
    fn create_mode_switch_request(&mut self) -> Result<(), NtStatus> {
        let s = self.faults.cfg_create.unwrap_or(0);
        self.rec(format!(
            "WdfRequestCreate(attrs=null,target=target)={}",
            st(s)
        ));
        self.cfg_mem_addr = None;
        self.cfg_user_buffer = None;
        if s == 0 { Ok(()) } else { Err(s) }
    }

    fn create_mode_switch_memory(&mut self, _r: (), packet: &[u8; 32]) -> Result<(), NtStatus> {
        let s = self.faults.cfg_mem.unwrap_or(0);
        self.rec(format!("WdfMemoryCreatePreallocated(ctx=null,parent=cfg_req,exec=inherit,sync=inherit,cleanup=null,destroy=null,size={})={}", packet.len(), st(s)));
        self.cfg_mem_addr = Some(packet.as_ptr() as usize);
        if s == 0 { Ok(()) } else { Err(s) }
    }

    fn format_mode_switch(&mut self, _r: (), _m: ()) -> NtStatus {
        let s = self.faults.cfg_format.unwrap_or(0);
        self.rec(format!("WdfIoTargetFormatRequestForInternalIoctl(target=target,req=cfg_req,ioctl=0x000b0191,in=cfg_mem,in_len=32,in_off=null,out=null,out_len=0,out_off=null)={}", st(s)));
        s
    }

    fn set_mode_switch_user_buffer(&mut self, _r: (), packet: &[u8; 32]) -> bool {
        if self.faults.irp_null {
            self.rec("WdfRequestWdmGetIrp(req=cfg_req)=null".into());
            return false;
        }
        self.cfg_user_buffer = Some((packet.as_ptr() as usize, *packet));
        true
    }

    fn send_mode_switch_synchronously(&mut self, _r: ()) -> bool {
        let (addr, bytes) = self
            .cfg_user_buffer
            .expect("UserBuffer not set before send");
        let ok = self.faults.cfg_send_false.is_none();
        let xfer_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let ptr = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        let mut raw32 = bytes.to_vec();
        raw32[..8].fill(0);
        let send = Send {
            in_is_userbuffer: self.cfg_mem_addr == Some(addr),
            xfer_id: bytes[12],
            xfer_len,
            rb_off: ptr.wrapping_sub(addr as u64),
            payload: bytes[16..16 + xfer_len as usize].to_vec(),
            raw32,
        };
        self.rec(format!(
            "WdfRequestSend(req=cfg_req,target=target,opts=0x2,timeout=0,ioctl=0x000b0191,in_len=32,in_is_userbuffer={},xfer_id=0x{:02x},xfer_len={},rb_off={},payload={},raw32={})={}",
            u8::from(send.in_is_userbuffer),
            send.xfer_id,
            send.xfer_len,
            send.rb_off,
            to_hex(&send.payload),
            to_hex(&send.raw32),
            if ok { "TRUE" } else { "FALSE" }
        ));
        self.sends.push(send);
        ok
    }

    fn mode_switch_status(&mut self, _r: ()) -> NtStatus {
        let s = self
            .faults
            .cfg_send_false
            .or(self.faults.cfg_send_true_status)
            .unwrap_or(0);
        self.rec(format!("WdfRequestGetStatus(req=cfg_req)={}", st(s)));
        s
    }

    fn delete_mode_switch_request(&mut self, _r: ()) {
        self.rec("WdfObjectDelete(obj=cfg_req)".into());
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

type Filter<'a> = Mock;
type UpperRequest = usize;
type LowerRead = usize;
type ReadMemory = ();
type QueuedCompletion = (usize, NtStatus);
/// `WDF_DEVICE_FAILED_ACTION` for `WdfDeviceSetFailed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailedAction {
    /// `WdfDeviceFailedAttemptRestart`.
    AttemptRestart,
    /// `WdfDeviceFailedNoRestart`.
    NoRestart,
}

#[path = "../../../../drivers/interface/bluetooth/hid/io/dispatch.rs"]
pub(crate) mod dispatch;
#[path = "../../../../drivers/interface/bluetooth/hid/io/pipeline.rs"]
pub(crate) mod pipeline;
#[path = "../../../../drivers/interface/bluetooth/hid/io/pnp.rs"]
pub(crate) mod pnp;

type AddDevice = Mock;
#[path = "../../../../drivers/interface/bluetooth/hid/device/add.rs"]
pub(crate) mod add;
