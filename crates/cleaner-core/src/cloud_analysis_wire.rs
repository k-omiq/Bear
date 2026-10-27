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
/// Listed among the gateway's `/capabilities` operations when its worker takes
/// a tile as an 8-bit grey PNG as well as RGB (`contract.py`
/// `ANALYSIS_GRAY_TILES_OPERATION`). The worker expands grey to RGB before the
/// models read it, so a tile whose every pixel is grey reaches them unchanged
/// in under half the bytes. Without it every tile is sent as RGB.
pub const GRAY_TILES_OPERATION: &str = "analysis.gray_tiles";

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
        validate_png(tile_png, self.tile_rect, &[png::ColorType::Rgb, png::ColorType::Grayscale])
    }
}

pub fn validate_png(bytes: &[u8], rect: TileRect, colors: &[png::ColorType]) -> Result<(), String> {
    if bytes.len() > MAX_PNG_BYTES { return Err("analysis PNG too large".into()); }
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(png::Limits { bytes: MAX_PNG_BYTES });
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    if info.width != rect.width || info.height != rect.height
        || !colors.contains(&info.color_type) || info.bit_depth != png::BitDepth::Eight
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
            validate_png(&mask, self.tile_rect, &[png::ColorType::Grayscale])?;
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
/// so no PNG is sent twice. Neighbouring tiles of the same pixels (a blank
/// page) share a digest and so travel together, but never past the request
/// bound: a run of them is cut at it, which keeps a tile's requests together
/// while the model count divides the bound.
pub fn batches(steps: &[(&AnalysisRequest, &[u8])]) -> Vec<std::ops::Range<usize>> {
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let (mut tiles, mut bytes) = (0usize, 0usize);
    let mut start = 0;
    while start < steps.len() {
        let sha = &steps[start].0.tile_png_sha256;
        let end = start + steps[start..].iter().take_while(|(request, _)| &request.tile_png_sha256 == sha)
            .take(BATCH_MAX_REQUESTS).count();
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

/// Listed among the gateway's `/capabilities` operations, beside
/// `analysis.batches`, when a batch may carry its tiles as one canvas
/// (`contract.py` `ANALYSIS_BATCH_CANVAS_OPERATION`). Neighbouring tiles
/// overlap ([`crate::cloud_tiles`]), so four 1024 tiles of a 1080x1536 page
/// carry 4.19 Mpx for a 1.66 Mpx page; a canvas carries each pixel once. The
/// gateway cuts every tile out of it and makes the tile's
/// [`canonical_tile_png`], whose digest the request names, so the requests,
/// their digests and the models' input are those of the tiles. Without the
/// operation every tile PNG is sent, as before.
pub const BATCH_CANVAS_OPERATION: &str = "analysis.batch_canvas";
/// Listed beside [`BATCH_CANVAS_OPERATION`] when the canvas may also be a
/// lossless WebP (`canvas_webp_b64`, `contract.py`
/// `ANALYSIS_BATCH_CANVAS_WEBP_OPERATION`). Scans are noisy and nearly grey:
/// WebP takes out what the colour channels share, which PNG cannot, so a
/// 1080x1536 page of `p19/c23` fell from 2.11 MB to 1.23 MB. The gateway cuts
/// the same canonical tiles out of either, so nothing past it changes.
pub const BATCH_CANVAS_WEBP_OPERATION: &str = "analysis.batch_canvas_webp";
/// The largest canvas, in pixels, a gateway decodes.
pub const BATCH_MAX_CANVAS_PIXELS: u64 = 16 * 1024 * 1024;

/// The one PNG of a tile's pixels (`rgb`, three bytes a pixel) that both sides
/// make: 8-bit grey when every pixel is grey, else RGB, an `sRGB` chunk, and
/// one `IDAT` of unfiltered rows in stored (uncompressed) deflate blocks of at
/// most 65535 bytes. Nothing in it depends on an encoder, so the gateway
/// rebuilds it byte for byte from the pixels it cuts out of a canvas
/// (`contract.py` `analysis_canonical_tile_png`). A 1024 RGB tile is about
/// 3.15 MB, inside [`MAX_PNG_BYTES`]; it travels only when a canvas would be
/// larger.
pub fn canonical_tile_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 || width > MAX_TILE_SIDE || height > MAX_TILE_SIDE
        || rgb.len() != width as usize * height as usize * 3
    {
        return Err("invalid canonical tile pixels".into());
    }
    let gray = rgb.chunks_exact(3).all(|p| p[0] == p[1] && p[1] == p[2]);
    let (color, channels) = if gray { (0u8, 1) } else { (2u8, 3) };
    let stride = width as usize * channels;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgb.chunks_exact(width as usize * 3) {
        raw.push(0);
        if gray { raw.extend(row.chunks_exact(3).map(|p| p[0])); } else { raw.extend_from_slice(row); }
    }
    let mut zlib = Vec::with_capacity(raw.len() + raw.len() / 65535 * 5 + 11);
    zlib.extend_from_slice(&[0x78, 0x01]);
    let blocks = raw.chunks(65535).count();
    for (index, block) in raw.chunks(65535).enumerate() {
        let len = block.len() as u16;
        zlib.push(u8::from(index + 1 == blocks));
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in raw.chunks(5552) {
        for &byte in chunk { a += u32::from(byte); b += a; }
        a %= 65521;
        b %= 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(data);
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        png.extend_from_slice(&crc.finalize().to_be_bytes());
    };
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, color, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"sRGB", &[1]);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    if png.len() > MAX_PNG_BYTES { return Err("canonical tile PNG exceeds transfer limit".into()); }
    Ok(png)
}

/// The container of a batch canvas: a PNG (`canvas_png_b64`), or a lossless
/// WebP (`canvas_webp_b64`) for a gateway that lists
/// [`BATCH_CANVAS_WEBP_OPERATION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasFormat {
    Png,
    Webp,
}

/// What one batch sends besides its metadata: the distinct tile PNGs
/// (`tiles_png_b64`), or one canvas (`canvas_png_b64` or `canvas_webp_b64`,
/// and `canvas_rect`).
#[derive(Debug, Clone)]
pub enum BatchUpload<'a> {
    Tiles(Vec<&'a [u8]>),
    Canvas { rect: TileRect, format: CanvasFormat, image: Vec<u8> },
}

impl BatchUpload<'_> {
    /// The image bytes it puts on the wire, before base64.
    pub fn bytes(&self) -> usize {
        match self {
            Self::Tiles(pngs) => pngs.iter().map(|png| png.len()).sum(),
            Self::Canvas { image, .. } => image.len(),
        }
    }
}

