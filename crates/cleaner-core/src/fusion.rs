//! Source-coordinate review evidence. Detector regions and COO polygons
//! associate with SAM pixels; neither can create an erase pixel. Every SAM
//! component survives, including components with no detector support.
//!
//! The grouping a production run cleans by is [`crate::text_groups::group`],
//! and this view carries exactly that grouping: each component names its
//! group, and review reasons are the grouping's explicit, evidence-backed
//! ones. A component with no reason is a candidate awaiting selection, not a
//! problem.
use crate::balloon::{BalloonBox, BalloonClass};
use crate::detect::DetBox;
use crate::mask::Rect;
use crate::text_groups::{self, ModelUse, ReviewReason, TextGroup};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    pub id: String,
    pub bounds: Rect,
    pub pixels: u32,
    pub rt_text_ids: Vec<String>,
    pub rt_bubble_ids: Vec<String>,
    pub coo_ids: Vec<String>,
    /// Why this component needs a look, from evidence only. Empty is not
    /// flagged: every component is a candidate awaiting selection, and only a
    /// reason makes one a problem.
    pub review_reasons: Vec<ReviewReason>,
    /// The text group a production run would clean this component in.
    /// `None` for a speck no box claimed, which stays evidence only.
    pub group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    pub id: String,
    pub kind: &'static str,
    pub bounds: Rect,
    pub score: f32,
    pub component_ids: Vec<String>,
    pub detector_only: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub component_id: String,
    pub region_id: String,
    pub shared_pixels: u32,
    pub fraction_of_component: f32,
    pub fraction_of_region: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewEvidence {
    pub width: u32,
    pub height: u32,
    pub components: Vec<Component>,
    pub regions: Vec<Region>,
    pub links: Vec<Link>,
    pub grouping_suggestions: Vec<Vec<String>>,
    /// The text groups a production run would clean, in reading order.
    pub groups: Vec<TextGroup>,
    /// The models behind this evidence, with their spatial preprocessing.
    pub models: Vec<ModelUse>,
    /// The same models spelled out: checkpoint, pinned revision, execution,
    /// spatial input and preprocessing ([`ModelUse::description`]).
    pub model_descriptions: Vec<text_groups::ModelDescription>,
    /// The exact input mask is deliberately outside this structure. A caller
    /// retains it byte-for-byte for preview and explicit approval.
    pub mask_pixels: u64,
}

#[derive(Debug, Clone)]
pub struct CooProposal {
    pub polygon: Vec<(i64, i64)>,
    pub score: f32,
}

fn polygon_contains(polygon: &[(i64, i64)], x: i64, y: i64) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let (x, y) = (x as f64 + 0.5, y as f64 + 0.5);
    let mut inside = false;
    for i in 0..polygon.len() {
        let (ax, ay) = polygon[i];
        let (bx, by) = polygon[(i + 1) % polygon.len()];
        if (ay as f64 > y) != (by as f64 > y) {
            let cross = ax as f64 + (y - ay as f64) * (bx - ax) as f64 / (by - ay) as f64;
            if x < cross {
                inside = !inside;
            }
        }
    }
    inside
}

fn polygon_bounds(polygon: &[(i64, i64)]) -> Rect {
    let left = polygon.iter().map(|p| p.0).min().unwrap_or(0);
    let right = polygon.iter().map(|p| p.0).max().unwrap_or(left);
    let top = polygon.iter().map(|p| p.1).min().unwrap_or(0);
    let bottom = polygon.iter().map(|p| p.1).max().unwrap_or(top);
    Rect::new(
        left,
        top,
        (right - left + 1).max(0) as u32,
        (bottom - top + 1).max(0) as u32,
    )
}

/// Join any subset of the three capabilities. `mask = None` makes a regions
/// only result; `boxes = []` makes a mask-only result. COO is caller supplied
/// and has no download or model dependency here.
pub fn fuse(
    width: u32,
    height: u32,
    mask: Option<&[u8]>,
    boxes: &[BalloonBox],
    coo: &[CooProposal],
) -> Result<ReviewEvidence, String> {
    let mut inputs = text_groups::Inputs::new(width, height, mask);
    inputs.rt = boxes;
    fuse_grouped(&inputs, coo)
}

