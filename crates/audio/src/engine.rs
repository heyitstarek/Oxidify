//! Native playback engine: Symphonia decoding, rodio output, [`PlaybackClock`] truth.
//!
//! Architecture: every load fully decodes the track into an in-memory `f32`
//! buffer. A custom [`rodio::Source`] (`EffectSource`) renders that buffer
//! through rate / pitch / reverb / gain stages into a [`rodio::Sink`].
//! `position_ms()` is audio-clock truth: a [`PlaybackClock`] seeded at every
//! play / pause / seek transition, extrapolated by wall clock times the
//! current rate, with drift correction against the sink position (150 ms
//! tolerance, mirroring [`PlaybackClock::resync_if_drifted`]) applied on
//! [`AudioEngine::poll_events`]. End of stream latches in `poll_events` and
//! emits [`EngineEvent::TrackEnded`] exactly once per load.
//!
//! Opus limitation (honest, never silence): Symphonia 0.5 ships no Opus codec,
//! and the standalone native Opus decoder crate needs a bundled C build that
//! is fragile on the Windows MSVC toolchain used here, so this engine does
//! not depend on it. Any input that smells like Opus (the `opus` hint, an
//! `OpusHead` magic inside Ogg, or an `A_OPUS` codec id inside WebM), or an
//! Ogg/WebM container that fails generic decoding, is rejected with
//! [`AudioError::Unsupported`]. Callers must surface that error to the user.
//!
//! Approximations (documented, all audible output stays click-reduced):
//! * Rate (`0.25..=3.0`) is a varispeed-style linear-interpolation resampler
//!   inside `EffectSource`: the fractional cursor advances `rate` input frames
//!   per output frame with linear blending. Cheap and smooth, but tempo and
//!   pitch move together (no phase-vocoder time stretch).
//! * Independent pitch (`linked == false` and `|st| >= 0.05`) is a granular
//!   Doppler shimmer approximation: 4 Hann-windowed taps
//!   ([`NUM_TAPS`](crate::dsp::NUM_TAPS)) over one
//!   [`GRAIN_PERIOD_S`](crate::dsp::GRAIN_PERIOD_S) grain, depth
//!   `min(0.035, |1 - 2^(st/12)| * T)` taken verbatim from
//!   [`DopplerPitchParams`](crate::dsp::DopplerPitchParams), summed with the
//!   [`WET_SUM_GAIN`](crate::dsp::WET_SUM_GAIN) 0.5 correction. Bypass uses the
//!   exact same predicate as `DopplerPitchParams::from_semitones`, and the dry
//!   path is bit-perfect. Expect shimmer/aliasing on extreme shifts; there is
//!   no formant preservation.
//! * Reverb is a Schroeder-style approximation (4 feedback combs per channel
//!   plus a one-pole 120 Hz highpass on the wet path), not the full
//!   generated-impulse studio reverb described in `dsp`. Wet gain honors
//!   [`ReverbParams`](crate::dsp::ReverbParams::wet_gain) exactly
//!   (`clamp(level, 0..1.2)`, 0 when disabled) and eases toward its target
//!   over [`REVERB_ATTACK_S`](crate::dsp::REVERB_ATTACK_S) so toggling never
//!   clicks. The tail is shorter than
//!   [`REVERB_TAIL_S`](crate::dsp::REVERB_TAIL_S); that constant documents the
//!   studio target, not this approximation.
//! * Gain (`play` / `pause` / `set_volume`) ramps linearly toward its target
//!   over [`RAMP_S`] (180 ms) per rendered sample, so there are no zipper
//!   clicks. Pause freezes the clock immediately and lets the 180 ms fade
//!   tail play out before pausing the sink via a single one-shot sleeping
//!   thread (no spin, no busy loop). Pausing within one fade window of the
//!   track end may truncate the last few ms of the tail.
//!
//! Other honest limitations:
//! * Full-file decode into memory: huge files cost RAM proportional to
//!   duration; stalls never happen, so [`EngineEvent::Stalled`] is currently
//!   never emitted (reserved for a future streaming path).
//! * Switching output devices rebuilds the stream and re-queues the current
//!   position, which costs a brief audible gap.
//! * Without any OS audio device, `new()` still succeeds in a silent mode:
//!   all clock / seek / event logic runs identically, only no samples move.
//!   Tests rely on this and assert no hardware behaviour.

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use thiserror::Error;

use crate::clock::PlaybackClock;
use crate::dsp::{
    DopplerPitchParams, GRAIN_PERIOD_S, NUM_TAPS, REVERB_ATTACK_S, REVERB_HIGHPASS_HZ,
    ReverbParams, WET_SUM_GAIN,
};
use crate::pipeline::{CodecSupport, decode_plan};

/// Gain smoothing window for play / pause / volume changes (no clicks).
pub const RAMP_S: f32 = 0.18;
/// Drift tolerance between the clock extrapolation and the sink position.
pub const RESYNC_TOLERANCE_MS: u64 = 150;
/// Allowed tempo range.
pub const RATE_MIN: f32 = 0.25;
/// Allowed tempo range.
pub const RATE_MAX: f32 = 3.0;
/// Fade-out tail honored before the sink actually pauses.
const FADE_PAUSE_MS: u64 = 180;

const OPUS_UNSUPPORTED_MSG: &str = "opus audio needs the native Opus decoder path, which is not built for this target (Windows MSVC); refusing instead of playing silence";

/// Engine-level errors. Decoding failures and unsupported inputs are loud
/// `Err` values here, never silent gaps.
#[derive(Debug, Error)]
pub enum AudioError {
    /// Filesystem / byte-source failure.
    #[error("io error: {0}")]
    Io(String),
    /// The container probed but samples could not be produced.
    #[error("decode error: {0}")]
    Decode(String),
    /// Honest refusal: unknown container or the unavailable Opus path.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Output device enumeration / opening failed.
    #[error("device error: {0}")]
    Device(String),
}

impl From<std::io::Error> for AudioError {
    fn from(e: std::io::Error) -> Self {
        AudioError::Io(e.to_string())
    }
}

/// Info about a successfully loaded track.
#[derive(Debug, Clone)]
pub struct LoadedInfo {
    /// Total length in ms. Always known: loads fully decode before returning.
    pub duration_ms: Option<u64>,
    /// Container/codec label (`wav`, `mp3`, `m4a`, `flac`, `ogg`, ...).
    pub codec: String,
}

