//! Device identification, per-transport parsing parameters, the mode-switch packet, and the device
//! state the C `DEVICE_CONTEXT` keeps.
//!
//! The driver has **no device table**. Support is decided by hard-coded comparisons (spec section 8):
//! the vendor ID must be Apple USB (0x05AC) or Apple Bluetooth (0x004C) and the product ID must be the
//! Magic Trackpad 2 (0x0265). [`Transport::from_ids`] reproduces those checks.

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU16, AtomicUsize, Ordering};

use bcm5974::config::MAGIC_TRACKPAD_2;
pub use bcm5974::config::{
    Bcm5974Param, MAGIC_TRACKPAD_2_PID as HID_PID_MAGIC_TRACKPAD_2,
    USB_VENDOR_ID_APPLE as HID_VID_APPLE_USB,
};

use crate::feature::FeatureChange;
use crate::input::InputLayout;
use crate::status::{NtStatus, STATUS_NOT_SUPPORTED};

/// `HID_VID_APPLE_BT` (Device.h:15): Apple's Bluetooth vendor ID.
pub const HID_VID_APPLE_BT: u16 = 0x004C;
/// `DEVICE_VERSION` (HidCommon.h:43): the version number reported upward in `HID_DEVICE_ATTRIBUTES`.
pub const DEVICE_VERSION: u16 = 0x0100;

/// True for the two vendor IDs the driver accepts (0x05AC, 0x004C). Used by the mode switch
/// (Device.c:308) and by the lower-read completion (Input.c:156).
#[must_use]
pub const fn is_apple_vendor(vendor_id: u16) -> bool {
    vendor_id == HID_VID_APPLE_USB || vendor_id == HID_VID_APPLE_BT
}

/// The parameters `PtpFilterConfigureMultiTouch` writes into the device context (Device.c:322-354).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportParams {
    /// `InputHeaderSize`: raw-report bytes before finger 0 (USB 12, BT 4).
    pub header_size: usize,
    /// `InputFingerSize`: bytes per finger record (9, `FSIZE_TYPE5`).
    pub finger_size: usize,
    /// `InputFingerDelta`: extra offset to finger 0 (0, `FDELTA_TYPE5`).
    pub finger_delta: usize,
    /// `InputButtonDelta`: absolute offset of the button byte (1, `BOFFSET_TYPE5`).
    pub button_delta: usize,
    /// `X` = [-3678, 3934].
    pub x: Bcm5974Param,
    /// `Y` = [-2479, 2586].
    pub y: Bcm5974Param,
}

/// The Magic Trackpad 2 transport, chosen by vendor ID (Device.c:322 / 343).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// VID 0x05AC (only bindable through the standalone INF).
    Usb,
    /// VID 0x004C (the shipping configuration).
    Bluetooth,
}

impl Transport {
    /// The support checks of `PtpFilterConfigureMultiTouch` (Device.c:308-317), in C order: an unknown
    /// vendor ID, then a product ID other than 0x0265, give `STATUS_NOT_SUPPORTED`. Neither failure
    /// writes any device-context field.
    ///
    /// # Errors
    /// Returns `STATUS_NOT_SUPPORTED` if the vendor or product ID has no supported configuration.
    pub const fn from_ids(vendor_id: u16, product_id: u16) -> Result<Self, NtStatus> {
        if !is_apple_vendor(vendor_id) {
            return Err(STATUS_NOT_SUPPORTED);
        }
        if product_id != HID_PID_MAGIC_TRACKPAD_2 {
            return Err(STATUS_NOT_SUPPORTED);
        }
        if vendor_id == HID_VID_APPLE_USB {
            Ok(Self::Usb)
        } else {
            Ok(Self::Bluetooth)
        }
    }

    /// The parsing parameters for this transport. They differ only in the header size.
    #[must_use]
    pub const fn params(self) -> TransportParams {
        let format = MAGIC_TRACKPAD_2.tp_type;
        TransportParams {
            header_size: match self {
                Self::Usb => format.header() as usize,
                Self::Bluetooth => 4, // HOFFSET_TYPE_BTH_5 = 2 * 2
            },
            finger_size: format.finger_size() as usize,
            finger_delta: format.delta() as usize,
            button_delta: format.button() as usize,
            x: MAGIC_TRACKPAD_2.x,
            y: MAGIC_TRACKPAD_2.y,
        }
    }

