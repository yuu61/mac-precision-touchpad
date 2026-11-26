//! Event keywords matching the C WPP flag bits in `Trace.h`.

/// `TRACE_DRIVER`: driver and device lifetime, PnP and power, and mode switching.
pub(crate) const DRIVER: u64 = 0x1;
/// `TRACE_DEVICE`: lower read requests.
pub(crate) const DEVICE: u64 = 0x2;
/// `TRACE_QUEUE`: queues and IOCTL dispatch.
pub(crate) const QUEUE: u64 = 0x4;
/// `TRACE_HID_INPUT`: input report conversion.
pub(crate) const HID_INPUT: u64 = 0x8;
