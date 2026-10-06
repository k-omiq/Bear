//! Bounded PNG result decoding and validation for cloud diffusion inference outputs.
//!
//! Decodes validated PNG output buffers returned by remote cloud providers into [`GeneratedCrop`]
//! representations ready for immutable host compositing.
//!
//! ## Invariants
//!
//! 1. **Prior Wire Contract Validation:** Every result payload MUST pass [`validate_result_bytes`]
//!    before pixel decoding commences, verifying handle binding, request metadata, dimensions,
//!    latent stride alignment, pixel limits, and SHA-256 result digest.
//! 2. **Bounded Resource Allocation & Decompression Bomb Protection:** Uses `png` 0.18 configured
//!    with explicit byte allocation limits ([`png::Limits`]) derived from [`ServiceLimits`].
//!    Encoded stream byte limits and decoded pixel count limits are bounded separately.
//! 3. **Strict Color & Depth Mode Enforcement:** Diffusion outputs MUST be non-interlaced,
//!    single-frame 8-bit RGB (24 bits per pixel). Grayscale, alpha channels, 16-bit depths,
//!    paletted/indexed modes, and interlacing fail closed.
//! 4. **No Multi-Frame / Animated Payloads:** APNG chunk streams and animated multi-frame responses
//!    are rejected.
//! 5. **Stream Integrity to IEND (`reader.finish()`):** The decode stream processes all chunks to `IEND`
//!    via `reader.finish()`, asserting chunk CRC checksums and rejecting trailing garbage or truncation.
//! 6. **Checked Buffer Geometry & Arithmetic:** Decoded sample buffers are checked for exact
//!    `width * height * 3` sample count using checked arithmetic.
//! 7. **Direct Compositor Compatibility:** Produces [`DecodedResultCrop`], which can construct a
//!    [`GeneratedCrop`] without copying for zero-overhead evaluation by [`crate::engines::render::PreparedRender`].

use std::io::Cursor;

use thiserror::Error;

use crate::cloud_wire::{
    validate_job_request_metadata, validate_job_status_response, validate_response_binding,
    validate_result_bytes, validate_result_metadata, JobExecutionStatus, JobRequestMetadata,
    JobStatusResponse, ResultMetadata, ServiceLimits, WireValidationError,
};
use crate::engines::render::GeneratedCrop;