    /// The vendor `SET_FEATURE` that switches the trackpad into raw multitouch mode (Device.c:335-361).
    #[must_use]
    pub const fn mode_switch(self) -> ModeSwitch {
        match self {
            Self::Usb => ModeSwitch {
                report_id: 0x02,
                payload: &[0x02, 0x01, 0x00, 0x00],
            },
            Self::Bluetooth => ModeSwitch {
                report_id: 0xF1,
                payload: &[0xF1, 0x02, 0x01],
            },
        }
    }
}

/// One mode-switch feature report: `HID_XFER_PACKET.reportId` and the `reportBufferLen` bytes at
/// `reportBuffer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeSwitch {
    /// `reportId` (USB 0x02, BT 0xF1).
    pub report_id: u8,
    /// The report bytes; `reportBufferLen` is their length (USB `02 01 00 00`, BT `F1 02 01`).
    pub payload: &'static [u8],
}

/// `sizeof(HID_XFER_PACKET)` on LP64: 16 bytes (`reportBuffer`@0, `reportBufferLen`@8, `reportId`@12,
/// 3 padding bytes). `GET/SET_FEATURE` reject a transfer buffer shorter than this.
pub const HID_XFER_PACKET_LEN: usize = 16;
/// `offsetof(HID_XFER_PACKET, reportBuffer)`.
pub const XFER_REPORT_BUFFER_OFFSET: usize = 0;
/// `offsetof(HID_XFER_PACKET, reportBufferLen)`.
pub const XFER_REPORT_BUFFER_LEN_OFFSET: usize = 8;
/// `offsetof(HID_XFER_PACKET, reportId)`.
pub const XFER_REPORT_ID_OFFSET: usize = 12;
/// `HID_XFER_PACKET_SIZE` (Device.h:12): the 32-byte buffer that holds the mode-switch packet followed
/// by its report bytes. It is both the preallocated WDFMEMORY (input length 32) and `Irp->UserBuffer`.
pub const HID_XFER_PACKET_SIZE: usize = 32;

/// The 32-byte mode-switch buffer: a `HID_XFER_PACKET` at offset 0 whose `reportBuffer` points at the
/// report bytes at offset 16 of the same buffer (self-referential, Device.c:319-361).
///
/// It is 8-byte aligned (the C `UCHAR[32]` is not guaranteed to be). All other bytes are zero, as after
/// the C `RtlZeroMemory`. The buffer must not move between [`XferBuffer::link_report_buffer`] and the
/// end of the synchronous send; [`crate::configure_multitouch`] keeps it in one local for that time.
#[repr(C, align(8))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XferBuffer {
    bytes: [u8; HID_XFER_PACKET_SIZE],
}

impl XferBuffer {
    /// Builds the zeroed packet with `reportBufferLen`, `reportId` and the report bytes. The
    /// `reportBuffer` pointer (bytes 0..8) is left 0 until [`Self::link_report_buffer`].
    #[must_use]
    pub fn new(mode_switch: ModeSwitch) -> Self {
        let mut bytes = [0_u8; HID_XFER_PACKET_SIZE];
        // A payload is at most 4 bytes, so the length always fits in the ULONG.
        let len = u32::try_from(mode_switch.payload.len()).unwrap_or(0);
        copy_into(
            &mut bytes,
            XFER_REPORT_BUFFER_LEN_OFFSET,
            &len.to_le_bytes(),
        );
        copy_into(&mut bytes, XFER_REPORT_ID_OFFSET, &[mode_switch.report_id]);
        copy_into(&mut bytes, HID_XFER_PACKET_LEN, mode_switch.payload);
        Self { bytes }
    }

    /// Writes `reportBuffer` = the address of byte 16 of this buffer (`(PUCHAR)pHidPacket +
    /// sizeof(HID_XFER_PACKET)`), little-endian, into bytes 0..8.
    pub fn link_report_buffer(&mut self) {
        // Pointer-to-integer conversion: the address the minidriver will dereference.
        let address = (self.bytes.as_ptr() as usize).wrapping_add(HID_XFER_PACKET_LEN) as u64;
        copy_into(
            &mut self.bytes,
            XFER_REPORT_BUFFER_OFFSET,
            &address.to_le_bytes(),
        );
    }

    /// The 32 bytes as they are sent.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; HID_XFER_PACKET_SIZE] {
        &self.bytes
    }

    /// Pointer to the start of the buffer, for `WdfMemoryCreatePreallocated` and `Irp->UserBuffer`.
    pub const fn as_mut_ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr()
    }
}