/// A canvas of `rect`'s size, grey or RGB with no alpha, as RGB pixels.
fn canvas_pixels(image: &[u8], format: CanvasFormat, rect: TileRect) -> Result<Vec<u8>, String> {
    if format == CanvasFormat::Png {
        return rgb_pixels(image, rect, BATCH_MAX_CANVAS_PIXELS);
    }
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(image)).map_err(|e| e.to_string())?;
    if decoder.dimensions() != (rect.width, rect.height) || decoder.has_alpha()
        || u64::from(rect.width) * u64::from(rect.height) > BATCH_MAX_CANVAS_PIXELS
    {
        return Err("analysis canvas geometry mismatch".into());
    }
    let mut buffer = vec![0; decoder.output_buffer_size().ok_or("WebP output too large")?];
    decoder.read_image(&mut buffer).map_err(|e| e.to_string())?;
    Ok(buffer)
}

/// A PNG of `rect`'s size, grey or RGB, as RGB pixels.
fn rgb_pixels(png: &[u8], rect: TileRect, max_pixels: u64) -> Result<Vec<u8>, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    decoder.set_limits(png::Limits { bytes: (max_pixels * 3) as usize });
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    if info.width != rect.width || info.height != rect.height || info.bit_depth != png::BitDepth::Eight
        || info.interlaced || u64::from(info.width) * u64::from(info.height) > max_pixels
    {
        return Err("analysis canvas or tile geometry mismatch".into());
    }
    let gray = match info.color_type {
        png::ColorType::Grayscale => true,
        png::ColorType::Rgb => false,
        _ => return Err("analysis canvas or tile colour mismatch".into()),
    };
    let mut buffer = vec![0; reader.output_buffer_size().ok_or("PNG output too large")?];
    reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    Ok(if gray { buffer.iter().flat_map(|&v| [v, v, v]).collect() } else { buffer })
}