/// Async engine notifications drained via [`AudioEngine::poll_events`].
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// The playhead reached the end of the loaded track (once per load).
    TrackEnded,
    /// Buffering stall state. Never emitted by the current full-memory
    /// decoder; reserved for a future streaming path.
    Stalled(bool),
    /// Asynchronous backend failure (device rebuilds report synchronously,
    /// so this is currently reserved for future use).
    Error(String),
    /// Current playhead in ms, emitted once per poll while playing.
    PositionMs(u64),
}

fn load_f32(slot: &AtomicU32) -> f32 {
    f32::from_bits(slot.load(Ordering::Relaxed))
}

fn store_f32(slot: &AtomicU32, value: f32) {
    slot.store(value.to_bits(), Ordering::Relaxed);
}

/// Live effect parameters shared with the audio thread via atomics, so
/// volume / rate / pitch / reverb changes apply without rebuilding the
/// source (which would click).
struct LiveParams {
    volume_target: AtomicU32,
    rate: AtomicU32,
    semitones: AtomicU32,
    linked: AtomicBool,
    reverb_on: AtomicBool,
    reverb_level: AtomicU32,
}

impl LiveParams {
    fn new() -> Self {
        let p = Self {
            volume_target: AtomicU32::new(0.0_f32.to_bits()),
            rate: AtomicU32::new(1.0_f32.to_bits()),
            semitones: AtomicU32::new(0.0_f32.to_bits()),
            linked: AtomicBool::new(true),
            reverb_on: AtomicBool::new(false),
            reverb_level: AtomicU32::new(0.0_f32.to_bits()),
        };
        store_f32(&p.volume_target, 0.0);
        store_f32(&p.rate, 1.0);
        store_f32(&p.semitones, 0.0);
        store_f32(&p.reverb_level, 0.0);
        p
    }

    fn set_volume_target(&self, v: f32) {
        store_f32(&self.volume_target, v);
    }

    fn volume_target(&self) -> f32 {
        load_f32(&self.volume_target)
    }

    fn set_rate(&self, r: f32) {
        store_f32(&self.rate, r);
    }

    fn rate(&self) -> f32 {
        load_f32(&self.rate)
    }

    fn set_pitch(&self, st: f32, linked: bool) {
        store_f32(&self.semitones, st);
        self.linked.store(linked, Ordering::Relaxed);
    }

    fn semitones(&self) -> f32 {
        load_f32(&self.semitones)
    }

    fn linked(&self) -> bool {
        self.linked.load(Ordering::Relaxed)
    }

    fn set_reverb(&self, enabled: bool, level: f32) {
        self.reverb_on.store(enabled, Ordering::Relaxed);
        store_f32(&self.reverb_level, level);
    }

    fn reverb_on(&self) -> bool {
        self.reverb_on.load(Ordering::Relaxed)
    }

    fn reverb_level(&self) -> f32 {
        load_f32(&self.reverb_level)
    }
}

/// Fully decoded track: interleaved `f32` samples plus stream geometry.
struct TrackData {
    samples: Vec<f32>,
    channels: usize,
    sample_rate: u32,
    frames: u64,
    duration_ms: u64,
}

/// In-memory byte source adapter so Symphonia can probe `Vec<u8>` loads.
struct MemSource {
    inner: Cursor<Vec<u8>>,
}

impl Read for MemSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for MemSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl MediaSource for MemSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.inner.get_ref().len() as u64)
    }
}

