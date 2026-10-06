//! Versioned, source-pixel write geometry for the optional text-shaped mode.
//!
//! The approved support is rebuilt from an immutable lettering mask and the
//! current manual corrections. Model holes and reading context are separate
//! inputs; neither grants permission to write a pixel.

use crate::mask::{Mask, Rect};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MASK_PLAN_VERSION: u32 = 1;
/// A bounded region raster, rather than a full-page JSON mask.
pub const MAX_PLAN_PIXELS: usize = 16 * 1024 * 1024;
pub const MAX_PADDING_PX: u32 = 64;

/// Missing geometry fields in old records mean the existing behavior. An
/// unknown future spelling is a serde error, never a silent legacy fallback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeometryPolicy {
    #[default]
    Legacy,
    TextShape,
}

/// Persistable, bounded binary raster. `bits` are row-major 0 or 255.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskRaster {
    pub bounds: Rect,
    pub bits: Vec<u8>,
}

impl From<Mask> for MaskRaster {
    fn from(mask: Mask) -> Self {
        Self {
            bounds: mask.bounds,
            bits: mask.bits,
        }
    }
}

impl MaskRaster {
    pub fn to_mask(&self) -> Mask {
        Mask {
            bounds: self.bounds,
            bits: self.bits.clone(),
        }
    }

    pub fn contains(&self, x: i64, y: i64) -> bool {
        self.bounds.contains(x, y)
            && self
                .bits
                .get(
                    ((y - self.bounds.y) as usize) * self.bounds.w as usize
                        + (x - self.bounds.x) as usize,
                )
                .is_some_and(|&v| v != 0)
    }

