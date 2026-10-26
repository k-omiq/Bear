//! The Gradient tool's arithmetic: evaluate a gradient, fit one to the pixels
//! around a selection, and dither it.
//!
//! The five types are Photoshop's. A gradient is a drag from `start` to `end`
//! and two lists of stops along it, colour and opacity, each at a position
//! from 0 (start) to 1 (end), as in Photoshop's Gradient Editor:
//!
//! - **Linear**: position along the drag.
//! - **Radial**: distance from `start`, the drag's length being 1.
//! - **Angle**: the angle around `start`, counter-clockwise from the drag.
//! - **Reflected**: linear, mirrored about `start`.
//! - **Diamond**: a square around `start` with a corner at `end` (the L1
//!   distance in the drag's frame).
//!
//! [`fit`] is the tool's **Match surroundings**: it fits a linear and a radial
//! model to the ring around the selection and keeps the one with the lower
//! error, with its two colour stops read off the fit. Video 2 of the plan (a
//! dark-red to black vertical gradient eyedropped by hand) is one click.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Linear,
    Radial,
    Angle,
    Reflected,
    Diamond,
}

/// A colour stop: position 0..=1, colour in 0..=255 per RGB channel.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColorStop {
    pub at: f64,
    pub color: [f64; 3],
}

/// An opacity stop: position 0..=1, opacity 0..=1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OpacityStop {
    pub at: f64,
    pub opacity: f64,
}

/// One gradient, in page pixels. Serialised into the layer's parameters so it
/// can be reopened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gradient {
    pub kind: Kind,
    pub start: [f64; 2],
    pub end: [f64; 2],
    pub colors: Vec<ColorStop>,
    pub opacities: Vec<OpacityStop>,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default = "yes")]
    pub dither: bool,
}

fn yes() -> bool {
    true
}

impl Gradient {
    /// Position 0..=1 of page point `(x, y)` along the gradient.
    pub fn position(&self, x: f64, y: f64) -> f64 {
        let (dx, dy) = (self.end[0] - self.start[0], self.end[1] - self.start[1]);
        let len2 = (dx * dx + dy * dy).max(1e-9);
        let len = len2.sqrt();
        let (px, py) = (x - self.start[0], y - self.start[1]);
        let along = (px * dx + py * dy) / len; // px along the drag
        let across = (px * dy - py * dx) / len; // px across it
        let t = match self.kind {
            Kind::Linear => along / len,
            Kind::Reflected => along.abs() / len,
            Kind::Radial => (px * px + py * py).sqrt() / len,
            Kind::Diamond => (along.abs() + across.abs()) / len,
            Kind::Angle => {
                // counter-clockwise on screen (y points down): `across` is
                // positive for a point above a rightward drag
                let a = across.atan2(along);
                a.rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU
            }
        };
        let t = t.clamp(0.0, 1.0);
        if self.reverse { 1.0 - t } else { t }
    }

    /// Colour (0..=255 per channel) and opacity (0..=1) at position `t`.
    pub fn at(&self, t: f64) -> ([f64; 3], f64) {
        let color = interpolate(&self.colors.iter().map(|s| (s.at, s.color)).collect::<Vec<_>>(), t, [0.0; 3]);
        let alpha = interpolate(&self.opacities.iter().map(|s| (s.at, [s.opacity])).collect::<Vec<_>>(), t, [1.0])[0];
        (color, alpha)
    }
}

fn interpolate<const N: usize>(stops: &[(f64, [f64; N])], t: f64, empty: [f64; N]) -> [f64; N] {
    if stops.is_empty() {
        return empty;
    }
    let mut sorted = stops.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    if t <= sorted[0].0 {
        return sorted[0].1;
    }
    for pair in sorted.windows(2) {
        let ((a, ca), (b, cb)) = (pair[0], pair[1]);
        if t <= b {
            let f = if b > a { (t - a) / (b - a) } else { 1.0 };
            let mut out = [0.0; N];
            for k in 0..N {
                out[k] = ca[k] + (cb[k] - ca[k]) * f;
            }
            return out;
        }
    }
    sorted.last().unwrap().1
}

/// Triangular dither noise in -1..1 levels for a pixel, repeatable per position.
pub fn dither_noise(x: i64, y: i64) -> f64 {
    let h = |v: u64| {
        let mut z = v.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    };
    let seed = (x as u64).wrapping_mul(0x1_0000_0001) ^ (y as u64).wrapping_mul(0x9E37_79B1);
    h(seed) + h(seed ^ 0xA5A5_A5A5) - 1.0
}

