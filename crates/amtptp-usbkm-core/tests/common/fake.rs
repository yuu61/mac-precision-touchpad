//! A fake of the WDF USB side, modelled on the fixture harness (tests/fixtures/README.md, "Harness
//! model"): one interrupt IN pipe (EP 0x83) by default, `GET_REPORT` answered with `02 00`, at most `mdlen`
//! (4) bytes delivered, failure injection per WDF call, and a call log in the fixtures' `ops` notation
//! (normalized: only the arguments the Rust port is expected to reproduce).
#![allow(
    dead_code,
    reason = "Each integration test target uses a different subset of these helpers"
)]

use amtptp_usbkm_core::status::STATUS_INVALID_DEVICE_STATE;
use core::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use amtptp_usbkm_core::lifecycle::{ContinuousReaderConfig, UsbHardware, UsbPipeInfo, UsbPipeType};
use amtptp_usbkm_core::state::UsbDeviceIds;
use amtptp_usbkm_core::status::NtStatus;
use amtptp_usbkm_core::wellspring::{ControlTransfer, UsbSetupPacket, WELLSPRING_DATA_LEN};

use super::{status_str, to_hex};

/// The transfer buffer; its drop is `WdfObjectDelete(memory)`.
pub struct FakeBuffer {
    data: [u8; WELLSPRING_DATA_LEN],
    ops: Rc<RefCell<Vec<String>>>,
}

impl AsMut<[u8; WELLSPRING_DATA_LEN]> for FakeBuffer {
    fn as_mut(&mut self) -> &mut [u8; WELLSPRING_DATA_LEN] {
        &mut self.data
    }
}

impl Drop for FakeBuffer {
    fn drop(&mut self) {
        self.ops.borrow_mut().push("obj_delete:memory".to_owned());
    }
}

pub struct FakeHw {
    pub ops: Rc<RefCell<Vec<String>>>,
    pub wire: Vec<String>,
    /// Injected failures by harness point name (`usbdev_create`, `ctl_read`, ...). 0 = no failure.
    pub inject: BTreeMap<String, i32>,
    /// `GET_REPORT` response (`02 00` by default).
    pub ctl_read_response: Vec<u8>,
    /// `(WDF_USB_PIPE_TYPE, endpoint address)` of the configured pipes.
    pub pipes: Vec<(u8, u8)>,
    pub ids: UsbDeviceIds,
    pub traits: u32,
    pub qpc_freq: i64,
    /// `UsbInterface != NULL` (glue-side handle in the real driver).
    pub iface: bool,
}

impl FakeHw {
    pub fn new(product_id: u16) -> Self {
        Self {
            ops: Rc::default(),
            wire: Vec::new(),
            inject: BTreeMap::new(),
            ctl_read_response: vec![0x02, 0x00],
            pipes: vec![(4, 0x83)],
            ids: UsbDeviceIds {
                vendor_id: 0x05ac,
                product_id,
                bcd_device: 0x0100,
            },
            traits: 0x4,
            qpc_freq: 10_000_000,
            iface: false,
        }
    }

    /// Applies `harness_inject <point>:<status>`.
    pub fn set_inject(&mut self, point: &str, status: i32) {
        if status == 0 {
            self.inject.remove(point);
        } else {
            self.inject.insert(point.to_owned(), status);
        }
    }

    fn injected(&self, point: &str) -> Option<i32> {
        self.inject.get(point).copied()
    }

    fn log(&self, op: String) {
        self.ops.borrow_mut().push(op);
    }

    fn log_st(&self, name: &str, args: &str, st: Option<i32>) {
        let mut op = name.to_owned();
        let mut parts: Vec<String> = Vec::new();
        if !args.is_empty() {
            parts.push(args.to_owned());
        }
        if let Some(st) = st {
            parts.push(format!("st={}", status_str(st)));
        }
        if !parts.is_empty() {
            op.push(':');
            op.push_str(&parts.join(","));
        }
        self.log(op);
    }