fn cut(canvas: &[u8], canvas_rect: TileRect, tile: TileRect) -> Option<Vec<u8>> {
    let end = |start: u32, len: u32| u64::from(start) + u64::from(len);
    if tile.x < canvas_rect.x || tile.y < canvas_rect.y
        || end(tile.x, tile.width) > end(canvas_rect.x, canvas_rect.width)
        || end(tile.y, tile.height) > end(canvas_rect.y, canvas_rect.height)
    {
        return None;
    }
    let (dx, dy) = ((tile.x - canvas_rect.x) as usize, (tile.y - canvas_rect.y) as usize);
    let stride = canvas_rect.width as usize * 3;
    Some((0..tile.height as usize).flat_map(|row| {
        let start = (dy + row) * stride + dx * 3;
        canvas[start..start + tile.width as usize * 3].iter().copied()
    }).collect())
}

/// What a batch of `tiles` (each tile's place on the page and its
/// [`canonical_tile_png`]) sends: one canvas in `canvas`'s format over their
/// bounding box, every pixel outside the tiles white so nothing the tiles do
/// not hold is sent, or the tile PNGs when that canvas would be larger or
/// `canvas` is `None`. A canvas WebP cannot hold (a side over 16383) goes as
/// a PNG, which every canvas gateway takes. Tiles
/// at two places can hold the same pixels (a blank margin); the canvas holds
/// both places and the tile PNGs name that PNG once, as the gateway requires.
/// Refused when a tile is not canonical or two tiles disagree where they
/// overlap, since the gateway would then cut other pixels than the requests
/// name.
pub fn batch_upload<'a>(tiles: &[(TileRect, &'a [u8])], canvas: Option<CanvasFormat>) -> Result<BatchUpload<'a>, String> {
    let pngs = || {
        let mut distinct: Vec<&'a [u8]> = Vec::new();
        for (_, png) in tiles {
            if !distinct.contains(png) { distinct.push(png); }
        }
        BatchUpload::Tiles(distinct)
    };
    let (Some(left), Some(top)) = (tiles.iter().map(|(r, _)| r.x).min(), tiles.iter().map(|(r, _)| r.y).min())
    else { return Ok(pngs()) };
    let right = tiles.iter().map(|(r, _)| r.x + r.width).max().unwrap_or(left);
    let bottom = tiles.iter().map(|(r, _)| r.y + r.height).max().unwrap_or(top);
    let rect = TileRect { x: left, y: top, width: right - left, height: bottom - top };
    let Some(format) = canvas.filter(|_| u64::from(rect.width) * u64::from(rect.height) <= BATCH_MAX_CANVAS_PIXELS)
    else { return Ok(pngs()) };
    let stride = rect.width as usize * 3;
    let mut pixels = vec![255u8; stride * rect.height as usize];
    let mut decoded = Vec::with_capacity(tiles.len());
    for (tile, png) in tiles {
        let rgb = rgb_pixels(png, *tile, MAX_TILE_PIXELS)?;
        let (dx, dy) = ((tile.x - left) as usize, (tile.y - top) as usize);
        for (row, line) in rgb.chunks_exact(tile.width as usize * 3).enumerate() {
            let start = (dy + row) * stride + dx * 3;
            pixels[start..start + line.len()].copy_from_slice(line);
        }
        decoded.push(rgb);
    }
    for ((tile, png), rgb) in tiles.iter().zip(&decoded) {
        if cut(&pixels, rect, *tile).as_deref() != Some(rgb.as_slice())
            || canonical_tile_png(rgb, tile.width, tile.height)?.as_slice() != *png
        {
            return Err("analysis tiles are not canonical or disagree where they overlap".into());
        }
    }
    let gray = pixels.chunks_exact(3).all(|p| p[0] == p[1] && p[1] == p[2]);
    let data: Vec<u8> = if gray { pixels.chunks_exact(3).map(|p| p[0]).collect() } else { pixels };
    let webp = (format == CanvasFormat::Webp).then(|| {
        let mut webp = Vec::new();
        let color = if gray { image_webp::ColorType::L8 } else { image_webp::ColorType::Rgb8 };
        image_webp::WebPEncoder::new(&mut webp).encode(&data, rect.width, rect.height, color).ok().map(|()| webp)
    }).flatten();
    let upload = match webp {
        Some(image) => BatchUpload::Canvas { rect, format: CanvasFormat::Webp, image },
        None => {
            let raster = crate::image::Raster {
                width: rect.width, height: rect.height,
                mode: if gray { crate::image::ColorMode::Gray } else { crate::image::ColorMode::Rgb },
                depth: crate::image::BitDepth::Eight, icc: None, palette: None, trns: None,
                srgb_intent: Some(1), color: Default::default(), data,
            };
            let image = crate::image::encode(&raster, crate::image::Format::Png).map_err(|e| e.to_string())?;
            BatchUpload::Canvas { rect, format: CanvasFormat::Png, image }
        }
    };
    Ok(if upload.bytes() < pngs().bytes() { upload } else { pngs() })
}