fn is_wav_stream(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

/// Ogg-encapsulated Opus always carries an `OpusHead` near the start.
fn sniff_opus(bytes: &[u8]) -> bool {
    if bytes.len() >= 4 && &bytes[0..4] == b"OggS" {
        let head = &bytes[..bytes.len().min(4096)];
        if head.windows(b"OpusHead".len()).any(|w| w == b"OpusHead") {
            return true;
        }
    }
    // WebM/Matroska with an Opus track advertises this codec id.
    if bytes.len() >= 4 && bytes[0] == 0x1A && bytes[1] == 0x45 && bytes[2] == 0xDF && bytes[3] == 0xA3
    {
        let head = &bytes[..bytes.len().min(65536)];
        if head.windows(b"A_OPUS".len()).any(|w| w == b"A_OPUS") {
            return true;
        }
    }
    false
}

fn normalize_hint(hint: &str) -> String {
    hint.trim().trim_start_matches('.').to_ascii_lowercase()
}

/// Map an extension / mime-ish hint onto the `(codec, mime)` pair that
/// [`decode_plan`] routes on.
fn guess_codec_mime(hint: &str) -> (String, String) {
    if hint.contains("mp3") || hint.contains("mpeg") {
        ("mp3".to_string(), "audio/mpeg".to_string())
    } else if hint.contains("wav") {
        ("wav".to_string(), "audio/wav".to_string())
    } else if hint.contains("flac") {
        ("flac".to_string(), "audio/flac".to_string())
    } else if hint.contains("aac") {
        ("aac".to_string(), "audio/aac".to_string())
    } else if hint.contains("m4a") {
        ("m4a".to_string(), "audio/mp4".to_string())
    } else if hint.contains("mp4") {
        ("mp4".to_string(), "audio/mp4".to_string())
    } else if hint.contains("ogg") || hint.contains("oga") {
        ("vorbis".to_string(), "audio/ogg".to_string())
    } else if hint.contains("webm") {
        ("vorbis".to_string(), "audio/webm".to_string())
    } else {
        (hint.to_string(), hint.to_string())
    }
}

/// Extension token suitable for the Symphonia probe hint.
fn probe_ext(hint: &str) -> &str {
    if hint.contains("mp3") || hint.contains("mpeg") {
        "mp3"
    } else if hint.contains("wav") {
        "wav"
    } else if hint.contains("flac") {
        "flac"
    } else if hint.contains("aac") {
        "aac"
    } else if hint.contains("m4a") {
        "m4a"
    } else if hint.contains("mp4") {
        "mp4"
    } else if hint.contains("ogg") || hint.contains("oga") {
        "ogg"
    } else if hint.contains("webm") {
        "webm"
    } else {
        hint
    }
}

/// Minimal strict WAV reader (PCM int 8/16/24/32 + float32, any channel
/// count, extra chunks skipped). Exists so the engine never depends on
/// container-probe heuristics for its own test fixtures.
fn decode_wav(bytes: &[u8]) -> Result<TrackData, String> {
    if !is_wav_stream(bytes) {
        return Err("not a RIFF/WAVE stream".to_string());
    }
    let read_u16 = |at: usize| -> Result<u16, String> {
        bytes
            .get(at..at + 2)
            .ok_or_else(|| "wav: truncated header".to_string())
            .map(|w| u16::from_le_bytes([w[0], w[1]]))
    };
    let read_u32 = |at: usize| -> Result<u32, String> {
        bytes
            .get(at..at + 4)
            .ok_or_else(|| "wav: truncated header".to_string())
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
    };
    let mut audio_format: Option<u16> = None;
    let mut channels: usize = 0;
    let mut sample_rate: u32 = 0;
    let mut bits: u16 = 0;
    let mut data: Option<&[u8]> = None;
    let mut cursor = 12usize;
    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = read_u32(cursor + 4)? as usize;
        let body = cursor + 8;
        let body_end = body.saturating_add(size).min(bytes.len());
        if id == b"fmt " {
            if body_end - body < 16 {
                return Err("wav: bad fmt chunk".to_string());
            }
            audio_format = Some(read_u16(body)?);
            channels = read_u16(body + 2)? as usize;
            sample_rate = read_u32(body + 4)?;
            bits = read_u16(body + 14)?;
        } else if id == b"data" {
            data = Some(&bytes[body..body_end]);
        }
        cursor = body_end + (body_end - body) % 2;
        if cursor > bytes.len() {
            break;
        }
        // Guard against a corrupt chunk size looping forever.
        if size == 0 && data.is_some() {
            break;
        }
    }
    let (format, data) = match (audio_format, data) {
        (Some(f), Some(d)) => (f, d),
        _ => return Err("wav: missing fmt or data chunk".to_string()),
    };
    if channels == 0 || channels > 16 || sample_rate == 0 {
        return Err("wav: invalid stream geometry".to_string());
    }
    let bytes_per_sample = match (format, bits) {
        (1, 8) | (1, 16) | (1, 24) | (1, 32) => (bits / 8) as usize,
        (3, 32) => 4,
        _ => return Err(format!("wav: unsupported format tag {format} / {bits}-bit")),
    };
    let frame_bytes = bytes_per_sample * channels;
    if frame_bytes == 0 {
        return Err("wav: invalid frame size".to_string());
    }
    let frames = data.len() / frame_bytes;
    if frames == 0 {
        return Err("wav: no audio frames".to_string());
    }
    let mut samples = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        for c in 0..channels {
            let at = frame * frame_bytes + c * bytes_per_sample;
            let v = match (format, bits) {
                (1, 8) => (data[at] as f32 - 128.0) / 128.0,
                (1, 16) => {
                    i16::from_le_bytes([data[at], data[at + 1]]) as f32 / 32768.0
                }
                (1, 24) => {
                    let raw = i32::from_le_bytes([data[at], data[at + 1], data[at + 2], 0]);
                    (raw >> 8) as f32 / 8_388_608.0
                }
                (1, 32) => {
                    i32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
                        as f32
                        / 2_147_483_648.0
                }
                (3, 32) => f32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]),
                _ => return Err("wav: unsupported sample encoding".to_string()),
            };
            samples.push(if v.is_finite() { v } else { 0.0 });
        }
    }
    let frames_u64 = frames as u64;
    let duration_ms = ((frames_u64 as u128 * 1000) / sample_rate.max(1) as u128).min(u64::MAX as u128)
        as u64;
    Ok(TrackData {
        samples,
        channels,
        sample_rate,
        frames: frames_u64,
        duration_ms,
    })
}

/// Full generic decode through Symphonia into one interleaved `f32` buffer.
/// Assumes a constant stream spec; a mid-stream spec change stops the decode
/// at that point (documented; essentially never happens for music files).
fn decode_probe(bytes: Vec<u8>, ext: &str) -> Result<TrackData, String> {
    let mss = MediaSourceStream::new(Box::new(MemSource { inner: Cursor::new(bytes) }), Default::default());
    let mut hint = Hint::new();
    if !ext.is_empty() {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe failed ({ext}): {e}"))?;
    let mut format = probed.format;
    let track_id = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.sample_rate.is_some())
        .or_else(|| format.tracks().first())
        .map(|t| t.id)
        .ok_or_else(|| "no audio tracks found".to_string())?;
    let codec_params = format
        .tracks()
        .iter()
        .find(|t| t.id == track_id)
        .map(|t| t.codec_params.clone())
        .ok_or_else(|| "selected track vanished".to_string())?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("decoder unavailable: {e}"))?;

    let mut samples: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut channels: usize = 0;
    let mut sample_rate: u32 = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let spec = *decoded.spec();
        if sample_buf.is_none() {
            channels = spec.channels.count();
            sample_rate = spec.rate;
            if channels == 0 || channels > 16 || sample_rate == 0 {
                return Err("invalid stream spec".to_string());
            }
            sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        }
        if spec.rate != sample_rate || spec.channels.count() != channels {
            break;
        }
        let buf = sample_buf.as_mut().expect("buffer initialized above");
        buf.copy_interleaved_ref(decoded);
        samples.extend_from_slice(buf.samples());
    }
    if samples.is_empty() || channels == 0 || sample_rate == 0 {
        return Err("no audio frames decoded".to_string());
    }
    let frames = (samples.len() / channels) as u64;
    let duration_ms =
        ((frames as u128 * 1000) / sample_rate as u128).min(u64::MAX as u128) as u64;
    Ok(TrackData {
        samples,
        channels,
        sample_rate,
        frames,
        duration_ms,
    })
}

fn ring_read(ring: &[f32], head: usize, delay: f32) -> f32 {
    debug_assert!(!ring.is_empty());
    let len = ring.len();
    let mut pos = head as f32 - 1.0 - delay.max(0.0);
    while pos < 0.0 {
        pos += len as f32;
    }
    let base = pos.floor();
    let i0 = (base as usize) % len;
    let i1 = (i0 + 1) % len;
    let frac = pos - base;
    ring[i0] + (ring[i1] - ring[i0]) * frac
}

struct CombState {
    buf: Vec<f32>,
    idx: usize,
}