    pub fn count(&self) -> usize {
        self.bits.iter().filter(|&&v| v != 0).count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MaskQualityState {
    Ready,
    NeedsCorrection { reason: String },
}

/// Inputs to a deterministic plan preparation. An empty addition/removal
/// raster is valid. Removal wins where a pixel is both added and removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskPlan {
    pub version: u32,
    pub region_id: String,
    pub source_sha256: String,
    pub lower_composite_sha256: String,
    pub algorithm_id: String,
    pub model_id: Option<String>,
    pub candidate_bounds: Rect,
    pub refinement_crop: Rect,
    pub base_revision: u64,
    pub correction_revision: u64,
    pub plan_revision: u64,
    pub base_mask: MaskRaster,
    pub additions: MaskRaster,
    pub removals: MaskRaster,
    pub padding_px: u32,
    /// Round input-hole expansion *from W*, without changing W itself.
    pub model_hole_margin_px: u32,
    pub reading_context: Rect,
    /// Optional alpha is an inward-only multiplier inside W. A zero outside W
    /// is mandatory; it cannot enlarge the patch support.
    pub blend_alpha: Option<MaskRaster>,
    pub quality: MaskQualityState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanIdentity {
    pub version: u32,
    pub region_id: String,
    pub source_sha256: String,
    pub lower_composite_sha256: String,
    pub algorithm_id: String,
    pub model_id: Option<String>,
    pub base_revision: u64,
    pub correction_revision: u64,
    pub plan_revision: u64,
    pub page_w: u32,
    pub page_h: u32,
    pub padding_px: u32,
    pub model_hole_margin_px: u32,
    pub candidate_bounds: Rect,
    pub refinement_crop: Rect,
    pub reading_context: Rect,
    /// SHA-256 over the exact bounds and bytes of approved W.
    pub support_sha256: String,
    pub blend_alpha_sha256: Option<String>,
    /// SHA-256 over every field above, including the support digest.
    pub identity_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedMaskPlan {
    pub identity: PlanIdentity,
    pub corrected_base: MaskRaster,
    pub write_support: MaskRaster,
    pub model_hole: MaskRaster,
    pub blend_alpha: Option<MaskRaster>,
    pub candidate_bounds: Rect,
    pub refinement_crop: Rect,
    pub reading_context: Rect,
}

impl PreparedMaskPlan {
    /// Rebuild from the immutable plan before applying or trusting a loaded
    /// artifact. This checks all raster bytes, context fields and identity,
    /// rather than trusting a stored digest alongside its stored raster.
    pub fn verify_against(
        &self,
        plan: &MaskPlan,
        page_w: u32,
        page_h: u32,
    ) -> Result<(), PlanError> {
        if *self != plan.prepare(page_w, page_h)? {
            return Err(PlanError::Invalid(
                "prepared plan does not match its source plan",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("text-shaped mask needs correction: {0}")]
    NeedsCorrection(String),
    #[error("unsupported mask plan version {0}")]
    UnsupportedVersion(u32),
    #[error("invalid or unsafe mask plan: {0}")]
    Invalid(&'static str),
}

fn validate_rect(rect: Rect, page_w: u32, page_h: u32) -> Result<usize, PlanError> {
    if rect.x < 0 || rect.y < 0 || rect.x > page_w as i64 || rect.y > page_h as i64 {
        return Err(PlanError::Invalid("mask bounds outside page"));
    }
    if rect.w > page_w - rect.x as u32 || rect.h > page_h - rect.y as u32 {
        return Err(PlanError::Invalid("mask bounds outside page"));
    }
    let n = (rect.w as usize)
        .checked_mul(rect.h as usize)
        .ok_or(PlanError::Invalid("mask area overflow"))?;
    if n > MAX_PLAN_PIXELS {
        return Err(PlanError::NeedsCorrection(
            "mask region exceeds the bounded raster budget".into(),
        ));
    }
    Ok(n)
}

fn validate_binary(mask: &MaskRaster, page_w: u32, page_h: u32) -> Result<(), PlanError> {
    if validate_rect(mask.bounds, page_w, page_h)? != mask.bits.len() {
        return Err(PlanError::Invalid(
            "mask raster length does not match bounds",
        ));
    }
    if mask.bits.iter().any(|&v| v != 0 && v != 255) {
        return Err(PlanError::Invalid("binary mask contains non-binary values"));
    }
    Ok(())
}

/// Dilate once from a base raster using integer source-pixel center distances.
/// A destination pixel belongs to the result iff some selected source pixel
/// center is at Euclidean distance <= `radius` from its center. At radius 0
/// the exact input raster is returned. This intentionally differs from the
/// legacy `Mask::dilated(1)`, which uses a square at small radii.
pub fn round_source_dilate(
    base: &MaskRaster,
    radius: u32,
    page_w: u32,
    page_h: u32,
) -> Result<MaskRaster, PlanError> {
    validate_binary(base, page_w, page_h)?;
    if radius > MAX_PADDING_PX {
        return Err(PlanError::NeedsCorrection(
            "padding exceeds the supported source-pixel radius".into(),
        ));
    }
    if radius == 0 || base.count() == 0 {
        return Ok(base.clone());
    }
    let bounds = base.bounds.grown(radius, page_w, page_h);
    let n = validate_rect(bounds, page_w, page_h)?;
    let mut bits = vec![0; n];
    let r = radius as i64;
    let rr = r * r;
    let extents: Vec<i64> = (-r..=r)
        .map(|dy| {
            let mut dx = r;
            while dx * dx + dy * dy > rr {
                dx -= 1;
            }
            dx
        })
        .collect();
    for sy in base.bounds.y..base.bounds.bottom() {
        let row = (sy - base.bounds.y) as usize * base.bounds.w as usize;
        for sx in base.bounds.x..base.bounds.right() {
            if base.bits[row + (sx - base.bounds.x) as usize] == 0 {
                continue;
            }
            for dy in -r..=r {
                let y = sy + dy;
                if y < bounds.y || y >= bounds.bottom() {
                    continue;
                }
                let dx = extents[(dy + r) as usize];
                let x0 = (sx - dx).max(bounds.x);
                let x1 = (sx + dx + 1).min(bounds.right());
                let i0 = (y - bounds.y) as usize * bounds.w as usize + (x0 - bounds.x) as usize;
                bits[i0..i0 + (x1 - x0) as usize].fill(255);
            }
        }
    }
    Ok(MaskRaster { bounds, bits })
}

fn corrected_base(
    base: &MaskRaster,
    additions: &MaskRaster,
    removals: &MaskRaster,
    page_w: u32,
    page_h: u32,
) -> Result<MaskRaster, PlanError> {
    for mask in [base, additions, removals] {
        validate_binary(mask, page_w, page_h)?;
    }
    let base_has_pixels = base.count() != 0;
    let additions_have_pixels = additions.count() != 0;
    if !base_has_pixels && !additions_have_pixels {
        return Ok(MaskRaster {
            bounds: Rect::new(0, 0, 0, 0),
            bits: Vec::new(),
        });
    }
    let mut bounds = if base_has_pixels {
        base.bounds
    } else {
        additions.bounds
    };
    if base_has_pixels && additions_have_pixels {
        let x = bounds.x.min(additions.bounds.x);
        let y = bounds.y.min(additions.bounds.y);
        let right = bounds.right().max(additions.bounds.right());
        let bottom = bounds.bottom().max(additions.bounds.bottom());
        bounds = Rect::new(x, y, (right - x) as u32, (bottom - y) as u32);
    }
    let n = validate_rect(bounds, page_w, page_h)?;
    let mut out = MaskRaster {
        bounds,
        bits: vec![0; n],
    };
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if (base.contains(x, y) || additions.contains(x, y)) && !removals.contains(x, y) {
                out.bits[(y - bounds.y) as usize * bounds.w as usize + (x - bounds.x) as usize] =
                    255;
            }
        }
    }
    Ok(out)
}

/// Hash the precise binary support raster, including its origin and extent.
fn raster_sha256(raster: &MaskRaster, domain: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(domain);
    h.update(raster.bounds.x.to_le_bytes());
    h.update(raster.bounds.y.to_le_bytes());
    h.update(raster.bounds.w.to_le_bytes());
    h.update(raster.bounds.h.to_le_bytes());
    h.update(&raster.bits);
    format!("{:x}", h.finalize())
}

pub fn support_sha256(support: &MaskRaster) -> String {
    raster_sha256(support, b"manga-cleaner/write-support/v1\0")
}

impl MaskPlan {
    pub fn prepare(&self, page_w: u32, page_h: u32) -> Result<PreparedMaskPlan, PlanError> {
        if self.version != MASK_PLAN_VERSION {
            return Err(PlanError::UnsupportedVersion(self.version));
        }
        if self.region_id.is_empty()
            || self.source_sha256.is_empty()
            || self.lower_composite_sha256.is_empty()
            || self.algorithm_id.is_empty()
        {
            return Err(PlanError::Invalid("missing plan identity"));
        }
        if let MaskQualityState::NeedsCorrection { reason } = &self.quality {
            return Err(PlanError::NeedsCorrection(reason.clone()));
        }
        for rect in [self.candidate_bounds, self.refinement_crop] {
            validate_rect(rect, page_w, page_h)?;
        }
        // A bounded read context may cross the anchor page into a verified
        // long-strip neighbor. It does not grant write permission there.
        let context_pixels = (self.reading_context.w as usize)
            .checked_mul(self.reading_context.h as usize)
            .ok_or(PlanError::Invalid("reading context area overflow"))?;
        if context_pixels == 0 || context_pixels > MAX_PLAN_PIXELS {
            return Err(PlanError::NeedsCorrection(
                "reading context exceeds the bounded raster budget".into(),
            ));
        }
        if self.padding_px > MAX_PADDING_PX || self.model_hole_margin_px > MAX_PADDING_PX {
            return Err(PlanError::NeedsCorrection(
                "requested radius exceeds the supported source-pixel range".into(),
            ));
        }
        let corrected_base = corrected_base(
            &self.base_mask,
            &self.additions,
            &self.removals,
            page_w,
            page_h,
        )?;
        if corrected_base.count() == 0 {
            return Err(PlanError::NeedsCorrection("lettering mask is empty".into()));
        }
        let write_support = round_source_dilate(&corrected_base, self.padding_px, page_w, page_h)?;
        let model_hole =
            round_source_dilate(&write_support, self.model_hole_margin_px, page_w, page_h)?;
        if let Some(alpha) = &self.blend_alpha {
            if validate_rect(alpha.bounds, page_w, page_h)? != alpha.bits.len() {
                return Err(PlanError::Invalid(
                    "blend alpha length does not match bounds",
                ));
            }
            for y in alpha.bounds.y..alpha.bounds.bottom() {
                for x in alpha.bounds.x..alpha.bounds.right() {
                    if alpha.bits[(y - alpha.bounds.y) as usize * alpha.bounds.w as usize
                        + (x - alpha.bounds.x) as usize]
                        != 0
                        && !write_support.contains(x, y)
                    {
                        return Err(PlanError::Invalid(
                            "blend alpha extends outside approved write support",
                        ));
                    }
                }
            }
        }
        let support_sha256 = support_sha256(&write_support);
        let mut identity = PlanIdentity {
            version: self.version,
            region_id: self.region_id.clone(),
            source_sha256: self.source_sha256.clone(),
            lower_composite_sha256: self.lower_composite_sha256.clone(),
            algorithm_id: self.algorithm_id.clone(),
            model_id: self.model_id.clone(),
            base_revision: self.base_revision,
            correction_revision: self.correction_revision,
            plan_revision: self.plan_revision,
            page_w,
            page_h,
            padding_px: self.padding_px,
            model_hole_margin_px: self.model_hole_margin_px,
            candidate_bounds: self.candidate_bounds,
            refinement_crop: self.refinement_crop,
            reading_context: self.reading_context,
            support_sha256,
            blend_alpha_sha256: self
                .blend_alpha
                .as_ref()
                .map(|alpha| raster_sha256(alpha, b"manga-cleaner/blend-alpha/v1\0")),
            identity_sha256: String::new(),
        };
        let mut h = Sha256::new();
        h.update(b"manga-cleaner/mask-plan-identity/v1\0");
        // A fixed struct field order makes this serialization deterministic.
        h.update(
            serde_json::to_vec(&identity)
                .map_err(|_| PlanError::Invalid("identity serialization failed"))?,
        );
        identity.identity_sha256 = format!("{:x}", h.finalize());
        Ok(PreparedMaskPlan {
            identity,
            corrected_base,
            write_support,
            model_hole,
            blend_alpha: self.blend_alpha.clone(),
            candidate_bounds: self.candidate_bounds,
            refinement_crop: self.refinement_crop,
            reading_context: self.reading_context,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(bounds: Rect, on: &[(i64, i64)]) -> MaskRaster {
        let mut mask = Mask::empty(bounds);
        for &(x, y) in on {
            mask.set(x, y, true);
        }
        mask.into()
    }

    fn plan(base: MaskRaster) -> MaskPlan {
        MaskPlan {
            version: MASK_PLAN_VERSION,
            region_id: "r1".into(),
            source_sha256: "source".into(),
            lower_composite_sha256: "underlay".into(),
            algorithm_id: "synthetic".into(),
            model_id: None,
            candidate_bounds: Rect::new(0, 0, 20, 20),
            refinement_crop: Rect::new(0, 0, 20, 20),
            base_revision: 1,
            correction_revision: 1,
            plan_revision: 1,
            base_mask: base,
            additions: raster(Rect::new(0, 0, 0, 0), &[]),
            removals: raster(Rect::new(0, 0, 0, 0), &[]),
            padding_px: 0,
            model_hole_margin_px: 5,
            reading_context: Rect::new(0, 0, 20, 20),
            blend_alpha: None,
            quality: MaskQualityState::Ready,
        }
    }

    #[test]
    fn zero_padding_is_exact_corrected_base_and_hole_is_independent() {
        let mut p = plan(raster(Rect::new(4, 4, 8, 8), &[(5, 5), (9, 9)]));
        p.additions = raster(Rect::new(4, 4, 8, 8), &[(7, 7)]);
        p.removals = raster(Rect::new(4, 4, 8, 8), &[(5, 5)]);
        let prepared = p.prepare(20, 20).unwrap();
        assert_eq!(prepared.write_support, prepared.corrected_base);
        assert!(!prepared.write_support.contains(5, 5));
        assert!(prepared.write_support.contains(7, 7));
        assert!(prepared.model_hole.contains(12, 7));
        assert!(!prepared.write_support.contains(12, 7));
    }

    #[test]
    fn additions_far_from_an_empty_base_do_not_allocate_the_gap() {
        let mut p = plan(raster(Rect::new(0, 0, 0, 0), &[]));
        p.additions = raster(Rect::new(3900, 5900, 2, 2), &[(3901, 5901)]);
        p.candidate_bounds = p.additions.bounds;
        p.refinement_crop = p.additions.bounds;
        p.reading_context = p.additions.bounds;
        p.model_hole_margin_px = 0;
        let prepared = p.prepare(4000, 6000).unwrap();
        assert_eq!(prepared.corrected_base.bounds, p.additions.bounds);
        assert_eq!(prepared.corrected_base.count(), 1);
        assert_eq!(prepared.write_support, prepared.corrected_base);
    }

    #[test]
    fn padding_rebuild_2_5_2_is_byte_identical() {
        let mut p = plan(raster(Rect::new(3, 3, 14, 14), &[(4, 4), (14, 14)]));
        p.padding_px = 2;
        let first = p.prepare(20, 20).unwrap();
        p.padding_px = 5;
        let wide = p.prepare(20, 20).unwrap();
        assert!(wide.write_support.count() > first.write_support.count());
        p.padding_px = 2;
        let again = p.prepare(20, 20).unwrap();
        assert_eq!(again.write_support, first.write_support);
        assert_eq!(again.identity.support_sha256, first.identity.support_sha256);
    }

    #[test]
    fn round_center_distance_and_edges_preserve_topology() {
        let dot = raster(Rect::new(0, 0, 1, 1), &[(0, 0)]);
        let one = round_source_dilate(&dot, 1, 10, 10).unwrap();
        assert!(one.contains(1, 0));
        assert!(one.contains(0, 1));
        assert!(!one.contains(1, 1));
        let two =
            round_source_dilate(&raster(Rect::new(4, 4, 1, 1), &[(4, 4)]), 2, 10, 10).unwrap();
        assert!(two.contains(6, 4));
        assert!(!two.contains(6, 5));
        let separated = raster(Rect::new(1, 1, 9, 9), &[(1, 1), (9, 9)]);
        let grown = round_source_dilate(&separated, 1, 12, 12).unwrap();
        assert!(!grown.contains(5, 5));
        assert!(!grown.contains(2, 2));
        assert_eq!(grown.count(), 10);

        // A ring is kept as actual source pixels. Round padding may thicken
        // its strokes, but it must not fill a distant interior like a hull.
        let mut ring = Mask::empty(Rect::new(1, 1, 9, 9));
        for x in 1..10 {
            ring.set(x, 1, true);
            ring.set(x, 9, true);
        }
        for y in 1..10 {
            ring.set(1, y, true);
            ring.set(9, y, true);
        }
        let ring = round_source_dilate(&ring.into(), 2, 12, 12).unwrap();
        assert!(!ring.contains(5, 5));
    }

    #[test]
    fn empty_and_unsafe_masks_require_correction() {
        let p = plan(raster(Rect::new(0, 0, 10, 10), &[]));
        assert!(matches!(
            p.prepare(20, 20),
            Err(PlanError::NeedsCorrection(_))
        ));
        let mut p = plan(raster(Rect::new(0, 0, 1, 1), &[(0, 0)]));
        p.padding_px = MAX_PADDING_PX + 1;
        assert!(matches!(
            p.prepare(20, 20),
            Err(PlanError::NeedsCorrection(_))
        ));
        p.padding_px = 0;
        p.blend_alpha = Some(raster(Rect::new(0, 0, 2, 2), &[(1, 1)]));
        assert!(matches!(p.prepare(20, 20), Err(PlanError::Invalid(_))));
    }

    #[test]
    fn preparation_identity_changes_with_underlay_and_exact_support() {
        let mut p = plan(raster(Rect::new(4, 4, 1, 1), &[(4, 4)]));
        let a = p.prepare(20, 20).unwrap();
        p.lower_composite_sha256 = "new underlay".into();
        let b = p.prepare(20, 20).unwrap();
        assert_eq!(a.identity.support_sha256, b.identity.support_sha256);
        assert_ne!(a.identity.identity_sha256, b.identity.identity_sha256);
        p.padding_px = 2;
        let c = p.prepare(20, 20).unwrap();
        assert_ne!(b.identity.support_sha256, c.identity.support_sha256);
        assert_ne!(b.identity.identity_sha256, c.identity.identity_sha256);
        for y in c.write_support.bounds.y..c.write_support.bounds.bottom() {
            for x in c.write_support.bounds.x..c.write_support.bounds.right() {
                if c.write_support.contains(x, y) {
                    assert!(c.model_hole.contains(x, y));
                }
            }
        }
        assert!(c.verify_against(&p, 20, 20).is_ok());
        let mut tampered = c.clone();
        tampered.write_support.bits[0] ^= 255;
        assert!(tampered.verify_against(&p, 20, 20).is_err());
    }

    #[test]
    fn unknown_geometry_fails_while_missing_defaults_to_legacy() {
        #[derive(Deserialize)]
        struct Settings {
            #[serde(default)]
            geometry_policy: GeometryPolicy,
        }
        assert_eq!(
            serde_json::from_str::<Settings>("{}")
                .unwrap()
                .geometry_policy,
            GeometryPolicy::Legacy
        );
        assert!(serde_json::from_str::<Settings>(r#"{"geometry_policy":"future"}"#).is_err());
    }

    #[test]
    fn a_4000_by_6000_page_keeps_a_small_plan_region_bounded() {
        let mut p = plan(raster(
            Rect::new(1800, 2800, 128, 128),
            &[(1801, 2801), (1926, 2926)],
        ));
        p.candidate_bounds = Rect::new(1780, 2780, 168, 168);
        p.refinement_crop = p.candidate_bounds;
        p.reading_context = Rect::new(1200, 2200, 1400, 1400);
        p.padding_px = 5;
        let prepared = p.prepare(4000, 6000).unwrap();
        assert_eq!(
            prepared.write_support.bounds,
            Rect::new(1795, 2795, 138, 138)
        );
        let resident_mask_bytes = p.base_mask.bits.len()
            + p.additions.bits.len()
            + p.removals.bits.len()
            + prepared.corrected_base.bits.len()
            + prepared.write_support.bits.len()
            + prepared.model_hole.bits.len();
        assert!(resident_mask_bytes < 128 * 1024);
    }

    /// Run this under `/usr/bin/time -l` to measure an actual decoded page
    /// resident alongside the bounded mask plan on the current host.
    #[test]
    #[ignore = "manual 4000x6000 resident-memory measurement"]
    fn benchmark_4000_by_6000_decoded_page_and_prepared_mask() {
        use crate::image::{BitDepth, ColorMode, Raster};
        let page = Raster {
            width: 4000,
            height: 6000,
            mode: ColorMode::Gray,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
        color: Default::default(),
            data: vec![255; 4000 * 6000],
        };
        let mut p = plan(raster(
            Rect::new(1800, 2800, 128, 128),
            &[(1801, 2801), (1926, 2926)],
        ));
        p.candidate_bounds = Rect::new(1780, 2780, 168, 168);
        p.refinement_crop = p.candidate_bounds;
        p.reading_context = Rect::new(1200, 2200, 1400, 1400);
        p.padding_px = 5;
        let prepared = p.prepare(page.width, page.height).unwrap();
        assert_eq!(page.data.len(), 24_000_000);
        assert!(prepared.write_support.count() > 0);
        std::hint::black_box((&page, &prepared));
    }
}
