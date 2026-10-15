//! DSP parameters for independent pitch control and studio reverb.
//!
//! Pitch shifting uses a 4-tap Hann-windowed granular Doppler shifter
//! (38ms grain, exact `|1 − 2^(st/12)| × T` modulation depth), with a
//! bit-perfect dry bypass whenever independent pitch is off.
//!
//! Reverb uses a generated stereo impulse response with a soft attack,
//! decorrelated diffusion, exponential decay, progressive
//! high-frequency damping, and a 120Hz highpass on the wet path.

/// 38ms grain window (`GRAIN_PERIOD`).
pub const GRAIN_PERIOD_S: f32 = 0.038;
/// 4 staggered taps summing to 2.0, scaled by 0.5 for unity gain.
pub const NUM_TAPS: usize = 4;
/// Wet-sum correction for the 4 overlapped Hann windows.
pub const WET_SUM_GAIN: f32 = 0.5;
/// Reverb impulse length in seconds.
pub const REVERB_TAIL_S: f32 = 2.8;
/// Soft attack to avoid clicks.
pub const REVERB_ATTACK_S: f32 = 0.015;
/// Highpass on the reverb wet path (keeps kick/808 tight).
pub const REVERB_HIGHPASS_HZ: f32 = 120.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DopplerPitchParams {
    pub semitones: f32,
    /// Equal to `|1 - 2^(st/12)| * T`, clamped to 0.035.
    pub delay_depth_s: f32,
    pub shift_up: bool,
    pub bypass: bool,
}

impl DopplerPitchParams {
    pub fn from_semitones(semitones: f32, pitch_linked: bool) -> Self {
        if pitch_linked || semitones.abs() < 0.05 {
            return Self {
                semitones,
                delay_depth_s: 0.0,
                shift_up: semitones > 0.0,
                bypass: true,
            };
        }
        let mult = 2.0_f32.powf(semitones / 12.0);
        let slope = (1.0 - mult).abs();
        let depth = (slope * GRAIN_PERIOD_S).min(0.035);
        Self {
            semitones,
            delay_depth_s: depth,
            shift_up: semitones > 0.0,
            bypass: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReverbParams {
    pub enabled: bool,
    pub level: f32,
}

impl ReverbParams {
    /// Mirrors TS: wet gain = clamp(level, 0..1.2), 0 when disabled.
    pub fn wet_gain(&self) -> f32 {
        if !self.enabled || self.level <= 0.01 {
            0.0
        } else {
            self.level.clamp(0.0, 1.2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doppler_depth_matches_ts_formula() {
        // +12 semitones: mult=2, slope=1, depth=min(0.035, 0.038)=0.035
        let p = DopplerPitchParams::from_semitones(12.0, false);
        assert!(!p.bypass);
        assert!((p.delay_depth_s - 0.035).abs() < 1e-6);
        assert!(p.shift_up);
    }

    #[test]
    fn linked_pitch_is_bitperfect_bypass() {
        let p = DopplerPitchParams::from_semitones(7.0, true);
        assert!(p.bypass);
        assert_eq!(p.delay_depth_s, 0.0);
    }

    #[test]
    fn reverb_wet_scaling() {
        assert_eq!(
            ReverbParams { enabled: true, level: 0.35 }.wet_gain(),
            0.35
        );
        assert_eq!(
            ReverbParams { enabled: false, level: 0.9 }.wet_gain(),
            0.0
        );
        assert_eq!(
            ReverbParams { enabled: true, level: 5.0 }.wet_gain(),
            1.2
        );
    }
}
