//! The script-identification model, and the convention it wants.
//!
//! `ogkalu/image-script-identification` - `osd_lstm.onnx`, 3.72 MB,
//! Apache-2.0, the only candidate with vertical-CJK labels. Its model card is
//! empty, so none of the following is documented anywhere; it comes from
//! `comic-translate`'s `script_detection.py`, which is the only implementation
//! that uses the file, and from measuring the result.
//!
//! - Input is `image float32[1, 1, 48, W]`: **one text line**, scaled to 48 px
//!   tall.
//! - A vertical line is rotated **counter-clockwise** first. The mirrored
//!   rotation does not fail - it returns `Hangul_vert` and `Fraktur` on
//!   vertical Japanese, confidently.
//! - A crop whose median is below 64 is inverted, so white-on-black text
//!   arrives in the polarity the model was trained on.
//! - Values are Tesseract's own black/white normalisation, not `0..1`:
//!   `(grey − black) / ((white − black) / 2) − 1`, with black and white taken
//!   from local minima and maxima along the **middle row only**.
//! - Output `scores float32[timesteps, 79]` is **CTC**, and class 2 - labelled
//!   `Broken` - is the blank. Averaging the field returns `Broken` for every
//!   input in every value range, which is what makes this worth writing down.

use std::path::Path;

use crate::gate::lines::Orientation;
use crate::image::Raster;
use crate::mask::Rect;

const STRIP_HEIGHT: usize = 48;
/// Unicharset index 0: an empty answer, not a script.
const NULL_CHAR: usize = 0;
/// Unicharset index 2, labelled `Broken`. The CTC blank.
const BLANK_CHAR: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum OsdError {
    #[error("the script-identification model could not be loaded: {0}")]
    Model(String),
    #[error("script identification failed: {0}")]
    Inference(String),
    #[error("the label list does not match the model: {labels} labels, {classes} classes")]
    Labels { labels: usize, classes: usize },
}

/// What one line came back as. `label` is the model's own vocabulary -
/// `Japanese_vert`, `Latin`, `HanS_vert` - and `strength` is how many
/// collapsed timesteps voted for it, which is the only confidence this model
/// offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineScript {
    pub label: String,
    pub strength: usize,
    /// Collapsed timesteps that voted for anything at all. A line where this is
    /// small said very little, whatever it said.
    pub total: usize,
}

pub struct Osd {
    session: ort::session::Session,
    labels: Vec<String>,
    selection: crate::accel::Selection,
    /// This session's row in [`crate::registry`]. See
    /// [`crate::detect::Detector`].
    lease: crate::registry::Lease,
}

impl Osd {
    /// 2 ms a line on the CPU against 8 ms on the GPU
    /// ([`crate::accel::SCRIPT_ID`]): a 48-px strip is dominated by the
    /// transfer, and the gate runs this once per text line.
    pub fn open(
        model: &Path,
        labels_json: &Path,
        preference: crate::accel::Preference,
    ) -> Result<Osd, OsdError> {
        let (session, selection) =
            crate::accel::open_session(model, &crate::accel::SCRIPT_ID, preference, None)
                .map_err(|e| OsdError::Model(e.to_string()))?;
        let raw = std::fs::read_to_string(labels_json).map_err(|e| OsdError::Model(e.to_string()))?;
        let labels = parse_labels(&raw);
        let lease = crate::registry::register_named(
            crate::registry::Kind::ScriptGate,
            crate::registry::Footprint::weights(model),
            crate::registry::Device::accelerator(selection.accelerator),
            Some("Image Script Identification OSD LSTM".into()),
        );
        Ok(Osd { session, labels, selection, lease })
    }

    pub fn selection(&self) -> &crate::accel::Selection {
        &self.selection
    }

    /// Whether the owner should give this session back at its next safe point.
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

    /// Identify one line. `None` means the model said nothing usable - too
    /// narrow to run, or every timestep was blank.
    pub fn identify(
        &mut self,
        page: &Raster,
        line: Rect,
        orientation: Orientation,
    ) -> Result<Option<LineScript>, OsdError> {
        self.lease.touch();
        let rotate = orientation == Orientation::Vertical;
        let (strip, width) = preprocess(page, line, rotate);
        if width < 3 {
            return Ok(None);
        }

        let input = ort::value::Tensor::from_array(([1usize, 1, STRIP_HEIGHT, width], strip))
            .map_err(|e| OsdError::Inference(e.to_string()))?;
        let outputs = self
            .session
            .run(ort::inputs![input])
            .map_err(|e| OsdError::Inference(e.to_string()))?;
        let (shape, scores) = outputs["scores"]
            .try_extract_tensor::<f32>()
            .map_err(|e| OsdError::Inference(e.to_string()))?;
        let classes = shape[shape.len() - 1] as usize;
        if classes != self.labels.len() {
            return Err(OsdError::Labels { labels: self.labels.len(), classes });
        }

        Ok(best_path(scores, classes).and_then(|(index, strength, total)| {
            if index == NULL_CHAR {
                None
            } else {
                self.labels
                    .get(index)
                    .map(|label| LineScript { label: label.clone(), strength, total })
            }
        }))
    }
}

