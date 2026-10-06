//! Detection: text boxes and a per-pixel mask.
//!
//! When selected, `comic_text_detector` returns three tensors and we use two
//! of them - the YOLO head and the segmentation mask. The mask is retained
//! past the auto pass for later review and mask fitting.
//!
//! Two rules from Phase 0 spike 6:
//!
//! - **Bind the outputs by channel count, not by index.** Upstream carries a
//!   defensive swap for exports that reverse `seg` and `det`, and the count is
//!   what tells them apart: the mask has one channel, the lines map has two.
//! - **The two box classes are languages**, `eng` and `ja`, not bubble kinds,
//!   and upstream annotates them `# cls could give wrong result`. They are
//!   carried through as a weak signal and nothing routes on them alone.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::image::Raster;
use crate::mask::Rect;

mod boxes;
mod letterbox;
mod nms;

pub use boxes::{
    BLOCK_LARGE_FACTOR, BLOCK_LARGE_HEIGHT_SHARE, BLOCK_LARGE_WIDTH_SHARE, MERGE_OVERLAP_SHARE,
    Region, SizeVerdict, TightBox, block_size_verdict, build as build_regions,
    build_separated as build_regions_separated, median_box_area, median_rect_area,
    overlaps_enough, size_verdict, sort_regions, tight_rect,
};
pub use letterbox::Letterbox;

/// Upstream's defaults, and they are defaults rather than discoveries: the
/// model was tuned with them.
pub const CONFIDENCE_THRESHOLD: f32 = 0.4;
pub const NMS_THRESHOLD: f32 = 0.35;
/// Above this, a segmentation pixel is text. Applied at native resolution,
/// after the mask has been resized back.
pub const MASK_THRESHOLD: u8 = 76; // 0.3 × 255

/// The language the detector guessed. Not load-bearing - the gate is a
/// separate model - but recorded, because a box the detector calls `eng`
/// and the gate calls Japanese is worth surfacing rather than averaging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedLanguage {
    English,
    Japanese,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetBox {
    /// In page coordinates, already back through the letterbox.
    pub rect: Rect,
    pub confidence: f32,
    pub language: DetectedLanguage,
}

/// The per-pixel segmentation mask, one byte per pixel, at **native**
/// resolution.
///
/// **A deviation, taken deliberately.** The usual approach grows the mask
/// candidates on the proxy and
/// upscales each one NEAREST, and then spends two of its six corrections
/// undoing the consequences: the ring has to be a native annulus rather than
/// the upscaled mask's contour, "which at k ≥ 2 lands on the anti-aliasing
/// fringe and biases the median 5–20 levels dark", and the periodicity check
/// has to run on a 2-D patch because "a NEAREST stair-step has a strong
/// periodic component at period *k*, so an FFT along the 1-px contour would
/// self-trigger on every downscaled page".
///
/// Growing natively removes both problems rather than compensating for them,
/// and it costs nothing that matters: growth runs per region on boxes of a few
/// hundred pixels, not per page. It was also measured - at proxy resolution the
/// line splitting in [`crate::gate`] flipped three of five regions from
/// `Vertical` to `Horizontal`, because a 55 px column is 23 proxy pixels and
/// the gaps between glyphs along the column survive the coarsening better than
/// the gaps between columns do.
///
/// §4's growth constants are proxy pixels, so [`Segmentation::proxy_scale`]
/// converts them.
///
/// Retention (§2a) is unaffected: what that section keeps small is the mask
/// **on disk**, and a bitonal proxy-resolution copy is what gets written there.
///
/// Kept as levels rather than thresholded: the threshold is one of the things a
/// re-run may change.
#[derive(Debug, Clone)]
pub struct Segmentation {
    pub width: u32,
    pub height: u32,
    pub levels: Vec<u8>,
    /// How the page was fitted for detection. Kept so a consumer can convert
    /// §4's proxy-pixel constants, and so the retained copy can be written back
    /// at proxy resolution.
    pub fit: Letterbox,
}

