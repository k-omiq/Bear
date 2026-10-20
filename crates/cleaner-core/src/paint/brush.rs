//! **Vendored, not written here.** Copied verbatim from the `freeditor`
//! repository (`rust/image-kernels/src/kernels/brush.rs`, commit `36f37dc`),
//! with two mechanical edits and no algorithmic ones: the
//! `#[cfg(target_arch = "wasm32")]` ABI exports are dropped - this crate links
//! the kernels natively and `wasm32-unknown-unknown` is not even installed,
//! and the `crate::types` / `crate::memory` dependencies go with them.
//! A third, later: `analytic_radial_coverage` is `pub(crate)`, so the eraser
//! (`erase.rs`) takes coverage off with the same tip the Brush lays it on with.
//! The `#[cfg(test)]` TS-oracle parity tests are kept as they stood, because
//! they are resolution-independent and they are what keeps the kernel verified.
//!
//! Subpixel brush-dab rasterizer - byte-exact Rust port of the TypeScript
//! oracle `stampDabSubpixel` / `accumulationToImageData` in
//! `apps/web/src/brush/brushRasterizer.ts`.
//!
//! PARITY CONTRACT (why this matches the TS oracle to the byte):
//! - The TS accumulation buffer is a `Float32Array`. Every write therefore
//!   rounds to f32. We mirror that by accumulating into `&mut [f32]` and doing
//!   `(value) as f32` on each store, with all intermediate arithmetic in f64
//!   (JS `number`). Reads widen f32 -> f64 exactly as JS does.
//! - JS `Math.round` (round half toward +inf) on a NON-NEGATIVE value equals
//!   Rust `f64::round` (round half away from zero). All quantized values here
//!   are non-negative, but we use an explicit `js_round` to keep the contract
//!   obvious and robust.
//! - Trigonometry is the ONLY source of cross-runtime float divergence, so it
//!   is NOT computed here: callers pass `cos_a`/`sin_a` (= `cos(-angle)`,
//!   `sin(-angle)`) precomputed in JS, exactly like the existing motion-blur
//!   kernel keeps trig out of Rust. Everything else (sub, mul, div, `sqrt` via
//!   `hypot`) is IEEE-deterministic across V8 and Rust.
//! - The seeded scatter PRNG (`hashUint32`/`dabRandom`/`dabJitter`) is pure
//!   32-bit integer math and ports exactly with `wrapping_*` ops.

// The vendored code keeps freeditor's own shape, including the wide kernel
// signatures and its explicit clamp ladders. Linting it into a different shape
// would break the one property that makes it auditable: it is a copy.
#![allow(clippy::manual_clamp)]


const EPSILON: f64 = 1e-6;

/// JS `Math.round`: round half toward +infinity. Matches `Float32Array`-free
/// integer quantization in the TS oracle for the non-negative values used here.
fn js_round(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let floor = x.floor();
    if x - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

/// JS `ToUint32` of an already integer-valued, finite f64.
fn to_uint32(x: f64) -> u32 {
    if !x.is_finite() {
        return 0;
    }
    x.rem_euclid(4_294_967_296.0) as u32
}

fn clamp01(value: f64) -> f64 {
    if value < 0.0 {
        0.0
    } else if value > 1.0 {
        1.0
    } else {
        value
    }
}

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

// --- Seeded dynamics PRNG (mirror of brushRandom.ts) -----------------------

fn hash_uint32(value: u32) -> u32 {
    let mut z = value.wrapping_add(0x9e37_79b9);
    z = (z ^ (z >> 16)).wrapping_mul(0x21f0_aaad);
    z = (z ^ (z >> 15)).wrapping_mul(0x735a_2d97);
    z ^ (z >> 15)
}

/// `dabRandom(seed, dabIndex, 'scatter')` - only the `scatter` channel
/// (salt `0x01`) is needed by the rasterizer.
fn dab_random_scatter(seed: u32, dab_index: u32) -> f64 {
    let salt = hash_uint32((0x01u32).wrapping_mul(0x9e37_79b1));
    let mixed = hash_uint32(seed ^ hash_uint32(dab_index) ^ salt);
    mixed as f64 / 4_294_967_296.0
}

fn dab_jitter_scatter(seed: u32, dab_index: u32) -> f64 {
    dab_random_scatter(seed, dab_index) * 2.0 - 1.0
}

// --- Tip coverage (mirror of brushTip.ts) ----------------------------------

/// A brush tip's coverage source. `Analytic` covers both `circular` and
/// `elliptical` TS kinds (geometrically identical - the caller squashes coords
/// by roundness before evaluating). `Mask` covers `bitmap`/`sampled`.
#[derive(Clone, Copy)]
pub enum TipShape<'a> {
    Analytic,
    Mask {
        mask: &'a [f32],
        width: usize,
        height: usize,
    },
}

fn soft_gradient_coverage(t: f64) -> f64 {
    let x = clamp01(t);
    if x <= 0.55 {
        1.0 - (x / 0.55) * 0.45
    } else {
        0.55 * (1.0 - (x - 0.55) / 0.45)
    }
}