/// [`fuse`] over the full grouping inputs: CTD boxes kept apart from Ogkalu's
/// so their precedence is the production one, and the run's models and seams.
pub fn fuse_grouped(
    inputs: &text_groups::Inputs,
    coo: &[CooProposal],
) -> Result<ReviewEvidence, String> {
    let (width, height, mask) = (inputs.width, inputs.height, inputs.mask);
    let length = width as usize * height as usize;
    if width == 0 || height == 0 || length > 100_000_000 {
        return Err("fusion page dimensions are invalid or too large".into());
    }
    if let Some(mask) = mask {
        if mask.len() != length || mask.iter().any(|pixel| *pixel != 0 && *pixel != 255) {
            return Err(format!("{} mask dimensions or binary values are invalid", inputs.pixel_model.name()));
        }
    }
    let grouping = text_groups::group(inputs)?;
    let labels = grouping.labels();
    let mut components: Vec<Component> = grouping
        .components
        .iter()
        .map(|component| Component {
            id: component.id.clone(),
            bounds: component.bounds,
            pixels: component.pixels,
            rt_text_ids: Vec::new(),
            rt_bubble_ids: Vec::new(),
            coo_ids: Vec::new(),
            review_reasons: component.reasons.clone(),
            group_id: component.group.clone(),
        })
        .collect();
    // A group's own reasons (a text box with nothing under it has no
    // component to carry one) stay on the group; a component inherits the
    // reasons of the group it belongs to that are about lettering.
    for group in &grouping.groups {
        for id in &group.component_ids {
            let index = text_groups::component_label(id).and_then(|label| (label as usize).checked_sub(1));
            if let Some(component) = index.and_then(|index| components.get_mut(index)) {
                for reason in &group.reasons {
                    if *reason != ReviewReason::MaskMissingUnderTextBox && !component.review_reasons.contains(reason) {
                        component.review_reasons.push(*reason);
                    }
                }
                component.review_reasons.sort();
            }
        }
    }

    let mut regions = Vec::new();
    let mut links = Vec::new();
    let labeled: Vec<(String, Rect, &'static str, f32)> = inputs
        .rt
        .iter()
        .enumerate()
        .map(|(i, box_)| {
            let kind = match box_.class {
                BalloonClass::Bubble => "bubble_context",
                BalloonClass::TextInBubble => "text_bubble",
                BalloonClass::TextFree => "text_free",
            };
            (format!("rt-{i:04}"), box_.rect, kind, box_.score)
        })
        .chain(inputs.ctd.iter().enumerate().map(|(i, item): (usize, &DetBox)| {
            (format!("ctd-{i:04}"), item.rect, "ctd_text", item.confidence)
        }))
        .collect();
    for (id, rect, kind, score) in labeled {
        let coverage = |x: i64, y: i64| rect.contains(x, y);
        let (component_ids, matches) =
            associate(width, height, rect, labels, &components, &coverage);
        for (component, shared, region_pixels) in matches {
            let target = &mut components[component];
            if kind == "bubble_context" {
                target.rt_bubble_ids.push(id.clone());
            } else {
                target.rt_text_ids.push(id.clone());
            }
            links.push(Link {
                component_id: target.id.clone(),
                region_id: id.clone(),
                shared_pixels: shared,
                fraction_of_component: shared as f32 / target.pixels as f32,
                fraction_of_region: shared as f32 / region_pixels as f32,
            });
        }
        regions.push(Region {
            id,
            kind,
            bounds: rect,
            score,
            detector_only: component_ids.is_empty() && kind != "bubble_context",
            component_ids,
        });
    }
    let mut grouping_suggestions = Vec::new();
    for (i, proposal) in coo.iter().enumerate() {
        if proposal.polygon.len() < 3 || !proposal.score.is_finite() {
            continue;
        }
        let id = format!("coo-{i:04}");
        let coverage = |x: i64, y: i64| polygon_contains(&proposal.polygon, x, y);
        let bounds = polygon_bounds(&proposal.polygon);
        let (component_ids, matches) =
            associate(width, height, bounds, labels, &components, &coverage);
        for (component, shared, region_pixels) in matches {
            let target = &mut components[component];
            target.coo_ids.push(id.clone());
            links.push(Link {
                component_id: target.id.clone(),
                region_id: id.clone(),
                shared_pixels: shared,
                fraction_of_component: shared as f32 / target.pixels as f32,
                fraction_of_region: shared as f32 / region_pixels as f32,
            });
        }
        if component_ids.len() > 1 {
            grouping_suggestions.push(component_ids.clone());
        }
        regions.push(Region {
            id,
            kind: "sfx_group",
            bounds,
            score: proposal.score,
            detector_only: component_ids.is_empty(),
            component_ids,
        });
    }
    Ok(ReviewEvidence {
        width,
        height,
        mask_pixels: mask.map_or(0, |m| m.iter().filter(|x| **x != 0).count() as u64),
        components,
        regions,
        links,
        grouping_suggestions,
        models: grouping.models.clone(),
        model_descriptions: text_groups::describe_models(&grouping.models),
        groups: grouping.groups,
    })
}

fn associate(
    width: u32,
    height: u32,
    bounds: Rect,
    labels: &[u32],
    components: &[Component],
    contains: &impl Fn(i64, i64) -> bool,
) -> (Vec<String>, Vec<(usize, u32, u32)>) {
    if components.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut shared = BTreeMap::<usize, u32>::new();
    let mut region_pixels = 0u32;
    let left = bounds.x.max(0).min(width as i64) as u32;
    let top = bounds.y.max(0).min(height as i64) as u32;
    let right = bounds.right().max(0).min(width as i64) as u32;
    let bottom = bounds.bottom().max(0).min(height as i64) as u32;
    for y in top..bottom {
        for x in left..right {
            if contains(x as i64, y as i64) {
                region_pixels += 1;
                let label = labels[y as usize * width as usize + x as usize];
                if label != 0 {
                    *shared.entry(label as usize - 1).or_default() += 1;
                }
            }
        }
    }
    let ids = shared
        .keys()
        .map(|index| components[*index].id.clone())
        .collect();
    let matches = shared
        .into_iter()
        .map(|(index, pixels)| (index, pixels, region_pixels))
        .collect();
    (ids, matches)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_capabilities_keep_every_component_and_detector_only() {
        let mut mask = vec![0; 8 * 5];
        mask[1] = 255;
        mask[10] = 255; // 8-connected one component
        mask[6] = 255; // unsupported component
        let boxes = [
            BalloonBox {
                rect: Rect::new(0, 0, 3, 3),
                class: BalloonClass::TextFree,
                score: 0.9,
            },
            BalloonBox {
                rect: Rect::new(4, 3, 2, 2),
                class: BalloonClass::TextInBubble,
                score: 0.8,
            },
        ];
        let mask_only = fuse(8, 5, Some(&mask), &[], &[]).unwrap();
        assert_eq!(mask_only.components.len(), 2);
        // Specks under the layout floor are evidence only: no group, no flag.
        assert!(mask_only.components.iter().all(|c| c.review_reasons.is_empty() && c.group_id.is_none()));
        let regions_only = fuse(8, 5, None, &boxes, &[]).unwrap();
        assert_eq!(
            regions_only
                .regions
                .iter()
                .filter(|r| r.detector_only)
                .count(),
            2
        );
        let fused = fuse(8, 5, Some(&mask), &boxes, &[]).unwrap();
        assert_eq!(fused.components.len(), 2);
        assert_eq!(fused.regions.iter().filter(|r| r.detector_only).count(), 1);
        assert_eq!(fused.mask_pixels, 3);
    }

    #[test]
    fn coo_only_suggests_grouping_and_never_adds_mask_pixels() {
        let mut mask = vec![0; 10 * 10];
        mask[2 * 10 + 2] = 255;
        mask[2 * 10 + 6] = 255;
        let proposal = CooProposal {
            polygon: vec![(1, 1), (8, 1), (8, 4), (1, 4)],
            score: 0.6,
        };
        let original = mask.clone();
        let fused = fuse(10, 10, Some(&mask), &[], &[proposal]).unwrap();
        assert_eq!(fused.components.len(), 2);
        assert_eq!(
            fused.grouping_suggestions,
            vec![vec!["sam-00001".to_owned(), "sam-00002".to_owned()]]
        );
        assert_eq!(fused.mask_pixels, 2);
        assert_eq!(mask, original);
        assert!(!fused.regions[0].detector_only);
    }

    /// The analysis preview's answer names each model by checkpoint, pinned
    /// revision, execution and spatial input, as serialized for the window.
    #[test]
    fn the_preview_names_its_exact_models() {
        let mut mask = vec![0; 16 * 16];
        mask[3 * 16 + 3] = 255;
        let mut inputs = text_groups::Inputs::new(16, 16, Some(&mask));
        inputs.models = vec![
            ModelUse::new(text_groups::EvidenceModel::SamTsL, text_groups::Execution::Cloud,
                text_groups::SpatialInput::OverlappingCloudTiles),
            ModelUse::new(text_groups::EvidenceModel::OgkaluFull, text_groups::Execution::Local,
                text_groups::SpatialInput::Halves),
        ];
        let fused = fuse_grouped(&inputs, &[]).unwrap();
        let json = serde_json::to_value(&fused).unwrap();
        let described = &json["modelDescriptions"];
        assert_eq!(described[0]["model"], "samTsL");
        assert_eq!(described[0]["name"], "SAM-TS-L lettering mask");
        assert_eq!(described[0]["checkpoint"], "mayocream/koharu-text-sam-ts-l");
        assert_eq!(described[0]["revision"], "5dd97423e0fbf2404264979136d47e8101144046");
        assert_eq!(described[0]["execution"], "cloud");
        assert!(described[0]["spatial"].as_str().unwrap().starts_with("cloud tiles 1.1.0"), "{described}");
        assert_eq!(described[1]["checkpoint"], "ogkalu/comic-text-and-bubble-detector detector.onnx (RT-DETR v2, FP32)");
        assert_eq!(described[1]["revision"], "16e8a622f91fabc6b5b65c96d32d1183f8843546");
        assert_eq!(described[1]["execution"], "local");
        // The bare tags the window already reads are unchanged.
        assert_eq!(json["models"][0], serde_json::json!({
            "model": "samTsL", "execution": "cloud", "spatial": "overlappingCloudTiles" }));
        let text = json.to_string();
        assert!(!text.contains("koharu-layout-rfdetr-seg-2xl-1152") && !text.contains("manga-text-segmentation-2025"));
    }
}