impl Segmentation {
    pub fn at(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            0
        } else {
            self.levels[(y as usize) * (self.width as usize) + x as usize]
        }
    }

    pub fn is_text(&self, x: u32, y: u32) -> bool {
        self.at(x, y) >= MASK_THRESHOLD
    }

    /// The segmentation as a binary lettering mask, 255 at or above
    /// [`MASK_THRESHOLD`]: the pixel evidence grouping reads where SAM-TS-L
    /// did not run.
    pub fn lettering_mask(&self) -> Vec<u8> {
        self.levels.iter().map(|&level| if level >= MASK_THRESHOLD { 255 } else { 0 }).collect()
    }

    /// How many native pixels one **proxy** pixel covers - `k` in §4's terms.
    /// A constant given in proxy pixels is multiplied by this.
    pub fn proxy_scale(&self) -> f32 {
        1.0 / self.fit.scale
    }
}

#[derive(Debug, Clone)]
pub struct Detection {
    pub boxes: Vec<DetBox>,
    pub segmentation: Segmentation,
}

#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error("the detector model could not be loaded: {0}")]
    Model(String),
    #[error("inference failed: {0}")]
    Inference(String),
    #[error("the model's outputs do not match the pinned contract: {0}")]
    Contract(String),
}

pub struct Detector {
    session: ort::session::Session,
    selection: crate::accel::Selection,
    /// This session's row in [`crate::registry`], which goes when the session
    /// does. It is read by [`Detector::spent`] and by nothing else here.
    lease: crate::registry::Lease,
}

impl Detector {
    /// Open the model. The ONNX Runtime must already be loaded -
    /// [`crate::runtime::load`] - because that failure has its own remedies and
    /// should not be reported as a model failure.
    ///
    /// `preference` is the user's setting. Left `Automatic` the detector lands
    /// on whichever provider was measured fastest for it and is present here -
    /// CoreML on Apple silicon ([`crate::accel::DETECTOR`]).
    pub fn open(model: &Path, preference: crate::accel::Preference) -> Result<Detector, DetectError> {
        let (session, selection) =
            crate::accel::open_session(model, &crate::accel::DETECTOR, preference, None)
                .map_err(|e| DetectError::Model(e.to_string()))?;
        let lease = crate::registry::register_named(
            crate::registry::Kind::TextDetector,
            crate::registry::Footprint::weights(model),
            crate::registry::Device::accelerator(selection.accelerator),
            Some("Comic Text Detector (CTD)".into()),
        );
        Ok(Detector { session, selection, lease })
    }

    /// Whether the owner should give this session back at its next safe point -
    /// somebody asked for it through the loaded-models tab, or nothing has
    /// wanted it for [`crate::registry::ONNX_IDLE_GRACE`]. Answering `true` costs
    /// the caller a reopen if a later page needs one, and that is the trade
    /// [`crate::registry`] documents.
    pub fn spent(&self) -> bool {
        self.lease.spent()
    }

    /// Give this session up for good, because it has failed in a way another
    /// run on it cannot survive - a reset adapter reported as device-removed.
    /// [`crate::registry::Lease::poison`] states what that costs and why it is
    /// not the same signal as an unload the user asked for.
    pub fn poison(&self) {
        self.lease.poison();
    }

    /// Where this session actually landed. Goes into every patch's provenance
    /// because a re-run on a different provider is not expected to
    /// reproduce the old one.
    pub fn selection(&self) -> &crate::accel::Selection {
        &self.selection
    }

    pub fn detect(&mut self, page: &Raster) -> Result<Detection, DetectError> {
        // The idle clock measures *work*, not wall time since the open.
        self.lease.touch();
        let (tensor, fit) = letterbox::to_tensor(page);
        let side = crate::constants::DETECTOR_INPUT as usize;

        let input = ort::value::Tensor::from_array(([1usize, 3, side, side], tensor))
            .map_err(|e| DetectError::Inference(e.to_string()))?;
        let outputs = self
            .session
            .run(ort::inputs![input])
            .map_err(|e| DetectError::Inference(e.to_string()))?;

        // Bound by shape, not by name or position - spike 6's rule.
        let mut head: Option<(Vec<i64>, Vec<f32>)> = None;
        let mut seg: Option<(Vec<i64>, Vec<f32>)> = None;
        for (_, value) in outputs.iter() {
            let (shape, data) = value
                .try_extract_tensor::<f32>()
                .map_err(|e| DetectError::Contract(e.to_string()))?;
            let shape: Vec<i64> = shape.iter().copied().collect();
            match shape.as_slice() {
                [1, _, 7] => head = Some((shape, data.to_vec())),
                [1, 1, _, _] => seg = Some((shape, data.to_vec())),
                // Two channels is the DBNet lines map. Unused (§2 uses the
                // first two outputs), and matched explicitly so an unexpected
                // fourth output is an error rather than a silent mask.
                [1, 2, _, _] => {}
                other => {
                    return Err(DetectError::Contract(format!("unexpected output shape {other:?}")));
                }
            }
        }

        let (head_shape, head_data) = head.ok_or_else(|| {
            DetectError::Contract("no [1, N, 7] detection head among the outputs".into())
        })?;
        let (seg_shape, seg_data) = seg.ok_or_else(|| {
            DetectError::Contract("no single-channel segmentation mask among the outputs".into())
        })?;

        let boxes = nms::decode(&head_data, head_shape[1] as usize, &fit, page.width, page.height);
        let segmentation = resize_segmentation(&seg_data, seg_shape[3] as usize, seg_shape[2] as usize, fit);

        Ok(Detection { boxes, segmentation })
    }
}

