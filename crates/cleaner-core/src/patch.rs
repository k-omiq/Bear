//! A patch: the pixels one region's edit produced, and the record of how.
//!
//! Patch layers, never a flattened page: the displayed page is
//! `raw + ordered visible patches`, and nothing ever writes back into the
//! source raster.

use crate::image::Raster;
use crate::mask::Mask;

/// Which rung produced a patch. `decline` is not here: a declined region has no
/// patch, which is the whole point of declining.
///
/// Serialised as the rung id - the suffix of [`Engine::rung_key`] - because that
/// is the `engine` field in a persisted patch record and the
/// `provenance.engine` the seam restates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// `denoise` is read as a fill: rung 1 (a fill smoothed together with the
    /// grain around it) was removed once pages were denoised whole, and a
    /// project saved before that still names it.
    #[serde(alias = "denoise")]
    Fill,
    Lama,
    Flux,
    Cloud,
    /// A brush stroke. **Not a rung**, and that is the whole of what separates
    /// it from the four rungs above: there is no ladder position to escalate from or
    /// to, no quality verdict, and nothing to fit - the user said where the
    /// paint goes and what colour it is, and that is authoritative. It is
    /// an [`Engine`] variant only because that is the field a patch records
    /// what made it in.
    Paint,
    /// A clone or heal stroke. A rung in exactly the same sense `Paint` is:
    /// none. The pixels are measured from the page rather than invented, which
    /// is why it is a separate word from `Paint` in the record.
    Clone,
}

impl Engine {
    /// The i18n key the seam carries for this rung. No English crosses the
    /// seam.
    pub fn rung_key(self) -> &'static str {
        match self {
            Engine::Fill => "ladder.rung.fill",
            Engine::Lama => "ladder.rung.lama",
            Engine::Flux => "ladder.rung.flux",
            Engine::Cloud => "ladder.rung.cloud",
            Engine::Paint => "ladder.rung.paint",
            Engine::Clone => "ladder.rung.clone",
        }
    }

    /// Whether this engine is a rung of the ladder rather than a hand tool
    /// that merely records itself in the same field.
    ///
    /// **The ladder's arithmetic has no answer for the two hand tools.**
    /// `rung()` is a total order the ceiling compares against, `step()` walks a
    /// Layers row's picker up and down it, and an automatic run escalates along
    /// it - none of which means anything for a stroke the user painted. Written
    /// as one predicate here rather than as an arm in each of those places, so
    /// a seventh rung and an eighth tool cannot drift apart.
    pub fn is_rung(self) -> bool {
        !matches!(self, Engine::Paint | Engine::Clone)
    }

    /// What the layer this engine produced may do once it exists.
    ///
    /// **Decided by the output, never by the gesture that asked for it.** A
    /// flat fill and a painted colour are given pixels: they mean the same
    /// thing wherever they sit, so they move, rotate and take a position lock.
    /// Everything else here was *read from where it is* - LaMa, FLUX and the
    /// cloud reconstruct the artwork under the mask, and clone and heal copy and blend
    /// the page at that spot - so the result is only true where it was made.
    /// A brush or shape stroke that ran one of those engines records that
    /// engine, which is what keeps its redraw fixed.
    pub fn layer_capabilities(self) -> LayerCapabilities {
        match self {
            Engine::Fill | Engine::Paint => LayerCapabilities {
                transform: LayerPlacement::Movable,
                lock: true,
                opacity: true,
            },
            Engine::Lama | Engine::Flux | Engine::Cloud | Engine::Clone => {
                LayerCapabilities { transform: LayerPlacement::Fixed, lock: false, opacity: true }
            }
        }
    }
}

/// Whether a layer's output can be placed somewhere else than it was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerPlacement {
    /// Given pixels: drag, rotate and nudge are all allowed.
    Movable,
    /// A redraw of what was under it. Stays where it is for good; a legacy
    /// layer that was moved before this rule keeps that geometry, frozen.
    Fixed,
    /// Nothing to place yet: a detection waiting to be cleaned.
    None,
}

