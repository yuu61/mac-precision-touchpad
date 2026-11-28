//! Hardware-independent logic of the `AmtPtpDeviceUsbKm` (KMDF, Wellspring, T2 and USB Magic Trackpad 2) driver.
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
//!   readers-failed and `EvtIoStop` decisions, and the queue and reader configuration.
//! - [`status`]: the NTSTATUS values and the [`status::Completion`] (status + Information) type.
//! - [`output`]: the request output buffer as written by `WdfMemoryCopyFromBuffer` ([`output::OutputMemory`]).
//!
//! The Rust port originally preserved observable C behaviors such as wLength 4, `DATA_ERROR` for 6+
//! fingers, and Information 0 for `GET/SET_FEATURE`. The reporting code now uses the PTP Scan Time
//! units and returns the complete HQA feature report.
//!
//! # How the driver uses this
//!
//! The device context holds the WDF handles (USB device, interface, pipe handles, `InputQueue`) and one
//! [`state::DeviceState`]:
//!
//! - **`EvtDriverDeviceAdd`**: `WdfFdoInitSetFilter`; PnP callbacks PrepareHardware/D0Entry/D0Exit only;
//!   `WdfDeviceCreate` with the context, `ptr.write(DeviceState::new())`, then
//!   [`state::DeviceState::on_device_create`], the recovery work item and the queues [`lifecycle::DEFAULT_QUEUE`] and
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
//!   - strings: query the USB descriptor in the requested language and include the NUL terminator;
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
//! - **Continuous reader completion** (≤ `DISPATCH_LEVEL`): sample the QPC counter, then
//!   call [`input::process_packet`] with `state`, `&buffer[..NumBytesTransferred]`, and the counter. On
//!   [`input::PacketAction::Deliver`], `WdfIoQueueRetrieveNextRequest(InputQueue)`; if there is none, drop
//!   the packet; otherwise retrieve its output memory and complete it with
//!   [`input::complete_read_report_to`] (this completes it on every path, fixing the C leaks B6/B7).
//! - **`EvtUsbTargetPipeReadersFailed`**: return [`lifecycle::readers_failed`] (always TRUE).
//! - **`EvtIoStop`**: [`lifecycle::io_stop`] (nothing to do).
//!
//! Scan Time uses the counter sampled in the reader callback and the frequency cached at D0 entry.

pub use crate::application::usb::{lifecycle, state, wellspring};
pub use crate::infrastructure::usb as config;
pub mod feature;
pub mod hid;
pub mod input;
pub mod output;
pub mod status;
