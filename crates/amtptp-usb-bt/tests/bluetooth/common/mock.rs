//! Recording model of the KMDF requests and queues used by the Bluetooth filter.

use std::collections::VecDeque;

use amtptp_core::hid::hidport::with_xfer_packet;

use amtptp_usb_bt::bluetooth::{
    CompletionQueue, ContactTracker, DeviceState, EnqueueOutcome, NtStatus, PostSendAction,
    ReadTicket, ReadTracker, XferPacket, XferPacketMut,
};

use super::{BUFFER_TOO_SMALL, FailedAction, INFO_IN, NO_MORE_ENTRIES, st, to_hex};

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
    pub(super) fn fdo_init_set_filter(&mut self) {
        self.rec("WdfFdoInitSetFilter".into());
    }

    pub(super) fn set_pnp_power_event_callbacks(&mut self) {
        self.rec("WdfDeviceInitSetPnpPowerEventCallbacks".into());
    }

    pub(super) fn create_device(&mut self) -> NtStatus {
        self.add_step("WdfDeviceCreate")
    }

    pub(super) fn store_wdm_device_object(&mut self) -> bool {
        self.rec("WdfDeviceWdmGetDeviceObject".into());
        !self.faults.wdm_null
    }

    pub(super) fn create_read_buffer_lookaside(&mut self) -> NtStatus {
        self.add_step("WdfLookasideListCreate")
    }

    pub(super) fn create_recovery_timer(&mut self) -> NtStatus {
        self.add_step("WdfTimerCreate")
    }

    pub(super) fn create_recovery_work_item(&mut self) -> NtStatus {
        self.add_step("WdfWorkItemCreate")
    }

    pub(super) fn create_default_queue(&mut self) -> NtStatus {
        self.add_step("WdfIoQueueCreate")
    }

    pub(super) fn create_read_queue(&mut self) -> NtStatus {
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

    pub(super) fn complete(&mut self, request: usize, status: NtStatus) {
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

    pub(super) fn set_information(&mut self, request: usize, information: usize) {
        self.uppers[request].info = information;
        self.rec(format!(
            "WdfRequestSetInformation(req={},info={information})",
            Self::up(request)
        ));
    }

    pub(super) fn retrieve_output_memory(&mut self, request: usize) -> Result<usize, NtStatus> {
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

    pub(super) fn copy_to_memory(&mut self, memory: usize, source: &[u8]) -> NtStatus {
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

    pub(super) fn retrieve_output_buffer(
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

    pub(super) fn get_feature_packet(&mut self, request: usize) -> Option<XferPacketMut<'_>> {
        self.uppers[request]
            .feature
            .as_mut()
            .map(|f| XferPacketMut {
                report_id: f.rid,
                report_buffer: &mut f.buf[..f.rblen],
            })
    }

    pub(super) fn set_feature_packet(&self, request: usize) -> Option<XferPacket<'_>> {
        self.uppers[request].feature.as_ref().map(|f| XferPacket {
            report_id: f.rid,
            report_buffer: &f.buf[..f.rblen],
        })
    }

    pub(super) fn forward_to_read_queue(&mut self, request: usize) -> NtStatus {
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

    pub(super) fn retrieve_next_parked_read(&mut self) -> Result<usize, NtStatus> {
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

    pub(super) fn parked_read_count(&self) -> usize {
        // A pure query: the C oracle logs no getters.
        self.read_queue.len()
    }

    pub(super) fn copy_string(&mut self, request: usize) -> Result<usize, NtStatus> {
        let result = self
            .faults
            .string
            .unwrap_or(Err(amtptp_usb_bt::bluetooth::STATUS_INVALID_DEVICE_STATE));
        self.rec(format!("CopyString(req={})", Self::up(request)));
        result
    }

    pub(super) fn create_lower_read(&mut self) -> Result<usize, NtStatus> {
        let s = self.faults.lower_create.unwrap_or(0);
        self.rec(format!("WdfRequestCreate(ctx=WORKER_REQUEST_CONTEXT,parent=device,exec=inherit,sync=inherit,cleanup=null,destroy=null,target=target)={}", st(s)));
        if s != 0 {
            return Err(s);
        }
        let read = self.next_lower_read;
        self.next_lower_read += 1;
        Ok(read)
    }

    pub(super) fn create_lower_read_memory(&mut self, _read: usize) -> Result<(), NtStatus> {
        let s = self.faults.lookaside.unwrap_or(0);
        self.rec(format!(
            "WdfMemoryCreateFromLookaside(list=lookaside)={}",
            st(s)
        ));
        if s == 0 { Ok(()) } else { Err(s) }
    }

    pub(super) fn format_lower_read(&mut self, _read: usize, _memory: ()) -> NtStatus {
        let s = self.faults.lower_format.unwrap_or(0);
        self.rec(format!("WdfIoTargetFormatRequestForInternalIoctl(target=target,req=lower_read,ioctl=0x000b000b,in=null,in_len=0,in_off=null,out=lower_mem,out_len=1024,out_off=null)={}", st(s)));
        s
    }

    pub(super) fn set_lower_read_completion(&mut self, _read: usize) {
        self.rec("WdfRequestSetCompletionRoutine(req=lower_read,routine=PtpFilterInputRequestCompletionCallback,ctx=req_context)".into());
    }

    pub(super) fn send_lower_read(&mut self, read: usize, ticket: ReadTicket) -> bool {
        let ok = !self.faults.lower_send_false;
        if ok {
            self.lower_reads_pending += 1;
            self.tickets.push((read, ticket));
        }
        self.rec(format!("WdfRequestSend(req=lower_read,target=target,opts=null,ioctl=0x000b000b,out_len=1024,completion=PtpFilterInputRequestCompletionCallback)={}", if ok { "TRUE" } else { "FALSE" }));
        ok
    }

    pub(super) fn lower_read_buffer(&self, _memory: ()) -> &[u8] {
        &self.lower_buf
    }

    pub(super) fn delete_lower_read(&mut self, _read: usize) {
        self.rec("WdfObjectDelete(obj=lower_read)".into());
    }

    pub(super) fn delete_lower_memory(&mut self, _memory: ()) {
        self.rec("WdfObjectDelete(obj=lower_mem)".into());
    }

    pub(super) fn in_flight_reads(&self) -> usize {
        self.tracker.in_flight()
    }

    pub(super) fn reset_contacts(&mut self) {
        self.contacts.reset();
    }

    pub(super) fn prepare_lower_read(
        &mut self,
        state: &DeviceState,
        read: usize,
    ) -> Option<ReadTicket> {
        if state.device_configured() {
            self.tracker.prepare(read)
        } else {
            None
        }
    }

    pub(super) fn complete_lower_read_send(
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

    pub(super) fn pack_completion(request: usize, status: NtStatus) -> (usize, NtStatus) {
        (request, status)
    }

    pub(super) fn with_pipeline<R, F>(&mut self, f: F) -> R
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

    pub(super) fn drain_upper_completions(&mut self, outcome: EnqueueOutcome<(usize, NtStatus)>) {
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

    pub(super) fn cancel_in_flight_reads(&mut self) -> usize {
        self.contacts.reset();
        let cancelled = self.tracker.suspend().iter().count();
        self.cancelled_reads += cancelled;
        cancelled
    }

    pub(super) fn set_failed(&mut self, action: FailedAction) {
        let a = match action {
            FailedAction::AttemptRestart => "attempt_restart",
            FailedAction::NoRestart => "no_restart",
        };
        self.rec(format!("WdfDeviceSetFailed(dev=device,action={a})"));
    }

    pub(super) fn start_recovery_timer(&mut self, due_time: i64) {
        self.rec(format!("WdfTimerStart(timer=timer,due={due_time})"));
    }

    pub(super) fn stop_recovery_timer(&mut self) {
        self.timer_stops += 1;
    }

    pub(super) fn enqueue_recovery_work_item(&mut self) {
        self.rec("WdfWorkItemEnqueue(item=workitem)".into());
    }

    pub(super) fn query_performance_counter(&mut self) -> i64 {
        self.rec("KeQueryPerformanceCounter(freq_ptr=null)".into());
        self.counter
    }

    pub(super) fn query_performance_frequency(&mut self) -> i64 {
        self.rec("KeQueryPerformanceCounter(freq_ptr=nonnull)".into());
        self.freq
    }

    pub(super) fn send_mode_report(
        &mut self,
        report: amtptp_usb_bt::bluetooth::ModeSwitch,
    ) -> NtStatus {
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