impl CombState {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            idx: 0,
        }
    }

    fn process(&mut self, x: f32, feedback: f32) -> f32 {
        let y = self.buf[self.idx];
        self.buf[self.idx] = x + y * feedback;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        y
    }
}

/// The single audible source queued into the sink. Owns its own effect
/// state (pitch rings, reverb combs, gain/wet followers) and reads shared
/// [`LiveParams`] every frame, so control changes glide instead of clicking.
/// Tempo uses an in-source linear-interpolation resampler (varispeed style).
struct EffectSource {
    data: Arc<TrackData>,
    params: Arc<LiveParams>,
    cursor: f64,
    dry: Vec<f32>,
    work: Vec<f32>,
    out_frame: Vec<f32>,
    pending_idx: usize,
    pending_len: usize,
    gain: f32,
    wet: f32,
    grain_pos: usize,
    pitch_rings: Vec<Vec<f32>>,
    pitch_head: usize,
    pitch_len: usize,
    combs: Vec<Vec<CombState>>,
    hp_lp: Vec<f32>,
}

const COMB_DELAYS_MS: [f32; 4] = [29.7, 37.1, 41.1, 43.7];
const COMB_FEEDBACK: f32 = 0.84;

impl EffectSource {
    fn new(data: Arc<TrackData>, start_ms: u64, params: Arc<LiveParams>) -> Self {
        let channels = data.channels.max(1);
        let sr = data.sample_rate.max(1);
        let start_frames =
            ((start_ms as u128 * sr as u128) / 1000).min(data.frames as u128) as u64;
        let pitch_len = ((0.035 * sr as f32) as usize + 16).max(32);
        let pitch_rings = (0..channels).map(|_| vec![0.0; pitch_len]).collect();
        let combs = (0..channels)
            .map(|_| {
                COMB_DELAYS_MS
                    .iter()
                    .map(|ms| CombState::new(((ms * sr as f32) / 1000.0) as usize))
                    .collect()
            })
            .collect();
        Self {
            data,
            params,
            cursor: start_frames as f64,
            dry: vec![0.0; channels],
            work: vec![0.0; channels],
            out_frame: vec![0.0; channels],
            pending_idx: 0,
            pending_len: 0,
            gain: 0.0,
            wet: 0.0,
            grain_pos: 0,
            pitch_rings,
            pitch_head: 0,
            pitch_len,
            combs,
            hp_lp: vec![0.0; channels],
        }
    }

    fn render_frame(&mut self) -> Option<()> {
        let channels = self.data.channels;
        let sr = self.data.sample_rate.max(1);
        let sr_f = sr as f32;
        let rate = self.params.rate().clamp(RATE_MIN, RATE_MAX);

        let f0 = self.cursor.floor() as u64;
        if f0 >= self.data.frames {
            return None;
        }
        let frac = (self.cursor - f0 as f64) as f32;
        for c in 0..channels {
            let base = (f0 as usize) * channels + c;
            let a = self.data.samples[base];
            let b = if f0 + 1 < self.data.frames {
                self.data.samples[base + channels]
            } else {
                a
            };
            self.dry[c] = a + (b - a) * frac;
        }
        self.cursor += rate as f64;

        // Keep the pitch rings warm even while bypassed so re-engaging
        // does not blast stale silence.
        for c in 0..channels {
            self.pitch_rings[c][self.pitch_head] = self.dry[c];
        }

        // Pitch: exact bypass predicate + depth from DopplerPitchParams.
        let semitones = self.params.semitones();
        let linked = self.params.linked();
        let pitch = DopplerPitchParams::from_semitones(
            if semitones.is_finite() { semitones } else { 0.0 },
            linked,
        );
        if pitch.bypass {
            self.work.copy_from_slice(&self.dry);
        } else {
            let grain_len = ((GRAIN_PERIOD_S * sr_f) as usize).max(1);
            let depth = (pitch.delay_depth_s * sr_f).round().max(0.0) as usize;
            self.grain_pos = (self.grain_pos + 1) % grain_len;
            for c in 0..channels {
                let mut acc = 0.0;
                for k in 0..NUM_TAPS {
                    let phase =
                        (self.grain_pos as f32 / grain_len as f32 + k as f32 * 0.25) % 1.0;
                    let delay = if pitch.shift_up {
                        depth as f32 * (1.0 - phase)
                    } else {
                        depth as f32 * phase
                    };
                    let s = ring_read(&self.pitch_rings[c], self.pitch_head, delay);
                    let window = 0.5 - 0.5 * (2.0 * PI * phase).cos();
                    acc += window * s;
                }
                self.work[c] = acc * WET_SUM_GAIN;
            }
        }
        self.pitch_head = (self.pitch_head + 1) % self.pitch_len;

        // Reverb: exact wet target from ReverbParams, eased over the attack
        // window so toggling never clicks.
        let level = self.params.reverb_level();
        let wet_target = ReverbParams {
            enabled: self.params.reverb_on(),
            level: if level.is_finite() { level } else { 0.0 },
        }
        .wet_gain();
        let wet_target = if wet_target.is_finite() {
            wet_target
        } else {
            0.0
        };
        let attack_step = 1.0 / (REVERB_ATTACK_S * sr_f).max(1.0);
        self.wet += (wet_target - self.wet).clamp(-attack_step, attack_step);
        let hp_a = {
            let dt = 1.0 / sr_f;
            let rc = 1.0 / (2.0 * PI * REVERB_HIGHPASS_HZ);
            dt / (rc + dt)
        };

        // Master gain glides toward its target over RAMP_S.
        let vol_target = self.params.volume_target().clamp(0.0, 1.0);
        let gain_step = 1.0 / (RAMP_S * sr_f).max(1.0);
        self.gain += (vol_target - self.gain).clamp(-gain_step, gain_step);

        for c in 0..channels {
            let x = self.work[c];
            let mut wet_mix = 0.0;
            for comb in self.combs[c].iter_mut() {
                wet_mix += comb.process(x, COMB_FEEDBACK);
            }
            wet_mix *= 0.25;
            let lp = &mut self.hp_lp[c];
            *lp += hp_a * (wet_mix - *lp);
            let mixed = x + self.wet * (wet_mix - *lp);
            self.out_frame[c] = mixed.clamp(-1.0, 1.0) * self.gain;
        }
        self.pending_idx = 0;
        self.pending_len = channels;
        Some(())
    }
}

