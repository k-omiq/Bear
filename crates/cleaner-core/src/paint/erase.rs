//! The eraser: a stroke's coverage taken back off a painted patch.
//!
//! A Brush patch holds its stroke as coverage (`mask`, 0 to 255) over pixels
//! that already carry the stroke blended over what was under it
//! ([`super::render_brush`]). The compositor weighs those pixels by the
//! coverage, so cutting the coverage is all an eraser has to do: where it is
//! cut to nothing, the page under the patch shows; where it is cut part way,
//! the patch's pixels and the page under it mix in that proportion, which is
//! the stroke at a lower strength over the same page.
//!
//! The eraser's own coverage is a Brush stroke's: the same dab plan
//! ([`super::plan::plan_stroke`]) and the same tip
//! (`brush.rs#analytic_radial_coverage`), and like the Brush it does not build
//! up: a pixel is taken down by the most any one dab covers it, capped at the
//! stroke's opacity.

use super::plan::{self, BrushSpec, StrokePoint};
use crate::mask::{Mask, Rect};
use crate::patch::Patch;

/// The largest box an eraser stroke may sweep, in page pixels. Past it the
/// stroke is refused rather than allocated: 4096 by 4096.
pub const MAX_ERASE_AREA: u64 = 16_777_216;

/// How much of each page pixel an eraser stroke takes off, 0 to 255, over the
/// box its dabs sweep. `None` for a stroke with no dabs or one past
/// [`MAX_ERASE_AREA`].
pub fn erase_coverage(points: &[StrokePoint], spec: &BrushSpec) -> Option<Mask> {
    let dabs = plan::plan_stroke(points, spec);
    let (x0, y0, x1, y1) = plan::bounds_of(&dabs, 1.0)?;
    let (x0, y0, x1, y1) = (x0.floor() as i64, y0.floor() as i64, x1.ceil() as i64, y1.ceil() as i64);
    if x1 <= x0 || y1 <= y0 || (x1 - x0) as u64 * (y1 - y0) as u64 > MAX_ERASE_AREA {
        return None;
    }
    let window = Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
    let (w, h) = (window.w as usize, window.h as usize);
    let opacity = (spec.opacity / 100.0).clamp(0.0, 1.0);
    let hardness = (spec.hardness / 100.0).clamp(0.0, 1.0);
    let mut cover = vec![0f32; w * h];
    for dab in &dabs {
        let alpha = dab.alpha.clamp(0.0, 1.0);
        if alpha <= 0.0 || dab.radius <= 0.0 {
            continue;
        }
        let reach = dab.radius + 1.0;
        let left = ((dab.x - reach).floor() as i64).max(window.x);
        let right = ((dab.x + reach).ceil() as i64).min(window.right() - 1);
        let top = ((dab.y - reach).floor() as i64).max(window.y);
        let bottom = ((dab.y + reach).ceil() as i64).min(window.bottom() - 1);
        for py in top..=bottom {
            for px in left..=right {
                // A page pixel is sampled on its integer coordinate, as
                // `brush.rs#stamp_dab_subpixel` samples it.
                let t = (px as f64 - dab.x).hypot(py as f64 - dab.y) / dab.radius;
                let covered = (super::brush::analytic_radial_coverage(t, hardness) * alpha).clamp(0.0, 1.0);
                if covered <= 0.0 {
                    continue;
                }
                let at = (py - window.y) as usize * w + (px - window.x) as usize;
                cover[at] = (opacity as f32).min(cover[at].max(covered as f32));
            }
        }
    }
    let mut mask = Mask::empty(window);
    for (bit, value) in mask.bits.iter_mut().zip(cover) {
        *bit = (value * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    (!mask.is_empty()).then_some(mask)
}

/// Cut a patch's coverage by an eraser's, where the patch is **drawn**: a
/// layer that was moved or turned is erased where it shows, by running
/// [`Patch::presented`]'s placement backwards for each of its pixels.
///
/// Answers whether any coverage came off. Lettering (`ink`) goes where less
/// than half the coverage is left: `ink` is the hole a Try again cleans
/// (`Preprocessing::hole`), and a pixel kept in it would be cleaned back over
/// the page the eraser showed. Under half is the binary hole's nearest answer
/// to a part-way erase.
pub fn erase_patch(patch: &mut Patch, erase: &Mask) -> bool {
    let style = patch.layer_style();
    let source = patch.mask.bounds;
    if source.w == 0 || source.h == 0 {
        return false;
    }
    let drawn = style.display_bounds(source).intersection(erase.bounds);
    if drawn.w == 0 || drawn.h == 0 {
        return false;
    }
    let cx = source.x as f64 + source.w as f64 / 2.0;
    let cy = source.y as f64 + source.h as f64 / 2.0;
    let (sin, cos) = style.rotation.to_radians().sin_cos();
    let mut changed = false;
    for y in source.y..source.bottom() {
        for x in source.x..source.right() {
            let old = patch.mask.coverage(x, y);
            if old == 0 {
                continue;
            }
            // `presented` reads source position `f` for drawn pixel `X` as
            // `f = c + R(X + 0.5 - offset - c)`; this is `X` from `f`.
            let (u, v) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
            let drawn_x = cx + u * cos - v * sin + style.offset_x as f64 - 0.5;
            let drawn_y = cy + u * sin + v * cos + style.offset_y as f64 - 0.5;
            let taken = erase.coverage(drawn_x.round() as i64, drawn_y.round() as i64);
            if taken == 0 {
                continue;
            }
            let left = (u32::from(old) * (255 - u32::from(taken)) + 127) / 255;
            patch.mask.set_coverage(x, y, left as u8);
            if left < 128 {
                patch.ink.set(x, y, false);
            }
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::fixtures;
    use crate::patch::{Engine, Provenance};

    fn spec(size: f64, hardness: f64, opacity: f64) -> BrushSpec {
        BrushSpec { size, hardness, opacity, flow: 100.0, spacing: 10.0, pressure_size: false, ..BrushSpec::default() }
    }

    fn point(x: f64, y: f64) -> StrokePoint {
        StrokePoint { x, y, p: 1.0 }
    }

    fn painted(bounds: Rect, layer: serde_json::Value) -> Patch {
        let mut snapshot = serde_json::json!({ "tool": "brush", "mode": "paint" });
        if !layer.is_null() {
            snapshot["layer"] = layer;
        }
        let mut pixels = fixtures::by_name("l8").raster;
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.data = vec![0; (bounds.w * bounds.h) as usize];
        Patch {
            id: "c-p001-h1".into(),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 1,
            visible: true,
            provenance: Provenance {
                engine: Engine::Paint,
                engine_version: String::new(),
                model_sha256: None,
                execution_provider: String::new(),
                params_snapshot: snapshot,
                mask_sha256: String::new(),
                source_sha256: String::new(),
                cloud: None,
                created: 0,
            },
        }
    }

    #[test]
    fn a_hard_stroke_takes_everything_off_inside_its_radius_and_nothing_outside() {
        let erase = erase_coverage(&[point(50.0, 50.0)], &spec(20.0, 100.0, 100.0)).unwrap();
        assert_eq!(erase.coverage(50, 50), 255);
        assert_eq!(erase.coverage(59, 50), 255);
        assert_eq!(erase.coverage(61, 50), 0);
    }

    #[test]
    fn opacity_caps_what_one_stroke_takes_however_often_it_crosses() {
        let erase = erase_coverage(
            &[point(50.0, 50.0), point(80.0, 50.0), point(50.0, 50.0)],
            &spec(20.0, 100.0, 40.0),
        )
        .unwrap();
        assert_eq!(erase.coverage(65, 50), 102);
    }

    #[test]
    fn a_patch_loses_coverage_where_the_stroke_crosses_it() {
        let mut patch = painted(Rect::new(40, 40, 30, 30), serde_json::Value::Null);
        let erase = erase_coverage(&[point(40.0, 55.0), point(70.0, 55.0)], &spec(10.0, 100.0, 100.0)).unwrap();
        assert!(erase_patch(&mut patch, &erase));
        assert_eq!(patch.mask.coverage(55, 55), 0);
        assert!(!patch.ink.contains(55, 55));
        assert_eq!(patch.mask.coverage(55, 41), 255);

        let mut half = painted(Rect::new(40, 40, 30, 30), serde_json::Value::Null);
        let soft = erase_coverage(&[point(55.0, 55.0)], &spec(10.0, 100.0, 50.0)).unwrap();
        assert!(erase_patch(&mut half, &soft));
        assert_eq!(half.mask.coverage(55, 55), 127);
        assert!(!half.ink.contains(55, 55), "under half left, out of the hole");

        let mut most = painted(Rect::new(40, 40, 30, 30), serde_json::Value::Null);
        let light = erase_coverage(&[point(55.0, 55.0)], &spec(10.0, 100.0, 40.0)).unwrap();
        assert!(erase_patch(&mut most, &light));
        assert_eq!(most.mask.coverage(55, 55), 153);
        assert!(most.ink.contains(55, 55), "more than half left, still in the hole");
    }

    #[test]
    fn a_moved_layer_is_erased_where_it_is_drawn() {
        let layer = serde_json::json!({ "opacity": 100, "offsetX": 100, "offsetY": 0, "rotation": 0.0, "locked": false });
        let mut patch = painted(Rect::new(0, 0, 20, 20), layer);
        // Over where the layer was made: it is not there any more.
        let there = erase_coverage(&[point(10.0, 10.0)], &spec(6.0, 100.0, 100.0)).unwrap();
        assert!(!erase_patch(&mut patch, &there));
        // Over where it is drawn now.
        let here = erase_coverage(&[point(110.0, 10.0)], &spec(6.0, 100.0, 100.0)).unwrap();
        assert!(erase_patch(&mut patch, &here));
        assert_eq!(patch.mask.coverage(10, 10), 0);
        assert_eq!(patch.mask.coverage(2, 2), 255);
    }

    #[test]
    fn a_stroke_off_the_patch_changes_nothing() {
        let mut patch = painted(Rect::new(0, 0, 10, 10), serde_json::Value::Null);
        let erase = erase_coverage(&[point(200.0, 200.0)], &spec(10.0, 100.0, 100.0)).unwrap();
        assert!(!erase_patch(&mut patch, &erase));
        assert!(patch.mask.bits.iter().all(|&bit| bit == 255));
    }
}
