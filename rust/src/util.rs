//! Small shared helpers.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

/// Milliseconds on a monotonic clock. The zero point is arbitrary and fixed
/// for the life of the process; only differences are meaningful.
pub fn now_ms() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as i64
}

/// A float published to the UI through an atomic, in thousandths. Levels,
/// gains and latencies all want the same treatment and none of them needs more
/// precision than this.
pub fn store_milli(cell: &AtomicU32, value: f32) {
    let v = if value.is_nan() || value <= 0.0 {
        0.0
    } else {
        value * 1000.0
    };
    cell.store(v.min(u32::MAX as f32) as u32, Ordering::Relaxed);
}

pub fn load_milli(cell: &AtomicU32) -> f32 {
    cell.load(Ordering::Relaxed) as f32 / 1000.0
}

/// Best effort: push a helper thread up so the network path is not scheduled
/// behind UI work. Failure is ignored - this is an optimisation, not a
/// requirement, and there is nothing useful to do about it.
#[cfg(target_os = "android")]
pub fn raise_thread_priority() {
    // SAFETY: setpriority on the calling thread with an in-range value. It
    // cannot fail in a way that matters and touches no memory we own.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, -16); // ANDROID_PRIORITY_AUDIO
    }
}

#[cfg(not(target_os = "android"))]
pub fn raise_thread_priority() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn milli_round_trips_and_refuses_to_wrap() {
        let cell = AtomicU32::new(0);
        store_milli(&cell, 1.5);
        assert!((load_milli(&cell) - 1.5).abs() < 1e-6);
        store_milli(&cell, -3.0);
        assert_eq!(load_milli(&cell), 0.0);
        store_milli(&cell, f32::NAN);
        assert_eq!(load_milli(&cell), 0.0);
        store_milli(&cell, f32::INFINITY);
        assert!(load_milli(&cell) > 0.0);
    }

    #[test]
    fn the_clock_moves_forward_only() {
        let a = now_ms();
        let b = now_ms();
        assert!(b >= a);
        assert!(a >= 0);
    }
}