/// Copies `src` to `dst[offset..]`, never panicking (bytes that do not fit are dropped; every caller
/// writes within the 32-byte buffer).
fn copy_into(dst: &mut [u8], offset: usize, src: &[u8]) {
    if let Some(tail) = dst.get_mut(offset..) {
        for (d, s) in tail.iter_mut().zip(src) {
            *d = *s;
        }
    }
}

/// A plain copy of every [`DeviceState`] field, for tracing and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceSnapshot {
    /// `VendorID` (the real VID of the lower device).
    pub vendor_id: u16,
    /// `ProductID`.
    pub product_id: u16,
    /// `VersionNumber`.
    pub version_number: u16,
    /// `InputHeaderSize`.
    pub input_header_size: usize,
    /// `InputFingerSize`.
    pub input_finger_size: usize,
    /// `InputFingerDelta`.
    pub input_finger_delta: usize,
    /// `InputButtonDelta`.
    pub input_button_delta: usize,
    /// `X`.
    pub x: Bcm5974Param,
    /// `Y`.
    pub y: Bcm5974Param,
    /// `DeviceConfigured`.
    pub device_configured: bool,
    /// `IsHidIoDetourCompleted`.
    pub detour_completed: bool,
    /// `HidIoTarget != NULL` (the glue owns the handle itself).
    pub io_target_set: bool,
    /// `PtpInputOn` (written by `SET_FEATURE` 0x04, never read: HF-B20).
    pub ptp_input_on: bool,
    /// `PtpReportTouch` (written by `SET_FEATURE` 0x06, never read: HF-B20).
    pub ptp_report_touch: bool,
    /// `PtpReportButton` (written by `SET_FEATURE` 0x06, never read: HF-B20).
    pub ptp_report_button: bool,
    /// `LastReportTime.QuadPart`: 0 or the QPC frequency (HF-B6).
    pub last_report_time: i64,
}

#[derive(Debug)]
struct AxisState {
    min: AtomicI32,
    max: AtomicI32,
}

impl AxisState {
    const fn new(p: Bcm5974Param) -> Self {
        Self {
            min: AtomicI32::new(p.min),
            max: AtomicI32::new(p.max),
        }
    }

    fn load(&self) -> Bcm5974Param {
        Bcm5974Param {
            min: self.min.load(Ordering::Relaxed),
            max: self.max.load(Ordering::Relaxed),
        }
    }

    fn store(&self, p: Bcm5974Param) {
        self.min.store(p.min, Ordering::Relaxed);
        self.max.store(p.max, Ordering::Relaxed);
    }
}

