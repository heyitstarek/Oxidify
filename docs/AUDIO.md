# Audio engine design

## Clock

`PlaybackClock` (`crates/audio/src/clock.rs`) is the single source of truth for position. The audio worker owns sample-count time; the UI reads an atomic snapshot and extrapolates:

- While playing: `anchor + (now − anchor) × rate`.
- While paused: frozen anchor.
- Rate changes re-anchor so there is no jump.
- Worker reports that disagree with the extrapolation by more than 150ms trigger a resync.

Lyrics, the timeline slider, and media-session position all derive from this clock.

## Decode routing

`decode_plan(codec, mime)` (`crates/audio/src/pipeline.rs`) routes each stream before playback is attempted:

- AAC / MP3 / FLAC / WAV → Symphonia pipeline.
- Opus (including `audio/webm` and Ogg Opus) → dedicated native Opus path.
- Anything else → explicit `UnsupportedCodec` error. Unknown audio is never played as silence.

Streaming, buffering, and seeking behaviour is verified per container, not assumed.

## DSP

`crates/audio/src/dsp.rs` defines the effect parameters:

- **Pitch**: granular Doppler shifter with a 38ms grain window and 4 staggered taps (wet-sum gain 0.5 for unity). Depth follows `min(0.035, |1 − 2^(st/12)| × 0.038)`. The shifter is fully bypassed (bit-perfect dry path) unless independent pitch is requested with at least 0.05 semitones of shift.
- **Reverb**: generated stereo impulse response, 2.8s tail with a 15ms soft attack, exponential decay, progressive high-frequency damping, and a 120Hz highpass on the wet path. Wet gain follows the user level clamped to 0..1.2, and is exactly 0 when reverb is off.

## Runtime rules

- One load generation at a time; superseded loads are abandoned.
- 180ms cosine fades on pause/resume/track change and volume restore.
- Sessions/URLs carry expiry; anything within 15s of expiry is refreshed, never reused.
- Buffering surfaces a loading state; stalls longer than 400ms are logged for diagnostics.
- Track-end is confirmed near the true duration before auto-advancing, so teardown artefacts can't skip tracks.
- One automatic recovery attempt (reload at the current position) precedes any user-facing error.
- Pausing must leave no hot DSP loop; output-device switches are handled without dropping the queue.
