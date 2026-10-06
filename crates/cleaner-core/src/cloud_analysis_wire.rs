//! Independent, review-only analysis contract for `/mc/analysis/v1`.
//!
//! 1.1.0: tiles overlap ([`crate::cloud_tiles`]). A request names its page
//! size and the core of the tile the client keeps, both inside the request
//! digest, and a tile that is not on the page's plan is refused on both
//! sides. A 1.0.0 gateway, whose tiles did not overlap, is refused by version.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const VERSION: &str = "1.1.0";
pub const SAM: &str = "text_mask_sam_ts@1";
pub const RT: &str = "text_regions_rt@1";
pub const MAX_TILE_SIDE: u32 = 1024;
pub const MAX_TILE_PIXELS: u64 = 1_048_576;
pub const MAX_PNG_BYTES: usize = 4_194_304;
pub const MAX_COMPONENTS: usize = 4096;
pub const MAX_BOXES: usize = 4096;
pub const MAX_RESPONSE_BYTES: usize = 6_500_000;
pub const MAX_MASK_B64_BYTES: usize = 4 * MAX_PNG_BYTES.div_ceil(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TileRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl TileRect {
    pub fn validate(self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 || self.width > MAX_TILE_SIDE
            || self.height > MAX_TILE_SIDE || u64::from(self.width) * u64::from(self.height) > MAX_TILE_PIXELS
            || self.x.checked_add(self.width).is_none() || self.y.checked_add(self.height).is_none()
        {
            return Err("invalid analysis tile rect".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisRequest {
    pub protocol_version: String,
    pub capability: String,
    pub graph_sha256s: Vec<String>,
    pub model_revision: String,
    pub tile_id: String,
    pub tile_rect: TileRect,
    pub tile_png_sha256: String,
    pub source_page_sha256: String,
    pub page_width: u32,
    pub page_height: u32,
    /// The part of the tile whose answer the client keeps, in page
    /// coordinates: [`crate::cloud_tiles::core_of`] for this tile.
    pub tile_core: TileRect,
    pub request_digest: String,
}

fn hex_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

impl AnalysisRequest {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate_fields()?;
        serde_json::to_vec(&serde_json::json!([
            "MC-ANA-V1", self.protocol_version, self.capability, self.graph_sha256s,
            self.model_revision, self.tile_id, self.tile_rect.x, self.tile_rect.y,
            self.tile_rect.width, self.tile_rect.height, self.tile_png_sha256,
            self.source_page_sha256, self.page_width, self.page_height,
            self.tile_core.x, self.tile_core.y, self.tile_core.width, self.tile_core.height
        ])).map_err(|e| e.to_string())
    }

    pub fn digest(&self) -> Result<String, String> {
        Ok(format!("{:x}", Sha256::digest(self.canonical_bytes()?)))
    }

    fn validate_fields(&self) -> Result<(), String> {
        if self.protocol_version != VERSION || !matches!(self.capability.as_str(), SAM | RT) {
            return Err("unknown analysis version or capability".into());
        }
        let count = if self.capability == SAM { 2 } else { 1 };
        if self.graph_sha256s.len() != count || !self.graph_sha256s.iter().all(|s| hex_hash(s))
            || !matches!(self.model_revision.len(), 40 | 64)
            || !self.model_revision.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            || self.tile_id.is_empty() || self.tile_id.len() > 64
            || !self.tile_id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            || !hex_hash(&self.tile_png_sha256) || !hex_hash(&self.source_page_sha256)
        {
            return Err("invalid analysis identity".into());
        }
        self.tile_rect.validate()?;
        if crate::cloud_tiles::core_of(self.page_width, self.page_height, self.tile_rect) != Some(self.tile_core) {
            return Err("analysis tile is not on the page's tile plan".into());
        }
        Ok(())
    }

    pub fn validate(&self, tile_png: &[u8]) -> Result<(), String> {
        self.validate_fields()?;
        if !hex_hash(&self.request_digest) || self.digest()? != self.request_digest {
            return Err("analysis request digest mismatch".into());
        }
        if tile_png.len() > MAX_PNG_BYTES
            || format!("{:x}", Sha256::digest(tile_png)) != self.tile_png_sha256
        {
            return Err("analysis tile PNG digest or byte limit mismatch".into());
        }
        validate_png(tile_png, self.tile_rect, png::ColorType::Rgb)
    }
}

pub fn validate_png(bytes: &[u8], rect: TileRect, color: png::ColorType) -> Result<(), String> {
    if bytes.len() > MAX_PNG_BYTES { return Err("analysis PNG too large".into()); }
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(png::Limits { bytes: MAX_PNG_BYTES });
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    if info.width != rect.width || info.height != rect.height
        || info.color_type != color || info.bit_depth != png::BitDepth::Eight
        || info.interlaced || info.animation_control.is_some()
    {
        return Err("analysis PNG geometry or format mismatch".into());
    }
    let mut buffer = vec![0; reader.output_buffer_size().ok_or("PNG output too large")?];
    reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    reader.finish().map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisBox {
    pub rect: TileRect,
    pub class: u8,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisTimings {
    pub load_ms: u32,
    pub preprocess_ms: u32,
    pub inference_ms: u32,
    pub postprocess_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisResult {
    pub protocol_version: String,
    pub capability: String,
    pub request_digest: String,
    pub tile_id: String,
    pub tile_rect: TileRect,
    pub graph_sha256s: Vec<String>,
    pub model_revision: String,
    pub mask_png_b64: Option<String>,
    pub components: Vec<TileRect>,
    pub boxes: Vec<AnalysisBox>,
    pub timings: AnalysisTimings,
    pub reported_cost_usd: Option<f64>,
}

impl AnalysisResult {
    pub fn mask_bytes(&self) -> Result<Option<Vec<u8>>, String> {
        let Some(encoded) = &self.mask_png_b64 else { return Ok(None) };
        if encoded.len() > MAX_MASK_B64_BYTES || encoded.len() % 4 != 0 || !encoded.is_ascii() {
            return Err("analysis encoded mask too large or malformed".into());
        }
        let mut decoded = Vec::with_capacity(encoded.len() / 4 * 3);
        for (index, chunk) in encoded.as_bytes().chunks_exact(4).enumerate() {
            let last = index == encoded.len() / 4 - 1;
            let mut sextets = [0u8; 4];
            let mut padding = 0;
            for (offset, &byte) in chunk.iter().enumerate() {
                sextets[offset] = match byte {
                    b'A'..=b'Z' => byte - b'A',
                    b'a'..=b'z' => byte - b'a' + 26,
                    b'0'..=b'9' => byte - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    b'=' if last && offset >= 2 => { padding += 1; 0 },
                    _ => return Err("invalid analysis base64 mask".into()),
                };
            }
            if padding > 2 || (padding == 1 && chunk[3] != b'=')
                || (padding == 2 && (chunk[2] != b'=' || chunk[3] != b'='))
                || (padding == 1 && sextets[2] & 0x03 != 0)
                || (padding == 2 && sextets[1] & 0x0f != 0)
            { return Err("invalid analysis base64 padding".into()); }
            decoded.push((sextets[0] << 2) | (sextets[1] >> 4));
            if padding < 2 { decoded.push((sextets[1] << 4) | (sextets[2] >> 2)); }
            if padding == 0 { decoded.push((sextets[2] << 6) | sextets[3]); }
        }
        if decoded.len() > MAX_PNG_BYTES { return Err("analysis mask PNG too large".into()); }
        Ok(Some(decoded))
    }

    pub fn validate(&self, request: &AnalysisRequest) -> Result<(), String> {
        if self.protocol_version != VERSION || self.capability != request.capability
            || self.request_digest != request.request_digest || self.tile_id != request.tile_id
            || self.tile_rect != request.tile_rect || self.graph_sha256s != request.graph_sha256s
            || self.model_revision != request.model_revision
            || self.components.len() > MAX_COMPONENTS || self.boxes.len() > MAX_BOXES
            || self.reported_cost_usd.is_some_and(|v| !v.is_finite() || v < 0.0)
        { return Err("analysis result identity or limits mismatch".into()); }
        let inside = |r: TileRect| r.validate().is_ok() && r.x.checked_add(r.width).is_some_and(|v| v <= request.tile_rect.width)
            && r.y.checked_add(r.height).is_some_and(|v| v <= request.tile_rect.height);
        if !self.components.iter().copied().all(inside)
            || !self.boxes.iter().all(|b| inside(b.rect) && b.class <= 2 && b.score.is_finite() && (0.0..=1.0).contains(&b.score))
        { return Err("analysis result geometry invalid".into()); }
        if self.capability == SAM {
            if !self.boxes.is_empty() { return Err("SAM result contained boxes".into()); }
            let mask = self.mask_bytes()?.ok_or("SAM mask missing")?;
            validate_png(&mask, self.tile_rect, png::ColorType::Grayscale)?;
        } else if self.mask_png_b64.is_some() || !self.components.is_empty() {
            return Err("RT result contained mask or components".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisError {
    pub protocol_version: String,
    pub error_code: String,
    pub message: String,
    pub enqueued: bool,
    pub request_digest: Option<String>,
}

impl AnalysisError {
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != VERSION || self.enqueued
            || !matches!(self.error_code.as_str(), "unauthorized" | "invalid_request" | "payload_too_large" | "capability_unavailable" | "inference_failed")
            || self.message.is_empty() || self.message.len() > 1024
            || self.request_digest.as_ref().is_some_and(|digest| !hex_hash(digest))
        { return Err("invalid analysis rejection".into()); }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisCapability {
    pub capability: String,
    pub graph_sha256s: Vec<String>,
    pub model_revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisLimits {
    pub max_tile_side: u32,
    pub max_tile_pixels: u64,
    pub max_png_bytes: usize,
    pub max_components: usize,
    pub max_boxes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisCapabilities {
    pub protocol_version: String,
    pub capabilities: Vec<AnalysisCapability>,
    pub limits: AnalysisLimits,
}

impl AnalysisCapabilities {
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != VERSION || self.capabilities.len() > 2
            || self.limits.max_tile_side == 0 || self.limits.max_tile_side > MAX_TILE_SIDE
            || self.limits.max_tile_pixels == 0 || self.limits.max_tile_pixels > MAX_TILE_PIXELS
            || self.limits.max_png_bytes == 0 || self.limits.max_png_bytes > MAX_PNG_BYTES
            || self.limits.max_components == 0 || self.limits.max_components > MAX_COMPONENTS
            || self.limits.max_boxes == 0 || self.limits.max_boxes > MAX_BOXES
        { return Err("invalid analysis capabilities or limits".into()); }
        let mut seen = std::collections::HashSet::new();
        for capability in &self.capabilities {
            if !matches!(capability.capability.as_str(), SAM | RT)
                || !seen.insert(capability.capability.as_str())
                || capability.graph_sha256s.len() != if capability.capability == SAM { 2 } else { 1 }
                || !capability.graph_sha256s.iter().all(|s| hex_hash(s))
                || !matches!(capability.model_revision.len(), 40 | 64)
                || !capability.model_revision.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            { return Err("invalid analysis capability".into()); }
        }
        Ok(())
    }
}

/// A batch: several requests of one source page in one GPU job
/// ([`crate::cloud_job_wire::JobKind::AnalysisBatch`]), the mirror of
/// `validate_analysis_batch` in `deploy/cloud/common/contract.py`. A tile job
/// pays the job's own cost (spawn, queue wait, the job store) once per tile and
/// model, about 2.3 s on Modal against 0.05 to 0.7 s of GPU work; a batch pays
/// it once. Each distinct tile PNG is sent once, however many models read it,
/// and the answer is each request's own [`AnalysisResult`], in request order.
pub const BATCH_MAX_REQUESTS: usize = 16;
pub const BATCH_MAX_TILES: usize = 8;
pub const BATCH_MAX_TILE_BYTES: usize = 16_000_000;
pub const BATCH_MAX_BODY_BYTES: usize = 24_000_000;
pub const BATCH_MAX_RESPONSE_BYTES: usize = 24_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisBatch {
    pub protocol_version: String,
    pub request_digest: String,
    pub requests: Vec<AnalysisRequest>,
}

impl AnalysisBatch {
    pub fn new(requests: Vec<AnalysisRequest>) -> Self {
        let request_digest = batch_digest(requests.iter().map(|request| request.request_digest.as_str()));
        Self { protocol_version: VERSION.into(), request_digest, requests }
    }
}

/// SHA-256 over the requests' digests, in order, each followed by a newline.
pub fn batch_digest<'a>(request_digests: impl IntoIterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    for digest in request_digests {
        hasher.update(digest.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisBatchResult {
    pub protocol_version: String,
    pub request_digest: String,
    pub results: Vec<AnalysisResult>,
}

impl AnalysisBatchResult {
    /// Bound to `batch`: its digest, one result per request, each valid for
    /// the request in its place.
    pub fn validate(&self, batch: &AnalysisBatch) -> Result<(), String> {
        if self.protocol_version != VERSION || self.request_digest != batch.request_digest
            || self.results.len() != batch.requests.len()
        {
            return Err("analysis batch result identity mismatch".into());
        }
        self.results.iter().zip(&batch.requests).try_for_each(|(result, request)| result.validate(request))
    }
}

/// Cut `steps` (one page's requests in send order, each with its tile PNG; a
/// tile's requests are next to each other) into batches within the bounds
/// above, as index ranges. A tile's requests are never split across batches,
/// so no PNG is sent twice.
pub fn batches(steps: &[(&AnalysisRequest, &[u8])]) -> Vec<std::ops::Range<usize>> {
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let (mut tiles, mut bytes) = (0usize, 0usize);
    let mut start = 0;
    while start < steps.len() {
        let sha = &steps[start].0.tile_png_sha256;
        let end = start + steps[start..].iter().take_while(|(request, _)| &request.tile_png_sha256 == sha).count();
        let png_len = steps[start].1.len();
        let open = ranges.last().is_some_and(|range| range.end == start
            && range.len() + (end - start) <= BATCH_MAX_REQUESTS && tiles < BATCH_MAX_TILES
            && bytes + png_len <= BATCH_MAX_TILE_BYTES);
        if open {
            ranges.last_mut().unwrap().end = end;
            tiles += 1;
            bytes += png_len;
        } else {
            ranges.push(start..end);
            (tiles, bytes) = (1, png_len);
        }
        start = end;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_analysis_unauthorized_error_fixture() {
        let fixture = include_str!("../../../deploy/cloud/fixtures/analysis_v1/unauthorized_error.json");
        let rejection: AnalysisError = serde_json::from_str(fixture).unwrap();
        assert!(rejection.validate().is_ok());
        assert_eq!(rejection.error_code, "unauthorized");
        assert!(rejection.request_digest.is_none());
    }

    #[test]
    fn cloud_analysis_shared_failure_fixtures() {
        let unavailable: AnalysisError = serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/analysis_v1/capability_unavailable_error.json")).unwrap();
        unavailable.validate().unwrap();
        assert_eq!(unavailable.error_code, "capability_unavailable");
        assert!(unavailable.request_digest.is_none());

        let failed: AnalysisError = serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/analysis_v1/inference_failed_error.json")).unwrap();
        failed.validate().unwrap();
        assert_eq!(failed.error_code, "inference_failed");
        assert_eq!(failed.request_digest.as_deref(), Some("6b05d6efca8c347fe3ebb2ac8021c1c0584c13c1fab60559590e12045311d393"));
    }

    #[test]
    fn batch_digest_matches_the_gateway() {
        // The same vector is in deploy/cloud/tests/test_gpu_jobs.py.
        assert_eq!(batch_digest([&*"a".repeat(64), &*"b".repeat(64)]),
            "913f9338fb6c17253f3a14816fc08d52522454bfc19a8c99538a197cfb23fb41");
    }

    #[test]
    fn batches_keep_each_tile_whole_and_stay_in_bounds() {
        let fixtures: serde_json::Value =
            serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/analysis_v1/vectors.json")).unwrap();
        let base: AnalysisRequest = serde_json::from_value(fixtures["valid"][0]["request"].clone()).unwrap();
        let plan = |tiles: usize, models: usize, png_len: usize| {
            let pngs: Vec<Vec<u8>> = (0..tiles).map(|_| vec![0u8; png_len]).collect();
            let requests: Vec<AnalysisRequest> = (0..tiles).flat_map(|tile| (0..models).map(move |_| tile))
                .map(|tile| AnalysisRequest { tile_png_sha256: format!("{tile:064x}"), ..base.clone() }).collect();
            let steps: Vec<(&AnalysisRequest, &[u8])> = requests.iter().enumerate()
                .map(|(index, request)| (request, pngs[index / models].as_slice())).collect();
            batches(&steps)
        };
        // A 1080x1536 page: four tiles, both models, one batch.
        assert_eq!(plan(4, 2, 1_000_000), vec![0..8]);
        // Request and tile bounds; a tile's two requests stay together.
        assert_eq!(plan(10, 2, 1_000), vec![0..16, 16..20]);
        assert_eq!(plan(20, 1, 1_000), vec![0..8, 8..16, 16..20]);
        // The PNG byte bound.
        assert_eq!(plan(4, 2, 6_000_000), vec![0..4, 4..8]);
        assert!(plan(0, 2, 1).is_empty());
    }

    #[test]
    fn cloud_analysis_vectors() {
        let fixtures = include_str!("../../../deploy/cloud/fixtures/analysis_v1/vectors.json");
        let cases: serde_json::Value = serde_json::from_str(fixtures).unwrap();
        for case in cases["valid"].as_array().unwrap() {
            let req: AnalysisRequest = serde_json::from_value(case["request"].clone()).unwrap();
            assert_eq!(req.canonical_bytes().unwrap(), case["canonical"].as_str().unwrap().as_bytes());
            assert_eq!(req.digest().unwrap(), case["digest"].as_str().unwrap());
        }
        for case in cases["invalid"].as_array().unwrap() {
            let parsed = serde_json::from_value::<AnalysisRequest>(case.clone());
            assert!(match parsed { Err(_) => true, Ok(r) => r.digest().is_err() });
        }
    }

    #[test]
    fn cloud_analysis_capability_and_rejection_are_strict() {
        let advertised = AnalysisCapabilities {
            protocol_version: VERSION.into(),
            capabilities: vec![AnalysisCapability {
                capability: SAM.into(),
                graph_sha256s: vec!["a".repeat(64), "b".repeat(64)],
                model_revision: "c".repeat(40),
            }],
            limits: AnalysisLimits {
                max_tile_side: MAX_TILE_SIDE, max_tile_pixels: MAX_TILE_PIXELS,
                max_png_bytes: MAX_PNG_BYTES, max_components: MAX_COMPONENTS, max_boxes: MAX_BOXES,
            },
        };
        advertised.validate().unwrap();
        let mut invalid = serde_json::to_value(&advertised).unwrap();
        invalid["unexpected"] = serde_json::json!(1);
        assert!(serde_json::from_value::<AnalysisCapabilities>(invalid).is_err());
        let rejection = AnalysisError {
            protocol_version: VERSION.into(), error_code: "invalid_request".into(),
            message: "bad tile".into(), enqueued: false, request_digest: None,
        };
        rejection.validate().unwrap();
    }

    #[test]
    fn cloud_analysis_mask_geometry_is_bound_to_tile() {
        fn gray_png(width: u32, height: u32) -> Vec<u8> {
            let mut bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut bytes, width, height);
                encoder.set_color(png::ColorType::Grayscale);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().unwrap();
                writer.write_image_data(&vec![0; (width * height) as usize]).unwrap();
            }
            bytes
        }
        let fixture: serde_json::Value = serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/analysis_v1/vectors.json")).unwrap();
        let request: AnalysisRequest = serde_json::from_value(fixture["valid"][0]["request"].clone()).unwrap();
        let result = AnalysisResult {
            protocol_version: VERSION.into(), capability: SAM.into(),
            request_digest: request.request_digest.clone(), tile_id: request.tile_id.clone(),
            tile_rect: request.tile_rect, graph_sha256s: request.graph_sha256s.clone(),
            model_revision: request.model_revision.clone(), mask_png_b64: Some(encode_base64(&gray_png(16, 16))),
            components: vec![], boxes: vec![],
            timings: AnalysisTimings { load_ms: 0, preprocess_ms: 0, inference_ms: 0, postprocess_ms: 0 },
            reported_cost_usd: None,
        };
        result.validate(&request).unwrap();
        let mut oversized = result.clone();
        oversized.mask_png_b64 = Some(encode_base64(&gray_png(17, 16)));
        assert!(oversized.validate(&request).is_err());
    }

    #[test]
    fn cloud_analysis_shared_wire_response_is_bounded() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/analysis_v1/vectors.json")).unwrap();
        let request: AnalysisRequest = serde_json::from_value(fixture["valid"][0]["request"].clone()).unwrap();
        let response: AnalysisResult = serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/analysis_v1/sam_response.json")).unwrap();
        response.validate(&request).unwrap();
        assert_eq!(response.request_digest, fixture["valid_response"]["request_digest"].as_str().unwrap());
        assert_eq!(format!("{:x}", Sha256::digest(response.mask_bytes().unwrap().unwrap())),
            fixture["valid_response"]["mask_png_sha256"].as_str().unwrap());
        let mut too_large = response.clone();
        too_large.mask_png_b64 = Some("A".repeat(MAX_MASK_B64_BYTES + 4));
        assert!(too_large.validate(&request).is_err());
    }

    fn encode_base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let a = chunk[0];
            let b = *chunk.get(1).unwrap_or(&0);
            let c = *chunk.get(2).unwrap_or(&0);
            out.push(ALPHABET[(a >> 2) as usize] as char);
            out.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
            out.push(if chunk.len() > 1 { ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { ALPHABET[(c & 63) as usize] as char } else { '=' });
        }
        out
    }
}