    /// Takes the ops and wire logs (for one fixture step).
    pub fn take_logs(&mut self) -> (Vec<String>, String) {
        let ops = core::mem::take(&mut *self.ops.borrow_mut());
        let wire = core::mem::take(&mut self.wire);
        let wire = if wire.is_empty() {
            "-".to_owned()
        } else {
            wire.join(";")
        };
        (ops, wire)
    }
}

impl ControlTransfer for FakeHw {
    type Buffer = FakeBuffer;

    fn allocate_transfer_buffer(&mut self) -> Result<FakeBuffer, NtStatus> {
        let st = self.injected("mem_create");
        self.log_st("mem_create", "", st);
        match st {
            Some(st) => Err(st),
            // Deliberately not zero: the core must zero the buffer itself (RtlZeroMemory).
            None => Ok(FakeBuffer {
                data: [0xEE; WELLSPRING_DATA_LEN],
                ops: Rc::clone(&self.ops),
            }),
        }
    }

    fn control_transfer(
        &mut self,
        setup: &UsbSetupPacket,
        data: &mut [u8; WELLSPRING_DATA_LEN],
    ) -> NtStatus {
        let setup_hex = to_hex(&setup.to_bytes());
        let mdlen = data.len();
        let dir = if setup.is_device_to_host() {
            "in"
        } else {
            "out"
        };
        let args = format!("setup={setup_hex},mdlen={mdlen},dir={dir}");
        if setup.is_device_to_host() {
            if let Some(st) = self.injected("ctl_read") {
                self.log_st("ctl_xfer", &args, Some(st));
                self.wire
                    .push(format!("in:{setup_hex}:-:{:08x}", st as u32));
                st
            } else {
                let n = self.ctl_read_response.len().min(mdlen);
                data[..n].copy_from_slice(&self.ctl_read_response[..n]);
                self.log_st("ctl_xfer", &args, Some(0));
                self.wire
                    .push(format!("in:{setup_hex}:{}:00000000", to_hex(&data[..n])));
                0
            }
        } else {
            let st = self.injected("ctl_write").unwrap_or(0);
            self.log_st("ctl_xfer", &args, Some(st));
            self.wire.push(format!(
                "out:{setup_hex}:{}:{:08x}",
                to_hex(&data[..]),
                st as u32
            ));
            st
        }
    }
}

impl UsbHardware for FakeHw {
    fn create_usb_device(&mut self) -> NtStatus {
        let st = self.injected("usbdev_create");
        self.log_st("usbdev_create", "", st);
        st.unwrap_or(0)
    }

    fn device_descriptor(&mut self) -> UsbDeviceIds {
        self.log("usbdev_get_desc".to_owned());
        self.ids
    }

    fn retrieve_device_traits(&mut self) -> Result<u32, NtStatus> {
        let st = self.injected("retrieve_info");
        self.log_st("usbdev_retrieve_info", "", st);
        match st {
            Some(st) => Err(st),
            None => Ok(self.traits),
        }
    }

    fn select_single_interface(&mut self) -> Result<u8, NtStatus> {
        if let Some(st) = self.injected("select_config") {
            self.log_st("usbdev_select_config", "type=2", Some(st));
            self.iface = false;
            Err(st)
        } else {
            let Ok(n) = u8::try_from(self.pipes.len()) else {
                self.log_st(
                    "usbdev_select_config",
                    "type=2",
                    Some(STATUS_INVALID_DEVICE_STATE),
                );
                self.iface = false;
                return Err(STATUS_INVALID_DEVICE_STATE);
            };
            self.log_st("usbdev_select_config", &format!("type=2,npipes={n}"), None);
            self.iface = true;
            Ok(n)
        }
    }

