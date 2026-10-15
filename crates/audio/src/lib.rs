//! Native audio engine.
//!
//! There is no browser audio element here: decoding runs through Symphonia
//! (AAC/MP3/FLAC and friends) plus a dedicated native Opus path, output goes
//! through the platform audio device, and `PlaybackClock` is the single
//! source of truth for position. Lyrics and UI widgets extrapolate from it,
//! so seeking, buffering, rate changes, and pause/resume all stay in sync.

pub mod clock;
pub mod dsp;
pub mod engine;
pub mod pipeline;

pub use clock::PlaybackClock;
pub use dsp::{DopplerPitchParams, GRAIN_PERIOD_S, NUM_TAPS, ReverbParams};
pub use engine::{AudioEngine, AudioError, EngineEvent, LoadedInfo};
pub use pipeline::{AudioPipelineError, CodecSupport, decode_plan};