/// What the interface may offer on a layer, and what the native side accepts.
///
/// Crosses the seam as `mask.capabilities`:
/// `{ transform: 'movable'|'fixed'|'none', lock: boolean, opacity: boolean }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerCapabilities {
    pub transform: LayerPlacement,
    /// Whether the optional position lock means anything here.
    pub lock: bool,
    pub opacity: bool,
}

impl LayerCapabilities {
    /// A detection has no output, so it has no output transform and no output
    /// opacity either.
    pub const DETECTION: LayerCapabilities =
        LayerCapabilities { transform: LayerPlacement::None, lock: false, opacity: false };
}

/// A layer change the capabilities do not allow. Each one is a catalogue key,
/// the way a decline reason is, so the refusal reaches the user by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LayerRefusal {
    /// A detection waiting to be cleaned has no output to style.
    #[error("masks.refused.noOutput")]
    NoOutput,
    /// Moving or rotating a redraw that is fixed where it was made.
    #[error("masks.refused.fixed")]
    Fixed,
    /// Moving or rotating a layer the user locked in place.
    #[error("masks.refused.locked")]
    Locked,
    /// A position lock on a layer that has no position to lock.
    #[error("masks.refused.noLock")]
    NoLock,
}

impl LayerRefusal {
    pub fn reason_key(self) -> &'static str {
        match self {
            LayerRefusal::NoOutput => "masks.refused.noOutput",
            LayerRefusal::Fixed => "masks.refused.fixed",
            LayerRefusal::Locked => "masks.refused.locked",
            LayerRefusal::NoLock => "masks.refused.noLock",
        }
    }
}

/// What a cloud request cost and where it went. Separate from the rest of the
/// record because it is absent for every local rung.
///
/// When deserializing legacy manifests, optional fields are omitted or null
/// without fabricating unmeasured data.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CloudRecord {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe_id: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Enough to reproduce a patch, or to know that it cannot be reproduced.
///
/// Field names are verbatim, because this record is
/// what gets persisted per patch and the seam contract restates the names.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Provenance {
    pub engine: Engine,
    pub engine_version: String,
    pub model_sha256: Option<String>,
    pub execution_provider: String,
    /// The thresholds, dilations and radii actually used - not the defaults.
    ///
    /// A JSON object, not a string holding JSON: it is a snapshot of
    /// values and `src/lib/model/types.js` declares it
    /// `Record<string, unknown>`, so a string here would reach both the
    /// manifest and the seam double-encoded.
    pub params_snapshot: serde_json::Value,
    pub mask_sha256: String,
    pub source_sha256: String,
    pub cloud: Option<CloudRecord>,
    /// Seconds since the Unix epoch. Stored as a number rather than formatted,
    /// for the same reason `RelativeTime` crosses the seam as data. The
    /// interface's own model declares this field an ISO 8601 string, so the
    /// adapter formats it.
    pub created: u64,
}

#[derive(Debug, Clone)]
pub struct Patch {
    pub id: String,
    /// The **applied** mask: post-growth, pre-isolation. The fidelity contract
    /// is stated over exactly this, and the two readings differ by up to
    /// 26 px on the inpaint path.
    pub mask: Mask,
    /// **The lettering this edit removed**: [`crate::fit::Fitted::ink`] for a
    /// rung, the stroke itself for a hand tool, and `mask` when nothing
    /// narrower is known. Usually a subset of `mask`, and on the fill path a
    /// much smaller one - rung 0 paints the whole grown balloon, and the grown
    /// balloon is not where the text was. The mask file an export writes
    /// beside a page is the union of these rather than of `mask`, because a
    /// typesetter asking "where was the text" wants the lettering, not the
    /// fill. Nothing else reads it: the compositor, the fidelity contract and
    /// a PSD's layer mask are all stated over `mask`.
    pub ink: Mask,
    /// Pixels covering `mask.bounds`, in the page's own mode and depth. A patch
    /// is never in a different colour space from the page it belongs to - that
    /// is what makes "no helpful promotion" enforceable.
    pub pixels: Raster,
    pub order: u32,
    pub visible: bool,
    pub provenance: Provenance,
}

impl Patch {
    /// Whether the patch's pixel buffer actually covers its mask.
    pub fn is_well_formed(&self) -> bool {
        self.pixels.width == self.mask.bounds.w && self.pixels.height == self.mask.bounds.h
    }

