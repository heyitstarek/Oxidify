# Oxidify

A fast, lightweight, visually rich native desktop music player, built in Rust.

- **Native**: single executable for Windows, Linux, and macOS. No webview, no bundled browser.
- **Stack**: Rust + `egui`/`eframe` for UI, `wgpu`-class GPU visuals, a real-time-safe audio engine (`CPAL` output, `Symphonia` decoding plus a native Opus path), `reqwest`/`Tokio` networking, `SQLite` metadata cache, OS keychain secrets.
- **Local-first**: play and organise your own audio files with synced-lyrics display and animated cover-reactive backgrounds. An optional remote catalog service can be attached via a configurable base URL.

## Workspace

| Crate | Responsibility |
|---|---|
| `crates/core` | Domain models, queue rules, lyric timing |
| `crates/api` | Optional remote service client (auth, catalog, streaming, lyric events) |
| `crates/audio` | Playback clock, decode routing, DSP parameters |
| `crates/storage` | SQLite metadata, blob cache, preferences, secrets |
| `crates/graphics` | Aether animated-background engine, WGSL stages |
| `crates/ui` | `egui` views, layout, and interaction |
| `crates/app` | `oxidify` binary: startup, shell, desktop integration |

## Build & run

```powershell
cargo test --workspace
cargo run -p oxidify-app
```

## Docs

- `docs/ARCHITECTURE.md` — module boundaries and threading rules
- `docs/AUDIO.md` — playback engine design
- `docs/LIBRARY_AND_SYNC.md` — local library, cache, and remote sync
- `docs/VISUALS.md` — Aether background design
- `docs/ROADMAP.md` — milestones and performance targets
