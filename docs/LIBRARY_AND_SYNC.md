# Library, cache, and remote sync

## Local library

Oxidify scans user-chosen folders for audio files, reads embedded tags, and indexes everything in SQLite (`tracks`: identity, title, artists, album, duration, file path, cover reference, full JSON row). Cover art is extracted to the file cache. Fast prefix search runs against the index; playlists, play history, and key-value preferences live in the same database.

## Cache

Four file-cache categories — `covers`, `audio`, `lyrics`, `metadata` — each independently clearable. Cache keys are sanitised so directory traversal is impossible. Remote audio longer than 20 minutes is streamed, never fully background-cached.

## Remote sync (optional)

A self-hosted-compatible remote service can be attached by setting a base URL (no default; plain HTTP is rejected except for loopback development URLs):

- Token auth (`login`/`register`/`refresh`/`logout`) with tokens in the OS keychain, proactive refresh before expiry, and single-flight refresh so parallel requests share one renewal.
- Catalog: search, track/album/artist/playlist details, recommendations, library likes, user playlists, history.
- Playback sessions: `POST /v1/tracks/{id}/playback` returns a short-lived `streamUrl` plus codec/bitrate/MIME metadata; relative URLs resolve against the base URL.
- Lyrics arrive as a `text/event-stream` of `metadata` / `provider` / `done` frames (`data:` payloads, blank-line dispatch, malformed frames discarded).

Every request carries `x-request-id`, `x-client-version`, and `x-platform` for tracing. Endpoint shapes are versioned in `crates/api`; see `docs/ROADMAP.md` for rollout order.