/// A flat JSON array of strings. Parsed by hand rather than pulling a JSON
/// dependency into the core for one file with no nesting.
fn parse_labels(raw: &str) -> Vec<String> {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// CTC best path: argmax per timestep, collapse repeats, drop the blank, then
/// the most frequent label that remains. Returns `(index, its count, all
/// non-blank counts)`.
fn best_path(scores: &[f32], classes: usize) -> Option<(usize, usize, usize)> {
    let steps = scores.len() / classes;
    let mut counts: Vec<(usize, usize)> = Vec::new();
    let mut previous = BLANK_CHAR;

    for t in 0..steps {
        let row = &scores[t * classes..(t + 1) * classes];
        let best = row
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)?;
        if best != BLANK_CHAR && best != previous {
            match counts.iter_mut().find(|(i, _)| *i == best) {
                Some((_, n)) => *n += 1,
                None => counts.push((best, 1)),
            }
        }
        previous = best;
    }

    let total: usize = counts.iter().map(|(_, n)| n).sum();
    // Ties break on the lower unicharset index, so two runs of the same page
    // agree.
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    counts.first().map(|(index, strength)| (*index, *strength, total))
}

/// `(1, 1, 48, W)`, in the convention above.
fn preprocess(page: &Raster, line: Rect, rotate: bool) -> (Vec<f32>, usize) {
    let (src_w, src_h) = if rotate { (line.h, line.w) } else { (line.w, line.h) };
    if src_h == 0 || src_w == 0 {
        return (Vec::new(), 0);
    }
    let scale = STRIP_HEIGHT as f32 / src_h as f32;
    let out_w = ((src_w as f32 * scale).round() as usize).max(1);

    let mut grey = vec![0u8; STRIP_HEIGHT * out_w];
    for oy in 0..STRIP_HEIGHT {
        for ox in 0..out_w {
            let sx = (ox as f32 + 0.5) / scale - 0.5;
            let sy = (oy as f32 + 0.5) / scale - 0.5;
            // `np.rot90(m, k=1)` on a W×H source is `out[i, j] = m[j, W−1−i]`.
            // Here `src_h` is the rotated height, which is the original width,
            // so the output pixel `(sx, sy)` reads the original at row `sx`,
            // column `src_h − 1 − sy`.
            let (cx, cy) = if rotate {
                (src_h as f32 - 1.0 - sy, sx)
            } else {
                (sx, sy)
            };
            let px = (line.x as f32 + cx).clamp(0.0, (page.width - 1) as f32) as u32;
            let py = (line.y as f32 + cy).clamp(0.0, (page.height - 1) as f32) as u32;
            grey[oy * out_w + ox] = (page.luma16_at(px, py) >> 8) as u8;
        }
    }

    let mut sorted = grey.clone();
    sorted.sort_unstable();
    if (sorted[sorted.len() / 2] as f32) < 64.0 {
        for value in grey.iter_mut() {
            *value = 255 - *value;
        }
    }

    let (black, white) = black_white(&grey, out_w);
    let contrast = {
        let c = (white - black) / 2.0;
        if c <= 0.0 { 1.0 } else { c }
    };
    let normalised = grey.iter().map(|v| (*v as f32 - black) / contrast - 1.0).collect();
    (normalised, out_w)
}

/// Tesseract `networkio.cpp` `ComputeBlackWhite`, over the middle row only.
fn black_white(grey: &[u8], w: usize) -> (f32, f32) {
    let mut mins = [0i64; 256];
    let mut maxes = [0i64; 256];
    if w >= 3 {
        let row = &grey[(STRIP_HEIGHT / 2) * w..(STRIP_HEIGHT / 2 + 1) * w];
        for x in 1..w - 1 {
            let (previous, current, next) = (row[x - 1], row[x], row[x + 1]);
            if (current < previous && current <= next) || (current <= previous && current < next) {
                mins[current as usize] += 1;
            }
            if (current > previous && current >= next) || (current >= previous && current > next) {
                maxes[current as usize] += 1;
            }
        }
    }
    if mins.iter().sum::<i64>() == 0 {
        mins[0] = 1;
    }
    if maxes.iter().sum::<i64>() == 0 {
        maxes[255] = 1;
    }
    (ile(&mins, 0.25), ile(&maxes, 0.75))
}

/// Tesseract `STATS::ile`, over 256 buckets.
fn ile(buckets: &[i64; 256], fraction: f32) -> f32 {
    let total: i64 = buckets.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let target = (fraction * total as f32).clamp(1.0, total as f32);
    let mut sum = 0i64;
    let mut index = 0usize;
    while index <= 255 && (sum as f32) < target {
        sum += buckets[index];
        index += 1;
    }
    if index > 0 {
        index as f32 - (sum as f32 - target) / buckets[index - 1] as f32
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ctc_blank_is_not_a_verdict() {
        // Three timesteps, all blank. Nothing survives the collapse.
        let mut scores = vec![0f32; 3 * 5];
        for t in 0..3 {
            scores[t * 5 + BLANK_CHAR] = 1.0;
        }
        assert_eq!(best_path(&scores, 5), None);
    }

    #[test]
    fn repeats_collapse_and_the_majority_wins() {
        // 4, 4, blank, 4, 3 → 4 counted twice (the blank breaks the run), 3 once.
        let classes = 6;
        let path = [4usize, 4, BLANK_CHAR, 4, 3];
        let mut scores = vec![0f32; path.len() * classes];
        for (t, class) in path.iter().enumerate() {
            scores[t * classes + class] = 1.0;
        }
        let (index, strength, total) = best_path(&scores, classes).expect("a verdict");
        assert_eq!(index, 4);
        assert_eq!(strength, 2);
        assert_eq!(total, 3);
    }

    #[test]
    fn labels_parse_from_the_flat_array_the_model_ships() {
        let labels = parse_labels("[\"NULL\", \"Joined\", \"Broken\", \"Latin\"]");
        assert_eq!(labels, vec!["NULL", "Joined", "Broken", "Latin"]);
        assert_eq!(labels[BLANK_CHAR], "Broken", "the blank must be index 2");
    }
}
