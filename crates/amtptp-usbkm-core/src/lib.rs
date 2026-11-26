//! Hardware-independent logic of the `AmtPtpDeviceUsbKm` (KMDF, Apple T2 USB trackpad) driver.
//!
//! The C driver (`src/AmtPtpDeviceUsbKm`) is a KMDF lower filter under `mshidkmdf` that acts as the HID
//! minidriver of interface 2 of the T2 keyboard/trackpad. Everything it does that does not need WDF is
//! here, as pure functions or as sequences over small traits the WDF glue implements:
//!
//! - [`config`]: `Bcm5974ConfigTable` and the PID lookup (unknown PIDs get the 0xFFFF fallback row).
//! - [`state`]: the shared per-device state (the non-handle part of `DEVICE_CONTEXT`), all atomics.
//! - [`hid`]: report/HID descriptors, device attributes, the internal-IOCTL routing and the synchronous
//!   descriptor/attribute handlers.
//! - [`feature`]: `GET_FEATURE` / `SET_FEATURE` (`HID_XFER_PACKET`) handling.
//! - [`wellspring`]: the wellspring mode switch (two HID class control transfers) over
//!   [`wellspring::ControlTransfer`].
//! - [`input`]: reader packet → 50-byte PTP input report, `ScanTime`, and `READ_REPORT` completion.
//! - [`lifecycle`]: `PrepareHardware` / `D0Entry` / `D0Exit` sequencing over [`lifecycle::UsbHardware`], the
//!   readers-failed and `EvtIoStop` decisions, and the queue / reader / device-interface configuration.
//! - [`status`]: the NTSTATUS values and the [`status::Completion`] (status + Information) type.
//! - [`output`]: the request output buffer as written by `WdfMemoryCopyFromBuffer` ([`output::OutputMemory`]).
//!
//! Behavior follows the C exactly, including its observable bugs (`ScanTime` always 0, only one HQA byte
//! written, wLength 4, `DATA_ERROR` for 6+ fingers, Information 0 for `GET/SET_FEATURE`, ...). Undefined
//! behavior, leaks and memory-safety bugs are fixed (port policy P2). `docs/rust-port/usbkm.md` lists every
//! decision with its spec bug ID.
//!
//! # How the driver uses this
//!
//! The device context holds the WDF handles (USB device, interface, pipe handles, `InputQueue`) and one
//! [`state::DeviceState`]:
//!
//! - **`EvtDriverDeviceAdd`**: `WdfFdoInitSetFilter`; PnP callbacks PrepareHardware/D0Entry/D0Exit only;
//!   `WdfDeviceCreate` with the context, `ptr.write(DeviceState::new())`, then
//!   [`state::DeviceState::on_device_create`]; `WdfDeviceCreateDeviceInterface`
//!   ([`lifecycle::DEVICE_INTERFACE_GUID`]); the queues [`lifecycle::DEFAULT_QUEUE`] and
//!   [`lifecycle::INPUT_QUEUE`]. Return the first failure. (Do not call
//!   `WdfPdoInitAllowForwardingRequestToParent`: spec B12.)
//! - **`EvtDevicePrepareHardware` / `EvtDeviceD0Entry` / `EvtDeviceD0Exit`**: implement
//!   [`lifecycle::UsbHardware`] (+ [`wellspring::ControlTransfer`]) on a small per-call struct that wraps
//!   the device's WDF handles, then return [`lifecycle::prepare_hardware`], [`lifecycle::d0_entry`] or
//!   [`lifecycle::d0_exit`].
//! - **`EvtIoInternalDeviceControl`** (default queue): `match` [`hid::route_internal_ioctl`]:
//!   - descriptor / report descriptor: `WdfRequestRetrieveOutputMemory` → `WDFMEMORY` wrapper implementing
//!     [`output::OutputMemory`] with `WdfMemoryCopyFromBuffer` (or `Err(status)`) →
//!     [`hid::get_hid_descriptor_to`] / [`hid::get_report_descriptor_to`];
//!   - attributes: `WdfRequestRetrieveOutputBuffer(.., 32, ..)` → [`hid::get_device_attributes`] with
//!     [`state::DeviceState::usb_device_ids`];
//!   - `READ_REPORT`: `WdfRequestForwardToIoQueue(InputQueue)`; on failure complete with
//!     [`hid::read_report_forward_failed`];
//!   - `GET/SET_FEATURE`: [`feature::get_feature`] / [`feature::set_feature`] with the request's
//!     Output/InputBufferLength and a closure that reads `Irp->UserBuffer` as `HID_XFER_PACKET`;
//!   - otherwise [`hid::not_supported`].
//!
//!   Complete every non-pending request like the C: `WdfRequestSetInformation(information)` only when
//!   `information` is non-zero (the C calls it exactly on those paths), then `WdfRequestComplete(status)`.
//!   (`WdfRequestCompleteWithInformation(status, 0)` would overwrite an incoming non-zero Information
//!   that the C leaves in place.)
//! - **Continuous reader completion** (≤ `DISPATCH_LEVEL`): `KeQueryPerformanceCounter(&x)`, then
//!   call [`input::process_packet`] with `state`, `&buffer[..NumBytesTransferred]`, and `x`. On
//!   [`input::PacketAction::Deliver`], `WdfIoQueueRetrieveNextRequest(InputQueue)`; if there is none, drop
//!   the packet; otherwise retrieve its output memory and complete it with
//!   [`input::complete_read_report_to`] (this completes it on every path, fixing the C leaks B6/B7).
//! - **`EvtUsbTargetPipeReadersFailed`**: return [`lifecycle::readers_failed`] (always TRUE).
//! - **`EvtIoStop`**: [`lifecycle::io_stop`] (nothing to do).
//!
//! `ScanTime`: the C passes the value `KeQueryPerformanceCounter` writes to its out-parameter (the
//! frequency), both in `D0Entry` and in the reader completion, so `ScanTime` is always 0. The glue does the
//! same ([`lifecycle::UsbHardware::query_performance_counter_out`] and the `perf_counter_out` argument of
//! [`input::process_packet`]).

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

pub mod config;
pub mod feature;
pub mod hid;
pub mod input;
pub mod lifecycle;
pub mod output;
pub mod state;
pub mod status;
pub mod wellspring;