    /// Presentation settings survive in the patch's provenance snapshot. Old
    /// manifests have no `layer` field and render exactly as before.
    pub fn layer_style(&self) -> LayerStyle {
        LayerStyle::from_snapshot(&self.provenance.params_snapshot)
    }

    /// Move and rotate the saved pixels and masks together for every consumer
    /// of `Job::load_patch`: tiles, underlays, flattened and layered exports.
    pub fn presented(mut self) -> Self {
        let style = self.layer_style();
        if style.offset_x == 0 && style.offset_y == 0 && style.rotation == 0.0 {
            return self;
        }
        let source = self.mask.bounds;
        if source.w == 0 || source.h == 0 { return self; }
        let cx = source.x as f64 + source.w as f64 / 2.0;
        let cy = source.y as f64 + source.h as f64 / 2.0;
        let angle = style.rotation.to_radians();
        let (sin, cos) = angle.sin_cos();
        let bounds = style.display_bounds(source);
        let mut mask = crate::mask::Mask::empty(bounds);
        let mut ink = crate::mask::Mask::empty(bounds);
        let mut pixels = self.pixels.clone();
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.data = vec![0; crate::composite::blank_len(bounds.w, bounds.h, &self.pixels)];
        let channels = pixels.mode.samples();
        // Palette indices have no numerical order, so an indexed page keeps
        // the nearest source pixel. Every other mode is resampled bilinearly,
        // weighted by the source mask's coverage, so a turned edge stays
        // smooth instead of stair-stepping and pixels outside the mask never
        // bleed into it.
        let smooth = pixels.mode != crate::image::ColorMode::Indexed;
        let top = (1u32 << pixels.depth.bits().min(16)) as f64 - 1.0;
        let mut acc = vec![0.0f64; channels];
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                let world_x = bounds.x as f64 + x as f64 + 0.5 - style.offset_x as f64 - cx;
                let world_y = bounds.y as f64 + y as f64 + 0.5 - style.offset_y as f64 - cy;
                let fx = cx + world_x * cos + world_y * sin;
                let fy = cy - world_x * sin + world_y * cos;
                let (sx, sy) = (fx.floor() as i64, fy.floor() as i64);
                if self.ink.contains(sx, sy) {
                    ink.set(bounds.x + x as i64, bounds.y + y as i64, true);
                }
                if !smooth {
                    if self.mask.contains(sx, sy) {
                        mask.set(bounds.x + x as i64, bounds.y + y as i64, true);
                        let (lx, ly) = ((sx - source.x) as u32, (sy - source.y) as u32);
                        for channel in 0..channels {
                            pixels.set_sample(x, y, channel, self.pixels.sample(lx, ly, channel));
                        }
                    }
                    continue;
                }
                // Pixel centres sit on half coordinates.
                let (u, v) = (fx - 0.5, fy - 0.5);
                let (i0, j0) = (u.floor() as i64, v.floor() as i64);
                let (tx, ty) = (u - i0 as f64, v - j0 as f64);
                let mut covered = 0.0;
                acc.iter_mut().for_each(|value| *value = 0.0);
                for (i, j, weight) in [
                    (i0, j0, (1.0 - tx) * (1.0 - ty)),
                    (i0 + 1, j0, tx * (1.0 - ty)),
                    (i0, j0 + 1, (1.0 - tx) * ty),
                    (i0 + 1, j0 + 1, tx * ty),
                ] {
                    let weight = weight * f64::from(self.mask.coverage(i, j)) / 255.0;
                    if weight <= 0.0 {
                        continue;
                    }
                    covered += weight;
                    let (lx, ly) = ((i - source.x) as u32, (j - source.y) as u32);
                    for (channel, value) in acc.iter_mut().enumerate() {
                        *value += weight * f64::from(self.pixels.sample(lx, ly, channel));
                    }
                }
                let coverage = (covered * 255.0).round().clamp(0.0, 255.0) as u8;
                if coverage == 0 {
                    continue;
                }
                mask.set_coverage(bounds.x + x as i64, bounds.y + y as i64, coverage);
                for (channel, value) in acc.iter().enumerate() {
                    pixels.set_sample(x, y, channel, (value / covered).round().clamp(0.0, top) as u16);
                }
            }
        }
        self.mask = mask;
        self.ink = ink;
        self.pixels = pixels;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LayerStyle {
    pub opacity: u8,
    pub offset_x: i32,
    pub offset_y: i32,
    pub rotation: f64,
    pub locked: bool,
}