impl Iterator for EffectSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pending_idx >= self.pending_len && self.render_frame().is_none() {
            return None;
        }
        let sample = self.out_frame[self.pending_idx];
        self.pending_idx += 1;
        Some(sample)
    }
}

impl rodio::Source for EffectSource {
    fn current_frame_len(&self) -> Option<usize> {
        let done = self.cursor.floor().max(0.0) as u64;
        let left_frames = self.data.frames.saturating_sub(done);
        let left_samples = left_frames.saturating_mul(self.data.channels as u64);
        Some(left_samples.max(1).min(usize::MAX as u64) as usize)
    }

    fn channels(&self) -> u16 {
        self.data.channels.min(u16::MAX as usize) as u16
    }

    fn sample_rate(&self) -> u32 {
        self.data.sample_rate.max(1)
    }

    fn total_duration(&self) -> Option<Duration> {
        let done = self.cursor.floor().max(0.0) as u64;
        let left = self.data.frames.saturating_sub(done);
        let sr = self.data.sample_rate.max(1) as f64;
        Some(Duration::from_secs_f64(left as f64 / sr))
    }
}

struct AudioOut {
    // Held alive for the whole engine lifetime: dropping the stream ends
    // all playback, so these fields are intentionally only read on drop.
    _stream: rodio::OutputStream,
    _handle: rodio::OutputStreamHandle,
    sink: Arc<rodio::Sink>,
}

/// Real, audible playback engine.
///
/// Position is [`PlaybackClock`] truth seeded from every transport change;
/// the rodio sink only renders sound and corrects slow drift. The engine
/// works with no audio hardware at all (silent mode): transport, clamping
/// and event logic behave identically, samples just go nowhere.
pub struct AudioEngine {
    clock: PlaybackClock,
    track: Option<Arc<TrackData>>,
    playing: bool,
    ended_emitted: bool,
    events: VecDeque<EngineEvent>,
    volume: f32,
    rate: f32,
    semitones: f32,
    pitch_linked: bool,
    reverb_enabled: bool,
    reverb_level: f32,
    params: Arc<LiveParams>,
    out: Option<AudioOut>,
    device_name: Option<String>,
    worker_base_ms: u64,
    worker_sink_s: f64,
    pause_token: Arc<AtomicU64>,
}

impl AudioEngine {
    /// Build the engine. Succeeds even with no OS audio device (silent
    /// mode); audible output attaches automatically when a device exists.
    pub fn new() -> Result<Self, AudioError> {
        let params = Arc::new(LiveParams::new());
        store_f32(&params.volume_target, 0.0);
        let mut engine = Self {
            clock: PlaybackClock::new(),
            track: None,
            playing: false,
            ended_emitted: false,
            events: VecDeque::new(),
            volume: 1.0,
            rate: 1.0,
            semitones: 0.0,
            pitch_linked: true,
            reverb_enabled: false,
            reverb_level: 0.0,
            params,
            out: None,
            device_name: None,
            worker_base_ms: 0,
            worker_sink_s: 0.0,
            pause_token: Arc::new(AtomicU64::new(0)),
        };
        engine.params.set_rate(1.0);
        if let Ok((stream, handle)) = rodio::OutputStream::try_default() {
            if let Ok(sink) = rodio::Sink::try_new(&handle) {
                sink.pause();
                engine.out = Some(AudioOut {
                    _stream: stream,
                    _handle: handle,
                    sink: Arc::new(sink),
                });
            }
        }
        Ok(engine)
    }

    /// Load a file and park the playhead at `start_ms` (clamped, paused).
    /// Never autoplays. Opus inputs fail with [`AudioError::Unsupported`].
    pub fn load_file(
        &mut self,
        path: &std::path::Path,
        start_ms: u64,
    ) -> Result<LoadedInfo, AudioError> {
        let bytes = std::fs::read(path).map_err(|e| AudioError::Io(e.to_string()))?;
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        self.load_bytes(bytes, &ext, start_ms)
    }

    /// Load raw bytes with an extension/mime hint (`wav`, `.mp3`,
    /// `audio/mpeg`, ...). Garbage fails loudly; unknown hints fail via
    /// [`decode_plan`]; Opus fails with [`AudioError::Unsupported`].
    pub fn load_bytes(
        &mut self,
        bytes: Vec<u8>,
        ext_hint: &str,
        start_ms: u64,
    ) -> Result<LoadedInfo, AudioError> {
        if bytes.is_empty() {
            return Err(AudioError::Decode("empty input buffer".to_string()));
        }
        let hint = normalize_hint(ext_hint);
        // WAV sniff is unambiguous, so it wins over the hint.
        if is_wav_stream(&bytes) {
            match decode_wav(&bytes) {
                Ok(track) => return Ok(self.install_track(track, "wav".to_string(), start_ms)),
                Err(first_err) => {
                    return self.load_via_probe(bytes, "wav", "wav", start_ms, Some(first_err));
                }
            }
        }
        if hint == "opus" || hint.contains("opus") || sniff_opus(&bytes) {
            return Err(AudioError::Unsupported(OPUS_UNSUPPORTED_MSG.to_string()));
        }
        let (codec_guess, mime_guess) = guess_codec_mime(&hint);
        match decode_plan(&codec_guess, &mime_guess) {
            Err(e) => Err(AudioError::Unsupported(e.to_string())),
            Ok(CodecSupport::Symphonia) => {
                let ext = probe_ext(&hint).to_string();
                self.load_via_probe(bytes, &ext, &codec_guess, start_ms, None)
            }
            Ok(CodecSupport::OpusNative) => {
                // Ogg/WebM without an Opus signature: probably Vorbis, so try
                // the generic path, but translate failure into the honest
                // Opus refusal rather than a cryptic decode error.
                let ext = probe_ext(&hint).to_string();
                match decode_probe(bytes, &ext) {
                    Ok(track) => Ok(self.install_track(track, codec_guess, start_ms)),
                    Err(_) => Err(AudioError::Unsupported(OPUS_UNSUPPORTED_MSG.to_string())),
                }
            }
            Ok(CodecSupport::Unsupported) => Err(AudioError::Unsupported(format!(
                "unsupported codec for hint {ext_hint:?}"
            ))),
        }
    }

