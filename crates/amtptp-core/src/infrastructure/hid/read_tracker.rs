//! Bookkeeping of the lower reads a driver has in flight, with explicit pre-send/sent ownership
//! and an *epoch* that every suspend advances.
//!
//! Lower-read submission spans three steps across a spinlock boundary:
//! 1. [`ReadTracker::prepare`] (under lock): claims a slot in the `Preparing` state and returns a
//!    unique [`ReadTicket`] written into the request context before `WdfRequestSend`.
//! 2. `WdfRequestSend` (outside lock): may fail synchronously, succeed asynchronously, or invoke
//!    the completion routine synchronously before returning.
//! 3. [`ReadTracker::complete_send`] (under lock): transitions a `Preparing` slot to `Sent`, drops
//!    an unsent slot (`DeleteUnsent`), does nothing if synchronous completion already unregistered
//!    the slot, or returns [`PostSendAction::CancelSent`] if [`ReadTracker::suspend`] ran while
//!    `WdfRequestSend` was in progress—guaranteeing `WdfRequestCancelSentRequest` is never called
//!    before `WdfRequestSend` has succeeded.

/// Maximum number of lower reads tracked per device by default.
pub const MAX_IN_FLIGHT_READS: usize = 32;

/// Epoch and unique sequence number assigned when a lower read is prepared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadTicket {
    epoch: u32,
    id: u64,
}

/// Whether a completed lower read belongs to the current epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowerReadOrigin {
    /// Claimed since the last suspend: eligible to serve an upper read of the same epoch.
    Current,
    /// Claimed before the device was suspended: drop without touching the upper queue.
    Stale,
}

/// Action the caller must take after `WdfRequestSend` returns and [`ReadTracker::complete_send`] runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostSendAction {
    /// Either the read is now `Sent`, or its completion routine already ran and unregistered it.
    None,
    /// `WdfRequestSend` returned `false`: delete the unsent request and its memory.
    DeleteUnsent,
    /// `WdfRequestSend` succeeded after a concurrent [`ReadTracker::suspend`]: cancel it now.
    CancelSent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Preparing,
    CancelWhenSent,
    Sent,
}

#[derive(Clone, Copy, Debug)]
struct Slot<H> {
    read: H,
    ticket: ReadTicket,
    phase: Phase,
}

/// Tracks lower reads across preparation, send, suspension, and completion.
#[derive(Debug)]
pub struct ReadTracker<H, const N: usize = MAX_IN_FLIGHT_READS> {
    epoch: u32,
    next_id: u64,
    slots: [Option<Slot<H>>; N],
}

impl<H: Copy, const N: usize> Default for ReadTracker<H, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H, const N: usize> ReadTracker<H, N> {
    /// Number of reads belonging to the current epoch (`Preparing` or `Sent`).
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|s| s.ticket.epoch == self.epoch)
            .count()
    }

    /// Current epoch (incremented by every [`Self::suspend`]).
    #[must_use]
    pub const fn epoch(&self) -> u32 {
        self.epoch
    }
}

impl<H: Copy, const N: usize> ReadTracker<H, N> {
    /// Empty tracker at epoch 0.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            epoch: 0,
            next_id: 1,
            slots: [None; N],
        }
    }

    /// Reserves a slot in the `Preparing` state before `WdfRequestSend`.
    pub fn prepare(&mut self, read: H) -> Option<ReadTicket> {
        let dst = self.slots.iter_mut().find(|slot| slot.is_none())?;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let ticket = ReadTicket {
            epoch: self.epoch,
            id,
        };
        *dst = Some(Slot {
            read,
            ticket,
            phase: Phase::Preparing,
        });
        Some(ticket)
    }

    /// Convenience for single-step registration in synchronous tests (`prepare` + `Sent`).
    pub fn claim(&mut self, read: H) -> Option<ReadTicket>
    where
        H: PartialEq,
    {
        let ticket = self.prepare(read)?;
        let _action = self.complete_send(read, ticket, true);
        Some(ticket)
    }

    /// Records the result of `WdfRequestSend` for a previously prepared read.
    #[must_use]
    pub fn complete_send(&mut self, read: H, ticket: ReadTicket, sent: bool) -> PostSendAction
    where
        H: PartialEq,
    {
        let slot_opt = self.find_slot_mut(read, ticket);
        if !sent {
            if let Some(slot) = slot_opt {
                *slot = None;
            }
            return PostSendAction::DeleteUnsent;
        }
        let Some(slot_ref) = slot_opt else {
            // Completion already ran synchronously inside `WdfRequestSend` and unregistered it.
            return PostSendAction::None;
        };
        match slot_ref.map(|s| s.phase) {
            Some(Phase::CancelWhenSent) => {
                *slot_ref = None;
                PostSendAction::CancelSent
            }
            Some(Phase::Preparing | Phase::Sent) => {
                if let Some(inner) = slot_ref.as_mut() {
                    inner.phase = Phase::Sent;
                }
                PostSendAction::None
            }
            None => PostSendAction::None,
        }
    }

    /// Unregisters `read` on completion and reports whether its epoch is still current.
    pub fn finish(&mut self, read: H, ticket: ReadTicket) -> LowerReadOrigin
    where
        H: PartialEq,
    {
        if let Some(slot) = self.find_slot_mut(read, ticket) {
            *slot = None;
        }
        if ticket.epoch == self.epoch {
            LowerReadOrigin::Current
        } else {
            LowerReadOrigin::Stale
        }
    }

    /// Advances the epoch, returns already-`Sent` reads to cancel immediately, and marks any
    /// `Preparing` reads to be cancelled as soon as their `WdfRequestSend` succeeds.
    pub fn suspend(&mut self) -> SuspendedReads<H, N> {
        self.epoch = self.epoch.wrapping_add(1);
        let mut reads = [None; N];
        let mut out_idx = 0_usize;
        for slot_opt in &mut self.slots {
            match slot_opt {
                Some(slot) if slot.phase == Phase::Sent => {
                    let handle = slot.read;
                    *slot_opt = None;
                    if let Some(dst) = reads.get_mut(out_idx) {
                        *dst = Some(handle);
                        out_idx += 1;
                    }
                }
                Some(slot) => {
                    slot.phase = Phase::CancelWhenSent;
                }
                None => {}
            }
        }
        SuspendedReads { reads }
    }

    fn find_slot_mut(&mut self, read: H, ticket: ReadTicket) -> Option<&mut Option<Slot<H>>>
    where
        H: PartialEq,
    {
        self.slots.iter_mut().find(|slot| {
            slot.is_some_and(|s| {
                if ticket.id != 0 {
                    s.ticket == ticket
                } else {
                    s.read == read
                }
            })
        })
    }
}