/// Crop the letterbox padding off the mask and resize it to the page.
///
/// Bilinear on the way out, matching upstream's `cv2.resize(..., INTER_LINEAR)`.
/// The padding is dropped first: a wide page leaves a black band down the right
/// of the model's output, and sampling it drags a dark edge into the mask -
/// which then reads as text and gets cleaned.
fn resize_segmentation(data: &[f32], w: usize, h: usize, fit: Letterbox) -> Segmentation {
    // Derived as a proportion rather than by subtracting the input's padding,
    // so a future export at half resolution does not silently sample it.
    let side = crate::constants::DETECTOR_INPUT as usize;
    let used_w = (w * fit.fitted_w as usize).div_ceil(side).max(1).min(w);
    let used_h = (h * fit.fitted_h as usize).div_ceil(side).max(1).min(h);

    let mut levels = vec![0u8; (fit.page_w * fit.page_h) as usize];
    let sx = used_w as f32 / fit.page_w as f32;
    let sy = used_h as f32 / fit.page_h as f32;

    for py in 0..fit.page_h {
        let fy = ((py as f32 + 0.5) * sy - 0.5).clamp(0.0, (used_h - 1) as f32);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(used_h - 1);
        let ty = fy - y0 as f32;
        for px in 0..fit.page_w {
            let fx = ((px as f32 + 0.5) * sx - 0.5).clamp(0.0, (used_w - 1) as f32);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(used_w - 1);
            let tx = fx - x0 as f32;

            let at = |x: usize, y: usize| data[y * w + x];
            let top = at(x0, y0) * (1.0 - tx) + at(x1, y0) * tx;
            let bottom = at(x0, y1) * (1.0 - tx) + at(x1, y1) * tx;
            let value = (top * (1.0 - ty) + bottom * ty).clamp(0.0, 1.0);
            levels[(py * fit.page_w + px) as usize] = (value * 255.0).round() as u8;
        }
    }

    Segmentation { width: fit.page_w, height: fit.page_h, levels, fit }
}

/* ------------------------------------------------------------------ */
/* One page, detected once                                             */
/* ------------------------------------------------------------------ */

/// The last page detected, and the source it was detected from.
///
/// **Detection is a pure function of the source pixels**, which is what makes a
/// memo honest here rather than a cache that can go stale: this application
/// never writes to a source, it writes patches beside one, so a page whose
/// bytes hash the same has the same text in it however many masks have been
/// laid over it. A
/// page edit therefore cannot invalidate this, and a source that really did
/// change hashes differently and misses.
///
/// **One entry**, replaced rather than accumulated. The segmentation is a
/// page-sized byte map - a megabyte or so for an ordinary page - and the
/// whole discipline is that nothing page-sized is held for longer than the
/// work that needs it. What
/// needs this is a *sequence of clicks on one page*, which is one page at a
/// time by construction.
/// The source's digest, and what detecting it produced.
type Remembered = Option<(String, Arc<Detection>)>;

static MEMO: OnceLock<Mutex<Remembered>> = OnceLock::new();

fn memo() -> MutexGuard<'static, Remembered> {
    MEMO.get_or_init(|| Mutex::new(None)).lock().unwrap_or_else(|e| e.into_inner())
}

