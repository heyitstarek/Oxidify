//! CPU-driven egui presentation of the Aether background.
//!
//! The GPU stage order from `docs/VISUALS.md` (tint → blur → eased blend →
//! domain warp → output grade) is reproduced here on the CPU so the animated
//! background also works on the `glow` backend, where no custom `wgpu`
//! passes are available.
//!
//! Mapping of stages to this implementation:
//!
//! * Blur: the cover is reduced to a small vertical gradient built from the
//!   dominant cover colors, which is inherently smooth like a heavily
//!   blurred image. No explicit multi-pass blur is performed on the CPU.
//! * Warp: two drifting hash-based value-noise fields displace the gradient
//!   lookup. Displacement is scaled by [`AetherParams::warp_scale`] and the
//!   drift clock runs at `time * 0.08`, matching the shader stage.
//! * Blend: [`AetherBackground::note_track_change`] snapshots the frame that
//!   is currently on screen; the next frames mix that snapshot with the new
//!   look using [`AetherParams::eased_blend`] over
//!   `transition_duration_ms`.
//! * Output: zoom (`scale`), `0.3` vignette, saturation boost, and temporal
//!   hash grain scaled by `dithering` are applied per pixel.
//!
//! [`AetherBackground::render_to_buffer`] is the pure core used both by
//! [`AetherBackground::paint`] and by headless tests. It is logically pure:
//! its output depends only on its arguments and on state installed through
//! `&mut` methods. It memoizes the produced frame internally so that
//! `note_track_change` can snapshot exactly what was last displayed.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use super::aether::AetherParams;

/// Default maximum buffer width in pixels.
///
/// The CPU buffer stays tiny; `egui` upscales the uploaded texture with
/// linear filtering when drawing the full rect.
pub const DEFAULT_MAX_WIDTH: usize = 192;

/// Smallest accepted buffer width; also the width used while degraded.
const MIN_WIDTH: usize = 16;

/// Widest buffer this engine will allocate, even if requested larger.
const HARD_MAX_WIDTH: usize = 512;

/// Upper bound for the buffer height, so extreme aspect ratios cannot make
/// the per-frame CPU cost explode.
const HARD_MAX_HEIGHT: usize = 512;

/// Neutral dark gradient (top → bottom) used when no palette is set.
const FALLBACK_STOPS: [[u8; 3]; 3] = [[34, 36, 54], [20, 22, 34], [12, 12, 20]];

/// Luminance weights, matching the output shader stage.
const LUMA: [f32; 3] = [0.299, 0.587, 0.114];

/// Vignette strength, matching the output shader stage.
const VIGNETTE: f32 = 0.3;

/// Warp drift clock scale, matching the warp shader stage.
const WARP_TIME_SCALE: f32 = 0.08;

/// A rendered frame kept for crossfades.
#[derive(Debug, Clone)]
struct Snapshot {
    pixels: Vec<[u8; 3]>,
    w: usize,
    h: usize,
}

/// CPU-driven Aether background for `egui`.
///
/// Owns a small pixel buffer that is re-rendered at most 30 times per
/// second, uploaded with [`egui::TextureOptions::LINEAR`], and drawn as a
/// full-rect image. See the module docs for the stage mapping.
pub struct AetherBackground {
    params: AetherParams,
    /// Gradient stops as linear 0..255 triples, brightest first.
    stops: Vec<[f32; 3]>,
    /// Bumps on every `set_palette`; latest write wins.
    generation: u64,
    degraded: bool,
    max_width: usize,
    /// Scaled animation clock in milliseconds (advanced by wall-clock delta
    /// times `animation_speed`).
    clock_ms: f64,
    /// Warp/dither clock frozen when degraded mode was entered.
    frozen_warp_ms: u64,
    /// Start of the active crossfade in `clock_ms` units, if any.
    transition_start_ms: Option<f64>,
    /// On-screen look captured by the last `note_track_change`.
    prev: Option<Snapshot>,
    /// Last frame produced by `render_to_buffer` / `paint`.
    last_rendered: RefCell<Option<Snapshot>>,
    /// Last extent produced by `paint`, used as a snapshot fallback.
    last_extent: (usize, usize),
    texture: Option<egui::TextureHandle>,
    tex_size: (usize, usize),
    dirty: bool,
    last_frame: Option<Instant>,
    last_upload: Option<Instant>,
}