/// Errors arising during cloud PNG result validation and decoding.
#[derive(Debug, Error, PartialEq)]
pub enum ResultDecodeError {
    #[error("cloud wire validation failed: {0}")]
    Wire(#[from] WireValidationError),

    #[error("png decoding failed: {0}")]
    PngDecoding(String),

    #[error("unsupported or mismatched color type in PNG result: expected RGB8 (color type 2)")]
    UnexpectedColorType,

    #[error("unsupported or mismatched bit depth in PNG result: expected 8-bit")]
    UnexpectedBitDepth,

    #[error("decoded frame geometry mismatch: expected {expected_w}x{expected_h}, got {actual_w}x{actual_h}")]
    FrameGeometryMismatch {
        expected_w: u32,
        expected_h: u32,
        actual_w: u32,
        actual_h: u32,
    },

    #[error("decoded buffer byte length mismatch: expected {expected}, got {actual}")]
    BufferLengthMismatch {
        expected: usize,
        actual: usize,
    },

    #[error("interlaced PNG result images are strictly forbidden")]
    InterlacingForbidden,

    #[error("multi-frame or animated PNG result images are strictly forbidden")]
    MultipleFramesForbidden,

    #[error("decompression resource limit exceeded")]
    DecompressionLimitExceeded,

    #[error("pixel count arithmetic overflow for {width}x{height}")]
    PixelCountOverflow { width: u32, height: u32 },

    #[error("pixel count {pixels} exceeds allowed service limit {max_pixels}")]
    PixelCountExceeded { pixels: u64, max_pixels: u64 },
}

/// Host safety ceilings independent of untrusted advertised service limits.
/// These are allocation limits, not measured model serving limits.
pub const MAX_RESULT_ENCODED_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_RESULT_PIXELS: u64 = 16 * 1024 * 1024;

/// Decode a validated analysis mask with a hard one-tile allocation ceiling.
pub fn decode_analysis_mask(
    bytes: &[u8],
    tile: crate::cloud_analysis_wire::TileRect,
) -> Result<Vec<u8>, ResultDecodeError> {
    tile.validate().map_err(ResultDecodeError::PngDecoding)?;
    crate::cloud_analysis_wire::validate_png(bytes, tile, png::ColorType::Grayscale)
        .map_err(ResultDecodeError::PngDecoding)?;
    let pixels = usize::try_from(u64::from(tile.width) * u64::from(tile.height))
        .map_err(|_| ResultDecodeError::DecompressionLimitExceeded)?;
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_limits(png::Limits { bytes: crate::cloud_analysis_wire::MAX_TILE_PIXELS as usize });
    let mut reader = decoder.read_info().map_err(|e| ResultDecodeError::PngDecoding(e.to_string()))?;
    if reader.output_buffer_size().is_none_or(|size| size != pixels) {
        return Err(ResultDecodeError::DecompressionLimitExceeded);
    }
    let mut data = vec![0; pixels];
    let frame = reader.next_frame(&mut data).map_err(|e| ResultDecodeError::PngDecoding(e.to_string()))?;
    if frame.width != tile.width || frame.height != tile.height || frame.buffer_size() != pixels {
        return Err(ResultDecodeError::FrameGeometryMismatch {
            expected_w: tile.width, expected_h: tile.height,
            actual_w: frame.width, actual_h: frame.height,
        });
    }
    reader.finish().map_err(|e| ResultDecodeError::PngDecoding(e.to_string()))?;
    Ok(data)
}

fn check_complete_png(bytes: &[u8]) -> Result<(), ResultDecodeError> {
    let mut offset = 8usize;
    while offset < bytes.len() {
        let header = bytes.get(offset..offset + 8).ok_or(ResultDecodeError::DecompressionLimitExceeded)?;
        let size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = offset.checked_add(size).and_then(|n| n.checked_add(12))
            .ok_or(ResultDecodeError::DecompressionLimitExceeded)?;
        let chunk = bytes.get(offset..end).ok_or(ResultDecodeError::DecompressionLimitExceeded)?;
        let crc = u32::from_be_bytes(chunk[chunk.len()-4..].try_into().unwrap());
        if crc32fast::hash(&chunk[4..chunk.len()-4]) != crc {
            return Err(ResultDecodeError::DecompressionLimitExceeded);
        }
        if &header[4..8] == b"IEND" {
            return if size == 0 && end == bytes.len() { Ok(()) }
                else { Err(ResultDecodeError::DecompressionLimitExceeded) };
        }
        offset = end;
    }
    Err(ResultDecodeError::DecompressionLimitExceeded)
}

/// An owned, validated 8-bit RGB raster crop decoded from a cloud inference result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedResultCrop {
    width: u32,
    height: u32,
    rgb8: Vec<u8>,
}

impl DecodedResultCrop {
    /// Construct a [`DecodedResultCrop`] directly from validated dimensions and sample buffer.
    pub fn new(width: u32, height: u32, rgb8: Vec<u8>) -> Result<Self, ResultDecodeError> {
        let expected_len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(3))
            .ok_or(ResultDecodeError::PixelCountOverflow { width, height })?;

        if rgb8.len() != expected_len {
            return Err(ResultDecodeError::BufferLengthMismatch {
                expected: expected_len,
                actual: rgb8.len(),
            });
        }

        Ok(Self {
            width,
            height,
            rgb8,
        })
    }

    /// Crop width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Crop height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Contiguous RGB8 pixel byte slice (`[R, G, B, R, G, B, ...]`).
    pub fn rgb8(&self) -> &[u8] {
        &self.rgb8
    }

    /// Consume into owned RGB8 byte vector.
    pub fn into_rgb8(self) -> Vec<u8> {
        self.rgb8
    }

    /// Borrow as a [`GeneratedCrop`] for direct host composition.
    pub fn as_generated_crop(&self) -> GeneratedCrop<'_> {
        GeneratedCrop::new(self.width, self.height, &self.rgb8)
    }
}