pub(crate) fn analytic_radial_coverage(t: f64, hardness: f64) -> f64 {
    let r = t.abs();
    if r >= 1.0 {
        return 0.0;
    }
    let h = clamp01(hardness);
    if h >= 1.0 - EPSILON {
        return 1.0;
    }
    let core = h;
    if r <= core {
        return 1.0;
    }
    let span = 1.0 - core;
    if span <= EPSILON {
        return if r < 1.0 { 1.0 } else { 0.0 };
    }
    let shell_t = (r - core) / span;
    soft_gradient_coverage(shell_t)
}

/// Bilinear sample of a single-channel f32 mask, clamp-to-edge. Mirrors
/// `sampleMaskBilinear`, including its degenerate `s10` index quirk.
fn sample_mask_bilinear(mask: &[f32], width: usize, height: usize, x: f64, y: f64) -> f64 {
    if width == 0 || height == 0 {
        return 0.0;
    }
    let cx = clamp(x, 0.0, (width - 1) as f64);
    let cy = clamp(y, 0.0, (height - 1) as f64);
    let x0 = cx.floor() as usize;
    let y0 = cy.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = cx - x0 as f64;
    let fy = cy - y0 as f64;
    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;
    let s00 = mask[y0 * width + x0] as f64;
    let s10 = mask[if y1 == y0 && x1 == x0 {
        y0 * width + x0
    } else {
        y0 * width + x1
    }] as f64;
    let s01 = mask[y1 * width + x0] as f64;
    let s11 = mask[y1 * width + x1] as f64;
    s00 * w00 + s10 * w10 + s01 * w01 + s11 * w11
}

fn evaluate_tip_coverage(tip: TipShape, nx: f64, ny: f64, hardness: f64) -> f64 {
    match tip {
        TipShape::Analytic => analytic_radial_coverage(nx.hypot(ny), hardness),
        TipShape::Mask {
            mask,
            width,
            height,
        } => {
            let u = ((clamp(nx, -1.0, 1.0) + 1.0) / 2.0) * (width as f64 - 1.0);
            let v = ((clamp(ny, -1.0, 1.0) + 1.0) / 2.0) * (height as f64 - 1.0);
            clamp01(sample_mask_bilinear(mask, width, height, u, v))
        }
    }
}

// --- Texture / dual / wet-edge hooks (mirror of brushRasterizer.ts) ---------

fn wrap_unit(v: f64) -> f64 {
    let m = ((v + 1.0) % 2.0 + 2.0) % 2.0;
    m - 1.0
}

/// Texture modulation settings. `scale_percent <= 0` => no scaling (factor 1).
#[derive(Clone, Copy)]
pub struct TextureSettings {
    pub scale_percent: f64,
    pub invert: bool,
}

fn sample_texture(
    texture: TipShape,
    nx: f64,
    ny: f64,
    settings: &TextureSettings,
) -> f64 {
    let scale = if settings.scale_percent > 0.0 {
        100.0 / settings.scale_percent
    } else {
        1.0
    };
    let sx = wrap_unit(nx * scale);
    let sy = wrap_unit(ny * scale);
    let sample = evaluate_tip_coverage(texture, sx, sy, 1.0);
    if settings.invert {
        1.0 - sample
    } else {
        sample
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum DualBlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
}

fn blend_dual(primary: f64, secondary: f64, mode: DualBlendMode) -> f64 {
    match mode {
        DualBlendMode::Multiply => primary * secondary,
        DualBlendMode::Screen => 1.0 - (1.0 - primary) * (1.0 - secondary),
        DualBlendMode::Overlay => {
            if primary < 0.5 {
                2.0 * primary * secondary
            } else {
                1.0 - 2.0 * (1.0 - primary) * (1.0 - secondary)
            }
        }
        DualBlendMode::Normal => primary,
    }
}

fn wet_edge_weight(radial_t: f64, coverage: f64) -> f64 {
    let rim_boost = 1.0 + 0.9 * radial_t * radial_t;
    rim_boost * (0.85 + 0.15 * (1.0 - coverage))
}

// --- Public dab + options ---------------------------------------------------

/// A single rasterizable dab. `seed_x`/`seed_y` default to `x`/`y` for the
/// deterministic scatter index (document-space, so cropped rasterization keeps
/// identical randomness).
#[derive(Clone, Copy)]
pub struct RasterizableDab {
    pub x: f64,
    pub y: f64,
    pub seed_x: Option<f64>,
    pub seed_y: Option<f64>,
    pub radius: f64,
    pub alpha: f64,
    /// Raw `dab.roundness` (clamped to [0.05, 1] inside). `None` => 1.0.
    pub roundness: Option<f64>,
}

/// Texture modulation hook: tip + settings, applied when `depth > 0`.
pub struct TextureHook<'a> {
    pub depth: f64,
    pub tip: TipShape<'a>,
    pub settings: TextureSettings,
}

/// Dual-brush hook: secondary tip modulates primary via `mode`, with seeded
/// scatter of `scatter_percent`% of radius.
pub struct DualHook<'a> {
    pub tip: TipShape<'a>,
    pub mode: DualBlendMode,
    pub scatter_percent: f64,
}

