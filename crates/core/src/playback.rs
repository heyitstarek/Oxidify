use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlayerStatus {
    #[default]
    Idle,
    Loading,
    Playing,
    Paused,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

/// Commands for the player state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerCommand {
    PlayRequested { track_id: String },
    PauseRequested,
    ResumeRequested,
    SeekCommitted { position_ms: u64 },
    SkipNext { is_auto_end: bool },
    SkipPrevious,
    TogglePlayPause,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectsState {
    /// Supported range is 0.25..=3.0; 1.0 = normal speed.
    pub playback_rate: f32,
    /// -12..=+12 semitones.
    pub pitch_semitones: f32,
    /// True = tempo and pitch move together. False = independent pitch
    /// via the granular shifter.
    pub pitch_linked: bool,
    pub keep_speed_across_tracks: bool,
    pub reverb_enabled: bool,
    /// 0.0..=1.0 wet mix.
    pub reverb_level: f32,
}

impl Default for EffectsState {
    fn default() -> Self {
        Self {
            playback_rate: 1.0,
            pitch_semitones: 0.0,
            pitch_linked: true,
            keep_speed_across_tracks: true,
            reverb_enabled: false,
            reverb_level: 0.35,
        }
    }
}

impl EffectsState {
    /// Active iff independent pitch is selected and the shift is audible
    /// (`!pitch_linked && |semitones| >= 0.05`).
    pub fn is_pitch_shifter_active(&self) -> bool {
        !self.pitch_linked && self.pitch_semitones.abs() >= 0.05
    }

    pub fn is_reverb_active(&self) -> bool {
        self.reverb_enabled && self.reverb_level > 0.01
    }

    pub fn reset_speed_and_pitch(&mut self) {
        self.playback_rate = 1.0;
        self.pitch_semitones = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_shifter_activation_rule() {
        let mut fx = EffectsState::default();
        assert!(!fx.is_pitch_shifter_active());
        fx.pitch_linked = false;
        fx.pitch_semitones = 0.04;
        assert!(!fx.is_pitch_shifter_active());
        fx.pitch_semitones = 0.05;
        assert!(fx.is_pitch_shifter_active());
    }
}