/// One pixel of the gradient: colour (0..=255, dithered when asked) and opacity.
pub fn shade(g: &Gradient, x: i64, y: i64) -> ([f64; 3], f64) {
    let (mut c, a) = g.at(g.position(x as f64 + 0.5, y as f64 + 0.5));
    if g.dither {
        let n = dither_noise(x, y) * 0.5;
        c.iter_mut().for_each(|v| *v = (*v + n).clamp(0.0, 255.0));
    }
    (c, a)
}

fn solve(ata: &mut [[f64; 3]; 3], atb: &mut [f64; 3]) -> Option<[f64; 3]> {
    // Gaussian elimination with partial pivoting
    let mut m = [[0.0; 4]; 3];
    for i in 0..3 {
        m[i][..3].copy_from_slice(&ata[i]);
        m[i][3] = atb[i];
    }
    for c in 0..3 {
        let p = (c..3).max_by(|&a, &b| m[a][c].abs().total_cmp(&m[b][c].abs()))?;
        if m[p][c].abs() < 1e-12 {
            return None;
        }
        m.swap(c, p);
        for r in 0..3 {
            if r != c {
                let f = m[r][c] / m[c][c];
                let pivot = m[c];
                for (v, p) in m[r].iter_mut().zip(pivot).skip(c) {
                    *v -= f * p;
                }
            }
        }
    }
    Some([m[0][3] / m[0][0], m[1][3] / m[1][1], m[2][3] / m[2][2]])
}

/// Least squares `v ~ a + b*u` over pairs, as `(a, b, mean squared error)`.
fn line_fit(us: &[f64], vs: &[f64]) -> (f64, f64, f64) {
    let n = us.len() as f64;
    let (mu, mv) = (us.iter().sum::<f64>() / n, vs.iter().sum::<f64>() / n);
    let (mut suu, mut suv) = (0.0, 0.0);
    for (u, v) in us.iter().zip(vs) {
        suu += (u - mu) * (u - mu);
        suv += (u - mu) * (v - mv);
    }
    let b = if suu > 1e-12 { suv / suu } else { 0.0 };
    let a = mv - b * mu;
    let mse = us.iter().zip(vs).map(|(u, v)| (v - a - b * u).powi(2)).sum::<f64>() / n;
    (a, b, mse)
}

/// **Match surroundings.** `samples` are the ring's pixels as
/// `(x, y, [r, g, b])` in page pixels, `selection` the selection's bounds as
/// `(x0, y0, x1, y1)`. Fits a plane to the luma (Linear, drag along its slope
/// across the selection) and a radial line about the luma's fitted extremum
/// (Radial), and returns the one with the lower error, its stops read off
/// per-channel line fits along the gradient's own position. `None` with fewer
/// than 16 samples.
pub fn fit(samples: &[(f64, f64, [f64; 3])], selection: (f64, f64, f64, f64)) -> Option<Gradient> {
    if samples.len() < 16 {
        return None;
    }
    let luma = |c: &[f64; 3]| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
    let (cx, cy) = ((selection.0 + selection.2) / 2.0, (selection.1 + selection.3) / 2.0);
    // plane: l = a + b*(x-cx) + c*(y-cy)
    let mut ata = [[0.0; 3]; 3];
    let mut atb = [0.0; 3];
    for (x, y, c) in samples {
        let row = [1.0, x - cx, y - cy];
        for i in 0..3 {
            for j in 0..3 {
                ata[i][j] += row[i] * row[j];
            }
            atb[i] += row[i] * luma(c);
        }
    }
    let plane = solve(&mut ata, &mut atb)?;
    let (gx, gy) = (plane[1], plane[2]);
    let slope = (gx * gx + gy * gy).sqrt();
    let corners = [(selection.0, selection.1), (selection.2, selection.1), (selection.0, selection.3), (selection.2, selection.3)];
    // a flat surround has no slope to follow; it is still a (flat) linear fit
    let linear = {
        let (ux, uy) = if slope > 1e-6 { (gx / slope, gy / slope) } else { (1.0, 0.0) };
        let proj = |x: f64, y: f64| (x - cx) * ux + (y - cy) * uy;
        let lo = corners.iter().map(|&(x, y)| proj(x, y)).fold(f64::INFINITY, f64::min);
        let hi = corners.iter().map(|&(x, y)| proj(x, y)).fold(f64::NEG_INFINITY, f64::max);
        let g = Gradient {
            kind: Kind::Linear,
            start: [cx + ux * lo, cy + uy * lo],
            end: [cx + ux * hi, cy + uy * hi],
            colors: vec![],
            opacities: vec![OpacityStop { at: 0.0, opacity: 1.0 }, OpacityStop { at: 1.0, opacity: 1.0 }],
            reverse: false,
            dither: true,
        };
        Some(g)
    };
    // radial about the luma extremum of a quadratic fit, when it lies in the selection
    let radial = radial_centre(samples, (cx, cy), &luma).and_then(|(ox, oy)| {
        if ox < selection.0 || ox > selection.2 || oy < selection.1 || oy > selection.3 {
            return None;
        }
        let far = corners.iter().map(|&(x, y)| ((x - ox).powi(2) + (y - oy).powi(2)).sqrt()).fold(0.0, f64::max);
        let far = samples.iter().map(|(x, y, _)| ((x - ox).powi(2) + (y - oy).powi(2)).sqrt()).fold(far, f64::max);
        Some(Gradient {
            kind: Kind::Radial,
            start: [ox, oy],
            end: [ox + far, oy],
            colors: vec![],
            opacities: vec![OpacityStop { at: 0.0, opacity: 1.0 }, OpacityStop { at: 1.0, opacity: 1.0 }],
            reverse: false,
            dither: true,
        })
    });
    let mut best: Option<(f64, Gradient)> = None;
    for mut g in [linear, radial].into_iter().flatten() {
        let ts: Vec<f64> = samples.iter().map(|(x, y, _)| unclamped_position(&g, *x, *y)).collect();
        let mut mse = 0.0;
        let mut ends = [[0.0; 3]; 2];
        for k in 0..3 {
            let vs: Vec<f64> = samples.iter().map(|(_, _, c)| c[k]).collect();
            let (a, b, e) = line_fit(&ts, &vs);
            mse += e;
            ends[0][k] = a.clamp(0.0, 255.0);
            ends[1][k] = (a + b).clamp(0.0, 255.0);
        }
        g.colors = vec![ColorStop { at: 0.0, color: ends[0] }, ColorStop { at: 1.0, color: ends[1] }];
        // a radial model has one more free parameter; ask it to earn it
        let score = if g.kind == Kind::Radial { mse * 1.25 } else { mse };
        if best.as_ref().is_none_or(|(s, _)| score < *s) {
            best = Some((score, g));
        }
    }
    best.map(|(_, g)| g)
}