/// Stroke-level stamp options (mirror of `StampDabOptions`).
pub struct StampOptions<'a> {
    pub opacity: f64,
    pub hardness: f64,
    pub build_up: bool,
    pub wet_edges: bool,
    pub texture: Option<TextureHook<'a>>,
    pub dual: Option<DualHook<'a>>,
    /// `options.seed ?? 0`, used for dual-brush scatter.
    pub seed: u32,
}

impl Default for StampOptions<'_> {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            hardness: 0.0,
            build_up: false,
            wet_edges: false,
            texture: None,
            dual: None,
            seed: 0,
        }
    }
}

/// Premultiplied-alpha Float32 accumulation buffer view. `color` is row-major
/// RGBA (4 f32/pixel, RGB premultiplied), `stroke_mask` is per-pixel max stroke
/// coverage. Both must be `width*height*{4,1}` long.
pub struct AccumulationView<'a> {
    pub color: &'a mut [f32],
    pub stroke_mask: &'a mut [f32],
    pub width: usize,
    pub height: usize,
}

/// Stamp one dab into the accumulation buffer. `cos_a`/`sin_a` are
/// `cos(-angle)`/`sin(-angle)` precomputed by the caller (JS). `dab_color` is
/// straight RGB in 0..1. Returns true if any pixel was touched.
#[allow(clippy::too_many_arguments)]
pub fn stamp_dab_subpixel(
    buffer: &mut AccumulationView,
    dab: &RasterizableDab,
    tip: TipShape,
    dab_color: [f64; 3],
    options: &StampOptions,
    cos_a: f64,
    sin_a: f64,
) -> bool {
    let width = buffer.width;
    let height = buffer.height;
    let radius = dab.radius;
    let alpha = clamp01(dab.alpha);
    if radius <= 0.0 || alpha <= 0.0 || width == 0 || height == 0 {
        return false;
    }

    let opacity = clamp01(options.opacity);
    let hardness = clamp01(options.hardness);
    let build_up = options.build_up;
    let wet_edges = options.wet_edges;
    let texture_depth = options
        .texture
        .as_ref()
        .map(|t| clamp01(t.depth))
        .unwrap_or(0.0);
    let roundness = dab.roundness.unwrap_or(1.0).clamp(0.05, 1.0);

    // Dual-brush deterministic scatter offset (pixels), derived from seed.
    let mut dual_offset_x = 0.0;
    let mut dual_offset_y = 0.0;
    if let Some(dual) = options.dual.as_ref() {
        let scatter_px = (dual.scatter_percent / 100.0) * radius;
        let seed = options.seed;
        let di = to_uint32(js_round(
            dab.seed_x.unwrap_or(dab.x) * 31.0 + dab.seed_y.unwrap_or(dab.y),
        ));
        dual_offset_x = dab_jitter_scatter(seed, di) * scatter_px;
        dual_offset_y = dab_jitter_scatter(seed, di.wrapping_add(1)) * scatter_px;
    }

    let r = dab_color[0];
    let g = dab_color[1];
    let b = dab_color[2];

    let reach = radius / roundness + 1.0;
    let min_x = (dab.x - reach).floor().max(0.0) as i64;
    let min_y = (dab.y - reach).floor().max(0.0) as i64;
    let max_x = (dab.x + reach).ceil().min((width - 1) as f64) as i64;
    let max_y = (dab.y + reach).ceil().min((height - 1) as f64) as i64;
    if min_x > max_x || min_y > max_y {
        return false;
    }

    let mut touched = false;

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let dx = px as f64 - dab.x;
            let dy = py as f64 - dab.y;
            let lx = dx * cos_a - dy * sin_a;
            let ly = (dx * sin_a + dy * cos_a) / roundness;
            let nx = lx / radius;
            let ny = ly / radius;
            let radial_t = nx.hypot(ny);
            if radial_t >= 1.0 {
                continue;
            }

            let mut coverage = evaluate_tip_coverage(tip, nx, ny, hardness);
            if coverage <= 0.0 {
                continue;
            }

            if texture_depth > 0.0 {
                if let Some(texture) = options.texture.as_ref() {
                    let sample = sample_texture(texture.tip, nx, ny, &texture.settings);
                    let modulated = coverage * sample;
                    coverage = coverage * (1.0 - texture_depth) + modulated * texture_depth;
                }
            }

            if let Some(dual) = options.dual.as_ref() {
                let sdx = px as f64 - (dab.x + dual_offset_x);
                let sdy = py as f64 - (dab.y + dual_offset_y);
                let slx = sdx * cos_a - sdy * sin_a;
                let sly = (sdx * sin_a + sdy * cos_a) / roundness;
                let secondary =
                    evaluate_tip_coverage(dual.tip, slx / radius, sly / radius, hardness);
                coverage = blend_dual(coverage, secondary, dual.mode);
            }

            if wet_edges {
                coverage = clamp01(coverage * wet_edge_weight(radial_t, coverage));
            }

            let dab_alpha = clamp01(coverage * alpha);
            if dab_alpha <= 0.0 {
                continue;
            }

            let idx = py as usize * width + px as usize;
            let prev_mask = buffer.stroke_mask[idx] as f64;
            let effective_alpha = if build_up {
                let target = prev_mask + (opacity - prev_mask) * dab_alpha;
                (target - prev_mask).max(0.0)
            } else {
                let target = opacity.min(prev_mask.max(dab_alpha));
                (target - prev_mask).max(0.0)
            };

            if effective_alpha <= EPSILON {
                if !build_up {
                    let new_mask = opacity.min(prev_mask.max(dab_alpha));
                    buffer.stroke_mask[idx] = new_mask as f32;
                }
                continue;
            }

            let ci = idx * 4;
            let dst_r = buffer.color[ci] as f64;
            let dst_g = buffer.color[ci + 1] as f64;
            let dst_b = buffer.color[ci + 2] as f64;
            let dst_a = buffer.color[ci + 3] as f64;
            let sa = effective_alpha;
            let inv = 1.0 - sa;
            buffer.color[ci] = (r * sa + dst_r * inv) as f32;
            buffer.color[ci + 1] = (g * sa + dst_g * inv) as f32;
            buffer.color[ci + 2] = (b * sa + dst_b * inv) as f32;
            buffer.color[ci + 3] = (sa + dst_a * inv) as f32;

            buffer.stroke_mask[idx] = if build_up {
                opacity.min(prev_mask + effective_alpha) as f32
            } else {
                opacity.min(prev_mask.max(dab_alpha)) as f32
            };

            touched = true;
        }
    }

    touched
}