impl Default for LayerStyle {
    fn default() -> Self {
        Self { opacity: 100, offset_x: 0, offset_y: 0, rotation: 0.0, locked: false }
    }
}

impl LayerStyle {
    pub fn from_snapshot(snapshot: &serde_json::Value) -> Self {
        serde_json::from_value::<Self>(snapshot.get("layer").cloned().unwrap_or_default())
            .unwrap_or_default()
            .sanitized()
    }

    pub fn sanitized(mut self) -> Self {
        self.opacity = self.opacity.min(100);
        self.offset_x = self.offset_x.clamp(-10_000, 10_000);
        self.offset_y = self.offset_y.clamp(-10_000, 10_000);
        // A turn past half way is the same turn the other way round, so a
        // rotation handle dragged through 180 wraps rather than sticking. A
        // value already in range is kept exactly: rewriting a stored -180 as
        // 180 would move the rounding of every pixel it places.
        self.rotation = match self.rotation {
            angle if !angle.is_finite() => 0.0,
            angle if (-180.0..=180.0).contains(&angle) => angle,
            angle => {
                let wrapped = angle.rem_euclid(360.0);
                if wrapped > 180.0 { wrapped - 360.0 } else { wrapped }
            }
        };
        // One zero, so `-0.0` and `0.0` are one appearance.
        if self.rotation == 0.0 {
            self.rotation = 0.0;
        }
        self
    }

    /// Whether the layer is drawn anywhere but where it was made.
    pub fn is_transformed(self) -> bool {
        self.offset_x != 0 || self.offset_y != 0 || self.rotation != 0.0
    }

    fn same_placement(self, other: LayerStyle) -> bool {
        self.offset_x == other.offset_x
            && self.offset_y == other.offset_y
            && self.rotation == other.rotation
    }

    /// A deliberate edit: `self` is the stored style, `requested` what the
    /// interface asks for. Anything the capabilities do not allow is refused
    /// whole - a half-applied edit would be a change nobody asked for.
    ///
    /// The position lock guards a *locked* layer's geometry. A request that
    /// unlocks and moves in one call is the undo of a lock, and is let through.
    pub fn edited(
        self,
        requested: LayerStyle,
        capabilities: LayerCapabilities,
    ) -> Result<LayerStyle, LayerRefusal> {
        let current = self.sanitized();
        let requested = requested.sanitized();
        if capabilities.transform == LayerPlacement::None {
            return if requested == current { Ok(current) } else { Err(LayerRefusal::NoOutput) };
        }
        if requested.opacity != current.opacity && !capabilities.opacity {
            return Err(LayerRefusal::NoOutput);
        }
        if !requested.same_placement(current) {
            if capabilities.transform != LayerPlacement::Movable {
                return Err(LayerRefusal::Fixed);
            }
            if current.locked && requested.locked {
                return Err(LayerRefusal::Locked);
            }
        }
        if requested.locked != current.locked && !capabilities.lock {
            return Err(LayerRefusal::NoLock);
        }
        Ok(requested)
    }

    /// History replay: the style a snapshot recorded, as far as the
    /// capabilities allow it now. Whatever they do not allow keeps its stored
    /// value, so an undo never moves a fixed redraw and never fails half way
    /// through a history that predates the rule.
    pub fn restored(self, requested: LayerStyle, capabilities: LayerCapabilities) -> LayerStyle {
        let current = self.sanitized();
        let mut out = requested.sanitized();
        if !capabilities.opacity {
            out.opacity = current.opacity;
        }
        if capabilities.transform != LayerPlacement::Movable {
            out.offset_x = current.offset_x;
            out.offset_y = current.offset_y;
            out.rotation = current.rotation;
        }
        if !capabilities.lock {
            out.locked = current.locked;
        }
        out
    }