/// Helper to assemble and validate [`ResultMetadata`] by joining prior [`JobRequestMetadata`] with a completed [`JobStatusResponse`].
pub fn result_metadata_from_status(
    status: &JobStatusResponse,
    request: &JobRequestMetadata,
    expected_handle: &str,
) -> Result<ResultMetadata, WireValidationError> {
    validate_job_status_response(status)?;
    validate_job_request_metadata(request)?;
    validate_response_binding(request, status, Some(expected_handle))?;

    if status.status != JobExecutionStatus::Completed {
        return Err(WireValidationError::InvalidAcceptedStatus(status.status));
    }

    let result_digest = status
        .result_digest
        .as_ref()
        .ok_or_else(|| WireValidationError::ResultDigestMismatch {
            declared: "missing result_digest in completed job status".to_string(),
            computed: "none".to_string(),
        })?
        .clone();

    let byte_length = status
        .result_bytes
        .ok_or(WireValidationError::ResultByteLengthMismatch {
            actual: 0,
            declared: 0,
        })?;

    let meta = ResultMetadata {
        handle: status.handle.clone(),
        job_id: status.job_id.clone(),
        attempt_id: status.attempt_id.clone(),
        request_digest: status.request_digest.clone(),
        recipe_id: status.recipe_id.clone(),
        preprocessing_version: status.preprocessing_version.clone(),
        model_id: status.model_id.clone(),
        model_revision: status.model_revision.clone(),
        native_mask_conditioning: status.native_mask_conditioning,
        result_digest,
        reported_cost_usd: status.reported_cost_usd,
        width: request.width,
        height: request.height,
        byte_length,
    };

    validate_result_metadata(&meta)?;
    Ok(meta)
}

