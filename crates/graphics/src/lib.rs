//! Aether animated backgrounds for Oxidify.
//!
//! Stage order: tint → multi-pass blur at small resolution → eased
//! crossfade on cover change → simplex domain warp → output grade (zoom,
//! vignette, saturation, animated dither). See `docs/VISUALS.md`.

pub mod aether;
pub mod background;

pub use aether::{AetherParams, OUTPUT_WGSL, WARP_WGSL};
pub use background::{AetherBackground, DEFAULT_MAX_WIDTH, extract_palette_rgba};