    pub fn display_bounds(self, source: crate::mask::Rect) -> crate::mask::Rect {
        if self.offset_x == 0 && self.offset_y == 0 && self.rotation == 0.0 { return source; }
        let cx = source.x as f64 + source.w as f64 / 2.0;
        let cy = source.y as f64 + source.h as f64 / 2.0;
        let (sin, cos) = self.rotation.to_radians().sin_cos();
        let corners = [
            (source.x as f64, source.y as f64),
            (source.right() as f64, source.y as f64),
            (source.x as f64, source.bottom() as f64),
            (source.right() as f64, source.bottom() as f64),
        ];
        let transformed = corners.map(|(x, y)| {
            let (dx, dy) = (x - cx, y - cy);
            (cx + dx * cos - dy * sin + self.offset_x as f64,
             cy + dx * sin + dy * cos + self.offset_y as f64)
        });
        let left = transformed.iter().map(|p| p.0).fold(f64::INFINITY, f64::min).floor() as i64;
        let top = transformed.iter().map(|p| p.1).fold(f64::INFINITY, f64::min).floor() as i64;
        let right = transformed.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max).ceil() as i64;
        let bottom = transformed.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max).ceil() as i64;
        crate::mask::Rect::new(left, top, (right - left) as u32, (bottom - top) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(opacity: u8, offset_x: i32, offset_y: i32, rotation: f64, locked: bool) -> LayerStyle {
        LayerStyle { opacity, offset_x, offset_y, rotation, locked }
    }

    #[test]
    fn a_saved_denoise_patch_reads_as_a_fill() {
        assert_eq!(serde_json::from_str::<Engine>("\"denoise\"").unwrap(), Engine::Fill);
        assert_eq!(serde_json::to_value(Engine::Fill).unwrap(), serde_json::json!("fill"));
    }

    #[test]
    fn capabilities_follow_the_output_not_the_gesture() {
        for engine in [Engine::Fill, Engine::Paint] {
            let caps = engine.layer_capabilities();
            assert_eq!(caps.transform, LayerPlacement::Movable, "{engine:?}");
            assert!(caps.lock && caps.opacity, "{engine:?}");
        }
        for engine in [Engine::Lama, Engine::Flux, Engine::Cloud, Engine::Clone] {
            let caps = engine.layer_capabilities();
            assert_eq!(caps.transform, LayerPlacement::Fixed, "{engine:?}");
            assert!(!caps.lock, "{engine:?} offered a lock");
            assert!(caps.opacity, "{engine:?} lost opacity");
        }
        let detection = LayerCapabilities::DETECTION;
        assert_eq!(detection.transform, LayerPlacement::None);
        assert!(!detection.lock && !detection.opacity);
        assert_eq!(
            serde_json::to_value(Engine::Lama.layer_capabilities()).unwrap(),
            serde_json::json!({"transform": "fixed", "lock": false, "opacity": true}),
        );
    }

    #[test]
    fn a_fixed_redraw_takes_opacity_and_refuses_every_move() {
        let caps = Engine::Lama.layer_capabilities();
        let stored = LayerStyle::default();
        assert_eq!(stored.edited(style(40, 0, 0, 0.0, false), caps), Ok(style(40, 0, 0, 0.0, false)));
        assert_eq!(stored.edited(style(100, 5, 0, 0.0, false), caps), Err(LayerRefusal::Fixed));
        assert_eq!(stored.edited(style(100, 0, 0, 12.0, false), caps), Err(LayerRefusal::Fixed));
        assert_eq!(stored.edited(style(100, 0, 0, 0.0, true), caps), Err(LayerRefusal::NoLock));
        assert_eq!(LayerRefusal::Fixed.to_string(), "masks.refused.fixed");
    }

    /// A redraw moved before the rule existed keeps that geometry: opacity
    /// still edits, the old move is neither reset nor editable.
    #[test]
    fn a_legacy_transformed_redraw_is_frozen_not_reset() {
        let caps = Engine::Flux.layer_capabilities();
        let legacy = style(100, 20, 12, 90.0, true);
        let faded = legacy.edited(style(50, 20, 12, 90.0, true), caps).unwrap();
        assert_eq!(faded, style(50, 20, 12, 90.0, true));
        assert_eq!(legacy.edited(LayerStyle::default(), caps), Err(LayerRefusal::Fixed));
        // History replay never fails and never moves it either.
        assert_eq!(legacy.restored(LayerStyle::default(), caps), style(100, 20, 12, 90.0, true));
    }

    #[test]
    fn a_movable_layer_moves_until_it_is_locked() {
        let caps = Engine::Paint.layer_capabilities();
        let free = style(100, 0, 0, 0.0, false);
        assert_eq!(free.edited(style(100, 30, -4, 45.0, false), caps), Ok(style(100, 30, -4, 45.0, false)));
        let locked = style(100, 30, -4, 45.0, true);
        assert_eq!(locked.edited(style(100, 31, -4, 45.0, true), caps), Err(LayerRefusal::Locked));
        assert_eq!(locked.edited(style(60, 30, -4, 45.0, true), caps), Ok(style(60, 30, -4, 45.0, true)));
        // Unlocking and moving together is the undo of a lock.
        assert_eq!(locked.edited(free, caps), Ok(free));
        assert_eq!(locked.restored(free, caps), free);
    }

    #[test]
    fn a_detection_has_no_output_to_style() {
        let caps = LayerCapabilities::DETECTION;
        let stored = LayerStyle::default();
        assert_eq!(stored.edited(style(50, 0, 0, 0.0, false), caps), Err(LayerRefusal::NoOutput));
        assert_eq!(stored.edited(style(100, 4, 0, 0.0, false), caps), Err(LayerRefusal::NoOutput));
        assert_eq!(stored.edited(stored, caps), Ok(stored));
        assert_eq!(stored.restored(style(50, 4, 0, 0.0, true), caps), stored);
    }

    #[test]
    fn a_rotation_past_half_a_turn_wraps_and_in_range_values_stay_exact() {
        assert_eq!(style(100, 0, 0, 190.0, false).sanitized().rotation, -170.0);
        assert_eq!(style(100, 0, 0, -190.0, false).sanitized().rotation, 170.0);
        assert_eq!(style(100, 0, 0, 540.0, false).sanitized().rotation, 180.0);
        assert_eq!(style(100, 0, 0, -180.0, false).sanitized().rotation, -180.0);
        assert_eq!(style(100, 0, 0, 180.0, false).sanitized().rotation, 180.0);
        assert!(style(100, 0, 0, -0.0, false).sanitized().rotation.is_sign_positive());
        assert_eq!(style(100, 0, 0, f64::NAN, false).sanitized().rotation, 0.0);
    }

    #[test]
    fn cloud_record_cost_none_serializes_explicit_null() {
        let record = CloudRecord {
            provider: "beam".to_string(),
            profile_id: Some("prof-1".to_string()),
            job_id: None,
            request_id: "req-123".to_string(),
            attempt_id: None,
            recipe_id: Some("flux-sdnq-v1".to_string()),
            model: "flux-schnell".to_string(),
            model_revision: None,
            tier: None,
            cost: None,
            duration_ms: Some(1200),
        };

        let json = serde_json::to_string(&record).unwrap();
        assert!(
            json.contains(r#""cost":null"#),
            "JSON must serialize explicit null cost: {json}"
        );

        let de: CloudRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(de.cost, None);
        assert_eq!(de.provider, "beam");
        assert_eq!(de.profile_id.as_deref(), Some("prof-1"));
        assert_eq!(de.recipe_id.as_deref(), Some("flux-sdnq-v1"));
    }

    #[test]
    fn cloud_record_cost_deserializes_missing_null_and_numeric() {
        // Missing cost field
        let missing_json = r#"{"provider":"modal","request_id":"req-1","model":"flux"}"#;
        let de_missing: CloudRecord = serde_json::from_str(missing_json).unwrap();
        assert_eq!(de_missing.cost, None);

        // Explicit null cost
        let null_json = r#"{"provider":"modal","request_id":"req-1","model":"flux","cost":null}"#;
        let de_null: CloudRecord = serde_json::from_str(null_json).unwrap();
        assert_eq!(de_null.cost, None);

        // Numeric cost
        let num_json = r#"{"provider":"modal","request_id":"req-1","model":"flux","cost":0.035}"#;
        let de_num: CloudRecord = serde_json::from_str(num_json).unwrap();
        assert_eq!(de_num.cost, Some(0.035));
    }
}