    fn configured_pipe(&mut self, index: u8) -> UsbPipeInfo {
        self.log(format!("usbif_get_pipe:idx={index}"));
        let (pipe_type, endpoint_address) = self.pipes[usize::from(index)];
        UsbPipeInfo {
            pipe_type: match pipe_type {
                1 => UsbPipeType::Control,
                2 => UsbPipeType::Isochronous,
                3 => UsbPipeType::Bulk,
                4 => UsbPipeType::Interrupt,
                _ => UsbPipeType::Invalid,
            },
            endpoint_address,
        }
    }

    fn set_no_maximum_packet_size_check(&mut self, index: u8) {
        self.log(format!("pipe_no_max_pkt_check:pipe={index}"));
    }

    fn configure_continuous_reader(
        &mut self,
        index: u8,
        config: &ContinuousReaderConfig,
    ) -> NtStatus {
        let st = self.injected("cont_reader");
        let args = format!(
            "pipe={index},len={},hdr={},trl={},npending={}",
            config.transfer_length,
            config.header_length,
            config.trailer_length,
            config.num_pending_reads
        );
        self.log_st("pipe_config_cont_reader", &args, st);
        st.unwrap_or(0)
    }

    fn start_pipe(&mut self, index: u8) -> NtStatus {
        let st = self.injected("iotarget_start");
        self.log_st("iotarget_start", &format!("pipe={index}"), st);
        st.unwrap_or(0)
    }

    fn stop_pipe(&mut self, index: u8) {
        self.log(format!("iotarget_stop:pipe={index},action=1"));
    }

    fn query_performance_frequency(&mut self) -> i64 {
        self.log("qpc".to_owned());
        self.qpc_freq
    }
}

/// Normalizes a fixture `ops` string to the notation [`FakeHw`] logs: keeps only the arguments the Rust
/// port reproduces. Returns the normalized ops and the number of `obj_delete:null` entries removed
/// (`WdfObjectDelete(NULL)`, spec B4, fixed in Rust).
pub fn normalize_ops(ops: &str) -> (Vec<String>, usize) {
    if ops == "-" {
        return (Vec::new(), 0);
    }
    let mut out = Vec::new();
    let mut null_deletes = 0;
    for op in ops.split('|') {
        let (name, args) = op.split_once(':').unwrap_or((op, ""));
        if name == "obj_delete" {
            if args == "null" {
                null_deletes += 1;
            } else {
                out.push(format!("obj_delete:{args}"));
            }
            continue;
        }
        let keep: &[&str] = match name {
            "usbif_get_pipe" => &["idx"],
            "pipe_no_max_pkt_check" | "iotarget_start" => &["pipe"],
            "iotarget_stop" => &["pipe", "action"],
            "pipe_config_cont_reader" => &["pipe", "len", "hdr", "trl", "npending"],
            "ctl_xfer" => &["setup", "mdlen", "dir"],
            "usbdev_select_config" => &["type", "npipes"],
            "set_pnp_power_cb" => &["prepare", "d0entry", "d0exit", "others"],
            "devif" => &["guid", "ref"],
            "queuecreate" => &[
                "dispatch",
                "power",
                "default",
                "iodefault",
                "ioread",
                "iowrite",
                "iodevctl",
                "iointdevctl",
                "iostop",
                "ioresume",
                "iocanceled",
            ],
            _ => &[],
        };
        let mut parts: Vec<String> = Vec::new();
        let mut st = None;
        if !args.is_empty() {
            for kv in args.split(',') {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                match k {
                    "st" => st = Some(v.to_owned()),
                    _ if keep.contains(&k) => parts.push(format!("{k}={v}")),
                    _ => {}
                }
            }
        }
        // ctl_xfer always carries st in the fixtures; keep it as is.
        if let Some(st) = st {
            parts.push(format!("st={st}"));
        }
        let mut s = name.to_owned();
        if !parts.is_empty() {
            s.push(':');
            s.push_str(&parts.join(","));
        }
        out.push(s);
    }
    (out, null_deletes)
}