/// Reads that were already `Sent` when [`ReadTracker::suspend`] ran.
#[derive(Debug)]
pub struct SuspendedReads<H, const N: usize = MAX_IN_FLIGHT_READS> {
    reads: [Option<H>; N],
}

impl<H: Copy, const N: usize> SuspendedReads<H, N> {
    /// Iterates over the sent reads that should be cancelled with `WdfRequestCancelSentRequest`.
    pub fn iter(&self) -> impl Iterator<Item = H> + '_ {
        self.reads.iter().filter_map(|slot| *slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_fill_the_slots() {
        let mut tracker = ReadTracker::<u32, 2>::new();
        let first = tracker.claim(1).expect("free slot");
        let _second = tracker.claim(2).expect("free slot");
        assert_eq!(tracker.claim(3), None);
        assert_eq!(tracker.in_flight(), 2);
        assert_eq!(tracker.finish(1, first), LowerReadOrigin::Current);
        assert_eq!(tracker.in_flight(), 1);
        assert!(tracker.claim(3).is_some());
    }

    #[test]
    fn suspend_hands_out_the_reads_and_makes_them_stale() {
        let mut tracker = ReadTracker::<u32, 4>::new();
        let old = tracker.claim(10).expect("free slot");
        let reads = tracker.suspend();
        assert!(reads.iter().eq([10]));
        assert_eq!(tracker.in_flight(), 0);
        assert_eq!(tracker.epoch(), 1);
        let new = tracker.claim(11).expect("free slot");
        assert_eq!(tracker.finish(10, old), LowerReadOrigin::Stale);
        assert_eq!(tracker.in_flight(), 1);
        assert_eq!(tracker.finish(11, new), LowerReadOrigin::Current);
        assert!(tracker.suspend().iter().eq([]));
        assert_eq!(tracker.epoch(), 2);
    }

    #[test]
    fn suspend_during_prepare_defers_cancel_until_send_succeeds() {
        let mut tracker = ReadTracker::<u32, 4>::new();
        let sent_t = tracker.claim(1).expect("slot");
        let prep_ok = tracker.prepare(2).expect("slot");
        let prep_fail = tracker.prepare(3).expect("slot");

        // Suspend hands out only already-sent read 1; 2 and 3 are still unsent!
        let suspended = tracker.suspend();
        assert!(suspended.iter().eq([1]));
        assert_eq!(tracker.in_flight(), 0);

        // Read 2's WdfRequestSend succeeds -> deferred CancelSent; completion is Stale.
        assert_eq!(
            tracker.complete_send(2, prep_ok, true),
            PostSendAction::CancelSent
        );
        assert_eq!(tracker.finish(2, prep_ok), LowerReadOrigin::Stale);

        // Read 3's WdfRequestSend fails -> DeleteUnsent (never cancelled as sent).
        assert_eq!(
            tracker.complete_send(3, prep_fail, false),
            PostSendAction::DeleteUnsent
        );
        assert_eq!(tracker.finish(1, sent_t), LowerReadOrigin::Stale);
    }

    #[test]
    fn synchronous_completion_before_complete_send_is_safe_even_on_handle_reuse() {
        let mut tracker = ReadTracker::<u32, 2>::new();
        let t1 = tracker.prepare(42).expect("slot");

        // Completion runs inside WdfRequestSend, deletes handle 42, and another thread reuses 42!
        assert_eq!(tracker.finish(42, t1), LowerReadOrigin::Current);
        let t2 = tracker.prepare(42).expect("reused handle");

        // Outer thread's complete_send for t1 must NOT touch t2's slot.
        assert_eq!(tracker.complete_send(42, t1, true), PostSendAction::None);
        assert_eq!(tracker.in_flight(), 1);
        assert_eq!(tracker.complete_send(42, t2, true), PostSendAction::None);
        assert_eq!(tracker.finish(42, t2), LowerReadOrigin::Current);
    }
}