/// Un-premultiply the accumulation color buffer to straight RGBA bytes. Mirror
/// of `accumulationToImageData`.
pub fn accumulation_to_image_data(color: &[f32], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * height * 4];
    for i in 0..(width * height) {
        let ci = i * 4;
        let a = clamp01(color[ci + 3] as f64);
        if a <= EPSILON {
            // out already zeroed
            continue;
        }
        out[ci] = js_round(clamp01(color[ci] as f64 / a) * 255.0) as u8;
        out[ci + 1] = js_round(clamp01(color[ci + 1] as f64 / a) * 255.0) as u8;
        out[ci + 2] = js_round(clamp01(color[ci + 2] as f64 / a) * 255.0) as u8;
        out[ci + 3] = js_round(a * 255.0) as u8;
    }
    out
}

// --- WASM ABI export --------------------------------------------------------
//
// Whole-stroke batch entry point mirroring the TS `rasterizeDabsToImageData`
// live call site (one call per stroke commit). The kernel owns the f32
// accumulation buffer internally; the caller packs a flat dab descriptor and
// option/tip metadata, exactly as `imageKernelFallback.ts` does for blur.

/// Per-dab f64 descriptor stride. Layout:
/// `[x, y, seed_x, seed_y, radius, alpha, roundness, cos_a, sin_a, r, g, b]`.
/// `cos_a`/`sin_a` are `cos(-angle)`/`sin(-angle)` precomputed in JS (no trig in
/// Rust). `seed_x`/`seed_y` are pre-resolved (`dab.seedX ?? dab.x`). `r,g,b` are
/// the per-dab straight color (0..1), pre-resolved (`dab.color ?? strokeColor`).
#[cfg(test)]
mod tests {
    use super::*;

    // cos(-angle)/sin(-angle) f64 bit patterns captured from V8 (Math.cos/sin)
    // by scripts/genBrushDabFixtures.mts, fed in to isolate trig from parity.
    const ANGLE0_COS: u64 = 0x3ff0_0000_0000_0000; // 1.0
    const ANGLE0_SIN: u64 = 0x8000_0000_0000_0000; // -0.0
    const ANGLE30_COS: u64 = 0x3feb_b67a_e858_4cab;
    const ANGLE30_SIN: u64 = 0xbfdf_ffff_ffff_ffff;