/// The non-handle fields of the C `DEVICE_CONTEXT` (spec 3.1), shared by every callback.
///
/// The C driver reads and writes these fields without any lock from the parallel default queue, the
/// lower-read completion routine, the passive timer, the work item and the PnP/power callbacks. To keep
/// that behaviour without data races (port policy P2), every field is an atomic accessed with `Relaxed`
/// ordering, which is what the plain C loads and stores amount to. Put one `DeviceState` in the WDF
/// device context (initialize it with [`DeviceState::new`], which equals KMDF's zero-filled context).
///
/// The glue keeps the WDF handles (queues, timer, work item, lookaside list, I/O target) itself.
#[derive(Debug)]
pub struct DeviceState {
    vendor_id: AtomicU16,
    product_id: AtomicU16,
    version_number: AtomicU16,
    input_header_size: AtomicUsize,
    input_finger_size: AtomicUsize,
    input_finger_delta: AtomicUsize,
    input_button_delta: AtomicUsize,
    x: AxisState,
    y: AxisState,
    device_configured: AtomicBool,
    detour_completed: AtomicBool,
    io_target_set: AtomicBool,
    ptp_input_on: AtomicBool,
    ptp_report_touch: AtomicBool,
    ptp_report_button: AtomicBool,
    last_report_time: AtomicI64,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceState {
    /// All fields zero / FALSE: the state after `PtpFilterCreateDevice` (KMDF zero-fills the context
    /// and the C code then sets the IDs to 0 and `DeviceConfigured = FALSE` again).
    #[must_use]
    pub const fn new() -> Self {
        Self::with(&DeviceSnapshot {
            vendor_id: 0,
            product_id: 0,
            version_number: 0,
            input_header_size: 0,
            input_finger_size: 0,
            input_finger_delta: 0,
            input_button_delta: 0,
            x: Bcm5974Param { min: 0, max: 0 },
            y: Bcm5974Param { min: 0, max: 0 },
            device_configured: false,
            detour_completed: false,
            io_target_set: false,
            ptp_input_on: false,
            ptp_report_touch: false,
            ptp_report_button: false,
            last_report_time: 0,
        })
    }

    /// A state holding exactly the values of `s` (tests, and glue diagnostics).
    #[must_use]
    pub const fn with(s: &DeviceSnapshot) -> Self {
        Self {
            vendor_id: AtomicU16::new(s.vendor_id),
            product_id: AtomicU16::new(s.product_id),
            version_number: AtomicU16::new(s.version_number),
            input_header_size: AtomicUsize::new(s.input_header_size),
            input_finger_size: AtomicUsize::new(s.input_finger_size),
            input_finger_delta: AtomicUsize::new(s.input_finger_delta),
            input_button_delta: AtomicUsize::new(s.input_button_delta),
            x: AxisState::new(s.x),
            y: AxisState::new(s.y),
            device_configured: AtomicBool::new(s.device_configured),
            detour_completed: AtomicBool::new(s.detour_completed),
            io_target_set: AtomicBool::new(s.io_target_set),
            ptp_input_on: AtomicBool::new(s.ptp_input_on),
            ptp_report_touch: AtomicBool::new(s.ptp_report_touch),
            ptp_report_button: AtomicBool::new(s.ptp_report_button),
            last_report_time: AtomicI64::new(s.last_report_time),
        }
    }

    /// Reads every field (each with its own relaxed load).
    pub fn snapshot(&self) -> DeviceSnapshot {
        DeviceSnapshot {
            vendor_id: self.vendor_id(),
            product_id: self.product_id(),
            version_number: self.version_number.load(Ordering::Relaxed),
            input_header_size: self.input_header_size.load(Ordering::Relaxed),
            input_finger_size: self.input_finger_size.load(Ordering::Relaxed),
            input_finger_delta: self.input_finger_delta.load(Ordering::Relaxed),
            input_button_delta: self.input_button_delta.load(Ordering::Relaxed),
            x: self.x.load(),
            y: self.y.load(),
            device_configured: self.device_configured(),
            detour_completed: self.detour_completed(),
            io_target_set: self.io_target_set(),
            ptp_input_on: self.ptp_input_on.load(Ordering::Relaxed),
            ptp_report_touch: self.ptp_report_touch.load(Ordering::Relaxed),
            ptp_report_button: self.ptp_report_button.load(Ordering::Relaxed),
            last_report_time: self.last_report_time(),
        }
    }

    /// `VendorID`.
    pub fn vendor_id(&self) -> u16 {
        self.vendor_id.load(Ordering::Relaxed)
    }

    /// `ProductID`.
    pub fn product_id(&self) -> u16 {
        self.product_id.load(Ordering::Relaxed)
    }

    /// Stores `VendorID`, `ProductID` and `VersionNumber` (`SelfManagedIoInit` after the attributes query,
    /// Device.c:221-223; `PrepareHardware` with zeros).
    pub fn set_ids(&self, vendor_id: u16, product_id: u16, version_number: u16) {
        self.vendor_id.store(vendor_id, Ordering::Relaxed);
        self.product_id.store(product_id, Ordering::Relaxed);
        self.version_number.store(version_number, Ordering::Relaxed);
    }

    /// The fields the lower-read completion reads to parse a packet.
    pub fn input_layout(&self) -> InputLayout {
        InputLayout {
            header_size: self.input_header_size.load(Ordering::Relaxed),
            finger_size: self.input_finger_size.load(Ordering::Relaxed),
            finger_delta: self.input_finger_delta.load(Ordering::Relaxed),
            button_delta: self.input_button_delta.load(Ordering::Relaxed),
            x_min: self.x.min.load(Ordering::Relaxed),
            y_min: self.y.min.load(Ordering::Relaxed),
        }
    }

    /// Writes the transport parameters (`ConfigureMultiTouch`, before the send).
    pub fn apply_transport_params(&self, p: &TransportParams) {
        self.input_finger_size
            .store(p.finger_size, Ordering::Relaxed);
        self.input_header_size
            .store(p.header_size, Ordering::Relaxed);
        self.input_finger_delta
            .store(p.finger_delta, Ordering::Relaxed);
        self.input_button_delta
            .store(p.button_delta, Ordering::Relaxed);
        self.x.store(p.x);
        self.y.store(p.y);
    }

    /// `DeviceConfigured`: the mode switch is done and upper reads may trigger lower reads.
    pub fn device_configured(&self) -> bool {
        self.device_configured.load(Ordering::Relaxed)
    }

    /// Sets `DeviceConfigured`.
    pub fn set_device_configured(&self, configured: bool) {
        self.device_configured.store(configured, Ordering::Relaxed);
    }

    /// `IsHidIoDetourCompleted`.
    pub fn detour_completed(&self) -> bool {
        self.detour_completed.load(Ordering::Relaxed)
    }

    /// Sets `IsHidIoDetourCompleted = TRUE` (Detour.c:76).
    pub fn set_detour_completed(&self) {
        self.detour_completed.store(true, Ordering::Relaxed);
    }

    /// `HidIoTarget != NULL`.
    pub fn io_target_set(&self) -> bool {
        self.io_target_set.load(Ordering::Relaxed)
    }

    /// Records that the glue stored the default I/O target (Detour.c:77).
    pub fn set_io_target_set(&self) {
        self.io_target_set.store(true, Ordering::Relaxed);
    }

    /// `LastReportTime.QuadPart`.
    pub fn last_report_time(&self) -> i64 {
        self.last_report_time.load(Ordering::Relaxed)
    }

    /// `KeQueryPerformanceCounter(&ctx->LastReportTime)`: stores the value written through the
    /// frequency pointer (Device.c:237, 276).
    pub fn store_last_report_time(&self, value: i64) {
        self.last_report_time.store(value, Ordering::Relaxed);
    }

    /// Stores `value` and returns the previous `LastReportTime` (the read-then-write of Input.c:188-190).
    pub fn swap_last_report_time(&self, value: i64) -> i64 {
        self.last_report_time.swap(value, Ordering::Relaxed)
    }

    /// Applies a decoded `SET_FEATURE` ([`crate::decode_set_feature`]).
    pub fn apply_feature_change(&self, change: FeatureChange) {
        match change {
            FeatureChange::None => {}
            FeatureChange::InputMode { ptp_input_on } => {
                self.ptp_input_on.store(ptp_input_on, Ordering::Relaxed);
            }
            FeatureChange::SelectiveReporting {
                button_report,
                surface_report,
            } => {
                self.ptp_report_button
                    .store(button_report, Ordering::Relaxed);
                self.ptp_report_touch
                    .store(surface_report, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_checks_in_c_order() {
        assert_eq!(
            Transport::from_ids(0x004C, 0x0265),
            Ok(Transport::Bluetooth)
        );
        assert_eq!(Transport::from_ids(0x05AC, 0x0265), Ok(Transport::Usb));
        assert_eq!(
            Transport::from_ids(0x1234, 0x0265),
            Err(STATUS_NOT_SUPPORTED)
        );
        assert_eq!(
            Transport::from_ids(0x004C, 0x0266),
            Err(STATUS_NOT_SUPPORTED)
        );
        assert_eq!(Transport::Usb.params().header_size, 12);
        assert_eq!(Transport::Bluetooth.params().header_size, 4);
    }

    #[test]
    fn xfer_buffer_layout() {
        let mut b = XferBuffer::new(Transport::Bluetooth.mode_switch());
        assert_eq!(
            b.as_bytes()[8..],
            [
                3, 0, 0, 0, 0xF1, 0, 0, 0, 0xF1, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        assert_eq!(b.as_bytes()[..8], [0; 8]);
        b.link_report_buffer();
        let base = b.as_bytes().as_ptr() as u64;
        let ptr = u64::from_le_bytes(b.as_bytes()[..8].try_into().unwrap());
        assert_eq!(ptr - base, 16);
        assert_eq!(core::mem::align_of::<XferBuffer>(), 8);
    }

    #[test]
    fn state_round_trip() {
        let s = DeviceSnapshot {
            vendor_id: 0x4C,
            product_id: 0x265,
            input_header_size: 4,
            x: MAGIC_TRACKPAD_2.x,
            last_report_time: -5,
            ptp_report_touch: true,
            ..DeviceSnapshot::default()
        };
        assert_eq!(DeviceState::with(&s).snapshot(), s);
        assert_eq!(DeviceState::new().snapshot(), DeviceSnapshot::default());
    }
}