    fn load_via_probe(
        &mut self,
        bytes: Vec<u8>,
        ext: &str,
        codec_label: &str,
        start_ms: u64,
        prior_err: Option<String>,
    ) -> Result<LoadedInfo, AudioError> {
        match decode_probe(bytes, ext) {
            Ok(track) => Ok(self.install_track(track, codec_label.to_string(), start_ms)),
            Err(e) => {
                let mut msg = e;
                if let Some(first) = prior_err {
                    msg = format!("{first}; probe also failed: {msg}");
                }
                Err(AudioError::Decode(msg))
            }
        }
    }

    fn install_track(
        &mut self,
        track: TrackData,
        codec: String,
        start_ms: u64,
    ) -> LoadedInfo {
        let duration_ms = track.duration_ms;
        let start = start_ms.min(duration_ms);
        self.track = Some(Arc::new(track));
        self.clock = PlaybackClock::new();
        self.clock.set_position(start);
        self.playing = false;
        self.ended_emitted = false;
        self.events.clear();
        self.params.set_volume_target(0.0);
        self.requeue(start);
        LoadedInfo {
            duration_ms: Some(duration_ms),
            codec,
        }
    }

    /// (Re)queue the audible source at `start_ms`, preserving the
    /// paused/playing transport state. A fresh source always starts its gain
    /// at zero and glides up, so seeks and device switches never click.
    fn requeue(&mut self, start_ms: u64) {
        self.bump_epoch();
        if let Some(out) = self.out.as_ref() {
            out.sink.clear();
            if let Some(data) = self.track.clone() {
                let source = EffectSource::new(data, start_ms, Arc::clone(&self.params));
                out.sink.append(source);
            }
            if self.playing {
                out.sink.play();
            }
        }
        self.snapshot_worker(start_ms);
    }

    fn snapshot_worker(&mut self, base_ms: u64) {
        self.worker_base_ms = base_ms;
        self.worker_sink_s = self
            .out
            .as_ref()
            .map(|out| out.sink.get_pos().as_secs_f64())
            .unwrap_or(0.0);
        if !self.worker_sink_s.is_finite() || self.worker_sink_s < 0.0 {
            self.worker_sink_s = 0.0;
        }
    }

    fn bump_epoch(&mut self) -> u64 {
        let next = self.pause_token.load(Ordering::SeqCst).wrapping_add(1);
        self.pause_token.store(next, Ordering::SeqCst);
        next
    }

    fn seek_to(&mut self, target: u64) {
        let duration = self.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
        let clamped = target.min(duration);
        self.clock.set_position(clamped);
        if clamped < duration {
            self.ended_emitted = false;
        }
        self.params.set_volume_target(if self.playing {
            self.volume
        } else {
            0.0
        });
        self.requeue(clamped);
    }

    /// Start (or resume) playback with a 180 ms fade-in. Restarts from zero
    /// when the playhead sits at the end. No-op without a loaded track.
    pub fn play(&mut self) {
        if self.track.is_none() {
            return;
        }
        if self.past_end() {
            self.seek_to(0);
        }
        self.clock.play();
        self.playing = true;
        self.bump_epoch();
        self.params.set_volume_target(self.volume);
        if let Some(out) = self.out.as_ref() {
            out.sink.play();
        }
        let base = self.clock.estimated_position_ms();
        self.snapshot_worker(base);
    }