impl AetherBackground {
    /// Creates a background with default parameters and no palette
    /// (neutral dark fallback until [`Self::set_palette`] is called).
    pub fn new(params: AetherParams) -> Self {
        Self {
            params,
            stops: FALLBACK_STOPS
                .iter()
                .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
                .collect(),
            generation: 0,
            degraded: false,
            max_width: DEFAULT_MAX_WIDTH,
            clock_ms: 0.0,
            frozen_warp_ms: 0,
            transition_start_ms: None,
            prev: None,
            last_rendered: RefCell::new(None),
            last_extent: (0, 0),
            texture: None,
            tex_size: (0, 0),
            dirty: true,
            last_frame: None,
            last_upload: None,
        }
    }

    /// Replaces the tuning parameters.
    pub fn set_params(&mut self, params: AetherParams) {
        self.params = params;
        self.dirty = true;
    }

    /// Installs the dominant cover colors. An empty slice selects the
    /// neutral dark fallback. Last write wins: every call supersedes all
    /// previous ones via an internal generation counter.
    pub fn set_palette(&mut self, colors: Vec<[u8; 3]>) {
        self.generation = self.generation.wrapping_add(1);
        self.stops = if colors.is_empty() {
            FALLBACK_STOPS
                .iter()
                .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
                .collect()
        } else {
            let mut sorted = colors;
            sorted.sort_by(|a, b| {
                luminance_f32(b)
                    .partial_cmp(&luminance_f32(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            sorted
                .iter()
                .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
                .collect()
        };
        self.dirty = true;
    }

    /// Starts the eased crossfade (~`transition_duration_ms`) from the
    /// frame currently on screen to the new look.
    ///
    /// The previous look is snapshotted from the last produced frame, so a
    /// stale or reordered palette update can never flash: blending always
    /// starts from what was actually displayed and ends at the latest
    /// palette (last-write-wins by generation counter).
    pub fn note_track_change(&mut self) {
        let snap = self.last_rendered.borrow().clone().or_else(|| {
            // Nothing was ever rendered (e.g. track change before first
            // paint): fall back to rendering the current look at the last
            // known extent so a later frame still has a valid snapshot.
            if self.last_extent.0 > 0 && self.last_extent.1 > 0 {
                Some(Snapshot {
                    pixels: render_pixels(
                        &self.stops,
                        &self.params,
                        self.last_extent.0,
                        self.last_extent.1,
                        self.frozen_warp_ms,
                        self.clock_ms as u64,
                    ),
                    w: self.last_extent.0,
                    h: self.last_extent.1,
                })
            } else {
                None
            }
        });
        match snap {
            Some(frame) => {
                self.prev = Some(frame);
                self.transition_start_ms = Some(self.clock_ms);
            }
            None => {
                self.prev = None;
                self.transition_start_ms = None;
            }
        }
        self.dirty = true;
    }

    /// Enables degraded mode: the buffer width is halved and warp drift is
    /// frozen. Engaged automatically under GPU pressure so playback and
    /// navigation never stutter.
    pub fn set_degraded(&mut self, degraded: bool) {
        if degraded && !self.degraded {
            self.frozen_warp_ms = self.clock_ms as u64;
        }
        self.degraded = degraded;
        self.dirty = true;
    }

    /// Returns whether degraded mode is active.
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// Overrides the default 192px buffer width cap. Clamped to
    /// `16..=512`.
    pub fn set_max_width(&mut self, width: usize) {
        self.max_width = width.clamp(MIN_WIDTH, HARD_MAX_WIDTH);
        self.dirty = true;
    }

    /// Computes the buffer extent for a rect of the given size, keeping the
    /// aspect ratio. The width is capped at `max_width` (halved while
    /// degraded) and the height is bounded so degenerate rects cannot blow
    /// up the per-frame cost. Returns `(1, 1)` for non-positive inputs.
    pub fn buffer_extent(&self, rect_w: f32, rect_h: f32) -> (usize, usize) {
        if !rect_w.is_finite() || !rect_h.is_finite() || rect_w <= 0.0 || rect_h <= 0.0 {
            return (1, 1);
        }
        let cap = if self.degraded {
            (self.max_width / 2).max(1)
        } else {
            self.max_width
        };
        let w = (rect_w.round() as usize).clamp(1, cap);
        let h = ((w as f32 * rect_h / rect_w).round() as usize).clamp(1, HARD_MAX_HEIGHT);
        (w, h)
    }

    /// Pure rendering core used by [`Self::paint`]: renders a `w × h`
    /// buffer for the given animation clock value in milliseconds.
    ///
    /// Deterministic: identical arguments and identical installed state
    /// always produce identical pixels. Blends the snapshotted previous
    /// frame over the fresh frame while a crossfade started by
    /// [`Self::note_track_change`] is in flight.
    pub fn render_to_buffer(&self, w: usize, h: usize, time_ms: u64) -> Vec<[u8; 3]> {
        if w == 0 || h == 0 {
            return Vec::new();
        }
        let warp_ms = if self.degraded {
            self.frozen_warp_ms
        } else {
            time_ms
        };
        let current = render_pixels(&self.stops, &self.params, w, h, warp_ms, time_ms);
        let blended = match (&self.prev, self.transition_progress(time_ms)) {
            (Some(prev), Some(p)) if prev.w == w && prev.h == h && p < 1.0 => {
                let e = AetherParams::eased_blend(p);
                prev.pixels
                    .iter()
                    .zip(current.iter())
                    .map(|(a, b)| mix_pixel(*a, *b, e))
                    .collect()
            }
            _ => current,
        };
        *self.last_rendered.borrow_mut() = Some(Snapshot {
            pixels: blended.clone(),
            w,
            h,
        });
        blended
    }

    /// Paints the animated background over `rect`.
    ///
    /// Advances an internal clock scaled by `animation_speed`, re-renders
    /// the small buffer at most 30 times per second, uploads it with linear
    /// filtering, and draws it as a full-rect image. Schedules a repaint so
    /// the animation keeps evolving.
    pub fn paint(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if !rect.is_positive() {
            return;
        }
        let _ = ui.allocate_rect(rect, egui::Sense::hover());

        let now = Instant::now();
        let dt_ms = match self.last_frame {
            Some(t) => now.duration_since(t).as_secs_f64() * 1000.0,
            None => 33.0,
        };
        self.last_frame = Some(now);
        let speed = if self.params.animation_speed.is_finite() {
            self.params.animation_speed.max(0.0)
        } else {
            1.0
        };
        self.clock_ms += dt_ms * speed as f64;
        let time_ms = self.clock_ms as u64;

        let (w, h) = self.buffer_extent(rect.width(), rect.height());
        self.last_extent = (w, h);

        let due = match self.last_upload {
            Some(t) => now.duration_since(t) >= Duration::from_millis(33),
            None => true,
        };
        if self.dirty || due || self.texture.is_none() || self.tex_size != (w, h) {
            let pixels = self.render_to_buffer(w, h, time_ms);
            let flat: Vec<u8> = pixels.iter().flatten().copied().collect();
            let image = egui::ColorImage::from_rgb([w, h], &flat);
            let handle =
                ui.ctx()
                    .load_texture("oxidify/aether-background", image, egui::TextureOptions::LINEAR);
            self.texture = Some(handle);
            self.tex_size = (w, h);
            self.last_upload = Some(now);
            self.dirty = false;
            // Drop a finished crossfade snapshot so it cannot leak into a
            // later, differently-sized transition.
            if self.transition_progress(time_ms).is_none_or(|p| p >= 1.0) {
                self.prev = None;
                self.transition_start_ms = None;
            }
        }

        if let Some(tex) = &self.texture {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter()
                .image(tex.id(), rect, uv, egui::Color32::WHITE);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }

    /// Crossfade progress in `0..=1` for a render-clock value, or `None`
    /// when no transition is active.
    fn transition_progress(&self, time_ms: u64) -> Option<f32> {
        let start = self.transition_start_ms?;
        if self.prev.is_none() {
            return None;
        }
        let duration = self.params.transition_duration_ms;
        if duration == 0 {
            return Some(1.0);
        }
        let elapsed = (time_ms as f64 - start).max(0.0);
        Some((elapsed / duration as f64).min(1.0) as f32)
    }
}

/// Extracts up to `max_colors` dominant colors from raw RGB pixels with
/// median-cut quantization (no dependencies).
///
/// The pixel set is boxed whole, then the box with the widest channel range
/// is repeatedly split at the median along its longest channel until
/// `max_colors` boxes exist or no box can split further. Each output color
/// is the mean of its box. Returns an empty vec when `pixels` is empty or
/// `max_colors` is zero. Order of the returned colors is unspecified.
pub fn extract_palette_rgba(pixels: &[[u8; 3]], max_colors: usize) -> Vec<[u8; 3]> {
    if pixels.is_empty() || max_colors == 0 {
        return Vec::new();
    }
    let mut boxes: Vec<Vec<[u8; 3]>> = vec![pixels.to_vec()];
    while boxes.len() < max_colors {
        let split_at = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() >= 2 && channel_ranges(b).0 > 0)
            .max_by_key(|(_, b)| (channel_ranges(b).0, b.len()))
            .map(|(i, _)| i);
        let index = match split_at {
            Some(i) => i,
            None => break,
        };
        let (_, channel) = channel_ranges(&boxes[index]);
        let mut entries = std::mem::take(&mut boxes[index]);
        entries.sort_by_key(|c| c[channel]);
        let mid = entries.len() / 2;
        let right = entries.split_off(mid);
        boxes[index] = entries;
        boxes.push(right);
    }
    boxes.iter().map(|b| mean_color(b)).collect()
}

/// Mean color of a non-empty pixel run.
fn mean_color(run: &[[u8; 3]]) -> [u8; 3] {
    debug_assert!(!run.is_empty());
    let mut sum = [0u64; 3];
    for p in run {
        sum[0] += p[0] as u64;
        sum[1] += p[1] as u64;
        sum[2] += p[2] as u64;
    }
    let n = run.len() as u64;
    [
        (sum[0] / n) as u8,
        (sum[1] / n) as u8,
        (sum[2] / n) as u8,
    ]
}

/// Longest channel index and the widest channel range of a pixel run.
fn channel_ranges(run: &[[u8; 3]]) -> (u16, usize) {
    let mut mins = [u8::MAX; 3];
    let mut maxs = [u8::MIN; 3];
    for p in run {
        for c in 0..3 {
            mins[c] = mins[c].min(p[c]);
            maxs[c] = maxs[c].max(p[c]);
        }
    }
    let ranges = [
        (maxs[0] as u16).saturating_sub(mins[0] as u16),
        (maxs[1] as u16).saturating_sub(mins[1] as u16),
        (maxs[2] as u16).saturating_sub(mins[2] as u16),
    ];
    let mut channel = 0;
    for c in 1..3 {
        if ranges[c] > ranges[channel] {
            channel = c;
        }
    }
    (ranges[channel], channel)
}

/// Rec.601 luma of an sRGB triplet in 0..255 units.
fn luminance_f32(c: &[u8; 3]) -> f32 {
    c[0] as f32 * LUMA[0] + c[1] as f32 * LUMA[1] + c[2] as f32 * LUMA[2]
}

/// Renders the full per-pixel pipeline: vertical gradient from the palette
/// stops, displaced by two drifting value-noise fields, then tint wash,
/// zoom, vignette, saturation boost, and temporal hash grain.
fn render_pixels(
    stops: &[[f32; 3]],
    params: &AetherParams,
    w: usize,
    h: usize,
    warp_ms: u64,
    dither_ms: u64,
) -> Vec<[u8; 3]> {
    debug_assert!(!stops.is_empty());
    let warp_scale = if params.warp_intensity.is_finite() {
        params.warp_intensity.max(0.0) * 0.28
    } else {
        0.0
    };
    let tint_k = params.tint_intensity.clamp(0.0, 1.0);
    let tint = [
        (params.tint_color[0].clamp(0.0, 1.0) * 255.0),
        (params.tint_color[1].clamp(0.0, 1.0) * 255.0),
        (params.tint_color[2].clamp(0.0, 1.0) * 255.0),
    ];
    let saturation = if params.saturation.is_finite() {
        params.saturation.max(0.0)
    } else {
        1.0
    };
    let zoom = if params.scale.is_finite() && params.scale > 0.0 {
        params.scale
    } else {
        1.0
    };
    let wobble = params.warp_intensity.clamp(0.0, 2.0) * 0.18;
    let drift = warp_ms as f32 / 1000.0 * WARP_TIME_SCALE;
    let dither_amp = if params.dithering.is_finite() {
        params.dithering.max(0.0) * 255.0
    } else {
        0.0
    };
    let quantum = dither_ms / 16;

    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            let v = (y as f32 + 0.5) / h as f32;
            // Output zoom about the center, clamped like the shader stage.
            let zu = ((u - 0.5) / zoom + 0.5).clamp(0.0, 1.0);
            let zv = ((v - 0.5) / zoom + 0.5).clamp(0.0, 1.0);
            // Two drifting value-noise fields.
            let n1 = value_noise(zu * 2.5 + drift * 1.7, zv * 2.5 - drift * 1.1);
            let n2 = value_noise(zu * 5.0 - drift * 1.3 + 11.0, zv * 5.0 + drift * 0.9 + 5.0);
            // Displace the vertical gradient lookup.
            let vd = (zv + (n1 - 0.5) * warp_scale + (n2 - 0.5) * warp_scale * 0.5)
                .clamp(0.0, 1.0);
            let mut rgb = sample_gradient(stops, vd);
            // Gentle luminance wobble so the warp reads in dark regions.
            let glow = 1.0 + (n1 - 0.5) * wobble;
            rgb[0] *= glow;
            rgb[1] *= glow;
            rgb[2] *= glow;
            // Tint wash.
            rgb[0] += (tint[0] - rgb[0]) * tint_k;
            rgb[1] += (tint[1] - rgb[1]) * tint_k;
            rgb[2] += (tint[2] - rgb[2]) * tint_k;
            // Vignette about the real (unzoomed) UV.
            let dx = u - 0.5;
            let dy = v - 0.5;
            let vig = 1.0 - (dx * dx + dy * dy) * VIGNETTE;
            rgb[0] *= vig;
            rgb[1] *= vig;
            rgb[2] *= vig;
            // Saturation boost about Rec.601 gray.
            let gray = rgb[0] * LUMA[0] + rgb[1] * LUMA[1] + rgb[2] * LUMA[2];
            rgb[0] += (rgb[0] - gray) * (saturation - 1.0);
            rgb[1] += (rgb[1] - gray) * (saturation - 1.0);
            rgb[2] += (rgb[2] - gray) * (saturation - 1.0);
            // Temporal hash grain.
            let grain = hash_grain(x as u32, y as u32, quantum) * dither_amp;
            out.push([
                (rgb[0] + grain).clamp(0.0, 255.0) as u8,
                (rgb[1] + grain).clamp(0.0, 255.0) as u8,
                (rgb[2] + grain).clamp(0.0, 255.0) as u8,
            ]);
        }
    }
    out
}

/// Samples a multi-stop gradient; `t = 0` is the first (brightest) stop.
fn sample_gradient(stops: &[[f32; 3]], t: f32) -> [f32; 3] {
    if stops.len() == 1 {
        return stops[0];
    }
    let scaled = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = (scaled.floor() as usize).min(stops.len() - 2);
    let f = scaled - i as f32;
    let (a, b) = (stops[i], stops[i + 1]);
    [
        a[0] + (b[0] - a[0]) * f,
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
    ]
}

/// Deterministic integer hash of a lattice point, mapped to `[0, 1)`.
fn hash_lattice(ix: i32, iy: i32) -> f32 {
    let mut h = (ix as u32)
        .wrapping_mul(0x8da6_b343)
        ^ (iy as u32).wrapping_mul(0xd816_3841);
    h ^= h >> 13;
    h = h.wrapping_mul(0xca6b_3a9d);
    h ^= h >> 16;
    h as f32 * (1.0 / 4294967296.0)
}

/// Smooth hash-based value noise with bilinear falloff between lattice
/// values. Deterministic across runs and platforms for the same inputs.
fn value_noise(x: f32, y: f32) -> f32 {
    let xi = x.floor() as i32;
    let yi = y.floor() as i32;
    let xf = x - x.floor();
    let yf = y - y.floor();
    let u = xf * xf * (3.0 - 2.0 * xf);
    let v = yf * yf * (3.0 - 2.0 * yf);
    let a = hash_lattice(xi, yi);
    let b = hash_lattice(xi + 1, yi);
    let c = hash_lattice(xi, yi + 1);
    let d = hash_lattice(xi + 1, yi + 1);
    a + (b - a) * u + (c - a) * v + (a - b - c + d) * u * v
}

/// Temporal grain in `[-0.5, 0.5)`, stable for a given time quantum and
/// pixel so frames stay deterministic.
fn hash_grain(x: u32, y: u32, quantum: u64) -> f32 {
    let mut h: u32 = x
        .wrapping_mul(0x85eb_ca6b)
        ^ y.wrapping_mul(0xc2b2_ae35)
        ^ (quantum as u32).wrapping_mul(0x27d4_eb2f)
        ^ ((quantum >> 32) as u32).wrapping_mul(0x1656_25e3);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h as f32 * (1.0 / 4294967296.0) - 0.5
}

/// Cosine-eased mix of two pixels, channels rounded to the nearest byte.
fn mix_pixel(a: [u8; 3], b: [u8; 3], eased: f32) -> [u8; 3] {
    let e = eased.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * e).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * e).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * e).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_params() -> AetherParams {
        AetherParams {
            dithering: 0.0,
            ..AetherParams::default()
        }
    }

    fn mean_channel(buf: &[[u8; 3]], channel: usize) -> f64 {
        buf.iter().map(|p| p[channel] as f64).sum::<f64>() / buf.len() as f64
    }

    #[test]
    fn palette_splits_red_green_halves() {
        let mut pixels = vec![[255u8, 0, 0]; 100];
        pixels.extend(vec![[0u8, 255, 0]; 100]);
        let palette = extract_palette_rgba(&pixels, 2);
        assert_eq!(palette.len(), 2);
        let near = |target: [u8; 3]| {
            palette.iter().any(|c| {
                c.iter()
                    .zip(target.iter())
                    .all(|(a, b)| (*a as i16 - *b as i16).abs() <= 8)
            })
        };
        assert!(near([255, 0, 0]), "missing red in {palette:?}");
        assert!(near([0, 255, 0]), "missing green in {palette:?}");
    }

    #[test]
    fn palette_edge_cases() {
        assert!(extract_palette_rgba(&[], 4).is_empty());
        assert!(extract_palette_rgba(&[[1, 2, 3]], 0).is_empty());
        let single = extract_palette_rgba(&[[10, 20, 30]; 16], 4);
        assert_eq!(single, vec![[10, 20, 30]]);
        // More boxes requested than distinct colors: still bounded.
        let few = extract_palette_rgba(&[[255, 0, 0], [0, 0, 255]], 8);
        assert!(few.len() <= 2 && !few.is_empty());
    }

    #[test]
    fn buffer_width_is_capped_and_keeps_aspect() {
        let bg = AetherBackground::new(AetherParams::default());
        assert_eq!(bg.buffer_extent(2000.0, 1000.0), (192, 96));
        assert_eq!(bg.buffer_extent(400.0, 200.0), (192, 96));
        assert_eq!(bg.buffer_extent(100.0, 50.0), (100, 50));
        assert_eq!(bg.buffer_extent(0.0, 10.0), (1, 1));
    }

    #[test]
    fn degraded_halves_buffer_width() {
        let mut bg = AetherBackground::new(AetherParams::default());
        assert!(bg.buffer_extent(2000.0, 1000.0).0 == 192);
        bg.set_degraded(true);
        assert!(bg.is_degraded());
        assert_eq!(bg.buffer_extent(2000.0, 1000.0), (96, 48));
        bg.set_degraded(false);
        assert_eq!(bg.buffer_extent(2000.0, 1000.0), (192, 96));
    }

    #[test]
    fn render_size_and_determinism() {
        let bg = AetherBackground::new(test_params());
        let a = bg.render_to_buffer(48, 24, 1234);
        let b = bg.render_to_buffer(48, 24, 1234);
        assert_eq!(a.len(), 48 * 24);
        assert_eq!(a, b);
        assert!(bg.render_to_buffer(0, 0, 0).is_empty());
    }

    #[test]
    fn render_reacts_to_palette() {
        let mut bg = AetherBackground::new(test_params());
        bg.set_palette(vec![[200, 30, 30]]);
        let red = bg.render_to_buffer(32, 16, 0);
        bg.set_palette(vec![[30, 30, 200]]);
        let blue = bg.render_to_buffer(32, 16, 0);
        assert_ne!(red, blue);
        assert!(mean_channel(&red, 0) > mean_channel(&red, 2));
        assert!(mean_channel(&blue, 2) > mean_channel(&blue, 0));
    }

    #[test]
    fn empty_palette_falls_back_to_dark() {
        let mut bg = AetherBackground::new(test_params());
        bg.set_palette(vec![]);
        let buf = bg.render_to_buffer(32, 16, 0);
        let avg = (mean_channel(&buf, 0) + mean_channel(&buf, 1) + mean_channel(&buf, 2)) / 3.0;
        assert!(avg < 64.0, "fallback should stay dark, got {avg}");
    }

    #[test]
    fn crossfade_blends_previous_into_current() {
        let mut bg = AetherBackground::new(test_params());
        bg.set_palette(vec![[220, 30, 30]]);
        let before = bg.render_to_buffer(32, 16, 0);
        bg.set_palette(vec![[30, 220, 60]]);
        bg.note_track_change();

        // t = 0: still fully the previous look.
        let start = bg.render_to_buffer(32, 16, 0);
        assert_eq!(start, before);

        // Mid-transition: a genuine blend of both endpoints.
        let duration = bg.params.transition_duration_ms;
        let mid = bg.render_to_buffer(32, 16, duration / 2);
        let mut green_only = AetherBackground::new(test_params());
        green_only.set_palette(vec![[30, 220, 60]]);
        let end_target = green_only.render_to_buffer(32, 16, duration / 2);
        assert_ne!(mid, before);
        assert_ne!(mid, end_target);
        for (m, a, b) in mid.iter().zip(before.iter()).zip(end_target.iter()).map(|((m, a), b)| (m, a, b)) {
            for c in 0..3 {
                let (lo, hi) = if a[c] <= b[c] { (a[c], b[c]) } else { (b[c], a[c]) };
                assert!(
                    (lo..=hi).contains(&m[c]),
                    "mid pixel should sit between endpoints: {m:?} vs {a:?}/{b:?}"
                );
            }
        }

        // Past the transition: fully the new look.
        let done = bg.render_to_buffer(32, 16, duration + 500);
        let end_ref = green_only.render_to_buffer(32, 16, duration + 500);
        assert_eq!(done, end_ref);
    }

    #[test]
    fn degraded_freezes_warp_drift() {
        let live = AetherBackground::new(test_params());
        assert_ne!(
            live.render_to_buffer(48, 24, 0),
            live.render_to_buffer(48, 24, 9000),
            "live warp should drift with time"
        );
        let mut cold = AetherBackground::new(test_params());
        cold.set_degraded(true);
        assert_eq!(
            cold.render_to_buffer(48, 24, 0),
            cold.render_to_buffer(48, 24, 9000),
            "degraded warp should be frozen"
        );
    }

    #[test]
    fn value_noise_stays_in_unit_range() {
        for (x, y) in [(0.0, 0.0), (3.7, -2.1), (100.25, 40.75), (-7.5, 9.9)] {
            let n = value_noise(x, y);
            assert!((0.0..=1.0).contains(&n), "noise out of range: {n}");
        }
    }
}
