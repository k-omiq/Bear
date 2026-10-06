//! Full ogkalu RT-DETR-v2 FP32 region path used by the chapter proof. Boxes
//! are review evidence only; they never become a pixel mask.
use std::path::Path;
use std::time::{Duration, Instant};

use crate::balloon::{BalloonBox, BalloonClass};
use crate::image::Raster;
use crate::mask::Rect;
use crate::sam_ts::{pillow_bilinear_rgb, Cancellation};

const INPUT: usize = 640;
const SCORE: f32 = 0.35;

pub struct FullRegions {
    session: ort::session::Session,
    lease: crate::registry::Lease,
    selection: crate::accel::Selection,
}

impl FullRegions {
    pub fn open_cpu(path: &Path) -> Result<(Self, Duration), String> {
        Self::open(path, crate::accel::Preference::CpuOnly)
    }

    pub fn open(path: &Path, preference: crate::accel::Preference) -> Result<(Self, Duration), String> {
        let started = Instant::now();
        let (session, selection) = crate::accel::open_session(path, &crate::accel::FULL_RT, preference, Some(4))
            .map_err(|e| format!("Ogkalu comic text & bubble detector (Full) graph: {e}"))?;
        let lease = crate::registry::register_named(crate::registry::Kind::BalloonDetector,
            crate::registry::Footprint::weights(path),
            crate::registry::Device::accelerator(selection.accelerator),
            Some("Ogkalu comic text & bubble detector (Full)".into()));
        Ok((Self { session, lease, selection }, started.elapsed()))
    }

    pub fn selection(&self) -> &crate::accel::Selection { &self.selection }

    /// The run may release this session at its next region boundary.
    pub fn spent(&self) -> bool { self.lease.spent() }

    /// Two contiguous vertical tiles with Pillow bilinear 640-square input,
    /// as in `spikes/chapter-rtdetr/compare_variants.py`. Outputs use source
    /// page coordinates. Odd heights put the extra row in the lower tile.
    pub fn detect_halves(&mut self, page: &Raster) -> Result<Vec<BalloonBox>, String> {
        self.detect_halves_cancellable(page, &Cancellation::default())
    }

    pub fn detect_halves_cancellable(&mut self, page: &Raster, cancel: &Cancellation) -> Result<Vec<BalloonBox>, String> {
        let height = page.height as usize;
        let middle = height / 2;
        let tiles = if middle == 0 {
            vec![(0, height)]
        } else {
            vec![(0, middle), (middle, height)]
        };
        self.detect_bands(page, tiles, cancel)
    }

    /// The whole raster as one 640-square input, as the cloud worker runs
    /// each tile. For offline comparison against the cloud plan.
    #[cfg(test)]
    pub(crate) fn detect_whole(&mut self, page: &Raster) -> Result<Vec<BalloonBox>, String> {
        self.detect_bands(page, vec![(0, page.height as usize)], &Cancellation::default())
    }

