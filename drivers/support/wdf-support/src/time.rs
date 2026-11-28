//! Access to the Windows performance counter for input-report timestamps.

use wdk_sys::LARGE_INTEGER;

/// Returns the current performance-counter value in ticks.
#[must_use]
pub fn query_performance_counter() -> i64 {
    // SAFETY: the optional frequency output may be NULL, and the routine is callable at any IRQL.
    let counter = unsafe { wdk_sys::ntddk::KeQueryPerformanceCounter(core::ptr::null_mut()) };

    // SAFETY: QuadPart covers the entire initialized union, and all bit patterns are valid.
    unsafe { counter.QuadPart }
}

/// Returns the performance-counter frequency in ticks per second.
#[must_use]
pub fn query_performance_frequency() -> i64 {
    let mut frequency = LARGE_INTEGER { QuadPart: 0 };

    // SAFETY: frequency points to a writable LARGE_INTEGER, and the routine is callable at any IRQL.
    unsafe {
        wdk_sys::ntddk::KeQueryPerformanceCounter(&raw mut frequency);
    }

    // SAFETY: QuadPart covers the entire initialized union, and all bit patterns are valid.
    unsafe { frequency.QuadPart }
}
