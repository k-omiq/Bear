//! The stroke planner: a polyline and a brush, to a list of dabs.
//!
//! **In native page pixels, and deterministic.** The interface plans the same
//! stroke for its preview at proxy scale; parity here is parity of the *plan*
//! and not of the pixels, so this walks the polyline in the page's own
//! coordinates and emits a dab every `spacing% × diameter` of arc length.
//! Nothing here reads a clock, a random number generator, or anything but its
//! arguments.
//!
//! Phase 1 is the minimal planner: spacing interpolation, pressure to radius and
//! to alpha, hardness passed through. freeditor's five stabilisers and its
//! seven-driver dynamics matrix are Phase 5, and `seed` is carried and
//! recorded rather than consumed - nothing in this phase jitters.

use super::brush::RasterizableDab;

/// One pointer sample, in **native page pixels**, with its pressure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokePoint {
    pub x: f64,
    pub y: f64,
    /// `0..1`. The seam sends `0.5` where the device did not say.
    pub p: f64,
}

/// The brush, as the contract carries it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushSpec {
    /// Diameter, in native page pixels.
    pub size: f64,
    /// `0..100`.
    pub hardness: f64,
    /// `0..100`, per-dab alpha before opacity caps it.
    pub flow: f64,
    /// `0..100`, the stroke-level ceiling `stamp_dab_subpixel` applies.
    pub opacity: f64,
    /// Percent **of the diameter** between dabs. Clamped to `1..=50`.
    pub spacing: f64,
    pub pressure_size: bool,
    pub pressure_opacity: bool,
    pub seed: u32,
    /// The tip's outline. A stroke saved before there was a choice is round.
    pub tip: Tip,
}

/// The outline of one dab: a disc, or a square on the page's axes whose side is
/// the diameter. Hardness is the same falloff either way, measured from the
/// centre in the tip's own distance (`Tip::distance`), so a soft square has a
/// square core and a square rim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tip {
    #[default]
    Round,
    Square,
}

impl Tip {
    /// The seam's word for a tip. Anything but `square` is round, which is
    /// what every stroke sent before the choice existed was.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("square") => Self::Square,
            _ => Self::Round,
        }
    }

    /// The word the seam and a patch's provenance use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Round => "round",
            Self::Square => "square",
        }
    }

    /// How far `(dx, dy)` is from the centre, in units of the radius: the
    /// straight-line distance for a disc, the larger of the two axis distances
    /// for a square. 1 is the tip's edge. `brushsurface.js#tipDistance` is the
    /// same function, so preview and commit cover the same pixels.
    pub fn distance(self, dx: f64, dy: f64) -> f64 {
        match self {
            Self::Round => dx.hypot(dy),
            Self::Square => dx.abs().max(dy.abs()),
        }
    }
}

impl Default for BrushSpec {
    fn default() -> Self {
        Self {
            size: 24.0,
            hardness: 50.0,
            flow: 100.0,
            opacity: 100.0,
            spacing: 10.0,
            pressure_size: true,
            pressure_opacity: false,
            seed: 0,
            tip: Tip::Round,
        }
    }
}

/// The smallest dab that is still a dab. Below this a "stroke" is a line of
/// isolated sub-pixel specks, and the mask it produces is not contiguous.
pub const MIN_RADIUS: f64 = 0.5;

/// The spacing floor and ceiling, as percentages of the diameter. A spacing of
/// zero is an infinite dab count; anything above half a diameter is a dotted
/// line, which no brush parameter should be able to ask for by accident.
const MIN_SPACING_PERCENT: f64 = 1.0;
const MAX_SPACING_PERCENT: f64 = 50.0;

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if !v.is_finite() {
        return lo;
    }
    v.max(lo).min(hi)
}

/// The distance between two dab centres, in native pixels.
pub fn spacing_px(brush: &BrushSpec) -> f64 {
    let percent = clamp(brush.spacing, MIN_SPACING_PERCENT, MAX_SPACING_PERCENT);
    (percent / 100.0 * brush.size.max(1.0)).max(0.1)
}

fn radius_at(brush: &BrushSpec, pressure: f64) -> f64 {
    let scale = if brush.pressure_size { clamp(pressure, 0.0, 1.0) } else { 1.0 };
    (brush.size.max(0.0) / 2.0 * scale).max(MIN_RADIUS)
}

