//! Bookkeeping of the lower reads a driver has in flight, with an *epoch* that every suspend advances.
//!
//! A filter or function driver that serves upper `IOCTL_HID_READ_REPORT` requests with its own lower
//! reads registers each lower read with [`ReadTracker::claim`] before it sends it and unregisters it with
//! [`ReadTracker::finish`] when it completes. [`ReadTracker::suspend`] (when the device leaves D0) starts a
//! new epoch and hands back the reads in flight, which the driver cancels: a read claimed in an earlier
//! epoch is [`LowerReadOrigin::Stale`] when it completes, so its data is dropped instead of serving an
//! upper read queued after the resume.
//!
//! The driver keeps one tracker per device behind a spin lock (the completion routine runs at
//! `DISPATCH_LEVEL`); holding `&mut ReadTracker` stands for holding that lock. Whatever gates new reads
//! (e.g. "the device is configured") must be checked under the same lock as the claim, so a claim that
//! races with a suspend is either handed out by it or refused.

/// The most lower reads that can be in flight at once by default. HIDCLASS keeps a handful of reads
/// pending; a read beyond the limit is not issued (the upper request waits for a later completion).
pub const MAX_IN_FLIGHT_READS: usize = 32;

/// The epoch a lower read was claimed in. The driver stores it in the read's request context and passes
/// it back to [`ReadTracker::finish`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadTicket {
    epoch: u32,
}

/// Whether a completed lower read belongs to the current epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowerReadOrigin {
    /// Claimed since the last suspend: it serves the oldest queued upper read.
    Current,
    /// Claimed before the device was suspended: its data is dropped and the upper queue is not touched.
    Stale,
}

/// The lower reads in flight and the current epoch.
///
/// `H` is the read's handle (a `WDFREQUEST` wrapper in a driver, a label in tests); `N` is the number of
/// slots.
#[derive(Debug)]
pub struct ReadTracker<H, const N: usize = MAX_IN_FLIGHT_READS> {
    epoch: u32,
    in_flight: [Option<H>; N],
}

impl<H: Copy, const N: usize> Default for ReadTracker<H, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H, const N: usize> ReadTracker<H, N> {
    /// Number of reads of the current epoch in flight.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.iter().filter(|slot| slot.is_some()).count()
    }

    /// The current epoch (incremented by every [`Self::suspend`]).
    #[must_use]
    pub const fn epoch(&self) -> u32 {
        self.epoch
    }
}

impl<H: Copy, const N: usize> ReadTracker<H, N> {
    /// No reads in flight, epoch 0.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            epoch: 0,
            in_flight: [None; N],
        }
    }

    /// Registers lower read `read`, created and formatted but not yet sent, and returns the ticket to
    /// store in its context. `None` when all `N` slots are taken: the driver deletes the read without
    /// sending it.
    pub fn claim(&mut self, read: H) -> Option<ReadTicket> {
        let slot = self.in_flight.iter_mut().find(|slot| slot.is_none())?;
        *slot = Some(read);
        Some(ReadTicket { epoch: self.epoch })
    }

    /// Unregisters lower read `read` when it completes (or when its send failed) and tells whether it
    /// belongs to the current epoch. A read the last suspend handed out is no longer registered; its
    /// ticket is stale.
    pub fn finish(&mut self, read: H, ticket: ReadTicket) -> LowerReadOrigin
    where
        H: PartialEq,
    {
        if let Some(slot) = self
            .in_flight
            .iter_mut()
            .find(|slot| slot.is_some_and(|registered| registered == read))
        {
            *slot = None;
        }
        if ticket.epoch == self.epoch {
            LowerReadOrigin::Current
        } else {
            LowerReadOrigin::Stale
        }
    }

    /// Starts a new epoch and hands back the reads that were in flight, for the driver to cancel. Their
    /// completions report [`LowerReadOrigin::Stale`]. A second call only advances the epoch again and
    /// returns no reads.
    pub fn suspend(&mut self) -> SuspendedReads<H, N> {
        self.epoch = self.epoch.wrapping_add(1);
        let mut reads = [None; N];
        for (taken, slot) in reads.iter_mut().zip(self.in_flight.iter_mut()) {
            *taken = slot.take();
        }
        SuspendedReads { reads }
    }
}

/// The reads that were in flight when the device was suspended. The driver cancels each one
/// (`WdfRequestCancelSentRequest`); their completions report [`LowerReadOrigin::Stale`].
#[derive(Debug)]
pub struct SuspendedReads<H, const N: usize = MAX_IN_FLIGHT_READS> {
    reads: [Option<H>; N],
}

impl<H: Copy, const N: usize> SuspendedReads<H, N> {
    /// The reads to cancel.
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
}
