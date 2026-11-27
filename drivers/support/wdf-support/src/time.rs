//! Access to the Windows performance counter for input-report timestamps.

use wdk_sys::LARGE_INTEGER;

/// Returns the current performance-counter value in ticks.
#[must_use]
pub fn query_performance_counter() -> i64 {
    #[cfg(driver_model__driver_type = "KMDF")]
    // SAFETY: the optional frequency output may be NULL, and the routine is callable at any IRQL.
    let counter = unsafe { wdk_sys::ntddk::KeQueryPerformanceCounter(core::ptr::null_mut()) };

    #[cfg(driver_model__driver_type = "UMDF")]
    let counter = {
        let mut counter = LARGE_INTEGER { QuadPart: -1 };
        // SAFETY: counter points to a writable LARGE_INTEGER on the stack.
        unsafe { wdk_sys::windows::QueryPerformanceCounter(&raw mut counter) };
        counter
    };

    // SAFETY: QuadPart covers the entire initialized union, and all bit patterns are valid.
    unsafe { counter.QuadPart }
}

/// Returns the performance-counter frequency in ticks per second.
#[must_use]
pub fn query_performance_frequency() -> i64 {
    let mut frequency = LARGE_INTEGER { QuadPart: 0 };

    #[cfg(driver_model__driver_type = "KMDF")]
    // SAFETY: frequency points to a writable LARGE_INTEGER, and the routine is callable at any IRQL.
    unsafe {
        wdk_sys::ntddk::KeQueryPerformanceCounter(&raw mut frequency);
    }

    #[cfg(driver_model__driver_type = "UMDF")]
    // SAFETY: frequency points to a writable LARGE_INTEGER on the stack.
    unsafe {
        wdk_sys::windows::QueryPerformanceFrequency(&raw mut frequency);
    }

    // SAFETY: QuadPart covers the entire initialized union, and all bit patterns are valid.
    unsafe { frequency.QuadPart }
}
