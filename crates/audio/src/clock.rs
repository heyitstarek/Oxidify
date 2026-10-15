use std::time::Instant;

/// Audio-clock position tracker.
///
/// The web-facing animator this mirrors extrapolates
/// `anchor + (now − anchor)` while playing and resyncs past 150ms of drift;
/// this type encodes that behaviour so UI and tests share one implementation.
#[derive(Debug, Clone)]
pub struct PlaybackClock {
    anchor_position_ms: u64,
    anchor_instant: Option<Instant>,
    playing: bool,
    playback_rate: f32,
}

impl PlaybackClock {
    pub fn new() -> Self {
        Self {
            anchor_position_ms: 0,
            anchor_instant: None,
            playing: false,
            playback_rate: 1.0,
        }
    }

    pub fn set_position(&mut self, position_ms: u64) {
        self.anchor_position_ms = position_ms;
        if self.playing {
            self.anchor_instant = Some(Instant::now());
        }
    }

    pub fn play(&mut self) {
        self.playing = true;
        self.anchor_instant = Some(Instant::now());
    }

    pub fn pause(&mut self, frozen_position_ms: u64) {
        self.playing = false;
        self.anchor_position_ms = frozen_position_ms;
        self.anchor_instant = None;
    }

    pub fn set_rate(&mut self, rate: f32) {
        // Re-anchor so a rate change does not jump.
        let now = self.estimated_position_ms();
        self.anchor_position_ms = now;
        self.playback_rate = rate;
        if self.playing {
            self.anchor_instant = Some(Instant::now());
        }
    }

    /// Resync when the worker reports a position that disagrees with the
    /// extrapolation by more than `tolerance_ms` (TS uses 150ms).
    pub fn resync_if_drifted(&mut self, worker_position_ms: u64, tolerance_ms: u64) {
        let est = self.estimated_position_ms();
        if est.abs_diff(worker_position_ms) > tolerance_ms {
            self.anchor_position_ms = worker_position_ms;
            if self.playing {
                self.anchor_instant = Some(Instant::now());
            }
        }
    }

    pub fn estimated_position_ms(&self) -> u64 {
        if !self.playing {
            return self.anchor_position_ms;
        }
        let Some(t0) = self.anchor_instant else {
            return self.anchor_position_ms;
        };
        let dt_ms = t0.elapsed().as_secs_f32() * 1000.0 * self.playback_rate;
        self.anchor_position_ms.saturating_add(dt_ms.max(0.0) as u64)
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }
}

impl Default for PlaybackClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn paused_clock_is_frozen() {
        let mut c = PlaybackClock::new();
        c.set_position(10_000);
        assert_eq!(c.estimated_position_ms(), 10_000);
    }

    #[test]
    fn playing_clock_advances_with_rate() {
        let mut c = PlaybackClock::new();
        c.set_position(0);
        c.set_rate(2.0);
        c.play();
        sleep(Duration::from_millis(60));
        let est = c.estimated_position_ms();
        assert!(est >= 80 && est < 400, "est={est}");
    }

    #[test]
    fn drift_resync_matches_150ms_rule() {
        let mut c = PlaybackClock::new();
        c.set_position(1000);
        c.play();
        c.resync_if_drifted(1005, 150);
        assert_eq!(c.estimated_position_ms() >= 1000, true);
        c.resync_if_drifted(5000, 150);
        let est = c.estimated_position_ms();
        assert!(est >= 4900 && est < 5600, "est={est}");
    }
}
