//! The Bluetooth Request Blocks (BRBs) this filter reads or changes.
//!
//! wdk-sys has no `bthddi.h` bindings, so the layout is written out here. Every offset and size was
//! measured by compiling the WDK 10.0.28000 `bthddi.h` with `offsetof`/`sizeof` for x64 (MSVC
//! 14.44) and for ARM64 (MSVC 14.52); both give the same values. The build refuses any other
//! target. A field is only read after the header's `Length` shows that the BRB is at least as large
//! as the structure of its type.

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("AmtPtpBthFilter: the BRB offsets were measured for x64 and ARM64 only");

/// `IOCTL_INTERNAL_BTH_SUBMIT_BRB` (internal, `METHOD_NEITHER`; the BRB is `Others.Arg1`).
pub(crate) const IOCTL_SUBMIT_BRB: u32 = 0x0041_0003;

/// `sizeof(BRB_HEADER)`.
const HEADER_SIZE: usize = 112;
/// `BRB_HEADER.Length` (ULONG).
const LENGTH: usize = 16;
/// `BRB_HEADER.Type` (USHORT).
const TYPE: usize = 22;

/// `BRB_L2CA_OPEN_CHANNEL`.
const T_OPEN: u16 = 0x0102;
/// `BRB_L2CA_OPEN_CHANNEL_RESPONSE`.
const T_OPEN_RESPONSE: u16 = 0x0103;
/// `BRB_L2CA_CLOSE_CHANNEL`.
const T_CLOSE: u16 = 0x0104;
/// `BRB_L2CA_ACL_TRANSFER`.
const T_ACL: u16 = 0x0105;
/// `BRB_L2CA_OPEN_ENHANCED_CHANNEL`.
const T_OPEN_ENHANCED: u16 = 0x0212;
/// `BRB_L2CA_OPEN_ENHANCED_CHANNEL_RESPONSE`.
const T_OPEN_ENHANCED_RESPONSE: u16 = 0x0213;

/// `ChannelHandle` of `_BRB_L2CA_OPEN_CHANNEL` and `_BRB_L2CA_OPEN_ENHANCED_CHANNEL`.
const OPEN_CHANNEL_HANDLE: usize = 112;
/// The `Psm` / `Response` union (USHORT) of both open structures. On completion it holds the PSM
/// for an outgoing open and for an accepted incoming one (seen on hardware, Windows 11 26300).
const OPEN_PSM: usize = 120;
/// `sizeof(_BRB_L2CA_OPEN_CHANNEL)`.
const OPEN_SIZE: usize = 368;
/// `sizeof(_BRB_L2CA_OPEN_ENHANCED_CHANNEL)`.
const OPEN_ENHANCED_SIZE: usize = 480;

/// `_BRB_L2CA_CLOSE_CHANNEL.ChannelHandle`.
const CLOSE_CHANNEL_HANDLE: usize = 120;
/// `sizeof(_BRB_L2CA_CLOSE_CHANNEL)`.
const CLOSE_SIZE: usize = 128;

/// `_BRB_L2CA_ACL_TRANSFER.ChannelHandle`.
const ACL_CHANNEL_HANDLE: usize = 120;
/// `_BRB_L2CA_ACL_TRANSFER.TransferFlags` (ULONG).
const ACL_TRANSFER_FLAGS: usize = 128;
/// `_BRB_L2CA_ACL_TRANSFER.BufferSize` (ULONG).
const ACL_BUFFER_SIZE: usize = 132;
/// `_BRB_L2CA_ACL_TRANSFER.Buffer` (PVOID).
const ACL_BUFFER: usize = 136;
/// `_BRB_L2CA_ACL_TRANSFER.BufferMDL` (PMDL).
const ACL_BUFFER_MDL: usize = 144;
/// `_BRB_L2CA_ACL_TRANSFER.RemainingBufferSize` (ULONG).
const ACL_REMAINING: usize = 160;
/// `sizeof(_BRB_L2CA_ACL_TRANSFER)`.
const ACL_SIZE: usize = 168;

