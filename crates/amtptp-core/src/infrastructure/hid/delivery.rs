//! Single-drainer FIFO serializer for completing upper `IOCTL_HID_READ_REPORT` requests in the
//! exact order reports were generated, without holding a spinlock across `WdfRequestComplete`.

use super::read_tracker::MAX_IN_FLIGHT_READS;

/// Outcome of pushing a ready-to-complete item under the pipeline spinlock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueOutcome<T> {
    /// No thread was draining; the caller becomes the active drainer, completes this item outside
    /// the spinlock, and then loops calling [`CompletionQueue::next_to_complete`].
    StartDrain(T),
    /// Another thread is already draining and will complete this item in FIFO order.
    Queued,
    /// Ring capacity was exceeded while draining; complete immediately rather than dropping `T`.
    Overflow(T),
}

/// Bounded, `no_std`, allocation-free FIFO serializer for upper-request completions.
#[derive(Debug)]
pub struct CompletionQueue<T, const N: usize = MAX_IN_FLIGHT_READS> {
    buf: [Option<T>; N],
    head: usize,
    len: usize,
    draining: bool,
}

impl<T, const N: usize> Default for CompletionQueue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> CompletionQueue<T, N> {
    /// Creates an idle queue with no pending items.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [const { None }; N],
            head: 0,
            len: 0,
            draining: false,
        }
    }

    /// Submits `item` inside the critical section that generated its report.
    #[must_use]
    pub fn enqueue(&mut self, item: T) -> EnqueueOutcome<T> {
        if !self.draining {
            self.draining = true;
            return EnqueueOutcome::StartDrain(item);
        }
        if self.len >= N || N == 0 {
            return EnqueueOutcome::Overflow(item);
        }
        let idx = (self.head + self.len) % N;
        if let Some(slot) = self.buf.get_mut(idx) {
            *slot = Some(item);
            self.len += 1;
            EnqueueOutcome::Queued
        } else {
            EnqueueOutcome::Overflow(item)
        }
    }

    /// Pops the next FIFO item for the active drainer, or clears `draining` when empty.
    pub fn next_to_complete(&mut self) -> Option<T> {
        if self.len == 0 || N == 0 {
            self.draining = false;
            return None;
        }
        let item = self.buf.get_mut(self.head).and_then(Option::take);
        self.head = (self.head + 1) % N;
        self.len -= 1;
        if item.is_none() && self.len == 0 {
            self.draining = false;
        }
        item
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_producer_preserves_strict_fifo_completion_order() {
        let mut q = CompletionQueue::<u32, 4>::new();

        // CPU 0 generates report 1 (liftoff) and becomes the drainer.
        assert_eq!(q.enqueue(1), EnqueueOutcome::StartDrain(1));

        // While CPU 0 is outside the lock completing 1, CPU 1 and CPU 2 generate reports 2 and 3.
        assert_eq!(q.enqueue(2), EnqueueOutcome::Queued);
        assert_eq!(q.enqueue(3), EnqueueOutcome::Queued);

        // CPU 0 finishes completing 1 and drains 2 then 3 in exact generation order.
        assert_eq!(q.next_to_complete(), Some(2));
        assert_eq!(q.next_to_complete(), Some(3));
        assert_eq!(q.next_to_complete(), None);

        // Next arrival starts a fresh drain cycle.
        assert_eq!(q.enqueue(4), EnqueueOutcome::StartDrain(4));
        assert_eq!(q.next_to_complete(), None);
    }

    #[test]
    fn overflow_returns_item_without_dropping() {
        let mut q = CompletionQueue::<u32, 2>::new();
        assert_eq!(q.enqueue(10), EnqueueOutcome::StartDrain(10));
        assert_eq!(q.enqueue(11), EnqueueOutcome::Queued);
        assert_eq!(q.enqueue(12), EnqueueOutcome::Queued);
        assert_eq!(q.enqueue(13), EnqueueOutcome::Overflow(13));
        assert_eq!(q.next_to_complete(), Some(11));
        assert_eq!(q.next_to_complete(), Some(12));
        assert_eq!(q.next_to_complete(), None);
    }
}
