//! Shared manual-queue draining, serialized completion draining, and safe lower-read cancellation.

use core::ptr::{null, null_mut};

use amtptp_core::hid::delivery::{CompletionQueue, EnqueueOutcome};
use amtptp_core::hid::read_tracker::{MAX_IN_FLIGHT_READS, SuspendedReads};
use wdk_sys::{NTSTATUS, WDFQUEUE, WDFREQUEST, WDFREQUEST__, call_unsafe_wdf_function_binding};

use crate::handle::WdfHandle;
use crate::ntstatus::NT_SUCCESS;
use crate::request::Request;
use crate::sync::SpinLocked;

/// Retrieves every request from a manual `WDFQUEUE` and completes it with `status` (`Information = 0`).
/// Returns the number of completed requests.
///
/// # Safety
/// `queue` must be null or a live manual `WDFQUEUE` whose queued requests are owned by the caller
/// once retrieved, at IRQL <= `DISPATCH_LEVEL`.
pub unsafe fn drain_manual_queue(queue: WDFQUEUE, status: NTSTATUS) -> u32 {
    if queue.is_null() {
        return 0;
    }
    let mut drained: u32 = 0;
    loop {
        let mut raw: WDFREQUEST = null_mut();
        // SAFETY: `queue` is a live manual queue per caller contract; `raw` is writable.
        let retrieved = unsafe {
            call_unsafe_wdf_function_binding!(WdfIoQueueRetrieveNextRequest, queue, &raw mut raw)
        };
        if !NT_SUCCESS(retrieved) || raw.is_null() {
            break;
        }
        // SAFETY: `WdfIoQueueRetrieveNextRequest` transferred ownership of `raw` to this driver.
        let request = unsafe { Request::from_raw(raw, status) };
        request.complete(status);
        drained = drained.wrapping_add(1);
    }
    drained
}

/// Adds one object reference to `request` while holding the tracker spinlock so the handle stays
/// valid until [`cancel_and_dereference_read`] runs outside the spinlock.
///
/// # Safety
/// `request` must be a live `WDFREQUEST` that cannot be deleted while the caller holds the lock.
pub unsafe fn reference_read(request: WDFREQUEST) {
    // SAFETY: `request` is alive per caller contract; NULL tag matches `WdfObjectReference`.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfObjectReferenceActual,
            request.cast(),
            null_mut(),
            0,
            null()
        );
    }
}

/// Cancels a previously referenced sent `WDFREQUEST` and drops the temporary reference.
/// Call **outside** any spinlock that the request's completion routine acquires.
///
/// # Safety
/// `request` must have an active reference acquired via [`reference_read`] and must have been
/// submitted via `WdfRequestSend`, at IRQL <= `DISPATCH_LEVEL`.
pub unsafe fn cancel_and_dereference_read(request: WDFREQUEST) {
    // SAFETY: `request` was referenced by `reference_read` and sent; IRQL <= `DISPATCH_LEVEL`.
    unsafe {
        let _cancelled = call_unsafe_wdf_function_binding!(WdfRequestCancelSentRequest, request);
        call_unsafe_wdf_function_binding!(
            WdfObjectDereferenceActual,
            request.cast(),
            null_mut(),
            0,
            null()
        );
    }
}

/// References every handle in `suspended` while still inside the tracker spinlock, returning the
/// array and count to pass to [`cancel_referenced_reads`] once the spinlock is released.
///
/// # Safety
/// Every handle in `suspended` must be a live `WDFREQUEST` protected from deletion by the held lock.
#[must_use]
pub unsafe fn collect_reads_to_cancel(
    suspended: &SuspendedReads<WdfHandle<WDFREQUEST__>>,
) -> ([WDFREQUEST; MAX_IN_FLIGHT_READS], usize) {
    let mut held: [WDFREQUEST; MAX_IN_FLIGHT_READS] = [null_mut(); MAX_IN_FLIGHT_READS];
    let mut count = 0_usize;
    for (slot, read) in held.iter_mut().zip(suspended.iter()) {
        let raw = read.get();
        // SAFETY: each suspended read was registered and has not been deleted under this lock.
        unsafe { reference_read(raw) };
        *slot = raw;
        count += 1;
    }
    (held, count)
}

/// Cancels and dereferences the `count` requests returned by [`collect_reads_to_cancel`].
///
/// # Safety
/// Must be called outside the tracker spinlock at IRQL <= `DISPATCH_LEVEL` with the exact `(held, count)`
/// returned by [`collect_reads_to_cancel`].
pub unsafe fn cancel_referenced_reads(held: &[WDFREQUEST; MAX_IN_FLIGHT_READS], count: usize) {
    for &read in held.iter().take(count) {
        // SAFETY: each entry in `..count` was referenced by `collect_reads_to_cancel`.
        unsafe { cancel_and_dereference_read(read) };
    }
}

/// Runs `complete_fn` in strict FIFO order for an [`EnqueueOutcome`] produced under `lock`.
///
/// # Safety
/// `lock` must be initialized (`SpinLocked::create`), the caller must NOT hold `lock`, and IRQL
/// must be <= `DISPATCH_LEVEL`.
pub unsafe fn drain_completions<S, T, const N: usize, G, C>(
    lock: &SpinLocked<S>,
    outcome: EnqueueOutcome<T>,
    mut get_queue: G,
    mut complete_fn: C,
) where
    G: FnMut(&mut S) -> &mut CompletionQueue<T, N>,
    C: FnMut(T),
{
    match outcome {
        EnqueueOutcome::Queued => {}
        EnqueueOutcome::Overflow(item) => complete_fn(item),
        EnqueueOutcome::StartDrain(first) => {
            complete_fn(first);
            loop {
                // SAFETY: `lock` is initialized and not held here; `next_to_complete` is pure.
                let next = unsafe { lock.with(|state| get_queue(state).next_to_complete()) };
                let Some(item) = next else {
                    break;
                };
                complete_fn(item);
            }
        }
    }
}