/// The gateway's side of a canvas: each of `tiles` cut out of it as its
/// [`canonical_tile_png`]. Mirrors `contract.py` `analysis_canvas_tiles`; the
/// app uses it in tests, standing in for the gateway.
pub fn canvas_tiles(rect: TileRect, format: CanvasFormat, image: &[u8], tiles: &[TileRect]) -> Result<Vec<Vec<u8>>, String> {
    if rect.width == 0 || rect.height == 0 || image.len() > BATCH_MAX_TILE_BYTES {
        return Err("invalid analysis canvas".into());
    }
    let pixels = canvas_pixels(image, format, rect)?;
    tiles.iter().map(|tile| {
        let rgb = cut(&pixels, rect, *tile).ok_or("analysis tile outside its canvas")?;
        canonical_tile_png(&rgb, tile.width, tile.height)
    }).collect()
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
        // Twelve blank tiles of one page share a digest: 24 requests, cut at the bound.
        let blank: Vec<AnalysisRequest> = (0..24).map(|_| base.clone()).collect();
        let png = [0u8; 10];
        let steps: Vec<(&AnalysisRequest, &[u8])> = blank.iter().map(|request| (request, png.as_slice())).collect();
        assert_eq!(batches(&steps), vec![0..16, 16..24]);
    }

    fn pattern(width: u32, height: u32, gray: bool) -> Vec<u8> {
        (0..height).flat_map(|y| (0..width).flat_map(move |x| if gray {
            let v = ((3 * x + 5 * y) % 256) as u8;
            [v, v, v]
        } else {
            [((7 * x + 3 * y) % 256) as u8, ((5 * x + 11 * y) % 256) as u8, ((13 * x + y) % 256) as u8]
        })).collect()
    }

    #[test]
    fn cloud_canonical_tile_png_matches_the_gateway() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/analysis_v1/canonical_tiles.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let (width, height) = (case["width"].as_u64().unwrap() as u32, case["height"].as_u64().unwrap() as u32);
            let gray = case["pattern"] == "gray";
            let rgb = pattern(width, height, gray);
            let png = canonical_tile_png(&rgb, width, height).unwrap();
            assert_eq!(png.len() as u64, case["png_bytes"].as_u64().unwrap());
            assert_eq!(format!("{:x}", Sha256::digest(&png)), case["png_sha256"].as_str().unwrap());
            let rect = TileRect { x: 0, y: 0, width, height };
            validate_png(&png, rect, &[if gray { png::ColorType::Grayscale } else { png::ColorType::Rgb }]).unwrap();
            assert_eq!(rgb_pixels(&png, rect, MAX_TILE_PIXELS).unwrap(), rgb);
        }
        // A full RGB tile fits a tile request's PNG bound.
        let full = canonical_tile_png(&vec![7u8; 1024 * 1024 * 3], 1024, 1024).unwrap();
        assert!(full.len() <= MAX_PNG_BYTES && full.len() > 1024 * 1024);
        assert!(canonical_tile_png(&[0; 3], 1, 2).is_err());
    }

    /// The WebP canvases in this fixture are what this encoder made; the
    /// gateway test decodes the same bytes with Pillow to the same pixels.
    #[test]
    fn cloud_webp_canvas_fixture_decodes_on_both_sides() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/analysis_v1/canvas_webp.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let (width, height) = (case["width"].as_u64().unwrap() as u32, case["height"].as_u64().unwrap() as u32);
            let hex = case["webp_hex"].as_str().unwrap();
            let webp: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
            let rect = TileRect { x: 0, y: 0, width, height };
            assert_eq!(canvas_pixels(&webp, CanvasFormat::Webp, rect).unwrap(), pattern(width, height, case["pattern"] == "gray"));
        }
    }

    fn tile_of(page: &[u8], page_width: u32, rect: TileRect) -> Vec<u8> {
        cut(page, TileRect { x: 0, y: 0, width: page_width, height: (page.len() / 3 / page_width as usize) as u32 }, rect).unwrap()
    }

    #[test]
    fn cloud_canvas_carries_each_tile_once_and_blanks_what_was_not_approved() {
        for (gray, format) in [false, true].into_iter().flat_map(|gray| [(gray, CanvasFormat::Png), (gray, CanvasFormat::Webp)]) {
            let (width, height) = (1080, 1536);
            let page = pattern(width, height, gray);
            let plan = crate::cloud_tiles::planned(width, height).unwrap();
            assert_eq!(plan.len(), 4);
            let pngs: Vec<Vec<u8>> = plan.iter()
                .map(|t| canonical_tile_png(&tile_of(&page, width, t.tile), t.tile.width, t.tile.height).unwrap()).collect();
            // The whole page: one canvas of the page, cut back into the same tiles.
            let all: Vec<(TileRect, &[u8])> = plan.iter().zip(&pngs).map(|(t, png)| (t.tile, png.as_slice())).collect();
            let BatchUpload::Canvas { rect, format: sent_as, image } = batch_upload(&all, Some(format)).unwrap()
                else { panic!("tiles sent") };
            assert_eq!((rect, sent_as), (TileRect { x: 0, y: 0, width, height }, format));
            assert!(image.len() < pngs.iter().map(Vec::len).sum::<usize>());
            let tiles: Vec<TileRect> = plan.iter().map(|t| t.tile).collect();
            assert_eq!(canvas_tiles(rect, format, &image, &tiles).unwrap(), pngs);
            // The gateway reads the container the body names, never another.
            let other = if format == CanvasFormat::Png { CanvasFormat::Webp } else { CanvasFormat::Png };
            assert!(canvas_tiles(rect, other, &image, &tiles).is_err());
            // A partial review of two diagonal tiles: their bounding box is the
            // page, and every pixel outside both tiles is white.
            let chosen = [all[0], all[3]];
            let BatchUpload::Canvas { rect, image, .. } = batch_upload(&chosen, Some(format)).unwrap() else { panic!("tiles sent") };
            assert_eq!(canvas_tiles(rect, format, &image, &[plan[0].tile, plan[3].tile]).unwrap(), vec![pngs[0].clone(), pngs[3].clone()]);
            let sent = canvas_pixels(&image, format, rect).unwrap();
            let inside = |x: u32, y: u32, r: TileRect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
            let mut blank = 0;
            for y in 0..height {
                for x in 0..width {
                    let at = ((y * width + x) * 3) as usize;
                    if inside(x, y, plan[0].tile) || inside(x, y, plan[3].tile) {
                        assert_eq!(sent[at..at + 3], page[at..at + 3]);
                    } else {
                        assert_eq!(sent[at..at + 3], [255, 255, 255]);
                        blank += 1;
                    }
                }
            }
            assert!(blank > 0);
            // Without the operation the tile PNGs go, each distinct one once:
            // this pattern repeats every 512 rows, so each lower tile is the
            // PNG of the tile above it.
            assert_eq!((&pngs[0], &pngs[1]), (&pngs[2], &pngs[3]));
            assert!(matches!(batch_upload(&all, None).unwrap(), BatchUpload::Tiles(p) if p.len() == 2));
        }
    }

    /// WebP holds at most 16383 pixels a side; a wider canvas goes as a PNG.
    #[test]
    fn cloud_canvas_wider_than_webp_goes_as_png() {
        let png = canonical_tile_png(&[255; 1024 * 3], 1024, 1).unwrap();
        let tiles: Vec<(TileRect, &[u8])> =
            (0..17).map(|i| (TileRect { x: i * 1024, y: 0, width: 1024, height: 1 }, png.as_slice())).collect();
        let BatchUpload::Canvas { rect, format, image } = batch_upload(&tiles, Some(CanvasFormat::Webp)).unwrap()
            else { panic!("tiles sent") };
        assert_eq!((rect.width, format), (17 * 1024, CanvasFormat::Png));
        let places: Vec<TileRect> = tiles.iter().map(|(rect, _)| *rect).collect();
        assert_eq!(canvas_tiles(rect, format, &image, &places).unwrap(), vec![png; 17]);
    }

    /// Two tiles of a blank page hold the same pixels: the canvas still holds
    /// both places, and the tile PNGs name the one PNG once.
    #[test]
    fn cloud_canvas_keeps_two_places_of_one_blank_tile() {
        let plan = crate::cloud_tiles::planned(1500, 256).unwrap();
        let png = canonical_tile_png(&vec![255; 1024 * 256 * 3], 1024, 256).unwrap();
        let tiles = [(plan[0].tile, png.as_slice()), (plan[1].tile, png.as_slice())];
        let BatchUpload::Canvas { rect, format, image } = batch_upload(&tiles, Some(CanvasFormat::Webp)).unwrap()
            else { panic!("tiles sent") };
        assert_eq!(rect, TileRect { x: 0, y: 0, width: 1500, height: 256 });
        assert_eq!(canvas_tiles(rect, format, &image, &[plan[0].tile, plan[1].tile]).unwrap(), vec![png.clone(), png.clone()]);
        assert!(matches!(batch_upload(&tiles, None).unwrap(), BatchUpload::Tiles(p) if p.len() == 1));
    }

    #[test]
    fn cloud_canvas_refuses_tiles_it_cannot_reproduce() {
        let (width, height) = (1080, 1536);
        let page = pattern(width, height, false);
        let plan = crate::cloud_tiles::planned(width, height).unwrap();
        let first = tile_of(&page, width, plan[0].tile);
        // A tile encoded by an ordinary encoder is not canonical.
        let raster = crate::image::Raster {
            width: 1024, height: 1024, mode: crate::image::ColorMode::Rgb, depth: crate::image::BitDepth::Eight,
            icc: None, palette: None, trns: None, srgb_intent: Some(1), color: Default::default(), data: first.clone(),
        };
        let compressed = crate::image::encode(&raster, crate::image::Format::Png).unwrap();
        assert!(batch_upload(&[(plan[0].tile, &compressed)], Some(CanvasFormat::Png)).is_err());
        // Two tiles that disagree where they overlap.
        let mut other = tile_of(&page, width, plan[1].tile);
        other[0] ^= 1;
        let (a, b) = (canonical_tile_png(&first, 1024, 1024).unwrap(), canonical_tile_png(&other, 1024, 1024).unwrap());
        assert!(batch_upload(&[(plan[0].tile, &a), (plan[1].tile, &b)], Some(CanvasFormat::Webp)).is_err());
        // The gateway refuses a tile outside the canvas.
        let BatchUpload::Canvas { rect, format, image } = batch_upload(&[(plan[0].tile, &a)], Some(CanvasFormat::Webp)).unwrap()
            else { panic!("tile sent") };
        assert!(canvas_tiles(rect, format, &image, &[plan[3].tile]).is_err());
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
