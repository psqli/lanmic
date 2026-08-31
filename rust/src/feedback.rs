//! Feedback suppression by frequency shifting.
//!
//! Howlround needs two things: loop gain at or above unity, and a round trip
//! that comes back in phase. A reinforcement system can rarely give up the
//! gain, so this takes away the phase: shifting the whole mix by a few hertz
//! means a tone that leaves the loudspeaker at 1000 Hz re-enters the microphone
//! at 1005 Hz, leaves again at 1010, and never accumulates at any one
//! frequency. It is worth roughly 6-10 dB of extra gain before ringing, and it
//! is what installed speech systems have used for decades.
//!
//! Echo cancellation is not an option here and no amount of work would make it
//! one: the microphone and the loudspeaker are different devices, so the
//! capturing end has no reference signal to cancel against.
//!
//! The cost is a small inharmonicity - partials move by a fixed number of hertz
//! rather than in proportion - which is inaudible on speech at these depths and
//! is why the shift is adjustable and can be switched off.
//!
//! Per sample: eight second-order allpass sections, a two-by-two rotation, and
//! a multiply-add. No transforms, no allocation, no branches in the loop.

/// Squared allpass coefficients for the two chains of a Hilbert transform pair.
/// Their phase responses stay 90 degrees apart across the audio band, which is
/// what lets the shift below be single-sideband rather than a ring modulator's
/// two mirrored copies.
const CHAIN_I: [f32; 4] = [0.478_965_1, 0.876_182_3, 0.976_597_6, 0.997_155_4];
const CHAIN_Q: [f32; 4] = [0.161_758_6, 0.733_028_1, 0.945_349_2, 0.990_599_8];

