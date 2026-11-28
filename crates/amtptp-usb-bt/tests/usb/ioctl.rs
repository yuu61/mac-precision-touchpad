//! Replays `tests/usb/fixtures/ioctl.txt` (61 records): dispatcher routing for every code, `READ_REPORT`
//! pend / forward failure, FIFO service of pended reads, `EvtIoStop`, `ReadersFailed`.

use super::common;
use amtptp_core::hid::ioctl::IOCTL_HID_GET_STRING;

use std::collections::VecDeque;

use super::fake::FakeHw;
use amtptp_core::domain::ContactTracker;
use amtptp_usb_bt::usb::feature::{GetFeaturePacket, SetFeaturePacket, get_feature, set_feature};
use amtptp_usb_bt::usb::hid::{
    InternalIoctl, not_supported, read_report_forward_failed, route_internal_ioctl,
};
use amtptp_usb_bt::usb::input::{PacketAction, complete_read_report, process_packet};
use amtptp_usb_bt::usb::lifecycle::{self, IoStopAction, io_stop, readers_failed};
use amtptp_usb_bt::usb::state::DeviceState;
use amtptp_usb_bt::usb::status::Completion;
use common::{Record, Tally, hex, kv, load, num, replay_descriptor_ioctl, status, to_hex};

fn completion_of(r: &Record) -> Completion {
    Completion {
        status: status(r.req("status")),
        information: num(r.req("info")) as usize,
    }
}

fn replay_route(r: &Record) -> &'static str {
    let id = r.id();
    let code = num(r.req("ioctl")) as u32;
    match route_internal_ioctl(code) {
        InternalIoctl::GetDeviceDescriptor
        | InternalIoctl::GetReportDescriptor
        | InternalIoctl::GetDeviceAttributes => {
            let bucket = replay_descriptor_ioctl(r);
            assert_eq!(bucket, "ioctl/driver", "{id}");
            "route/descriptor"
        }
        InternalIoctl::ReadReport => match r.get("inject") {
            // WdfRequestForwardToIoQueue succeeded: pending, not completed, one more queued request.
            None => {
                assert_eq!(r.req("completed"), "0", "{id}");
                assert_eq!(r.req("qdepth_before"), "0", "{id}");
                assert_eq!(r.req("qdepth_after"), "1", "{id}");
                let outlen = num(r.req("outlen")) as usize;
                let prefill = hex(r.req("prefill"))[0];
                assert_eq!(hex(r.req("out")), vec![prefill; outlen], "{id}: untouched");
                assert_eq!(r.req("ops"), "fwd_to_queue:queue=1", "{id}");
                "route/read_report_pended"
            }
            // The forward failed with an injected (framework) status: completed with it.
            Some(inject) => {
                let st = status(inject.strip_prefix("fwd_to_queue:").expect("inject"));
                assert_eq!(r.req("completed"), "1", "{id}");
                assert_eq!(read_report_forward_failed(st), completion_of(r), "{id}");
                assert_eq!(r.req("qdepth_after"), "0", "{id}");
                "route/read_report_forward_failed"
            }
        },
        route @ (InternalIoctl::GetFeature | InternalIoctl::SetFeature) => {
            // A valid HID_XFER_PACKET with report id 0x55 (README "route").
            let report_id = num(r.req("xfer_report_id")) as u8;
            let outlen = num(r.req("outlen")) as usize;
            let inlen = num(r.req("inlen")) as usize;
            let mut buf = [0_u8; 8];
            let mut irp_read = false;
            let completion = if route == InternalIoctl::GetFeature {
                get_feature(outlen, || {
                    irp_read = true;
                    Some(GetFeaturePacket {
                        report_id,
                        report_buffer: &mut buf,
                    })
                })
            } else {
                let state = DeviceState::new();
                let mut hw = FakeHw::new(0x027a);
                let c = set_feature(
                    inlen,
                    || {
                        irp_read = true;
                        Some(SetFeaturePacket {
                            report_id,
                            report_buffer: &buf,
                        })
                    },
                    &state,
                    &mut hw,
                );
                assert_eq!(hw.take_logs(), (Vec::new(), "-".to_owned()), "{id}");
                c
            };
            assert!(irp_read, "{id}");
            assert_eq!(r.req("completed"), "1", "{id}");
            assert_eq!(completion, completion_of(r), "{id}");
            assert_eq!(
                r.req("ops"),
                format!(
                    "get_params|wdm_get_irp|complete:st={},info=0",
                    r.req("status")
                ),
                "{id}"
            );
            "route/feature"
        }
        InternalIoctl::GetString => {
            assert_eq!(code, IOCTL_HID_GET_STRING);
            "route/string"
        }
        InternalIoctl::NotSupported => {
            assert_eq!(r.req("completed"), "1", "{id}");
            assert_eq!(not_supported(), completion_of(r), "{id}");
            let outlen = num(r.req("outlen")) as usize;
            let prefill = hex(r.req("prefill"))[0];
            assert_eq!(hex(r.req("out")), vec![prefill; outlen], "{id}: untouched");
            assert_eq!(r.req("qdepth_after"), "0", "{id}");
            "route/not_supported"
        }
    }
}

