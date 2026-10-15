# Aether background design

Aether is Oxidify's cover-reactive animated background. It runs entirely on the GPU behind the `egui` UI.

## Stage order

1. **Tint** — a luma-masked colour wash over the current cover.
2. **Blur** — 8 Kawase-style passes at 160×160.
3. **Blend** — cosine-eased crossfade (≈1s) between the outgoing and incoming album treatments on track change.
4. **Warp** — two-octave simplex domain warp evolving at `time × 0.08`, displacement scaled by `intensity × 0.28` (default intensity 1.1).
5. **Output** — 1.2× zoom, 0.3 vignette, 1.4 saturation, and 60fps-quantised hash dithering at 0.012 to kill banding.

## Defaults

Blur 160px, 8 passes, speed 1.0, transition 1000ms, saturation 1.4, tint `#28283c` @ 0.15, dither 0.012, zoom 1.2.

## Rules

- Out-of-order cover loads are dropped by sequence id; only the newest cover is ever processed.
- Resize recreates only the warp target; the small blur chain stays fixed-size.
- Degraded mode (fewer passes / frozen warp) engages automatically under GPU pressure so playback and navigation never stutter.
- Lyrics and controls always composite above the background with guaranteed contrast.