    struct DabSpec<'a> {
        dab: RasterizableDab,
        tip: TipShape<'a>,
        color: [f64; 3],
    }

    fn run_scenario(
        width: usize,
        height: usize,
        dabs: &[DabSpec],
        options: &StampOptions,
        cos_a: f64,
        sin_a: f64,
    ) -> Vec<u8> {
        let mut color = vec![0f32; width * height * 4];
        let mut stroke_mask = vec![0f32; width * height];
        let mut view = AccumulationView {
            color: &mut color,
            stroke_mask: &mut stroke_mask,
            width,
            height,
        };
        for spec in dabs {
            stamp_dab_subpixel(&mut view, &spec.dab, spec.tip, spec.color, options, cos_a, sin_a);
        }
        accumulation_to_image_data(&color, width, height)
    }

    fn dab(x: f64, y: f64, radius: f64, alpha: f64) -> RasterizableDab {
        RasterizableDab {
            x,
            y,
            seed_x: None,
            seed_y: None,
            radius,
            alpha,
            roundness: None,
        }
    }

    #[test]
    fn s1_circular_soft_matches_ts_oracle() {
        let got = run_scenario(
            9,
            9,
            &[DabSpec {
                dab: dab(4.3, 4.6, 3.5, 1.0),
                tip: TipShape::Analytic,
                color: [1.0, 1.0, 1.0],
            }],
            &StampOptions {
                opacity: 1.0,
                hardness: 0.0,
                ..Default::default()
            },
            f64::from_bits(ANGLE0_COS),
            f64::from_bits(ANGLE0_SIN),
        );
        assert_eq!(got, S1);
    }

    #[test]
    fn s2_circular_hard_matches_ts_oracle() {
        let got = run_scenario(
            9,
            9,
            &[DabSpec {
                dab: dab(4.0, 4.0, 3.0, 1.0),
                tip: TipShape::Analytic,
                color: [0.9, 0.1, 0.2],
            }],
            &StampOptions {
                opacity: 1.0,
                hardness: 0.6,
                ..Default::default()
            },
            f64::from_bits(ANGLE0_COS),
            f64::from_bits(ANGLE0_SIN),
        );
        assert_eq!(got, S2);
    }

    #[test]
    fn s3_elliptical_rotated_matches_ts_oracle() {
        let mut d = dab(5.5, 5.5, 4.0, 0.9);
        d.roundness = Some(0.5);
        let got = run_scenario(
            11,
            11,
            &[DabSpec {
                dab: d,
                tip: TipShape::Analytic,
                color: [0.2, 0.4, 0.8],
            }],
            &StampOptions {
                opacity: 1.0,
                hardness: 0.2,
                ..Default::default()
            },
            f64::from_bits(ANGLE30_COS),
            f64::from_bits(ANGLE30_SIN),
        );
        assert_eq!(got, S3);
    }

    #[test]
    fn s4_opacity_cap_no_buildup_matches_ts_oracle() {
        let got = run_scenario(
            9,
            9,
            &[
                DabSpec {
                    dab: dab(4.0, 4.0, 3.0, 0.8),
                    tip: TipShape::Analytic,
                    color: [1.0, 0.0, 0.0],
                },
                DabSpec {
                    dab: dab(5.0, 4.0, 3.0, 0.8),
                    tip: TipShape::Analytic,
                    color: [1.0, 0.0, 0.0],
                },
            ],
            &StampOptions {
                opacity: 0.5,
                hardness: 0.3,
                build_up: false,
                ..Default::default()
            },
            f64::from_bits(ANGLE0_COS),
            f64::from_bits(ANGLE0_SIN),
        );
        assert_eq!(got, S4);
    }

    #[test]
    fn s5_buildup_airbrush_matches_ts_oracle() {
        let got = run_scenario(
            9,
            9,
            &[
                DabSpec {
                    dab: dab(4.0, 4.0, 3.0, 0.5),
                    tip: TipShape::Analytic,
                    color: [1.0, 0.0, 0.0],
                },
                DabSpec {
                    dab: dab(4.0, 4.0, 3.0, 0.5),
                    tip: TipShape::Analytic,
                    color: [1.0, 0.0, 0.0],
                },
                DabSpec {
                    dab: dab(4.0, 4.0, 3.0, 0.5),
                    tip: TipShape::Analytic,
                    color: [1.0, 0.0, 0.0],
                },
            ],
            &StampOptions {
                opacity: 0.9,
                hardness: 0.3,
                build_up: true,
                ..Default::default()
            },
            f64::from_bits(ANGLE0_COS),
            f64::from_bits(ANGLE0_SIN),
        );
        assert_eq!(got, S5);
    }

    #[test]
    fn s6_wet_edges_matches_ts_oracle() {
        let got = run_scenario(
            11,
            11,
            &[DabSpec {
                dab: dab(5.5, 5.5, 4.5, 0.85),
                tip: TipShape::Analytic,
                color: [0.1, 0.6, 0.3],
            }],
            &StampOptions {
                opacity: 1.0,
                hardness: 0.0,
                wet_edges: true,
                ..Default::default()
            },
            f64::from_bits(ANGLE0_COS),
            f64::from_bits(ANGLE0_SIN),
        );
        assert_eq!(got, S6);
    }

    #[test]
    fn s7_texture_matches_ts_oracle() {
        let texture_mask: [f32; 4] = [1.0, 0.2, 0.3, 0.9];
        let got = run_scenario(
            11,
            11,
            &[DabSpec {
                dab: dab(5.5, 5.5, 4.5, 1.0),
                tip: TipShape::Analytic,
                color: [0.8, 0.7, 0.2],
            }],
            &StampOptions {
                opacity: 1.0,
                hardness: 0.1,
                texture: Some(TextureHook {
                    depth: 0.7,
                    tip: TipShape::Mask {
                        mask: &texture_mask,
                        width: 2,
                        height: 2,
                    },
                    settings: TextureSettings {
                        scale_percent: 150.0,
                        invert: true,
                    },
                }),
                ..Default::default()
            },
            f64::from_bits(ANGLE0_COS),
            f64::from_bits(ANGLE0_SIN),
        );
        assert_eq!(got, S7);
    }

    #[test]
    fn s8_dual_brush_matches_ts_oracle() {
        let mut d = dab(5.5, 5.5, 4.0, 1.0);
        d.seed_x = Some(12.0);
        d.seed_y = Some(7.0);
        let got = run_scenario(
            11,
            11,
            &[DabSpec {
                dab: d,
                tip: TipShape::Analytic,
                color: [0.3, 0.3, 0.9],
            }],
            &StampOptions {
                opacity: 1.0,
                hardness: 0.2,
                dual: Some(DualHook {
                    tip: TipShape::Analytic,
                    mode: DualBlendMode::Multiply,
                    scatter_percent: 50.0,
                }),
                seed: 12345,
                ..Default::default()
            },
            f64::from_bits(ANGLE0_COS),
            f64::from_bits(ANGLE0_SIN),
        );
        assert_eq!(got, S8);
    }

    #[test]
    fn s9_bitmap_tip_matches_ts_oracle() {
        let mask: [f32; 9] = [0.1, 0.5, 0.1, 0.5, 1.0, 0.5, 0.1, 0.5, 0.1];
        let got = run_scenario(
            9,
            9,
            &[DabSpec {
                dab: dab(4.5, 4.5, 3.5, 1.0),
                tip: TipShape::Mask {
                    mask: &mask,
                    width: 3,
                    height: 3,
                },
                color: [1.0, 1.0, 1.0],
            }],
            &StampOptions {
                opacity: 1.0,
                hardness: 0.0,
                ..Default::default()
            },
            f64::from_bits(ANGLE0_COS),
            f64::from_bits(ANGLE0_SIN),
        );
        assert_eq!(got, S9);
    }

    #[test]
    fn s10_per_dab_color_matches_ts_oracle() {
        let got = run_scenario(
            9,
            9,
            &[
                DabSpec {
                    dab: dab(4.0, 4.0, 2.5, 0.9),
                    tip: TipShape::Analytic,
                    color: [1.0, 0.0, 0.0],
                },
                DabSpec {
                    dab: dab(5.0, 5.0, 2.5, 0.9),
                    tip: TipShape::Analytic,
                    color: [0.0, 0.0, 1.0],
                },
            ],
            &StampOptions {
                opacity: 1.0,
                hardness: 0.4,
                ..Default::default()
            },
            f64::from_bits(ANGLE0_COS),
            f64::from_bits(ANGLE0_SIN),
        );
        assert_eq!(got, S10);
    }

    #[test]
    fn blend_dual_modes_match_ts_contract() {
        // Mirrors blendDual() in brushRasterizer.ts for every mode.
        assert_eq!(blend_dual(0.4, 0.5, DualBlendMode::Normal), 0.4);
        assert_eq!(blend_dual(0.4, 0.5, DualBlendMode::Multiply), 0.2);
        assert!((blend_dual(0.4, 0.5, DualBlendMode::Screen) - 0.7).abs() < 1e-12);
        // overlay, primary < 0.5 => 2*p*s
        assert!((blend_dual(0.4, 0.5, DualBlendMode::Overlay) - 0.4).abs() < 1e-12);
        // overlay, primary >= 0.5 => 1 - 2*(1-p)*(1-s)
        assert!((blend_dual(0.6, 0.5, DualBlendMode::Overlay) - 0.6).abs() < 1e-12);
    }

    #[test]
    fn integer_prng_matches_ts_contract() {
        // hashUint32(0) reference value computed from brushRandom.ts.
        assert_eq!(hash_uint32(0), 1_684_164_658);
        // dabJitterScatter(12345, 379) reference value from the TS PRNG.
        let j = dab_jitter_scatter(12345, 379);
        assert!((j - (-0.210_002_814_419_567_58)).abs() < 1e-15);
    }

    // --- Pinned TS-oracle outputs (scripts/genBrushDabFixtures.mts) ---------

    const S1: [u8; 324] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        255, 255, 255, 3, 255, 255, 255, 53, 255, 255, 255, 79, 255, 255, 255, 72, 255, 255, 255, 35,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 62,
        255, 255, 255, 128, 255, 255, 255, 158, 255, 255, 255, 151, 255, 255, 255, 104, 255, 255, 255, 32,
        0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 13, 255, 255, 255, 100, 255, 255, 255, 170,
        255, 255, 255, 215, 255, 255, 255, 200, 255, 255, 255, 148, 255, 255, 255, 65, 0, 0, 0, 0,
        0, 0, 0, 0, 255, 255, 255, 16, 255, 255, 255, 104, 255, 255, 255, 174, 255, 255, 255, 225,
        255, 255, 255, 207, 255, 255, 255, 151, 255, 255, 255, 69, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 255, 255, 255, 72, 255, 255, 255, 141, 255, 255, 255, 170, 255, 255, 255, 162,
        255, 255, 255, 116, 255, 255, 255, 41, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        255, 255, 255, 16, 255, 255, 255, 69, 255, 255, 255, 96, 255, 255, 255, 89, 255, 255, 255, 50,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 255, 255, 255, 8, 255, 255, 255, 3, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    const S2: [u8; 324] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        230, 25, 51, 45, 230, 26, 51, 179, 229, 25, 51, 220, 230, 26, 51, 179, 230, 25, 51, 45,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 230, 26, 51, 179,
        229, 26, 51, 255, 229, 26, 51, 255, 229, 26, 51, 255, 230, 26, 51, 179, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 229, 25, 51, 220, 229, 26, 51, 255,
        229, 26, 51, 255, 229, 26, 51, 255, 229, 25, 51, 220, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 230, 26, 51, 179, 229, 26, 51, 255, 229, 26, 51, 255,
        229, 26, 51, 255, 230, 26, 51, 179, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 230, 25, 51, 45, 230, 26, 51, 179, 229, 25, 51, 220, 230, 26, 51, 179,
        230, 25, 51, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    const S3: [u8; 484] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 51, 102, 204, 11, 51, 102, 204, 17, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 51, 102, 204, 10, 51, 102, 204, 95, 51, 102, 204, 140, 51, 102, 204, 135,
        51, 102, 204, 78, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 51, 102, 204, 95, 51, 102, 204, 178,
        51, 102, 204, 229, 51, 102, 204, 196, 51, 102, 204, 124, 51, 102, 204, 11, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 51, 102, 204, 11,
        51, 102, 204, 124, 51, 102, 204, 196, 51, 102, 204, 229, 51, 102, 204, 178, 51, 102, 204, 95,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 51, 102, 204, 78, 51, 102, 204, 135, 51, 102, 204, 140,
        51, 102, 204, 95, 51, 102, 204, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        51, 102, 204, 17, 51, 102, 204, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    const S4: [u8; 324] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        255, 0, 0, 20, 255, 0, 0, 91, 255, 0, 0, 117, 255, 0, 0, 107, 255, 0, 0, 85,
        255, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 91,
        255, 0, 0, 128, 255, 0, 0, 128, 255, 0, 0, 128, 255, 0, 0, 114, 255, 0, 0, 91,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 117, 255, 0, 0, 128,
        255, 0, 0, 128, 255, 0, 0, 128, 255, 0, 0, 123, 255, 0, 0, 117, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 91, 255, 0, 0, 128, 255, 0, 0, 128,
        255, 0, 0, 128, 255, 0, 0, 114, 255, 0, 0, 91, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 255, 0, 0, 20, 255, 0, 0, 91, 255, 0, 0, 117, 255, 0, 0, 107,
        255, 0, 0, 85, 255, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    const S5: [u8; 324] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        255, 0, 0, 31, 255, 0, 0, 104, 255, 0, 0, 121, 255, 0, 0, 104, 255, 0, 0, 31,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 104,
        255, 0, 0, 144, 255, 0, 0, 156, 255, 0, 0, 144, 255, 0, 0, 104, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 121, 255, 0, 0, 156,
        255, 0, 0, 159, 255, 0, 0, 156, 255, 0, 0, 121, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 104, 255, 0, 0, 144, 255, 0, 0, 156,
        255, 0, 0, 144, 255, 0, 0, 104, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 255, 0, 0, 31, 255, 0, 0, 104, 255, 0, 0, 121, 255, 0, 0, 104,
        255, 0, 0, 31, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    const S6: [u8; 484] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        26, 153, 76, 21, 25, 153, 76, 65, 26, 153, 77, 85, 26, 153, 77, 85, 25, 153, 76, 65,
        26, 153, 76, 21, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        26, 153, 76, 21, 26, 153, 77, 85, 26, 153, 77, 120, 26, 153, 77, 136, 26, 153, 77, 136,
        26, 153, 77, 120, 26, 153, 77, 85, 26, 153, 76, 21, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 25, 153, 76, 65, 26, 153, 77, 120, 25, 153, 76, 145, 25, 153, 76, 153,
        25, 153, 76, 153, 25, 153, 76, 145, 26, 153, 77, 120, 25, 153, 76, 65, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 26, 153, 77, 85, 26, 153, 77, 136, 25, 153, 76, 153,
        26, 153, 76, 168, 26, 153, 76, 168, 25, 153, 76, 153, 26, 153, 77, 136, 26, 153, 77, 85,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 26, 153, 77, 85, 26, 153, 77, 136,
        25, 153, 76, 153, 26, 153, 76, 168, 26, 153, 76, 168, 25, 153, 76, 153, 26, 153, 77, 136,
        26, 153, 77, 85, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 25, 153, 76, 65,
        26, 153, 77, 120, 25, 153, 76, 145, 25, 153, 76, 153, 25, 153, 76, 153, 25, 153, 76, 145,
        26, 153, 77, 120, 25, 153, 76, 65, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        26, 153, 76, 21, 26, 153, 77, 85, 26, 153, 77, 120, 26, 153, 77, 136, 26, 153, 77, 136,
        26, 153, 77, 120, 26, 153, 77, 85, 26, 153, 76, 21, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 26, 153, 76, 21, 25, 153, 76, 65, 26, 153, 77, 85,
        26, 153, 77, 85, 25, 153, 76, 65, 26, 153, 76, 21, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    const S7: [u8; 484] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        204, 179, 51, 8, 204, 178, 51, 29, 204, 178, 51, 42, 204, 179, 51, 44, 204, 178, 51, 33,
        204, 179, 51, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        204, 179, 51, 8, 204, 179, 51, 40, 204, 179, 51, 67, 204, 178, 51, 84, 204, 179, 51, 87,
        204, 179, 51, 74, 204, 178, 51, 47, 204, 179, 51, 10, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 204, 178, 51, 28, 204, 179, 51, 67, 204, 178, 51, 95, 204, 179, 51, 113,
        204, 179, 51, 115, 204, 178, 51, 101, 204, 179, 51, 75, 204, 179, 51, 33, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 204, 179, 51, 41, 204, 178, 51, 82, 204, 179, 51, 112,
        204, 179, 51, 139, 204, 179, 51, 141, 204, 179, 51, 116, 204, 178, 51, 88, 204, 178, 51, 45,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 204, 179, 51, 42, 204, 178, 51, 84,
        204, 179, 51, 113, 204, 178, 51, 140, 204, 178, 51, 141, 204, 178, 51, 115, 204, 179, 51, 86,
        204, 179, 51, 44, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 204, 178, 51, 31,
        204, 178, 51, 72, 204, 179, 51, 99, 204, 178, 51, 114, 204, 179, 51, 114, 204, 179, 51, 97,
        204, 178, 51, 70, 204, 178, 51, 30, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        204, 179, 51, 9, 204, 178, 51, 45, 204, 178, 51, 72, 204, 178, 51, 86, 204, 178, 51, 85,
        204, 178, 51, 69, 204, 179, 51, 42, 204, 179, 51, 8, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 204, 178, 51, 9, 204, 179, 51, 32, 204, 179, 51, 44,
        204, 178, 51, 43, 204, 178, 51, 30, 204, 179, 51, 8, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    const S8: [u8; 484] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 77, 77, 230, 6, 76, 76, 229, 32, 76, 76, 229, 53, 76, 76, 229, 46,
        76, 76, 229, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 76, 76, 229, 2, 77, 77, 229, 45, 76, 76, 229, 105, 76, 76, 229, 141,
        76, 76, 229, 131, 77, 77, 229, 79, 77, 77, 230, 18, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 77, 77, 230, 13, 77, 77, 230, 86, 76, 76, 229, 169,
        77, 77, 229, 241, 76, 76, 229, 218, 77, 77, 230, 133, 77, 77, 230, 49, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 76, 76, 229, 16, 77, 77, 230, 95,
        77, 77, 229, 189, 77, 77, 229, 255, 76, 76, 230, 247, 77, 77, 230, 146, 76, 76, 230, 58,
        77, 77, 230, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 77, 77, 229, 5,
        77, 77, 230, 65, 77, 77, 230, 140, 76, 76, 230, 194, 76, 76, 229, 175, 76, 76, 229, 110,
        77, 77, 230, 37, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 76, 76, 229, 19, 76, 76, 229, 66, 77, 77, 230, 98, 76, 76, 229, 91,
        77, 77, 230, 50, 77, 77, 230, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 77, 77, 230, 6, 77, 77, 230, 17,
        76, 76, 229, 15, 77, 77, 229, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    const S9: [u8; 324] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 255, 255, 255, 117, 255, 255, 255, 148, 255, 255, 255, 148, 255, 255, 255, 117,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 117,
        255, 255, 255, 150, 255, 255, 255, 184, 255, 255, 255, 184, 255, 255, 255, 150, 255, 255, 255, 117,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 148, 255, 255, 255, 184,
        255, 255, 255, 219, 255, 255, 255, 219, 255, 255, 255, 184, 255, 255, 255, 148, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 148, 255, 255, 255, 184, 255, 255, 255, 219,
        255, 255, 255, 219, 255, 255, 255, 184, 255, 255, 255, 148, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 255, 255, 255, 117, 255, 255, 255, 150, 255, 255, 255, 184, 255, 255, 255, 184,
        255, 255, 255, 150, 255, 255, 255, 117, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 255, 255, 255, 117, 255, 255, 255, 148, 255, 255, 255, 148, 255, 255, 255, 117,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    const S10: [u8; 324] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 255, 0, 0, 49, 255, 0, 0, 94, 255, 0, 0, 49, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 49,
        255, 0, 0, 178, 255, 0, 0, 229, 255, 0, 0, 178, 255, 0, 0, 49, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 94, 255, 0, 0, 229,
        255, 0, 0, 229, 255, 0, 0, 229, 109, 0, 146, 147, 0, 0, 255, 49, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 49, 255, 0, 0, 178, 255, 0, 0, 229,
        187, 0, 68, 193, 19, 0, 236, 195, 0, 0, 255, 94, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 49, 109, 0, 146, 147, 19, 0, 236, 195,
        0, 0, 255, 178, 0, 0, 255, 49, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 49, 0, 0, 255, 94, 0, 0, 255, 49,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];
}