/// Fully validate and decode a PNG result buffer from cloud diffusion inference into a [`DecodedResultCrop`].
///
/// 1. Enforces all wire validation invariants via [`validate_result_bytes`] (geometry, bounds, latent stride,
///    IHDR CRC-32, byte length, and SHA-256 digest).
/// 2. Bounds decoded pixel counts against [`ServiceLimits::max_pixels`].
/// 3. Configures a bounded `png::Decoder` with explicit memory limits to prevent decompression bombs.
/// 4. Validates that the PNG is strictly non-interlaced, non-animated (single frame), RGB8 (24-bit RGB),
///    and has exact matching frame dimensions.
/// 5. Decodes the full frame bytes, asserts exact buffer length, and consumes trailing chunks to `IEND` via `.finish()`.
pub fn decode_result_crop(
    result_bytes: &[u8],
    result_meta: &ResultMetadata,
    request_meta: &JobRequestMetadata,
    limits: &ServiceLimits,
    expected_handle: &str,
) -> Result<DecodedResultCrop, ResultDecodeError> {
    if result_bytes.len() as u64 > MAX_RESULT_ENCODED_BYTES
        || u64::from(result_meta.width) * u64::from(result_meta.height) > MAX_RESULT_PIXELS
    {
        return Err(ResultDecodeError::DecompressionLimitExceeded);
    }
    // 1. Validate against wire invariants & checksum
    validate_result_bytes(
        result_bytes,
        result_meta,
        request_meta,
        limits,
        expected_handle,
    )?;

    check_complete_png(result_bytes)?;

    // 2. Arithmetic overflow & separate decoded pixel bounds checks
    let pixels = (result_meta.width as u64)
        .checked_mul(result_meta.height as u64)
        .ok_or(ResultDecodeError::PixelCountOverflow {
            width: result_meta.width,
            height: result_meta.height,
        })?;

    if pixels > limits.max_pixels {
        return Err(ResultDecodeError::PixelCountExceeded {
            pixels,
            max_pixels: limits.max_pixels,
        });
    }

    let expected_byte_len = (result_meta.width as usize)
        .checked_mul(result_meta.height as usize)
        .and_then(|px| px.checked_mul(3))
        .ok_or(ResultDecodeError::PixelCountOverflow {
            width: result_meta.width,
            height: result_meta.height,
        })?;

    // 3. Set up bounded PNG decoder
    let mut decoder = png::Decoder::new(Cursor::new(result_bytes));
    decoder.set_transformations(png::Transformations::IDENTITY);

    let png_limits = png::Limits { bytes: expected_byte_len.saturating_add(1024 * 1024) };
    decoder.set_limits(png_limits);

    let mut reader = decoder
        .read_info()
        .map_err(|e| ResultDecodeError::PngDecoding(e.to_string()))?;

    let info = reader.info();

    if info.width != result_meta.width || info.height != result_meta.height {
        return Err(ResultDecodeError::FrameGeometryMismatch {
            expected_w: result_meta.width,
            expected_h: result_meta.height,
            actual_w: info.width,
            actual_h: info.height,
        });
    }

    if info.color_type != png::ColorType::Rgb {
        return Err(ResultDecodeError::UnexpectedColorType);
    }

    if info.bit_depth != png::BitDepth::Eight {
        return Err(ResultDecodeError::UnexpectedBitDepth);
    }

    if info.interlaced {
        return Err(ResultDecodeError::InterlacingForbidden);
    }

    if info.is_animated() {
        return Err(ResultDecodeError::MultipleFramesForbidden);
    }

    // Allocate exact buffer
    let mut rgb_buf = vec![0u8; expected_byte_len];
    let frame_info = reader
        .next_frame(&mut rgb_buf)
        .map_err(|e| ResultDecodeError::PngDecoding(e.to_string()))?;

    if frame_info.width != result_meta.width || frame_info.height != result_meta.height {
        return Err(ResultDecodeError::FrameGeometryMismatch {
            expected_w: result_meta.width,
            expected_h: result_meta.height,
            actual_w: frame_info.width,
            actual_h: frame_info.height,
        });
    }

    if frame_info.buffer_size() != expected_byte_len {
        return Err(ResultDecodeError::BufferLengthMismatch {
            expected: expected_byte_len,
            actual: frame_info.buffer_size(),
        });
    }

    // 4. Verify stream completes cleanly to IEND with valid CRC
    reader
        .finish()
        .map_err(|e| ResultDecodeError::PngDecoding(e.to_string()))?;

    Ok(DecodedResultCrop {
        width: result_meta.width,
        height: result_meta.height,
        rgb8: rgb_buf,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    use crate::cloud_wire::{provisional_fixture_limits, WireRenderRecipe};
    use crate::engines::render::PreparedRender;
    use crate::fit;
    use crate::image::{BitDepth, ColorMode, Raster};
    use crate::mask::{Mask, Rect};

    const JOB_REQUEST_FIXTURE: &str =
        include_str!("../../../deploy/cloud/fixtures/job_request_metadata.json");
    const JOB_STATUS_COMPLETED: &str =
        include_str!("../../../deploy/cloud/fixtures/job_status_completed.json");
    const TINY_IMAGE_BYTES: &[u8] =
        include_bytes!("../../../deploy/cloud/fixtures/tiny_image.png");

    fn make_valid_png_bytes(width: u32, height: u32, color_fn: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let mut data = Vec::with_capacity((width * height * 3) as usize);
            for y in 0..height {
                for x in 0..width {
                    data.extend_from_slice(&color_fn(x, y));
                }
            }
            writer.write_image_data(&data).unwrap();
        }
        bytes
    }

    fn make_request_and_result_meta(
        width: u32,
        height: u32,
        png_bytes: &[u8],
    ) -> (JobRequestMetadata, ResultMetadata) {
        let mut hasher = Sha256::new();
        hasher.update(png_bytes);
        let result_digest = format!("{:x}", hasher.finalize());

        let recipe = WireRenderRecipe {
            qwen_edit: None,
            recipe_id: "test-sdnq-v1".into(),
            preprocessing_version: "1.0.0".into(),
            model_id: "test-flux-schnell".into(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
            native_mask_conditioning: false,
        };

        let mut req = JobRequestMetadata {
            protocol_version: "1.0.0".into(),
            job_id: "job-test-001".into(),
            attempt_id: "attempt-test-001".into(),
            recipe: recipe.clone(),
            width,
            height,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            image_sha256: "2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f".into(),
            hint_sha256: "3552bb8bf493ba63896f7abfb3ee3d899bf7312cd0b3d87691a639344b051238".into(),
            request_digest: String::new(),
        };
        req.request_digest = crate::cloud_wire::compute_request_digest(&req);

        let result_meta = ResultMetadata {
            handle: "handle-test-modal-999".into(),
            job_id: req.job_id.clone(),
            attempt_id: req.attempt_id.clone(),
            request_digest: req.request_digest.clone(),
            recipe_id: recipe.recipe_id,
            preprocessing_version: recipe.preprocessing_version,
            model_id: recipe.model_id,
            model_revision: recipe.model_revision,
            native_mask_conditioning: false,
            result_digest,
            reported_cost_usd: Some(0.0012),
            width,
            height,
            byte_length: png_bytes.len() as u64,
        };

        (req, result_meta)
    }

    #[test]
    fn test_valid_tiny_fixture_png_decodes_successfully() {
        let req_meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        let status: JobStatusResponse = serde_json::from_str(JOB_STATUS_COMPLETED).unwrap();
        let result_meta = result_metadata_from_status(&status, &req_meta, "handle-test-modal-999").unwrap();
        let limits = provisional_fixture_limits();

        let decoded = decode_result_crop(
            TINY_IMAGE_BYTES,
            &result_meta,
            &req_meta,
            &limits,
            "handle-test-modal-999",
        )
        .expect("should decode valid fixture png");

        assert_eq!(decoded.width(), 16);
        assert_eq!(decoded.height(), 16);
        assert_eq!(decoded.rgb8().len(), 16 * 16 * 3);

        let generated = decoded.as_generated_crop();
        assert_eq!(generated.width(), 16);
        assert_eq!(generated.height(), 16);
        assert_eq!(generated.rgb8().len(), 16 * 16 * 3);
    }

    #[test]
    fn test_decoded_crop_integrates_with_prepared_render_composition() {
        // Create a synthetic source page and fitted region
        let page = Raster {
            width: 100,
            height: 100,
            mode: ColorMode::Rgb,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
        color: Default::default(),
            data: vec![200u8; 100 * 100 * 3],
        };

        let seed = Mask::filled(Rect::new(20, 20, 16, 16));
        let fitted = fit::fit(
            &page,
            &seed,
            1.0,
            0.0,
            &fit::EdgeMap::none(page.width, page.height),
            true,
        );

        let prepared = PreparedRender::prepare(&page, &fitted).unwrap();
        let (width, height) = (prepared.crop().w, prepared.crop().h);
        let png_bytes = make_valid_png_bytes(width, height, |x, y| {
            [(x % 256) as u8, (y % 256) as u8, 128]
        });
        let (req_meta, result_meta) = make_request_and_result_meta(width, height, &png_bytes);
        let decoded = decode_result_crop(&png_bytes, &result_meta, &req_meta,
            &provisional_fixture_limits(), "handle-test-modal-999").unwrap();

        let generated = decoded.as_generated_crop();
        let rendered = prepared.composite(&generated).expect("compositing must succeed");
        assert_eq!(rendered.pixels.width, prepared.bounds().w);
        assert_eq!(rendered.pixels.height, prepared.bounds().h);
    }

    #[test]
    fn test_truncated_bytes_rejected() {
        let width = 16;
        let height = 16;
        let png_bytes = make_valid_png_bytes(width, height, |_, _| [10, 20, 30]);
        let (req_meta, mut result_meta) = make_request_and_result_meta(width, height, &png_bytes);
        let limits = provisional_fixture_limits();

        // Truncate bytes halfway through
        let truncated = &png_bytes[0..png_bytes.len() / 2];
        result_meta.byte_length = truncated.len() as u64;

        let mut hasher = Sha256::new();
        hasher.update(truncated);
        result_meta.result_digest = format!("{:x}", hasher.finalize());

        let err = decode_result_crop(
            truncated,
            &result_meta,
            &req_meta,
            &limits,
            "handle-test-modal-999",
        )
        .unwrap_err();

        assert!(matches!(err, ResultDecodeError::PngDecoding(_) | ResultDecodeError::DecompressionLimitExceeded));
    }

    #[test]
    fn test_corrupted_crc_rejected() {
        let width = 16;
        let height = 16;
        let mut png_bytes = make_valid_png_bytes(width, height, |_, _| [10, 20, 30]);
        // Corrupt IHDR CRC bytes (bytes 29..33)
        png_bytes[29] ^= 0xFF;

        let (req_meta, result_meta) = make_request_and_result_meta(width, height, &png_bytes);
        let limits = provisional_fixture_limits();

        let err = decode_result_crop(
            &png_bytes,
            &result_meta,
            &req_meta,
            &limits,
            "handle-test-modal-999",
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ResultDecodeError::Wire(WireValidationError::PngCrcMismatch { .. })
                | ResultDecodeError::Wire(WireValidationError::ResultDigestMismatch { .. })
        ));
    }

    #[test]
    fn test_grayscale_color_type_rejected() {
        let width = 16;
        let height = 16;
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let data = vec![128u8; (width * height) as usize];
            writer.write_image_data(&data).unwrap();
        }

        let (req_meta, result_meta) = make_request_and_result_meta(width, height, &bytes);
        let limits = provisional_fixture_limits();

        let err = decode_result_crop(
            &bytes,
            &result_meta,
            &req_meta,
            &limits,
            "handle-test-modal-999",
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ResultDecodeError::Wire(WireValidationError::PngColorTypeMismatch { .. })
                | ResultDecodeError::UnexpectedColorType
        ));
    }

    #[test]
    fn test_rgba_color_type_rejected() {
        let width = 16;
        let height = 16;
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let data = vec![255u8; (width * height * 4) as usize];
            writer.write_image_data(&data).unwrap();
        }

        let (req_meta, result_meta) = make_request_and_result_meta(width, height, &bytes);
        let limits = provisional_fixture_limits();

        let err = decode_result_crop(
            &bytes,
            &result_meta,
            &req_meta,
            &limits,
            "handle-test-modal-999",
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ResultDecodeError::Wire(WireValidationError::PngColorTypeMismatch { .. })
                | ResultDecodeError::UnexpectedColorType
        ));
    }

    #[test]
    fn test_geometry_mismatch_rejected() {
        let width = 16;
        let height = 16;
        let png_bytes = make_valid_png_bytes(width, height, |_, _| [10, 20, 30]);
        let (req_meta, mut result_meta) = make_request_and_result_meta(width, height, &png_bytes);
        let limits = provisional_fixture_limits();

        // Tamper with declared metadata width
        result_meta.width = 32;

        let err = decode_result_crop(
            &png_bytes,
            &result_meta,
            &req_meta,
            &limits,
            "handle-test-modal-999",
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ResultDecodeError::Wire(WireValidationError::PngGeometryMismatch { .. })
        ));
    }

    #[test]
    fn test_hash_mismatch_rejected() {
        let width = 16;
        let height = 16;
        let png_bytes = make_valid_png_bytes(width, height, |_, _| [10, 20, 30]);
        let (req_meta, mut result_meta) = make_request_and_result_meta(width, height, &png_bytes);
        let limits = provisional_fixture_limits();

        // Tamper with result digest
        result_meta.result_digest = "0000000000000000000000000000000000000000000000000000000000000000".into();

        let err = decode_result_crop(
            &png_bytes,
            &result_meta,
            &req_meta,
            &limits,
            "handle-test-modal-999",
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ResultDecodeError::Wire(WireValidationError::ResultDigestMismatch { .. })
        ));
    }

    #[test]
    fn test_result_metadata_from_status_pending_rejected() {
        let req_meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        let mut status: JobStatusResponse = serde_json::from_str(JOB_STATUS_COMPLETED).unwrap();
        status.status = JobExecutionStatus::Pending;

        let err = result_metadata_from_status(&status, &req_meta, "handle-test-modal-999").unwrap_err();
        assert_eq!(err, WireValidationError::InvalidAcceptedStatus(JobExecutionStatus::Pending));
    }
    #[test]
    fn trailing_bytes_and_untrusted_allocation_limits_are_rejected() {
        let mut bytes = make_valid_png_bytes(16, 16, |_, _| [1, 2, 3]);
        bytes.extend_from_slice(b"trailing untrusted bytes");
        let (request, result) = make_request_and_result_meta(16, 16, &bytes);
        assert!(decode_result_crop(&bytes, &result, &request,
            &provisional_fixture_limits(), "handle-test-modal-999").is_err());
        let bytes = make_valid_png_bytes(16, 16, |_, _| [1, 2, 3]);
        let (mut request, mut result) = make_request_and_result_meta(16, 16, &bytes);
        request.width = 65536;
        request.height = 65536;
        result.width = request.width;
        result.height = request.height;
        let mut limits = provisional_fixture_limits();
        limits.max_width = u32::MAX;
        limits.max_height = u32::MAX;
        limits.max_pixels = u64::MAX;
        assert!(matches!(decode_result_crop(&bytes, &result, &request,
            &limits, "handle-test-modal-999"), Err(ResultDecodeError::DecompressionLimitExceeded)));
    }

}
