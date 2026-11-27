//! Hardware-independent logic of the `AmtPtpDeviceUsbUm` UMDF 2.15 driver (C: `src/AmtPtpDeviceUsbUm`):
//! Apple "Wellspring" (bcm5974) `MacBook` trackpads and the Magic Trackpad 2 over USB, exposed as a Windows
//! Precision Touchpad through mshidumdf.
//!
//! Everything that does not need WDF lives here, `no_std` and host-testable. The WDF glue
//! (`drivers/um/AmtPtpDeviceUsbUm`) owns the framework objects and implements the small traits in
//! [`io`]; this crate decides what to do, in which order, and with which result.
//!
//! | Module | C source | Content |
//! |---|---|---|
//! | [`config`] | AppleDefinition.h, Device.c:6-22 | `Bcm5974ConfigTable`, bounded PID lookup |
//! | [`descriptors`] | StaticHidRegistry.h, Hid.c | 6 report descriptors, HID descriptor, attributes |
//! | [`state`] | Device.h `DEVICE_CONTEXT` | per-device state with atomic fields |
//! | [`io`] | WDF DDIs | traits the glue implements |
//! | [`usb`] | Device.c:255-496, :758-802 | Wellspring-mode control transfers, setup packets |
//! | [`hid`] | Queue.c, Hid.c | IOCTL routing, descriptor/attribute/string/feature handlers |
//! | [`input`] | InputInterrupt.c | packet validation, contact decode, `ScanTime`, read completion |
//! | [`pnp`] | Device.c, InputInterrupt.c:6-80 | `PrepareHardware`, `D0Entry`, `D0Exit`, constants |
//! | [`status`] | ntstatus.h | the NTSTATUS values the driver itself returns |
//!
//! The core follows the C porting baseline documented in `docs/rust-port/specs/usbum.md`, with
//! updated Scan Time and HQA reporting.
//!
//! # How the driver uses this
//!
//! The device context holds a [`DeviceState`] (initialize it with [`DeviceState::new`] when the WDF
//! context is created) next to the WDF handles (`WDFUSBDEVICE`, interface, interrupt pipe, manual
//! `InputQueue`). The glue implements:
//!
//! - [`io::UsbTargetSetup`] over the USB target device / interface / pipes / reader configuration,
//! - [`io::ControlPipe`] (`WdfUsbTargetDeviceSendControlTransferSynchronously` with a 4-byte buffer
//!   memory descriptor) and [`io::InterruptPipeTarget`] (`WdfIoTargetStart` / `WdfIoTargetStop`), usually
//!   on one adapter struct,
//! - [`io::HidRequest`] over a `WDFREQUEST`, [`io::ReadQueue`] over the manual `InputQueue`,
//! - [`io::UsbStringSource`] over `WdfUsbTargetDeviceAllocAndQueryString` (delete the memory on drop).
//!
//! Callbacks then reduce to:
//!
//! | WDF callback | core call |
//! |---|---|
//! | `EvtDriverDeviceAdd` | `WdfFdoInitSetFilter`, device + context, `SurpriseRemovalOK`, interface [`pnp::GUID_DEVINTERFACE_AMTPTP_DEVICE`], default parallel queue (not power-managed as a filter) + manual `InputQueue` (`PowerManaged = WdfFalse`) |
//! | `EvtDevicePrepareHardware` | [`pnp::prepare_hardware`]; write `outcome.event` if any; return `outcome.status` |
//! | `EvtDeviceD0Entry` | [`pnp::d0_entry`] with `QueryPerformanceFrequency`; return `outcome.status` |
//! | `EvtDeviceD0Exit` | return [`pnp::d0_exit`] |
//! | `EvtIoDeviceControl` | [`hid::route_ioctl`], then the handler below; complete with `(status, information)` unless READ_REPORT was forwarded |
//! | GET_DEVICE_DESCRIPTOR | [`hid::get_hid_descriptor`] (write `outcome.event` if any) |
//! | GET_REPORT_DESCRIPTOR | [`hid::get_report_descriptor`] |
//! | GET_DEVICE_ATTRIBUTES | [`hid::get_device_attributes`] |
//! | GET_STRING | [`hid::get_string`] |
//! | UMDF GET_FEATURE / SET_FEATURE | [`hid::get_feature`] / [`hid::set_feature`] |
//! | READ_REPORT | `WdfRequestForwardToIoQueue(request, InputQueue)`; on failure complete with that status |
//! | anything else | complete with [`hid::IoctlOutcome::NOT_SUPPORTED`] |
//! | `EvtIoStop` | no-op (neither acknowledge nor complete) |
//! | reader completion | [`input::on_read_complete`] with the reader buffer cut to `NumBytesTransferred` |
//! | `EvtUsbTargetPipeReadersFailed` | return [`pnp::READERS_FAILED_RESULT`] |
//!
//! ```
//! use amtptp_usbum_core::{DeviceState, config, input};
//!
//! let state = DeviceState::new();
//! state.set_config_row(config::lookup_config_row(0x0265)); // Magic Trackpad 2
//! let cfg = state.config().unwrap();
//! assert_eq!(cfg.profile.tp_type.reader_transfer_length(), 156);
//! // 7th MT2 golden row of spec 7.6: the INT_MIN negation case gives Y = 0.
//! let contact = input::mt2_contact(cfg, &[0, 0, 0, 2, 1, 1, 0, 0, 0]);
//! assert_eq!((contact.x, contact.y), (3678, 0));
//! ```

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
pub mod descriptors;
pub mod hid;
pub mod input;
pub mod io;
pub mod pnp;
pub mod state;
pub mod status;
pub mod usb;

pub use state::DeviceState;
pub use status::NtStatus;