fn alpha_at(brush: &BrushSpec, pressure: f64) -> f64 {
    let flow = clamp(brush.flow, 0.0, 100.0) / 100.0;
    let scale = if brush.pressure_opacity { clamp(pressure, 0.0, 1.0) } else { 1.0 };
    clamp(flow * scale, 0.0, 1.0)
}

fn dab_at(brush: &BrushSpec, x: f64, y: f64, pressure: f64) -> RasterizableDab {
    RasterizableDab {
        x,
        y,
        // Document-space seeds, so a cropped rasterization keeps identical
        // randomness - the kernel's own contract for these two fields.
        seed_x: Some(x),
        seed_y: Some(y),
        radius: radius_at(brush, pressure),
        alpha: alpha_at(brush, pressure),
        roundness: None,
    }
}

/// Walk the polyline and emit the dabs.
///
/// A single point is one dab: a tap is a mark, not nothing. Two coincident
/// points are still one dab, because the walk advances by arc length and a
/// zero-length segment has none to give.
pub fn plan_stroke(points: &[StrokePoint], brush: &BrushSpec) -> Vec<RasterizableDab> {
    let mut dabs = Vec::new();
    let Some(first) = points.first() else { return dabs };
    dabs.push(dab_at(brush, first.x, first.y, first.p));
    if points.len() == 1 {
        return dabs;
    }

    let step = spacing_px(brush);
    // How far past the last emitted dab the walk has travelled. A segment
    // shorter than what is left over contributes its length and no dab, which
    // is what keeps spacing uniform *across* segment joins rather than
    // restarting at every pointer sample.
    let mut carried = 0.0f64;

    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let length = dx.hypot(dy);
        if !length.is_finite() || length <= 0.0 {
            continue;
        }
        let mut travelled = step - carried;
        while travelled <= length {
            let t = travelled / length;
            let pressure = a.p + (b.p - a.p) * t;
            dabs.push(dab_at(brush, a.x + dx * t, a.y + dy * t, pressure));
            travelled += step;
        }
        carried = length - (travelled - step);
    }

    dabs
}