    fn detect_bands(&mut self, page: &Raster, tiles: Vec<(usize, usize)>, cancel: &Cancellation) -> Result<Vec<BalloonBox>, String> {
        cancel.check()?;
        self.lease.touch();
        if page.width == 0 || page.height == 0 {
            return Err("Ogkalu comic text & bubble detector needs a nonempty page".into());
        }
        let rgb = page.to_rgb8();
        let width = page.width as usize;
        let mut found = Vec::new();
        let options = cancel.options()?;
        for (top, bottom) in tiles {
            cancel.check()?;
            let tile_height = bottom - top;
            let source = &rgb[top * width * 3..bottom * width * 3];
            let resized = pillow_bilinear_rgb(source, width, tile_height, INPUT, INPUT);
            let mut chw = vec![0f32; 3 * INPUT * INPUT];
            for y in 0..INPUT {
                for x in 0..INPUT {
                    for c in 0..3 {
                        chw[c * INPUT * INPUT + y * INPUT + x] =
                            resized[(y * INPUT + x) * 3 + c] as f32 / 255.0;
                    }
                }
            }
            cancel.check()?;
            let images = ort::value::Tensor::from_array(([1usize, 3, INPUT, INPUT], chw))
                .map_err(|e| e.to_string())?;
            let sizes = ort::value::Tensor::from_array((
                [1usize, 2],
                vec![page.width as i64, tile_height as i64],
            ))
            .map_err(|e| e.to_string())?;
            let output = self
                .session
                .run_with_options(ort::inputs!["images" => images, "orig_target_sizes" => sizes], &options)
                .map_err(|e| if cancel.check().is_err() { "analysis cancelled".into() } else { format!("Ogkalu comic text & bubble detector inference: {e}") })?;
            cancel.check()?;
            let (_, labels) = output["labels"]
                .try_extract_tensor::<i64>()
                .map_err(|e| e.to_string())?;
            let (_, boxes) = output["boxes"]
                .try_extract_tensor::<f32>()
                .map_err(|e| e.to_string())?;
            let (_, scores) = output["scores"]
                .try_extract_tensor::<f32>()
                .map_err(|e| e.to_string())?;
            if labels.len() != scores.len() || boxes.len() != labels.len() * 4 {
                return Err("Ogkalu comic text & bubble detector labels, scores, and boxes disagree in length".into());
            }
            for i in 0..labels.len() {
                if scores[i] < SCORE || !scores[i].is_finite() {
                    continue;
                }
                let Some(class) = BalloonClass::from_label(labels[i]) else {
                    continue;
                };
                let raw = &boxes[4 * i..4 * i + 4];
                if raw.iter().any(|number| !number.is_finite()) {
                    continue;
                }
                let x1 = raw[0].round_ties_even().clamp(0.0, page.width as f32) as i64;
                let y1 =
                    raw[1].round_ties_even().clamp(0.0, tile_height as f32) as i64 + top as i64;
                let x2 = raw[2].round_ties_even().clamp(0.0, page.width as f32) as i64;
                let y2 =
                    raw[3].round_ties_even().clamp(0.0, tile_height as f32) as i64 + top as i64;
                if x2 <= x1 || y2 <= y1 {
                    continue;
                }
                found.push(BalloonBox {
                    rect: Rect::new(x1, y1, (x2 - x1) as u32, (y2 - y1) as u32),
                    class,
                    score: scores[i],
                });
            }
        }
        cancel.check()?;
        found.sort_by(|a, b| {
            a.rect
                .y
                .cmp(&b.rect.y)
                .then(a.rect.x.cmp(&b.rect.x))
                .then(b.score.total_cmp(&a.score))
        });
        self.lease.touch();
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numpy_box_rounding_is_even() {
        assert_eq!(2.5f32.round_ties_even(), 2.0);
        assert_eq!(3.5f32.round_ties_even(), 4.0);
    }

    #[test]
    #[ignore = "manual local ONNX runtime and full RT-DETR graph required"]
    fn forced_gpu_never_silently_uses_cpu() {
        let runtime = std::env::var("RT_RUNTIME").expect("RT_RUNTIME");
        let graph = std::env::var("RT_FULL_GRAPH").expect("RT_FULL_GRAPH");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let backend = match std::env::var("RT_BACKEND").as_deref() {
            Ok("coreml") => crate::accel::Accelerator::CoreMl,
            _ => crate::accel::Accelerator::WebGpu,
        };
        match FullRegions::open(Path::new(&graph), crate::accel::Preference::Force(backend)) {
            Ok((model, _)) => assert_eq!(model.selection().accelerator, backend),
            Err(error) => eprintln!("{backend:?} rejected the full graph without CPU fallback: {error}"),
        }
    }

    #[test]
    #[ignore = "manual local ONNX runtime and full RT-DETR graph required"]
    fn native_cpu_full_graph_runs_synthetic_page() {
        let runtime = std::env::var("RT_RUNTIME").expect("RT_RUNTIME");
        let graph = std::env::var("RT_FULL_GRAPH").expect("RT_FULL_GRAPH");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let (mut model, _) = FullRegions::open_cpu(Path::new(&graph)).unwrap();
        assert_eq!(model.selection().accelerator, crate::accel::Accelerator::Cpu);
        let page = crate::image::fixtures::by_name("l8").raster;
        let _boxes = model.detect_halves(&page).unwrap();
    }

    #[test]
    #[ignore = "requires the local full RT graph, ONNX Runtime, Pillow-decoded chapter PNGs, and saved proof"]
    fn chapter_reference_pages_keep_regions_and_context() {
        let runtime = std::env::var("RT_RUNTIME").expect("RT_RUNTIME is required");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let decoded = std::env::var("RT_DECODED_DIR").expect("RT_DECODED_DIR is required");
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/chapter-rtdetr");
        let graph = root.join("artifacts/models/detector.onnx");
        let (mut model, load) = FullRegions::open_cpu(&graph).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("artifacts/chapter-109-fp32-halves.json")).unwrap(),
        )
        .unwrap();
        for number in 1..=45 {
            let stem = format!("{number:02}");
            let page = crate::image::decode(
                &std::fs::read(Path::new(&decoded).join(format!("{stem}.png"))).unwrap(),
            )
            .unwrap();
            let started = Instant::now();
            let found = model.detect_halves(&page).unwrap();
            eprintln!("RT page {stem}: {:?}", started.elapsed());
            let reference = saved["pages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["file"].as_str() == Some(&format!("{stem}.jpg")))
                .unwrap();
            let boxes = reference["boxes"].as_array().unwrap();
            assert_eq!(
                found.len(),
                boxes.len(),
                "RT box count differs on page {stem}"
            );
            for (actual, expected) in found.iter().zip(boxes) {
                let kind = match actual.class {
                    BalloonClass::Bubble => "bubble",
                    BalloonClass::TextInBubble => "text_bubble",
                    BalloonClass::TextFree => "text_free",
                };
                assert_eq!(
                    kind,
                    expected["class"].as_str().unwrap(),
                    "RT class on page {stem}"
                );
                assert_eq!(
                    (
                        actual.rect.x,
                        actual.rect.y,
                        actual.rect.w as i64,
                        actual.rect.h as i64
                    ),
                    (
                        expected["x"].as_i64().unwrap(),
                        expected["y"].as_i64().unwrap(),
                        expected["width"].as_i64().unwrap(),
                        expected["height"].as_i64().unwrap()
                    ),
                    "RT coordinates on page {stem}"
                );
                let reference_score = expected["score"].as_f64().unwrap();
                assert!(
                    (actual.score as f64 - reference_score).abs() < 1e-4,
                    "RT score on page {stem}: {} versus {reference_score}",
                    actual.score
                );
            }
        }
        eprintln!("full RT cold load {load:?}, reference pages exact");
    }

    #[test]
    #[ignore = "requires the existing small RT graph, ONNX Runtime, and local chapter PNGs"]
    fn small_existing_cpu_chapter_metrics() {
        let runtime = std::env::var("RT_RUNTIME").expect("RT_RUNTIME is required");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let decoded = std::env::var("RT_DECODED_DIR").expect("RT_DECODED_DIR is required");
        let graph = std::env::var("RT_SMALL_GRAPH").expect("RT_SMALL_GRAPH is required");
        let started = Instant::now();
        let mut model = crate::balloon::BalloonDetector::open(
            Path::new(&graph),
            crate::accel::Preference::CpuOnly,
        )
        .unwrap();
        eprintln!("small RT cold load {:?}", started.elapsed());
        for number in 1..=45 {
            let stem = format!("{number:02}");
            let page = crate::image::decode(
                &std::fs::read(Path::new(&decoded).join(format!("{stem}.png"))).unwrap(),
            )
            .unwrap();
            let started = Instant::now();
            let boxes = model.detect(&page).unwrap();
            eprintln!(
                "small RT page {stem}: {:?}, {} boxes",
                started.elapsed(),
                boxes.len()
            );
        }
    }
}