/// Detect a page, or hand back the detection this source already produced.
///
/// `source` is a digest of the file's bytes - [`crate::ingest::sha256_hex`] of
/// what was read off disk, which every caller here already computes for the
/// patch's provenance.
///
/// This is what stops a region edit re-running the whole page's detector for
/// every click: cleaning six balloons the gate held back used to be six
/// full-page detections, and it is now one. The session reuse that makes
/// the *first* one cheap is
/// [`crate::residency`]'s; this is the inference output, which is the larger
/// half of the bill.
pub fn detect_page(
    detector: &mut Detector,
    source: &str,
    page: &Raster,
) -> Result<Arc<Detection>, DetectError> {
    if let Some((remembered, detection)) = memo().as_ref() {
        if remembered == source {
            return Ok(Arc::clone(detection));
        }
    }
    let detection = Arc::new(detector.detect(page)?);
    *memo() = Some((source.to_owned(), Arc::clone(&detection)));
    Ok(detection)
}

/// Forget the remembered detection if no text detector is resident any more.
///
/// A memo that outlived every session would be a page-sized buffer held by an
/// application that has just been told to give its memory back - the close
/// button in the loaded-models tab says *free this*, and answering it while
/// keeping the largest thing the model produced would be answering it in the
/// letter only. Called from [`crate::residency::sweep`], which is where a
/// session is actually given back.
pub fn forget_unbacked() {
    if !crate::registry::any_loaded(crate::registry::Kind::TextDetector) {
        *memo() = None;
    }
}

/// Forget it unconditionally. For the tests, and for a source that a caller
/// knows has been rewritten under it.
pub fn forget() {
    *memo() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "manual real CTD graph and ONNX runtime required"]
    fn native_webgpu_detector_runs_without_cpu_nodes() {
        let runtime = std::env::var("RT_RUNTIME").expect("RT_RUNTIME");
        let graph = std::env::var("RT_GRAPH").expect("RT_GRAPH");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let mut model = Detector::open(Path::new(&graph), crate::accel::Preference::Force(crate::accel::Accelerator::WebGpu)).unwrap();
        assert_eq!(model.selection().accelerator, crate::accel::Accelerator::WebGpu);
        let page = crate::image::fixtures::by_name("l8").raster;
        let result = model.detect(&page).unwrap();
        assert_eq!(result.segmentation.levels.len(), page.width as usize * page.height as usize);
    }

    /// The padding must not be sampled. A wide page leaves a black band down
    /// the right of the model's output, and a resize that includes it drags a
    /// dark edge into the mask - which then reads as text and gets cleaned.
    #[test]
    fn the_padding_is_dropped_before_the_resize() {
        let side = crate::constants::DETECTOR_INPUT as usize;
        let fit = Letterbox {
            scale: 1.0,
            fitted_w: (side / 2) as u32,
            fitted_h: side as u32,
            page_w: 400,
            page_h: 800,
        };
        let mut data = vec![0f32; side * side];
        for y in 0..side {
            for x in 0..side / 2 {
                data[y * side + x] = 1.0;
            }
        }
        let seg = super::resize_segmentation(&data, side, side, fit);
        assert_eq!((seg.width, seg.height), (400, 800));
        assert!(seg.levels.iter().all(|v| *v == 255), "the padding was sampled");
        assert!(seg.is_text(399, 799));
        // §4's constants are proxy pixels; this is what converts them.
        assert!((seg.proxy_scale() - 1.0).abs() < 1e-3);
    }

    /// Out-of-bounds coordinates must not wrap into the next row.
    #[test]
    fn segmentation_at_out_of_bounds_returns_zero_and_does_not_wrap() {
        let mut levels = vec![0u8; 100 * 100];
        levels[100] = 255; // pixel at (x=0, y=1)
        let seg = Segmentation {
            width: 100,
            height: 100,
            levels,
            fit: Letterbox::fit(100, 100),
        };
        // (100, 0) is out of horizontal bounds and must not wrap to (0, 1)
        assert_eq!(seg.at(100, 0), 0);
        assert!(!seg.is_text(100, 0));
        // (0, 100) is out of vertical bounds
        assert_eq!(seg.at(0, 100), 0);
        assert!(!seg.is_text(0, 100));
    }
}