/// The bounding box of a planned stroke, grown by the largest dab's reach.
///
/// `(x0, y0, x1, y1)` as an inclusive-exclusive rectangle in page pixels,
/// before clamping. `margin` is the caller's own slack - `render_brush` uses 2 px
/// so the soft rim of the outermost dab is inside the surface it blends over.
pub fn bounds_of(dabs: &[RasterizableDab], margin: f64) -> Option<(f64, f64, f64, f64)> {
    let mut it = dabs.iter();
    let first = it.next()?;
    let reach = |d: &RasterizableDab| d.radius + margin;
    let (mut x0, mut y0) = (first.x - reach(first), first.y - reach(first));
    let (mut x1, mut y1) = (first.x + reach(first), first.y + reach(first));
    for dab in it {
        x0 = x0.min(dab.x - reach(dab));
        y0 = y0.min(dab.y - reach(dab));
        x1 = x1.max(dab.x + reach(dab));
        y1 = y1.max(dab.y + reach(dab));
    }
    Some((x0, y0, x1, y1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize, dx: f64) -> Vec<StrokePoint> {
        (0..n).map(|i| StrokePoint { x: i as f64 * dx, y: 0.0, p: 0.5 }).collect()
    }

    /// A tap is a mark. The planner used to be reachable with an empty stroke
    /// only through the caller's own guard; this states the two ends itself.
    #[test]
    fn no_points_is_no_dabs_and_one_point_is_one_dab() {
        let brush = BrushSpec::default();
        assert!(plan_stroke(&[], &brush).is_empty());
        assert_eq!(plan_stroke(&line(1, 0.0), &brush).len(), 1);
    }

    /// The spacing is arc length between centres, and it is exact: a 100 px
    /// straight line at a 24 px brush and 10% spacing steps 2.4 px, so the
    /// walk emits the origin plus `floor(100 / 2.4)` more.
    #[test]
    fn dabs_land_every_spacing_percent_of_a_diameter() {
        let brush = BrushSpec { size: 24.0, spacing: 10.0, ..BrushSpec::default() };
        let step = spacing_px(&brush);
        assert!((step - 2.4).abs() < 1e-12, "{step}");
        let dabs = plan_stroke(&[
            StrokePoint { x: 0.0, y: 0.0, p: 0.5 },
            StrokePoint { x: 100.0, y: 0.0, p: 0.5 },
        ], &brush);
        assert_eq!(dabs.len(), 1 + (100.0 / step).floor() as usize);
        for pair in dabs.windows(2) {
            let gap = (pair[1].x - pair[0].x).hypot(pair[1].y - pair[0].y);
            assert!((gap - step).abs() < 1e-9, "{gap}");
        }
    }

    /// **The leftover carries across a segment join.** A polyline of ten 1 px
    /// steps is a 10 px stroke, and it must produce the dabs a single 10 px
    /// segment does - not one per pointer sample, and not a restart of the
    /// spacing clock at every one of them.
    #[test]
    fn spacing_is_measured_along_the_whole_polyline_not_per_segment() {
        let brush = BrushSpec { size: 24.0, spacing: 10.0, ..BrushSpec::default() };
        let many = plan_stroke(&line(11, 1.0), &brush);
        let one = plan_stroke(&[
            StrokePoint { x: 0.0, y: 0.0, p: 0.5 },
            StrokePoint { x: 10.0, y: 0.0, p: 0.5 },
        ], &brush);
        assert_eq!(many.len(), one.len());
        for (a, b) in many.iter().zip(one.iter()) {
            assert!((a.x - b.x).abs() < 1e-9, "{} vs {}", a.x, b.x);
        }
    }

    /// Pressure drives whichever channels are switched on, and neither of them
    /// otherwise. The floor is the thing worth pinning: a pressure of zero on a
    /// pressure-size brush is still a dab, not a divide by nothing.
    #[test]
    fn pressure_maps_to_radius_and_alpha_only_where_it_is_enabled() {
        let both = BrushSpec {
            size: 20.0,
            flow: 80.0,
            pressure_size: true,
            pressure_opacity: true,
            ..BrushSpec::default()
        };
        let soft = plan_stroke(&[StrokePoint { x: 0.0, y: 0.0, p: 0.25 }], &both);
        assert_eq!(soft[0].radius, 2.5);
        assert!((soft[0].alpha - 0.2).abs() < 1e-12);

        let neither = BrushSpec { pressure_size: false, pressure_opacity: false, ..both };
        let flat = plan_stroke(&[StrokePoint { x: 0.0, y: 0.0, p: 0.25 }], &neither);
        assert_eq!(flat[0].radius, 10.0);
        assert!((flat[0].alpha - 0.8).abs() < 1e-12);

        let zero = plan_stroke(&[StrokePoint { x: 0.0, y: 0.0, p: 0.0 }], &both);
        assert_eq!(zero[0].radius, MIN_RADIUS);
    }

    /// Determinism is the whole reason the preview and the commit can differ in
    /// resolution and still be the same stroke.
    #[test]
    fn the_same_stroke_plans_to_the_same_dabs_every_time() {
        let brush = BrushSpec { spacing: 7.0, seed: 42, ..BrushSpec::default() };
        let points = line(20, 3.5);
        let a = plan_stroke(&points, &brush);
        let b = plan_stroke(&points, &brush);
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(b.iter()).all(|(x, y)| x.x == y.x && x.y == y.y && x.alpha == y.alpha));
    }

    /// Spacing is clamped at both ends, so no seam value can ask for an
    /// unbounded dab count or a dotted line.
    #[test]
    fn spacing_is_clamped_rather_than_trusted() {
        let brush = BrushSpec { size: 100.0, ..BrushSpec::default() };
        assert_eq!(spacing_px(&BrushSpec { spacing: 0.0, ..brush }), 1.0);
        assert_eq!(spacing_px(&BrushSpec { spacing: -5.0, ..brush }), 1.0);
        assert_eq!(spacing_px(&BrushSpec { spacing: 900.0, ..brush }), 50.0);
        assert_eq!(spacing_px(&BrushSpec { spacing: f64::NAN, ..brush }), 1.0);
    }

    /// The box is the union of the dabs' reaches, not of their centres - a
    /// bbox over centres clips the soft rim off both ends of every stroke.
    #[test]
    fn the_bounding_box_covers_every_dabs_reach() {
        let brush = BrushSpec { size: 10.0, pressure_size: false, ..BrushSpec::default() };
        let dabs = plan_stroke(&[
            StrokePoint { x: 20.0, y: 30.0, p: 1.0 },
            StrokePoint { x: 60.0, y: 30.0, p: 1.0 },
        ], &brush);
        let (x0, y0, x1, y1) = bounds_of(&dabs, 2.0).unwrap();
        assert_eq!((x0, y0, x1, y1), (13.0, 23.0, 67.0, 37.0));
        assert_eq!(bounds_of(&[], 2.0), None);
    }
}
