# Architecture

## Principles

1. The UI thread never blocks: networking, decoding, SQLite, and file I/O run on workers; the UI renders snapshots plus clock extrapolation.
2. The audio callback is sacred: no allocation, locking, or I/O inside it — pre-decoded ring buffers only. Load coordination (generations, fades, session expiry) lives on the control thread.
3. Stale async results are dropped, never applied: every load carries a generation id. The same rule covers lyric frames and cover loads.
4. One clock: the audio worker's sample count feeds `PlaybackClock`, which drives the timeline, lyrics, and system media position. UI timers never drive sync.
5. Secrets live in the OS store: tokens via the platform keychain; SQLite holds metadata and file paths only.

## Data flow (playback)

UI command → queue/state → audio control thread (new generation, abort prior) → cache probe → decoder (Symphonia or native Opus path) → ring buffer → audio callback → sample clock → `PlaybackClock` snapshot → UI/lyrics/media session.

## Crate boundaries

`app` (shell, tray, single-instance, updates) → `ui` (`egui` views and view models) → `core` (pure rules, no I/O) + `api` (HTTP/event streams) + `audio` (decode/DSP/clock) + `storage` (SQLite/files/prefs) + `graphics` (background passes). Networking stays off the UI thread; the real-time callback stays allocation-free.

## Performance budgets

Ordinary playback: cold start under 1s, under 250MB RAM during playback, near-zero idle CPU, 60 FPS navigation, no audible gaps on track change. The fullscreen animated background has its own, separate GPU budget with automatic quality fallback (fewer blur passes, reduced warp rate).
