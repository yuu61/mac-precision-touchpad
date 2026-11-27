//! Framework-object configuration the glue must use (from `PtpFilterCreateDevice`, Device.c:10-102,
//! and `PtpFilterIoQueueInitialize`, Queue.c:10-54), including the P2 deviations.
//!
//! These are plain values; the glue turns them into `WDF_*_CONFIG` structures with the wdf-support
//! INIT helpers.

/// `WDF_REL_TIMEOUT_IN_SEC(n)`: a relative due time in 100 ns units (negative).
#[must_use]
pub const fn rel_timeout_in_sec(seconds: i64) -> i64 {
    -(seconds * 10_000_000)
}

/// Due time of the recovery timer after a failed mode switch (Device.c:232, 266): 2 s.
pub const CONFIGURE_RETRY_DUE_TIME: i64 = rel_timeout_in_sec(2);
/// Due time of the recovery timer after a lower-read create/send failure or a malformed packet
/// (Input.c:63, 107, 171): 3 s.
pub const TRANSPORT_RETRY_DUE_TIME: i64 = rel_timeout_in_sec(3);

/// Failed lower reads in a row that are re-issued at once by the work item, as C did (not in C: C had
/// no limit). Past this, a failed read waits for the 3 s recovery timer instead, so a lower driver that
/// fails every read immediately (a dropped link) cannot keep a worker thread in a zero-delay loop.
///
/// `HidBth` fails a read with `STATUS_DEVICE_DATA_ERROR` now and then during normal use. It is the read sent
/// right after the Bluetooth device's own HIDCLASS took a multi-finger packet (`docs/rust-port/hidfilter.md`,
/// "Competing reader on Bluetooth"). On hardware these failures came singly or in pairs between
/// successful reads. Waiting 3 s for each (as the first version of this fix did) left no read in flight
/// after two of them and froze input for 3 s. The limit is far above such runs.
pub const MAX_IMMEDIATE_READ_RETRIES: u32 = 8;

/// `GUID_DEVICEINTERFACE_AmtPtpHidFilter` `{FF969022-3111-4441-8F88-875440172C2E}` (Device.h:7-9), as
/// (Data1, Data2, Data3, Data4). Registered with no reference string; no handler serves it (HF-B28,
/// preserved).
pub const DEVICE_INTERFACE_GUID: (u32, u16, u16, [u8; 8]) = (
    0xFF96_9022,
    0x3111,
    0x4441,
    [0x8F, 0x88, 0x87, 0x54, 0x40, 0x17, 0x2C, 0x2E],
);

/// `PTP_LIST_POOL_TAG` `'LTPA'` (Driver.h:32): the MSVC multi-character constant 0x4C545041, which
/// reads "APTL" in memory. Pool tag of the lookaside list (`NonPagedPoolNx`, entry size
/// [`crate::REPORT_BUFFER_SIZE`]).
pub const READ_BUFFER_POOL_TAG: u32 = u32::from_le_bytes(*b"APTL");

/// The lookaside list's parent. C passes `WDF_NO_OBJECT_ATTRIBUTES`, which parents it to the WDFDRIVER
/// and leaks one list per `AddDevice` (HF-B15). The port parents it to the device (port policy P2).
pub const READ_BUFFER_LOOKASIDE_PARENT_IS_DEVICE: bool = true;

/// `WDF_TIMER_CONFIG.Period` of the recovery timer: 0 (one-shot). Its attributes use
/// `ExecutionLevel = WdfExecutionLevelPassive` and `ParentObject = device`.
pub const RECOVERY_TIMER_PERIOD: u32 = 0;
/// `WDF_TIMER_CONFIG.AutomaticSerialization` of the recovery timer. C sets TRUE, which can make
/// `WdfTimerCreate` fail with `STATUS_WDF_INCOMPATIBLE_EXECUTION_LEVEL` (device execution level is
/// Dispatch); with synchronization scope None it has no effect. The port uses FALSE (HF-B16).
pub const RECOVERY_TIMER_AUTOMATIC_SERIALIZATION: bool = false;
/// `WDF_WORKITEM_CONFIG.AutomaticSerialization` of the recovery work item (parent = device): FALSE
/// for the same reason (HF-B16; C uses the INIT default TRUE).
pub const RECOVERY_WORK_ITEM_AUTOMATIC_SERIALIZATION: bool = false;

/// Default queue: `WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(WdfIoQueueDispatchParallel)` with
/// `PowerManaged = WdfUseDefault` (not power-managed on a filter), `EvtIoInternalDeviceControl` and a
/// no-op `EvtIoStop`. Its `QUEUE_CONTEXT.DeviceIoTarget` field is dropped (always NULL, HF-B19).
pub const DEFAULT_QUEUE_PARALLEL: bool = true;
/// `HidReadQueue`: `WDF_IO_QUEUE_CONFIG_INIT(WdfIoQueueDispatchManual)`, `PowerManaged = WdfFalse`,
/// no callbacks, default attributes (parent = device).
pub const READ_QUEUE_POWER_MANAGED: bool = false;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        assert_eq!(CONFIGURE_RETRY_DUE_TIME, -20_000_000);
        assert_eq!(TRANSPORT_RETRY_DUE_TIME, -30_000_000);
        assert_eq!(READ_BUFFER_POOL_TAG, 0x4C54_5041);
    }
}