/// `ACL_TRANSFER_DIRECTION_IN`.
const ACL_DIRECTION_IN: u32 = 0x1;

/// PSM of the HID interrupt channel.
pub(crate) const PSM_HID_INTERRUPT: u16 = 0x13;

// Every field lies past the header and inside the size `Length` is checked against.
const _: () = {
    assert!(TYPE + 2 <= HEADER_SIZE && LENGTH + 4 <= HEADER_SIZE);
    assert!(OPEN_CHANNEL_HANDLE >= HEADER_SIZE && OPEN_PSM + 2 <= OPEN_SIZE);
    assert!(OPEN_SIZE <= OPEN_ENHANCED_SIZE);
    assert!(CLOSE_CHANNEL_HANDLE >= HEADER_SIZE && CLOSE_CHANNEL_HANDLE + 8 <= CLOSE_SIZE);
    assert!(ACL_CHANNEL_HANDLE >= HEADER_SIZE && ACL_REMAINING + 4 <= ACL_SIZE);
};

/// A BRB of a known type whose `Length` covers its structure.
#[derive(Clone, Copy)]
pub(crate) enum Brb {
    /// `BRB_L2CA_OPEN_CHANNEL(_RESPONSE)` or the enhanced variants.
    Open(Raw),
    /// `BRB_L2CA_CLOSE_CHANNEL`.
    Close(Raw),
    /// `BRB_L2CA_ACL_TRANSFER`.
    Acl(Raw),
    /// Any other BRB; this filter passes it on untouched.
    Other,
}

/// A BRB pointer. Only valid while the request that carries it is owned by this driver.
#[derive(Clone, Copy)]
pub(crate) struct Raw(*mut u8);

impl Raw {
    /// Reads a `T` at `offset`.
    ///
    /// # Safety
    /// The BRB is live and at least `offset + size_of::<T>()` bytes long.
    const unsafe fn read<T: Copy>(self, offset: usize) -> T {
        // SAFETY: in bounds by the caller's guarantee; read_unaligned needs no alignment.
        unsafe { self.0.add(offset).cast::<T>().read_unaligned() }
    }

    /// Reads an open-channel BRB.
    ///
    /// # Safety
    /// `self` came from [`Brb::Open`] and its request is still owned.
    pub(crate) const unsafe fn open(self) -> Open {
        // SAFETY: Length covers the open structure (checked by `classify`).
        unsafe {
            Open {
                channel: self.read::<u64>(OPEN_CHANNEL_HANDLE),
                psm: self.read::<u16>(OPEN_PSM),
            }
        }
    }

    /// `ChannelHandle` of a close-channel BRB.
    ///
    /// # Safety
    /// `self` came from [`Brb::Close`] and its request is still owned.
    pub(crate) const unsafe fn close_channel(self) -> u64 {
        // SAFETY: Length covers the close structure (checked by `classify`).
        unsafe { self.read::<u64>(CLOSE_CHANNEL_HANDLE) }
    }

    /// Reads an ACL-transfer BRB.
    ///
    /// # Safety
    /// `self` came from [`Brb::Acl`] and its request is still owned.
    pub(crate) const unsafe fn acl(self) -> Acl {
        // SAFETY: Length covers the ACL-transfer structure (checked by `classify`).
        unsafe {
            Acl {
                raw: self,
                channel: self.read::<u64>(ACL_CHANNEL_HANDLE),
                flags: self.read::<u32>(ACL_TRANSFER_FLAGS),
                buffer_size: self.read::<u32>(ACL_BUFFER_SIZE),
                buffer: self.read::<*mut u8>(ACL_BUFFER),
                mdl: self.read::<*mut wdk_sys::MDL>(ACL_BUFFER_MDL),
                remaining: self.read::<u32>(ACL_REMAINING),
            }
        }
    }
}

