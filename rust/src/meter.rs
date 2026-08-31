//! Peak meter ballistics, shared by the transmitter, the mix bus and every
//! source strip.

use std::sync::atomic::{AtomicU32, Ordering};

/// Levels are published as milli-units of full scale in a relaxed atomic: the
/// audio thread stores, the UI polls, neither blocks.
pub const METER_FULL_SCALE: u32 = 1000;

/// Per-source and master gains reach 2x, so the meter has to be able to report
/// above full scale rather than pinning silently.
pub const METER_CEILING: u32 = 2000;

/// Instant attack, ~1/16-per-block decay: slow enough to read on an 80 ms UI
/// poll, fast enough to follow speech. `level` is linear, 1.0 == full scale.
pub fn update_peak_meter(meter: &AtomicU32, level: f32) {
    let cur = meter.load(Ordering::Relaxed);

    // NaN compares false, so it lands on zero rather than on a garbage cast.
    let mut next = if level > 0.0 {
        ((level * METER_FULL_SCALE as f32) as u32).min(METER_CEILING)
    } else {
        0
    };

    if next < cur {
        // Decay by 1/16 but never by less than one step. A bare `cur >> 4`
        // reaches zero only from above 16 and otherwise stalls, parking the
        // meter at -36 dBFS through dead silence.
        let step = (cur >> 4).max(1);
        next = cur.saturating_sub(step);
    }
    meter.store(next, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attack_is_instant_and_decay_is_gradual() {
        let m = AtomicU32::new(0);
        update_peak_meter(&m, 0.8);
        assert_eq!(m.load(Ordering::Relaxed), 800);
        update_peak_meter(&m, 0.9);
        assert_eq!(m.load(Ordering::Relaxed), 900);

        update_peak_meter(&m, 0.1);
        let v = m.load(Ordering::Relaxed);
        assert!((100..900).contains(&v), "decayed rather than snapped: {v}");
    }

    #[test]
    fn silence_actually_reaches_zero() {
        // A bare `cur >> 4` decay stalls at 15 and parks every meter at
        // -36 dBFS - 39% of a -60..0 dB bar - forever.
        let m = AtomicU32::new(METER_CEILING);
        for _ in 0..100_000 {
            update_peak_meter(&m, 0.0);
        }
        assert_eq!(m.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn overs_are_reported_up_to_the_ceiling() {
        let m = AtomicU32::new(0);
        update_peak_meter(&m, 1.5);
        assert_eq!(m.load(Ordering::Relaxed), 1500);
        update_peak_meter(&m, 99.0);
        assert_eq!(m.load(Ordering::Relaxed), METER_CEILING);
    }

    #[test]
    fn nan_and_negative_levels_do_not_wrap() {
        let m = AtomicU32::new(500);
        update_peak_meter(&m, f32::NAN);
        assert!(m.load(Ordering::Relaxed) < 500);
        update_peak_meter(&m, -3.0);
        assert!(m.load(Ordering::Relaxed) < 500);
    }
}
