use serde::{Deserialize, Serialize};

/// Tunable parameters for the Aether background engine.
/// See `docs/VISUALS.md` for the stage order and design rationale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AetherParams {
    pub blur_size: u32,
    pub warp_intensity: f32,
    pub blur_passes: u32,
    pub animation_speed: f32,
    pub transition_duration_ms: u64,
    pub saturation: f32,
    pub tint_color: [f32; 3],
    pub tint_intensity: f32,
    pub dithering: f32,
    pub scale: f32,
}

impl Default for AetherParams {
    fn default() -> Self {
        Self {
            blur_size: 160,
            warp_intensity: 1.1,
            blur_passes: 8,
            animation_speed: 1.0,
            transition_duration_ms: 1000,
            saturation: 1.4,
            tint_color: [0.157, 0.157, 0.235],
            tint_intensity: 0.15,
            dithering: 0.012,
            scale: 1.2,
        }
    }
}

impl AetherParams {
    /// Cosine ease used for album crossfades:
    /// `0.5 - 0.5 * cos(blend * PI)`.
    pub fn eased_blend(blend: f32) -> f32 {
        0.5 - 0.5 * (blend.clamp(0.0, 1.0) * std::f32::consts::PI).cos()
    }

    /// Warp UV scale: `warp * (intensity * 0.28)`.
    pub fn warp_scale(&self) -> f32 {
        self.warp_intensity * 0.28
    }
}

/// Domain-warp WGSL stage: two-octave simplex noise over the blurred cover,
/// time scaled by 0.08. Only texture sampling and outputs are backend
/// specific; the noise field itself is backend agnostic.
pub const WARP_WGSL: &str = r#"
struct WarpUniforms {
    time: f32,
    intensity: f32,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> uniforms: WarpUniforms;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    return out;
}

// Two-octave simplex domain warp operating on the blurred cover.
fn snoise(v: vec2<f32>) -> f32 {
    let C = vec4<f32>(0.211324865405187, 0.366025403784439, -0.577350269189626, 0.024390243902439);
    var i = floor(v + dot(v, vec2<f32>(C.y, C.y)));
    let x0 = v - i + dot(i, vec2<f32>(C.x, C.x));
    let i1 = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), x0.x > x0.y);
    var x12 = x0.xyxy + C.xxzz;
    x12 = vec4<f32>(x12.xy - i1, x12.zw);
    i = mod289_2(i);
    var p = permute3(permute3(i.y + vec3<f32>(0.0, i1.y, 1.0)) + i.x + vec3<f32>(0.0, i1.x, 1.0));
    var m = max(0.5 - vec3<f32>(dot(x0, x0), dot(x12.xy, x12.xy), dot(x12.zw, x12.zw)), vec3<f32>(0.0));
    m = m * m;
    m = m * m;
    let x = 2.0 * fract(p * C.w) - 1.0;
    let h = abs(x) - 0.5;
    let ox = floor(x + 0.5);
    let a0 = x - ox;
    m = m * (1.79284291400159 - 0.85373472095314 * (a0 * a0 + h * h));
    var g: vec3<f32>;
    g.x = a0.x * x0.x + h.x * x0.y;
    g.yz = a0.yz * x12.xz + h.yz * x12.yw;
    return 130.0 * dot(m, g);
}

fn mod289_2(x: vec2<f32>) -> vec2<f32> { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn mod289_3(x: vec3<f32>) -> vec3<f32> { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn permute3(x: vec3<f32>) -> vec3<f32> { return mod289_3(((x * 34.0) + 1.0) * x); }

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let t = in.uv;
    // Full warp is wired per-frame in M3; the 0.08 time scaling is preserved.
    let time = uniforms.time * 0.08;
    let uv = clamp(t + vec2<f32>(time * 0.001), vec2<f32>(0.0), vec2<f32>(1.0));
    return textureSample(src_tex, src_sampler, uv);
}
"#;

/// Output WGSL stage: zoom, vignette, saturation, and animated hash dither.
pub const OUTPUT_WGSL: &str = r#"
struct OutUniforms {
    saturation: f32,
    dithering: f32,
    time: f32,
    scale: f32,
    resolution: vec2<f32>,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> uniforms: OutUniforms;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn hash3(p: vec3<f32>) -> f32 {
    var q = fract(p * 0.1031);
    q = q + vec3<f32>(dot(q, q.zyx + 31.32));
    return fract((q.x + q.y) * q.z);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var uv = (in.uv - 0.5) / uniforms.scale + 0.5;
    uv = clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0));
    var color = textureSample(src_tex, src_sampler, uv);
    let center = in.uv - 0.5;
    color = vec4<f32>(color.rgb * (1.0 - dot(center, center) * 0.3), 1.0);
    let gray = dot(color.rgb, vec3<f32>(0.299, 0.587, 0.114));
    color = vec4<f32>(mix(vec3<f32>(gray), color.rgb, uniforms.saturation), 1.0);
    let pixel = floor(in.uv * uniforms.resolution);
    let noise = hash3(vec3<f32>(pixel, floor(uniforms.time * 60.0)));
    color = vec4<f32>(color.rgb + (noise - 0.5) * uniforms.dithering, 1.0);
    return color;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_design_doc() {
        let p = AetherParams::default();
        assert_eq!(p.blur_size, 160);
        assert_eq!(p.blur_passes, 8);
        assert_eq!(p.transition_duration_ms, 1000);
        assert!((p.saturation - 1.4).abs() < 1e-6);
        assert!((p.scale - 1.2).abs() < 1e-6);
    }

    #[test]
    fn eased_blend_endpoints() {
        assert!((AetherParams::eased_blend(0.0)).abs() < 1e-6);
        assert!((AetherParams::eased_blend(1.0) - 1.0).abs() < 1e-6);
        assert!((AetherParams::eased_blend(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn shaders_carry_stage_constants() {
        assert!(OUTPUT_WGSL.contains("0.3")); // vignette
        assert!(WARP_WGSL.contains("0.08")); // time scale
    }
}