/// One section of `H(z) = (a2 - z^-2) / (1 - a2 z^-2)`: unity magnitude at
/// every frequency, and only the phase does any work.
#[derive(Default, Clone, Copy)]
struct Allpass2 {
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Allpass2 {
    fn new(a2: f32) -> Self {
        Self {
            a2,
            ..Default::default()
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.a2 * (x + self.y2) - self.x2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

/// Single-sideband frequency shifter.
pub struct FrequencyShifter {
    chain_i: [Allpass2; 4],
    chain_q: [Allpass2; 4],
    /// The in-phase chain runs one sample behind, which is what completes the
    /// quadrature relationship between the two.
    delay: f32,
    sample_rate: f32,

    /// Rotation carried as a unit vector stepped by a fixed angle, so no
    /// trigonometry runs per sample and a change of shift is a change of step
    /// rather than a jump in phase - the output stays continuous.
    cos_step: f32,
    sin_step: f32,
    cos_phase: f32,
    sin_phase: f32,
    shift_hz: f32,
    /// Counts down to the next renormalisation of the rotation vector.
    until_renorm: u32,
}

/// The rotation vector drifts off the unit circle by a few ulps per step, so it
/// is renormalised occasionally. Every 4096 samples is a little over twice a
/// second and costs one square root.
const RENORM_INTERVAL: u32 = 4096;

impl FrequencyShifter {
    pub fn new(sample_rate: u32) -> Self {
        let mut shifter = Self {
            chain_i: CHAIN_I.map(Allpass2::new),
            chain_q: CHAIN_Q.map(Allpass2::new),
            delay: 0.0,
            sample_rate: sample_rate as f32,
            cos_step: 1.0,
            sin_step: 0.0,
            cos_phase: 1.0,
            sin_phase: 0.0,
            shift_hz: 0.0,
            until_renorm: RENORM_INTERVAL,
        };
        shifter.set_shift_hz(0.0);
        shifter
    }

    /// Sets the shift. Zero means bypass, which is exact rather than a shift of
    /// nothing: the allpass chains would still rewrite the phase.
    pub fn set_shift_hz(&mut self, hz: f32) {
        let hz = if hz.is_finite() {
            hz.clamp(0.0, 30.0)
        } else {
            0.0
        };
        if hz == self.shift_hz {
            return;
        }
        self.shift_hz = hz;
        let step = std::f32::consts::TAU * hz / self.sample_rate;
        self.cos_step = step.cos();
        self.sin_step = step.sin();
        if hz == 0.0 {
            // Nothing is running, so come back from a clean state rather than
            // with whatever was in the filters when the shift was turned off.
            self.reset();
        }
    }

    pub fn shift_hz(&self) -> f32 {
        self.shift_hz
    }

    fn reset(&mut self) {
        for s in self.chain_i.iter_mut().chain(self.chain_q.iter_mut()) {
            s.reset();
        }
        self.delay = 0.0;
        self.cos_phase = 1.0;
        self.sin_phase = 0.0;
        self.until_renorm = RENORM_INTERVAL;
    }

    /// Shifts `buf` in place. A no-op while the shift is zero.
    pub fn process(&mut self, buf: &mut [f32]) {
        if self.shift_hz == 0.0 {
            return;
        }
        for sample in buf.iter_mut() {
            let mut i = *sample;
            for section in self.chain_i.iter_mut() {
                i = section.process(i);
            }
            let mut q = *sample;
            for section in self.chain_q.iter_mut() {
                q = section.process(q);
            }
            let i_delayed = self.delay;
            self.delay = i;

            // Rotate the analytic signal: the upper sideband survives and the
            // lower one cancels, which is the difference between a shift and a
            // ring modulator.
            *sample = i_delayed * self.cos_phase + q * self.sin_phase;

            let cos = self.cos_phase * self.cos_step - self.sin_phase * self.sin_step;
            let sin = self.sin_phase * self.cos_step + self.cos_phase * self.sin_step;
            self.cos_phase = cos;
            self.sin_phase = sin;

            self.until_renorm -= 1;
            if self.until_renorm == 0 {
                let mag =
                    (self.cos_phase * self.cos_phase + self.sin_phase * self.sin_phase).sqrt();
                if mag > 0.0 {
                    self.cos_phase /= mag;
                    self.sin_phase /= mag;
                }
                self.until_renorm = RENORM_INTERVAL;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    /// Energy at `hz`, by correlating against a reference oscillator.
    fn energy_at(buf: &[f32], hz: f32) -> f32 {
        let w = std::f64::consts::TAU * hz as f64 / SR as f64;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, &s) in buf.iter().enumerate() {
            let phase = w * n as f64;
            re += s as f64 * phase.cos();
            im += s as f64 * phase.sin();
        }
        ((re * re + im * im).sqrt() / buf.len() as f64) as f32
    }

    fn tone(hz: f32, n: usize) -> Vec<f32> {
        let w = std::f32::consts::TAU * hz / SR as f32;
        (0..n).map(|i| (w * i as f32).sin()).collect()
    }

    #[test]
    fn a_tone_comes_out_at_the_shifted_frequency() {
        let mut shifter = FrequencyShifter::new(SR);
        shifter.set_shift_hz(5.0);
        // Long enough that 1000 and 1005 Hz are resolvable, and past the
        // filters' settling.
        let mut buf = tone(1000.0, SR as usize);
        shifter.process(&mut buf);

        let moved = energy_at(&buf, 1005.0);
        let stayed = energy_at(&buf, 1000.0);
        let mirrored = energy_at(&buf, 995.0);
        assert!(moved > 0.35, "shifted tone is weak: {moved}");
        assert!(
            stayed < moved * 0.1,
            "energy left at the original frequency: {stayed} vs {moved}"
        );
        assert!(
            mirrored < moved * 0.1,
            "lower sideband not cancelled: {mirrored} vs {moved}"
        );
    }

    #[test]
    fn the_shift_is_the_same_across_the_band() {
        // A frequency shift moves every partial by the same number of hertz,
        // unlike a pitch shift. That is the property the suppression relies on,
        // and the one that makes it inharmonic.
        for f in [200.0f32, 1000.0, 4000.0] {
            let mut shifter = FrequencyShifter::new(SR);
            shifter.set_shift_hz(7.0);
            let mut buf = tone(f, SR as usize);
            shifter.process(&mut buf);
            let moved = energy_at(&buf, f + 7.0);
            let stayed = energy_at(&buf, f);
            assert!(moved > 0.35, "{f} Hz: weak output {moved}");
            assert!(stayed < moved * 0.15, "{f} Hz: {stayed} left behind");
        }
    }

    #[test]
    fn it_holds_its_level() {
        // Allpass chains and a rotation both preserve magnitude, so the shift
        // must not become a gain change the limiter then has to clean up.
        let mut shifter = FrequencyShifter::new(SR);
        shifter.set_shift_hz(5.0);
        let mut buf = tone(1000.0, SR as usize);
        shifter.process(&mut buf);
        let peak = buf[SR as usize / 2..]
            .iter()
            .fold(0.0f32, |a, s| a.max(s.abs()));
        assert!((0.9..=1.1).contains(&peak), "peak drifted to {peak}");
    }

    #[test]
    fn zero_is_a_true_bypass() {
        let mut shifter = FrequencyShifter::new(SR);
        let original = tone(1000.0, 4800);
        let mut buf = original.clone();
        shifter.process(&mut buf);
        assert_eq!(buf, original, "bypass must not touch the samples at all");
    }

    #[test]
    fn silence_in_silence_out() {
        let mut shifter = FrequencyShifter::new(SR);
        shifter.set_shift_hz(5.0);
        let mut buf = vec![0.0f32; 4800];
        shifter.process(&mut buf);
        assert!(buf.iter().all(|s| s.abs() < 1e-12), "shifter is ringing");
    }

    #[test]
    fn absurd_and_broken_settings_are_clamped() {
        let mut shifter = FrequencyShifter::new(SR);
        shifter.set_shift_hz(1e9);
        assert_eq!(shifter.shift_hz(), 30.0);
        shifter.set_shift_hz(-4.0);
        assert_eq!(shifter.shift_hz(), 0.0);
        shifter.set_shift_hz(f32::NAN);
        assert_eq!(shifter.shift_hz(), 0.0);

        // And nothing it produces is ever unrepresentable.
        shifter.set_shift_hz(5.0);
        let mut buf = tone(500.0, 9600);
        shifter.process(&mut buf);
        assert!(buf.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn the_rotation_stays_on_the_unit_circle() {
        // Drift here would show up as a slow tremolo, which is exactly the kind
        // of fault that only appears after an hour on stage.
        let mut shifter = FrequencyShifter::new(SR);
        shifter.set_shift_hz(5.0);
        let mut buf = tone(1000.0, SR as usize * 30);
        shifter.process(&mut buf);
        let mag =
            (shifter.cos_phase * shifter.cos_phase + shifter.sin_phase * shifter.sin_phase).sqrt();
        assert!((mag - 1.0).abs() < 1e-4, "rotation drifted to {mag}");

        let late = &buf[buf.len() - SR as usize..];
        let peak = late.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!((0.9..=1.1).contains(&peak), "level drifted to {peak}");
    }
}
