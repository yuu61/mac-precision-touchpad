//! Hardware-independent logic of the `AmtPtpDeviceSpiKm` (KMDF, SPI) driver.
//!
//! `AmtPtpDeviceSpiKm` is a KMDF **lower filter** under `mshidkmdf.sys` on Apple's SPI HID child
//! (`SPI\VID_05ac&PID_xxxx&MI_02`). It answers the HID-minidriver internal IOCTLs itself, with a synthetic
//! identity (VID 0x8910 and its own Precision Touchpad report descriptor). Each upstream
//! `IOCTL_HID_READ_REPORT` is queued and triggers one lower read only if it arrives while the device is
//! `D0ActiveAndConfigured` ([`runtime::DeviceRuntime::should_issue_lower_read`]); reads queued while
//! unconfigured are served by later lower-read completions (a successful recovery timer issues exactly one
//! read for all of them, SPI-B12/B13, preserved). Apple's multitouch packet is converted into a 30-byte PTP
//! input report. This crate holds everything that does not need WDF; the driver crate
//! (`drivers/km/AmtPtpDeviceSpiKm`) is the glue.
//!
//! Modules:
//! - [`status`]: the `NTSTATUS` values (`NtStatus = i32`, same representation as `wdk_sys::NTSTATUS`).
//! - [`device`]: `SpiTrackpadConfigTable`, the PID-to-family switch, `REPORT_TYPE` and the
//!   `PrepareHardware` logic ([`device::DeviceConfig`]).
//! - [`descriptors`]: HID descriptor, the 8 report descriptors and `HID_DEVICE_ATTRIBUTES`.
//! - [`ioctl`]: internal-IOCTL routing.
//! - [`feature`]: the `GET/SET_FEATURE` handlers.
//! - [`input`]: packet parsing, `ScanTime` arithmetic and report construction.
//! - [`runtime`]: the shared mutable state (`DeviceStatus`, `LastReportTime`, PTP flags, all atomics)
//!   and the power/read state machine.
//! - [`wire`]: the bytes and parameters of the requests sent to the lower device, and `AddDevice` constants.
//!
//! Behavior follows the C driver at commit 2edb0f2 byte for byte (policy P1), except for the P2 fixes
//! recorded in `docs/rust-port/spikm.md` (zeroed input reports, packet bytes past the lower length read as
//! 0, no out-of-bounds read in `SET_FEATURE` 0x06, zero-initialized registry value, atomics). The fixtures
//! under `tests/fixtures/` (produced by running the real C code) are replayed by the integration tests.
//!
//! # How the driver uses this
//!
//! The device context holds a [`device::SharedDeviceConfig`] (per-field atomics; written only in
//! `PrepareHardware`, which runs again on every restart while reads may still complete, so readers take a
//! [`device::SharedDeviceConfig::load`] snapshot) and a [`runtime::DeviceRuntime`] (atomics; shared by every
//! callback), plus the WDF handles (`HidQueue`, the local I/O target, the lookaside list,
//! `PowerOnRecoveryTimer`).
//!
//! **`AddDevice`** (`WdfFdoInitSetFilter`; no `WdfPdoInitAllowForwardingRequestToParent`): the lookaside list
//! uses [`wire::LOWER_READ_BUFFER_LEN`] and [`wire::POOL_TAG`] (parent: the device); the passive-level
//! one-shot timer has period [`runtime::RECOVERY_TIMER_PERIOD`]; no local I/O target gives
//! [`wire::NO_IO_TARGET_STATUS`]; the device interface is [`wire::DEVICE_INTERFACE_GUID`]; the context
//! starts as [`runtime::DeviceRuntime::new`] / [`device::SharedDeviceConfig::new`].
//!
//! **`PrepareHardware`**:
//! 1. Send [`wire::LOWER_ATTRIBUTES_IOCTL`] synchronously with a zeroed [`wire::LOWER_ATTRIBUTES_LEN`]-byte
//!    output buffer, then `config = shared.load(); r = config.identify(status, &buf); shared.store(&config)`
//!    (the IDs are stored even when the lookup fails). On `Err(s)` return `s` (no registry access).
//! 2. Open the driver's Parameters key ([`device::PARAMETERS_KEY_ACCESS`]); if that fails use `None`, else
//!    query [`device::DESIRED_REPORT_TYPE_VALUE_NAME`] into a **zeroed** 4-byte buffer and use
//!    [`device::desired_report_type_value`] with `query_status` and `buf`, then close the key.
//!    `config.apply_desired_report_type(value)`, `shared.store(&config)` and return `STATUS_SUCCESS`.
//!
//! **`D0Entry`**: `runtime.on_d0_entry(freq)`, where `freq` is what `KeQueryPerformanceCounter(&mut freq)`
//! writes (the frequency, as C does).
//!
//! **`SelfManagedIoInit` / `SelfManagedIoRestart`** (one function): fill a [`wire::SetStatePacket`] in place
//! (`fill(true)`), then send its 255 bytes with `WdfIoTargetSendInternalIoctlSynchronously`
//! and [`wire::SET_STATE_IOCTL`], with no request handle, output, or options. Then call
//! `runtime.on_self_managed_io_init_or_restart(status, || freq)`; if the outcome has
//! `start_recovery_timer: Some(due)`, `WdfTimerStart(timer, due)`. Return `outcome.status`.
//!
//! **Recovery timer callback**: run the same mode switch, then
//! `runtime.on_recovery_timer(status, || issue_lower_read())`.
//!
//! **`D0Exit`**: `runtime.on_d0_exit()`, then complete every request retrieved from `HidQueue` with
//! [`runtime::D0_EXIT_DRAIN_STATUS`].
//!
//! **`EvtIoInternalDeviceControl`** dispatches according to [`ioctl::classify_internal_ioctl`].
//! - `GetDeviceDescriptor` / `GetReportDescriptor`: `WdfRequestRetrieveOutputMemory` (fail: complete with
//!   it), then call [`descriptors::hid_descriptor_for`] or [`descriptors::report_descriptor_for`]
//!   with `&config`.
//!   `Ok(bytes)`: `WdfMemoryCopyFromBuffer(mem, 0, bytes)` (fail: complete with it), Information =
//!   `bytes.len()`; `Err(s)`: complete with `s`.
//! - `GetDeviceAttributes`: `WdfRequestRetrieveOutputBuffer(32)`, then
//!   get [`descriptors::device_attributes`] for `&config` and write them to `buf`
//!   (10 bytes; `Reserved` untouched).
//!   Information = [`descriptors::DEVICE_ATTRIBUTES_INFORMATION`].
//! - `GetFeature` / `SetFeature`: see the [`feature`] module docs (Information always 0).
//! - `GetString`: format with the current type and send-and-forget to the lower target; on send failure
//!   complete with `WdfRequestGetStatus`.
//! - `ReadReport`: `WdfRequestForwardToIoQueue(HidQueue)` (fail: complete with it); then if
//!   `runtime.should_issue_lower_read()` issue one lower read (below). The dispatcher never completes it.
//! - `NotSupported`: complete with [`ioctl::NOT_SUPPORTED_STATUS`].
//!
//! **Issue a lower read**: `WdfRequestCreate` (parent: device), `WdfMemoryCreateFromLookaside`, format
//! [`wire::LOWER_READ_IOCTL`] with the memory as output, set the completion routine, `WdfRequestSend`. Any
//! failure deletes what was created and returns silently; the upstream request stays queued.
//!
//! **Lower read completion**: `WdfIoQueueRetrieveNextRequest(HidQueue)`; if none, skip to cleanup (data
//! dropped). Else `runtime.complete_lower_read(&shared.trackpad_info(), WdfRequestGetInformation(lower),
//! buffer, || freq)`; `Ok(bytes)`: retrieve output memory + copy 30 bytes + Information 30 + complete
//! SUCCESS (a WDF failure completes with that status instead); `Err(s)`: complete with `s`. Cleanup: take
//! the memory handle, delete the lower request, then delete the memory.

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

pub mod descriptors;
pub mod device;
pub mod feature;
pub mod input;
pub mod ioctl;
pub mod runtime;
pub mod status;
pub mod wire;
