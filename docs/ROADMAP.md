# Roadmap

## M1 — Feasibility prototype (current)

One native binary proving the hard parts together: authenticate against a configured service, play a real stream, seek/pause/resume/change tracks, show clock-driven synced lyrics, render one Aether background, and report CPU/RAM/frame-timing/audio health. Run with `cargo run -p oxidify-app`.

## M2 — Music player core

Queue management, play/pause/seek, shuffle/repeat, buffering, audio caching, track switching, history, offline recovery. The audio worker stays independent of the UI, with regression tests for every repaired defect.

## M3 — Complete interface

Sidebar, search, albums, artists, playlists, settings, fullscreen player with lyrics, queue panel, overlays. Spacing, typography, timings, and the Aether pipeline land here with visual comparisons.

## M4 — Desktop integration & hardening

Media keys, system tray, single-instance, startup/shutdown, keychain credentials, logging, updates, window management. Windows first, then Linux and macOS.

## Verification, performance, release

Side-by-side verification under identical conditions, resource benchmarks against the targets below, installer/update-path checks, reproducible builds.

## Targets

Cold start <1s · playback RAM <250MB · idle CPU ≈0 · 60 FPS navigation · no audible gaps · installer <50MB where practical · zero missing core features. All numbers measured on target hardware, never predicted.