/// The FIFO records are one scenario: three `READ_REPORTs` (50 bytes of 0xcc) pended, then four 1-finger
/// packets, one per record; each record shows all three requests after its packet. The device is PID
/// 0x027a (the outputs use x.min -6243 / y.max 7685).
struct Fifo {
    state: DeviceState,
    contacts: ContactTracker,
    reqs: Vec<(Vec<u8>, Option<Completion>)>,
    queue: VecDeque<usize>,
}

impl Fifo {
    fn new() -> Self {
        let state = DeviceState::new();
        state.on_device_create();
        let mut hw = FakeHw::new(0x027a);
        assert_eq!(lifecycle::prepare_hardware(&state, &mut hw), 0);
        assert_eq!(lifecycle::d0_entry(&state, &mut hw), 0);
        Self {
            state,
            contacts: ContactTracker::new(),
            reqs: vec![(vec![0xcc; 50], None); 3],
            queue: (0..3).collect(),
        }
    }

    fn deliver(&mut self, packet: &[u8]) {
        if let PacketAction::Deliver(result) =
            process_packet(&self.state, &mut self.contacts, packet, 10_000_000)
            && let Some(i) = self.queue.pop_front()
        {
            let (buf, done) = &mut self.reqs[i];
            *done = Some(complete_read_report(&result, Ok(&mut **buf)));
        }
    }

    fn check(&self, r: &Record) {
        let id = r.id();
        for (i, (buf, done)) in self.reqs.iter().enumerate() {
            let req = kv(r.req(&format!("req{}", i + 1)));
            if let Some(c) = done {
                assert_eq!(req["completed"], "1", "{id} req{}", i + 1);
                assert_eq!(status(&req["status"]), c.status, "{id}");
                assert_eq!(num(&req["info"]) as usize, c.information, "{id}");
                assert_eq!(req["queued"], "0", "{id}");
            } else {
                assert_eq!(req["completed"], "0", "{id} req{}", i + 1);
                assert_eq!(req["queued"], "1", "{id}");
            }
            assert_eq!(
                to_hex(buf),
                r.req(&format!("out{}", i + 1)),
                "{id} out{}",
                i + 1
            );
        }
    }
}

#[test]
fn ioctl_fixture() {
    let records = load("ioctl.txt");
    let mut tally = Tally::default();
    let mut fifo = Fifo::new();
    let mut fifo_next = 0;
    for r in &records {
        let id = r.id();
        match r.req("kind") {
            "route" => tally.add(replay_route(r)),
            "fifo" => {
                assert_eq!(
                    r.req("case"),
                    format!("fifo-packet-{fifo_next}"),
                    "{id}: order"
                );
                fifo_next += 1;
                let packet = hex(r.req("in"));
                assert_eq!(packet.len() as u64, num(r.req("n")));
                fifo.deliver(&packet);
                fifo.check(r);
                tally.add("fifo");
            }
            "io_stop" => {
                // EvtIoStop: no WDF call, the request is neither completed nor acknowledged.
                let flags = num(r.req("action_flags")) as u32;
                assert_eq!(io_stop(flags), IoStopAction::None, "{id}");
                assert_eq!(r.req("completed"), "0", "{id}");
                assert_eq!(r.req("queued"), "1", "{id}");
                assert_eq!(r.req("ops"), "-", "{id}");
                tally.add("io_stop");
            }
            "readers_failed" => {
                let ret = readers_failed(status(r.req("status")), status(r.req("usbd_status")));
                assert_eq!(ret, r.req("ret") == "1", "{id}");
                assert_eq!(r.req("ops"), "-", "{id}");
                tally.add("readers_failed");
            }
            other => panic!("{id}: unknown kind {other}"),
        }
    }
    tally.assert_exact(
        "ioctl.txt",
        61,
        &[
            ("fifo", 4),
            ("io_stop", 4),
            ("readers_failed", 3),
            ("route/descriptor", 3),
            ("route/feature", 2),
            ("route/not_supported", 38),
            ("route/string", 1),
            ("route/read_report_forward_failed", 3),
            ("route/read_report_pended", 3),
        ],
    );
}