impl Brb {
    /// Classifies the BRB of an `IOCTL_INTERNAL_BTH_SUBMIT_BRB` request.
    ///
    /// # Safety
    /// `ptr` is null or the BRB of a request this driver owns, and the result is only used while
    /// it still owns that request.
    pub(crate) unsafe fn classify(ptr: *mut u8) -> Self {
        if ptr.is_null() {
            return Self::Other;
        }
        let raw = Raw(ptr);
        // SAFETY: every BRB starts with a BRB_HEADER.
        let (kind, length) = unsafe { (raw.read::<u16>(TYPE), raw.read::<u32>(LENGTH)) };
        let length = usize::try_from(length).unwrap_or(0);
        match kind {
            T_OPEN | T_OPEN_RESPONSE if length >= OPEN_SIZE => Self::Open(raw),
            T_OPEN_ENHANCED | T_OPEN_ENHANCED_RESPONSE if length >= OPEN_ENHANCED_SIZE => {
                Self::Open(raw)
            }
            T_CLOSE if length >= CLOSE_SIZE => Self::Close(raw),
            T_ACL if length >= ACL_SIZE => Self::Acl(raw),
            _ => Self::Other,
        }
    }
}

/// The fields of an open-channel BRB.
pub(crate) struct Open {
    /// `ChannelHandle`.
    pub(crate) channel: u64,
    /// The `Psm` / `Response` union.
    pub(crate) psm: u16,
}

/// The fields of an ACL-transfer BRB.
pub(crate) struct Acl {
    raw: Raw,
    /// `ChannelHandle`.
    pub(crate) channel: u64,
    /// `TransferFlags`.
    flags: u32,
    /// `BufferSize`: the buffer's size on submission, the bytes received on completion.
    pub(crate) buffer_size: u32,
    /// `Buffer`.
    buffer: *mut u8,
    /// `BufferMDL`.
    mdl: *mut wdk_sys::MDL,
    /// `RemainingBufferSize`: the bytes of the L2CAP packet that did not fit.
    pub(crate) remaining: u32,
}

/// `MDL_MAPPED_TO_SYSTEM_VA | MDL_SOURCE_IS_NONPAGED_POOL`: `MappedSystemVa` is valid without
/// creating a mapping.
const MDL_HAS_SYSTEM_VA: i16 = 0x0001 | 0x0004;

impl Acl {
    /// Whether this is an incoming transfer.
    pub(crate) const fn is_in(&self) -> bool {
        self.flags & ACL_DIRECTION_IN != 0
    }

    /// The transfer buffer, if it is reachable without creating a mapping and holds at least `len`
    /// bytes.
    ///
    /// # Safety
    /// The BRB's request is still owned and the transfer's buffer holds at least `len` bytes (its
    /// submitted `BufferSize`). The slice must not outlive the request.
    pub(crate) unsafe fn buffer_mut<'a>(&self, len: usize) -> Option<&'a mut [u8]> {
        let data = if !self.buffer.is_null() {
            self.buffer
        } else if !self.mdl.is_null() {
            // SAFETY: the transfer's live MDL.
            let (flags, va, count) = unsafe {
                (
                    (*self.mdl).MdlFlags,
                    (*self.mdl).MappedSystemVa,
                    (*self.mdl).ByteCount,
                )
            };
            if flags & MDL_HAS_SYSTEM_VA == 0 || va.is_null() || (count as usize) < len {
                return None;
            }
            va.cast::<u8>()
        } else {
            return None;
        };
        // SAFETY: `len` bytes are writable by the caller's guarantee, and nothing else accesses
        // them while this driver owns the request.
        Some(unsafe { core::slice::from_raw_parts_mut(data, len) })
    }

    /// Sets `BufferSize` (the bytes the upper driver will see).
    ///
    /// # Safety
    /// The BRB's request is still owned.
    pub(crate) const unsafe fn set_buffer_size(&self, size: u32) {
        // SAFETY: Length covers the ACL-transfer structure; the BRB is writable, and the upper
        // driver reads it only after this driver completes the request.
        unsafe {
            self.raw
                .0
                .add(ACL_BUFFER_SIZE)
                .cast::<u32>()
                .write_unaligned(size);
        }
    }
}