    /// Freeze the playhead immediately, fade the audible tail over 180 ms,
    /// then pause the sink (one-shot sleeping thread, never a busy loop).
    /// No-op without a loaded track.
    pub fn pause(&mut self) {
        if self.track.is_none() {
            return;
        }
        let frozen = self.live_position();
        self.clock.pause(frozen);
        self.playing = false;
        self.params.set_volume_target(0.0);
        self.snapshot_worker(frozen);
        let epoch = self.bump_epoch();
        if let Some(out) = self.out.as_ref() {
            let sink = Arc::clone(&out.sink);
            let token = Arc::clone(&self.pause_token);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(FADE_PAUSE_MS));
                if token.load(Ordering::SeqCst) == epoch {
                    sink.pause();
                }
            });
        }
    }

    /// Move the playhead, clamped to the track duration. Keeps the current
    /// transport state. No-op without a loaded track.
    pub fn seek_ms(&mut self, ms: u64) {
        if self.track.is_none() {
            return;
        }
        self.seek_to(ms);
    }

    /// Master volume. NaN is ignored; anything else is clamped to 0..=1 and
    /// glides to the new target over 180 ms while playing. Never panics.
    pub fn set_volume(&mut self, v: f32) {
        if !v.is_finite() {
            return;
        }
        self.volume = v.clamp(0.0, 1.0);
        if self.playing {
            self.params.set_volume_target(self.volume);
        }
    }

    /// Tempo 0.25..=3.0 (varispeed resample in the source). Non-finite and
    /// non-positive inputs are ignored; the rest is clamped. The clock
    /// re-anchors so the change never jumps, and the sink-drift snapshot is
    /// refreshed for the same reason.
    pub fn set_rate(&mut self, r: f32) {
        if !r.is_finite() || r <= 0.0 {
            return;
        }
        self.rate = r.clamp(RATE_MIN, RATE_MAX);
        self.clock.set_rate(self.rate);
        self.params.set_rate(self.rate);
        let base = self.clock.estimated_position_ms();
        self.snapshot_worker(base);
    }

    /// Independent pitch shift. Non-finite input is ignored. The granular
    /// stage engages exactly when `linked == false && |st| >= 0.05`
    /// (see [`DopplerPitchParams`]); otherwise the dry path is bit-perfect.
    pub fn set_pitch(&mut self, semitones: f32, linked: bool) {
        if !semitones.is_finite() {
            return;
        }
        self.semitones = semitones;
        self.pitch_linked = linked;
        self.params.set_pitch(semitones, linked);
    }

    /// Studio reverb. NaN level is ignored; finite levels are clamped to
    /// 0..=1.2 and the wet gain eases in over the attack window.
    pub fn set_reverb(&mut self, enabled: bool, level: f32) {
        if !level.is_finite() {
            return;
        }
        self.reverb_enabled = enabled;
        self.reverb_level = level.clamp(0.0, 1.2);
        self.params.set_reverb(enabled, self.reverb_level);
    }

    fn live_position(&self) -> u64 {
        match self.track.as_ref() {
            None => 0,
            Some(data) => self
                .clock
                .estimated_position_ms()
                .min(data.duration_ms),
        }
    }

    fn past_end(&self) -> bool {
        match self.track.as_ref() {
            None => false,
            Some(data) => self.clock.estimated_position_ms() >= data.duration_ms,
        }
    }

    /// Audio-clock truth: clock extrapolation clamped to the duration.
    /// Returns 0 with no loaded track.
    pub fn position_ms(&self) -> u64 {
        self.live_position()
    }

    /// Known total length once a track is loaded (loads fully decode).
    pub fn duration_ms(&self) -> Option<u64> {
        self.track.as_ref().map(|t| t.duration_ms)
    }

    /// True while transport is playing and the playhead is before the end.
    pub fn is_playing(&self) -> bool {
        self.playing && !self.past_end()
    }

    /// Output device names; empty when enumeration fails or no backend
    /// exists. Never panics.
    pub fn output_devices(&self) -> Vec<String> {
        use rodio::cpal::traits::{DeviceTrait, HostTrait};
        let mut names = Vec::new();
        let host = rodio::cpal::default_host();
        if let Ok(devices) = host.output_devices() {
            for device in devices {
                if let Ok(name) = device.name() {
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
        }
        names
    }

    /// Switch output (`None` = OS default). Rebuilds the stream and
    /// re-queues at the current position (brief gap, documented). On failure
    /// the previous output is kept and an [`AudioError::Device`] is
    /// returned. Never panics.
    pub fn set_output_device(&mut self, name: Option<&str>) -> Result<(), AudioError> {
        use rodio::cpal::traits::{DeviceTrait, HostTrait};
        let host = rodio::cpal::default_host();
        let devices = host
            .output_devices()
            .map_err(|e| AudioError::Device(format!("enumerate outputs: {e}")))?;
        let mut chosen: Option<rodio::Device> = None;
        match name {
            None => {
                chosen = host.default_output_device();
            }
            Some(want) => {
                for device in devices {
                    match device.name() {
                        Ok(candidate) if candidate == want => {
                            chosen = Some(device);
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
        let device = match (chosen, name) {
            (Some(d), _) => d,
            (None, Some(want)) => {
                return Err(AudioError::Device(format!("output device not found: {want}")));
            }
            (None, None) => {
                return Err(AudioError::Device(
                    "no default output device available".to_string(),
                ));
            }
        };
        let (stream, handle) = rodio::OutputStream::try_from_device(&device)
            .map_err(|e| AudioError::Device(format!("open output: {e}")))?;
        let sink = rodio::Sink::try_new(&handle)
            .map_err(|e| AudioError::Device(format!("create sink: {e}")))?;
        sink.pause();
        self.out = Some(AudioOut {
            _stream: stream,
            _handle: handle,
            sink: Arc::new(sink),
        });
        self.device_name = name.map(str::to_string);
        let pos = self.live_position();
        self.requeue(pos);
        if self.playing {
            self.params.set_volume_target(self.volume);
        }
        Ok(())
    }

    /// Drain pending events. While playing, emits one [`EngineEvent::PositionMs`]
    /// per call; crossing the end latches transport, pauses the sink, and
    /// emits `PositionMs(duration)` + [`EngineEvent::TrackEnded`] exactly
    /// once per load. Also folds sink drift back into the clock past 150 ms.
    pub fn poll_events(&mut self) -> Vec<EngineEvent> {
        if self.playing {
            if let Some(out) = self.out.as_ref() {
                let sink_s = out.sink.get_pos().as_secs_f64();
                if sink_s.is_finite() {
                    // Queue rebuilds (seek/load/device switch) reset the
                    // sink-side counter: a backwards jump means "fresh
                    // source", so rebase the snapshot instead of snapping
                    // the clock.
                    if sink_s < self.worker_sink_s {
                        self.worker_sink_s = sink_s.max(0.0);
                    }
                    // Only trust the sink once it provably advances. A frozen
                    // backend (headless dummy device, suspended stream) must
                    // never drag the clock backwards; the clock stays the
                    // truth until the sink moves.
                    if sink_s > self.worker_sink_s + 0.005 {
                        let advanced_ms = (sink_s - self.worker_sink_s) * 1000.0
                            * f64::from(self.rate.max(RATE_MIN));
                        let worker = self
                            .worker_base_ms
                            .saturating_add(advanced_ms.max(0.0) as u64);
                        self.clock.resync_if_drifted(worker, RESYNC_TOLERANCE_MS);
                    }
                }
            }
            if self.past_end() {
                let end = self.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
                self.playing = false;
                self.clock.pause(end);
                self.bump_epoch();
                self.params.set_volume_target(0.0);
                if let Some(out) = self.out.as_ref() {
                    out.sink.clear();
                }
                self.snapshot_worker(end);
                if !self.ended_emitted {
                    self.ended_emitted = true;
                    self.events.push_back(EngineEvent::PositionMs(end));
                    self.events.push_back(EngineEvent::TrackEnded);
                }
            } else {
                self.events.push_back(EngineEvent::PositionMs(self.live_position()));
            }
        }
        self.events.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;
    use std::thread::sleep;

    fn make_wav_bytes(duration_ms: u64, sample_rate: u32) -> Vec<u8> {
        let frames = (duration_ms * sample_rate as u64 / 1000) as usize;
        let mut out = Vec::with_capacity(44 + frames * 2);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + frames * 2) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&((frames * 2) as u32).to_le_bytes());
        for i in 0..frames {
            let t = i as f32 / sample_rate as f32;
            let s = (2.0 * PI * 440.0 * t).sin() * 0.25;
            out.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
        }
        out
    }

    fn fixture(duration_ms: u64) -> Vec<u8> {
        make_wav_bytes(duration_ms, 16_000)
    }

    #[test]
    fn state_machine_load_play_pause_seek() {
        let mut engine = AudioEngine::new().expect("engine constructs headless");
        let info = engine.load_bytes(fixture(500), "wav", 0).expect("wav loads");
        assert_eq!(info.duration_ms, Some(500));
        assert!(!info.codec.is_empty());
        assert!(!engine.is_playing());
        assert_eq!(engine.position_ms(), 0);

        engine.play();
        assert!(engine.is_playing());
        engine.pause();
        assert!(!engine.is_playing());

        engine.seek_ms(100);
        assert_eq!(engine.position_ms(), 100);
        engine.play();
        assert!(engine.is_playing());
        // While playing the wall clock keeps running through the seek's
        // sink rebuild, so only a range assert is deterministic here.
        engine.seek_ms(250);
        let p = engine.position_ms();
        assert!(
            (250..750).contains(&p),
            "seek-while-playing should land near 250, got {p}"
        );
        engine.pause();
    }

    #[test]
    fn seek_clamps_past_duration() {
        let mut engine = AudioEngine::new().expect("engine constructs headless");
        let info = engine.load_bytes(fixture(300), ".wav", 0).expect("wav loads");
        let duration = info.duration_ms.expect("known duration");
        engine.seek_ms(u64::MAX);
        assert_eq!(engine.position_ms(), duration);
        assert_eq!(engine.duration_ms(), Some(duration));
    }

    #[test]
    fn load_start_clamps_past_duration() {
        let mut engine = AudioEngine::new().expect("engine constructs headless");
        let info = engine
            .load_bytes(fixture(300), "wav", 999_999)
            .expect("wav loads");
        let duration = info.duration_ms.expect("known duration");
        assert_eq!(engine.position_ms(), duration);
        assert!(!engine.is_playing());
    }

    #[test]
    fn position_monotonic_while_playing_and_frozen_while_paused() {
        let mut engine = AudioEngine::new().expect("engine constructs headless");
        engine.load_bytes(fixture(4000), "wav", 0).expect("wav loads");
        engine.play();
        sleep(Duration::from_millis(40));
        let p1 = engine.position_ms();
        sleep(Duration::from_millis(80));
        let p2 = engine.position_ms();
        assert!(p2 >= p1, "monotonic while playing: {p1} -> {p2}");

        engine.pause();
        let q1 = engine.position_ms();
        sleep(Duration::from_millis(60));
        let q2 = engine.position_ms();
        assert_eq!(q1, q2, "frozen while paused");
    }

    #[test]
    fn track_ended_fires_exactly_once() {
        let mut engine = AudioEngine::new().expect("engine constructs headless");
        let info = engine.load_bytes(fixture(150), "wav", 0).expect("wav loads");
        let duration = info.duration_ms.expect("known duration");
        engine.play();
        sleep(Duration::from_millis(600));
        let events = engine.poll_events();
        assert!(
            events.iter().any(|e| matches!(e, EngineEvent::TrackEnded)),
            "expected TrackEnded, got {events:?}"
        );
        assert!(!engine.is_playing());
        assert_eq!(engine.position_ms(), duration);
        let again = engine.poll_events();
        assert!(
            !again.iter().any(|e| matches!(e, EngineEvent::TrackEnded)),
            "TrackEnded must fire once, got {again:?}"
        );
    }

    #[test]
    fn garbage_bytes_are_loud_errors() {
        let mut engine = AudioEngine::new().expect("engine constructs headless");
        let garbage = vec![0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE, 0x10, 0x20, 0x30];
        assert!(engine.load_bytes(garbage.clone(), "mp3", 0).is_err());
        assert!(engine.load_bytes(garbage.clone(), "bin", 0).is_err());
        assert!(engine.load_bytes(Vec::new(), "wav", 0).is_err());
        assert!(engine.load_bytes(garbage, "", 0).is_err());
    }

    #[test]
    fn opus_inputs_are_honest_unsupported() {
        let mut engine = AudioEngine::new().expect("engine constructs headless");
        // Ogg container carrying an Opus head, mislabeled on purpose: the
        // sniff must still refuse with Unsupported, never silence.
        let mut fake = b"OggS".to_vec();
        fake.extend_from_slice(&[0u8; 64]);
        fake.extend_from_slice(b"OpusHead");
        let err = engine
            .load_bytes(fake, "wav", 0)
            .expect_err("opus must not load");
        assert!(matches!(err, AudioError::Unsupported(_)), "got {err:?}");
        let err = engine
            .load_bytes(vec![1, 2, 3, 4], "opus", 0)
            .expect_err("opus hint must not load");
        assert!(matches!(err, AudioError::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn setters_never_panic_on_edge_values() {
        let mut engine = AudioEngine::new().expect("engine constructs headless");
        engine.load_bytes(fixture(1000), "wav", 0).expect("wav loads");
        for v in [0.0, -1.0, -999.0, 2.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            engine.set_volume(v);
        }
        for r in [
            0.0,
            -2.0,
            0.1,
            0.25,
            1.0,
            3.0,
            100.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            engine.set_rate(r);
        }
        for st in [0.0, -30.0, 30.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            engine.set_pitch(st, true);
            engine.set_pitch(st, false);
        }
        for level in [0.0, -1.0, 999.0, f32::NAN, f32::INFINITY] {
            engine.set_reverb(true, level);
            engine.set_reverb(false, level);
        }
        engine.play();
        engine.set_volume(f32::NAN);
        engine.set_rate(f32::NAN);
        engine.set_pitch(f32::NAN, false);
        engine.set_reverb(true, f32::NAN);
        let _ = engine.position_ms();
        let _ = engine.poll_events();
        engine.pause();
        // Device surface never panics; results depend on host hardware.
        let _ = engine.output_devices();
        assert!(engine.set_output_device(Some("no-such-device-xyz")).is_err());
        let _ = engine.set_output_device(None);
    }

    #[test]
    fn transport_without_track_is_noop() {
        let mut engine = AudioEngine::new().expect("engine constructs headless");
        engine.play();
        engine.pause();
        engine.seek_ms(1234);
        assert_eq!(engine.position_ms(), 0);
        assert_eq!(engine.duration_ms(), None);
        assert!(!engine.is_playing());
        assert!(engine.poll_events().is_empty());
    }

    #[test]
    fn load_file_roundtrip_and_missing_file() {
        let mut engine = AudioEngine::new().expect("engine constructs headless");
        assert!(engine
            .load_file(std::path::Path::new("definitely-missing-file.wav"), 0)
            .is_err());
        let path = std::env::temp_dir().join("oxidify-audio-engine-test.wav");
        std::fs::write(&path, fixture(250)).expect("temp wav writable");
        let info = engine.load_file(&path, 0).expect("file loads");
        assert_eq!(info.duration_ms, Some(250));
        assert_eq!(info.codec, "wav");
        let _ = std::fs::remove_file(&path);
    }
}
