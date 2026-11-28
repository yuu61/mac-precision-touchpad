//! The HID interrupt channel: which L2CAP channels carry it, and the padding of `0x31` reports.
//!
//! A channel is an interrupt channel when its open (the outgoing `BRB_L2CA_OPEN_CHANNEL` at start,
//! or the accepted incoming one after the trackpad reconnects) completes with PSM `0x13`; it stops
//! being one when `HidBth` closes it.
//!
//! An incoming transfer is padded only when all of these hold: it is on an interrupt channel, its
//! buffer is exactly [`PADDED_PAYLOAD_LEN`] bytes (`HidBth` sizes it from HIDCLASS's read, which is
//! the declared report length), it succeeded, nothing of the L2CAP packet was left over, and the
//! payload is a `0x31` report no longer than the declared length ([`pad_interrupt_payload`]).
//! Other reports (`0x90`, `0x02`, ...) and other readers' transfers are left as they are.

use amtptp_usb_bt::bluetooth::bth::{PADDED_PAYLOAD_LEN, pad_interrupt_payload};
use tracelogging as tlg;
use wdf_support::ntstatus::NT_SUCCESS;
use wdk_sys::NTSTATUS;

use crate::brb::{Brb, PSM_HID_INTERRUPT, Raw};
use crate::{DeviceContext, KW_INTERRUPT, PROVIDER};

/// Completion tag of a channel open.
const TAG_OPEN: usize = 1 << 56;
/// Completion tag of an incoming transfer on an interrupt channel.
const TAG_INTERRUPT_IN: usize = 2 << 56;
/// Mask of the tag kind.
const TAG_KIND: usize = 0xFF << 56;

/// Looks at a BRB on its way down. Records closes; returns the completion tag for the BRBs whose
/// completion this module needs (`None`: send and forget).
///
/// # Safety
/// `brb` belongs to a request this driver owns.
pub(crate) unsafe fn on_submit(ctx: &DeviceContext, brb: Brb) -> Option<usize> {
    match brb {
        Brb::Open(_) => Some(TAG_OPEN),
        Brb::Close(raw) => {
            // SAFETY: a close-channel BRB of an owned request.
            let channel = unsafe { raw.close_channel() };
            if ctx.remove_channel(channel) {
                tlg::write_event!(
                    PROVIDER,
                    "InterruptChannelClosed",
                    level(Informational),
                    keyword(KW_INTERRUPT),
                    u64_hex("Channel", &channel)
                );
            }
            None
        }
        Brb::Acl(raw) => {
            // SAFETY: an ACL-transfer BRB of an owned request.
            let acl = unsafe { raw.acl() };
            (acl.is_in()
                && acl.buffer_size as usize == PADDED_PAYLOAD_LEN
                && ctx.is_interrupt_channel(acl.channel))
            .then_some(TAG_INTERRUPT_IN)
        }
        Brb::Other => None,
    }
}

/// Looks at a tagged BRB on completion, before the upper driver sees it.
///
/// # Safety
/// `brb` belongs to a request this driver owns and has not completed yet.
pub(crate) unsafe fn on_complete(ctx: &DeviceContext, brb: Brb, tag: usize, status: NTSTATUS) {
    if !NT_SUCCESS(status) {
        return;
    }
    match (tag & TAG_KIND, brb) {
        (TAG_OPEN, Brb::Open(raw)) => {
            // SAFETY: an open-channel BRB of an owned request.
            let open = unsafe { raw.open() };
            if open.psm == PSM_HID_INTERRUPT {
                let free_slot = ctx.add_interrupt_channel(open.channel);
                tlg::write_event!(
                    PROVIDER,
                    "InterruptChannelOpened",
                    level(Informational),
                    keyword(KW_INTERRUPT),
                    u64_hex("Channel", &open.channel),
                    bool8("FreeSlot", &free_slot)
                );
            }
        }
        // SAFETY: an ACL-transfer BRB of an owned request, tagged at submission.
        (TAG_INTERRUPT_IN, Brb::Acl(raw)) => unsafe { pad(raw) },
        _ => {}
    }
}

/// Pads a completed incoming transfer whose submitted buffer was [`PADDED_PAYLOAD_LEN`] bytes.
///
/// # Safety
/// `raw` is the ACL-transfer BRB of an owned, successfully completed request that [`on_submit`]
/// tagged.
unsafe fn pad(raw: Raw) {
    // SAFETY: forwarded.
    let acl = unsafe { raw.acl() };
    let received = acl.buffer_size as usize;
    if acl.remaining != 0 || received > PADDED_PAYLOAD_LEN {
        tlg::write_event!(
            PROVIDER,
            "PadSkipped",
            level(Verbose),
            keyword(KW_INTERRUPT),
            u32("Received", &acl.buffer_size),
            u32("Remaining", &acl.remaining)
        );
        return;
    }
    // SAFETY: the submitted buffer was PADDED_PAYLOAD_LEN bytes (checked at submission) and the
    // request is ours until it completes.
    let Some(buffer) = (unsafe { acl.buffer_mut(PADDED_PAYLOAD_LEN) }) else {
        tlg::write_event!(
            PROVIDER,
            "PadBufferUnavailable",
            level(Warning),
            keyword(KW_INTERRUPT)
        );
        return;
    };
    if let Some(len) = pad_interrupt_payload(buffer, received) {
        // SAFETY: an owned ACL-transfer BRB; `len` is PADDED_PAYLOAD_LEN, the submitted size.
        unsafe { acl.set_buffer_size(len as u32) };
        tlg::write_event!(
            PROVIDER,
            "ReportPadded",
            level(Verbose),
            keyword(KW_INTERRUPT),
            u32("Received", &acl.buffer_size)
        );
    }
}