/// Position along `g` without the clamp, so a fit can use samples past the ends.
fn unclamped_position(g: &Gradient, x: f64, y: f64) -> f64 {
    let (dx, dy) = (g.end[0] - g.start[0], g.end[1] - g.start[1]);
    let len2 = (dx * dx + dy * dy).max(1e-9);
    let (px, py) = (x - g.start[0], y - g.start[1]);
    match g.kind {
        Kind::Radial => (px * px + py * py).sqrt() / len2.sqrt(),
        _ => (px * dx + py * dy) / len2,
    }
}

/// The extremum of an isotropic quadratic `l = a + b x + c y + d (x^2 + y^2)` fitted to the luma.
fn radial_centre(samples: &[(f64, f64, [f64; 3])], c0: (f64, f64), luma: &dyn Fn(&[f64; 3]) -> f64) -> Option<(f64, f64)> {
    let mut ata = [[0.0; 4]; 4];
    let mut atb = [0.0; 4];
    let s = 100.0; // keep the squared term well scaled
    for (x, y, c) in samples {
        let (u, v) = ((x - c0.0) / s, (y - c0.1) / s);
        let row = [1.0, u, v, u * u + v * v];
        for i in 0..4 {
            for j in 0..4 {
                ata[i][j] += row[i] * row[j];
            }
            atb[i] += row[i] * luma(c);
        }
    }
    // 4x4 elimination
    let mut m = [[0.0; 5]; 4];
    for i in 0..4 {
        m[i][..4].copy_from_slice(&ata[i]);
        m[i][4] = atb[i];
    }
    for c in 0..4 {
        let p = (c..4).max_by(|&a, &b| m[a][c].abs().total_cmp(&m[b][c].abs()))?;
        if m[p][c].abs() < 1e-12 {
            return None;
        }
        m.swap(c, p);
        for r in 0..4 {
            if r != c {
                let f = m[r][c] / m[c][c];
                let pivot = m[c];
                for (v, p) in m[r].iter_mut().zip(pivot).skip(c) {
                    *v -= f * p;
                }
            }
        }
    }
    let coef: Vec<f64> = (0..4).map(|i| m[i][4] / m[i][i]).collect();
    if coef[3].abs() < 1e-9 {
        return None;
    }
    let (u, v) = (-coef[1] / (2.0 * coef[3]), -coef[2] / (2.0 * coef[3]));
    Some((c0.0 + u * s, c0.1 + v * s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_stop(kind: Kind) -> Gradient {
        Gradient {
            kind,
            start: [0.0, 0.0],
            end: [10.0, 0.0],
            colors: vec![ColorStop { at: 0.0, color: [0.0; 3] }, ColorStop { at: 1.0, color: [200.0; 3] }],
            opacities: vec![OpacityStop { at: 0.0, opacity: 1.0 }, OpacityStop { at: 1.0, opacity: 0.0 }],
            reverse: false,
            dither: false,
        }
    }

    #[test]
    fn the_five_types() {
        assert!((two_stop(Kind::Linear).position(5.0, 7.0) - 0.5).abs() < 1e-9);
        assert!((two_stop(Kind::Reflected).position(-5.0, 0.0) - 0.5).abs() < 1e-9);
        assert!((two_stop(Kind::Radial).position(0.0, 5.0) - 0.5).abs() < 1e-9);
        assert!((two_stop(Kind::Diamond).position(2.5, 2.5) - 0.5).abs() < 1e-9);
        // a quarter turn counter-clockwise on screen is up, y negative
        assert!((two_stop(Kind::Angle).position(0.0, -5.0) - 0.25).abs() < 1e-9);
        let mut g = two_stop(Kind::Linear);
        g.reverse = true;
        assert!((g.position(2.0, 0.0) - 0.8).abs() < 1e-9);
    }

    #[test]
    fn stops_interpolate_colour_and_opacity_apart() {
        let mut g = two_stop(Kind::Linear);
        g.colors.push(ColorStop { at: 0.5, color: [255.0, 0.0, 0.0] });
        let (c, a) = g.at(0.25);
        assert_eq!(c, [127.5, 0.0, 0.0]);
        assert!((a - 0.75).abs() < 1e-9);
    }

    #[test]
    fn dither_stays_small_and_centred() {
        let n: Vec<f64> = (0..4000).map(|i| dither_noise(i % 63, i / 63)).collect();
        assert!(n.iter().all(|v| v.abs() <= 1.0));
        assert!((n.iter().sum::<f64>() / n.len() as f64).abs() < 0.05);
    }

    #[test]
    fn match_surroundings_finds_a_vertical_ramp() {
        // video 2: dark red at the top to black at the bottom
        let mut samples = Vec::new();
        for y in 0..100 {
            for x in 0..60 {
                let inside = (15..45).contains(&x) && (20..80).contains(&y);
                if !inside {
                    let f = y as f64 / 99.0;
                    samples.push((x as f64, y as f64, [120.0 * (1.0 - f), 10.0 * (1.0 - f), 0.0]));
                }
            }
        }
        let g = fit(&samples, (15.0, 20.0, 45.0, 80.0)).unwrap();
        assert_eq!(g.kind, Kind::Linear);
        // drag runs vertically, from the light (red) end down to black, or the reverse
        assert!((g.end[0] - g.start[0]).abs() < 1.0 && (g.end[1] - g.start[1]).abs() > 50.0);
        let top = g.at(g.position(30.0, 20.0)).0;
        assert!((top[0] - 120.0 * (1.0 - 20.0 / 99.0)).abs() < 3.0, "{top:?}");
    }

    #[test]
    fn match_surroundings_finds_a_radial_glow() {
        let mut samples = Vec::new();
        for y in 0..120 {
            for x in 0..120 {
                let inside = (40..80).contains(&x) && (40..80).contains(&y);
                let r = ((x as f64 - 62.0).powi(2) + (y as f64 - 58.0).powi(2)).sqrt();
                if !inside {
                    let v = (250.0 - 2.0 * r).max(0.0);
                    samples.push((x as f64, y as f64, [v, v, v]));
                }
            }
        }
        let g = fit(&samples, (40.0, 40.0, 80.0, 80.0)).unwrap();
        assert_eq!(g.kind, Kind::Radial);
        assert!((g.start[0] - 62.0).abs() < 4.0 && (g.start[1] - 58.0).abs() < 4.0, "{:?}", g.start);
    }

    #[test]
    fn match_surroundings_on_flat_paper_is_flat() {
        let mut samples = Vec::new();
        for y in 0..60 {
            for x in 0..60 {
                if !((20..40).contains(&x) && (20..40).contains(&y)) {
                    samples.push((x as f64, y as f64, [255.0; 3]));
                }
            }
        }
        let g = fit(&samples, (20.0, 20.0, 40.0, 40.0)).unwrap();
        assert!(g.colors.iter().all(|s| s.color.iter().all(|&v| (v - 255.0).abs() < 1e-6)), "{:?}", g.colors);
    }
}
