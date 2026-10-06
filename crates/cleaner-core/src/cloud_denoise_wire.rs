//! Page denoise contract for `/mc/denoise/v1` (`deploy/cloud/common/denoise.py`).
//!
//! One whole page and a recipe go up; one PNG of the same size comes back. The
//! recipe rules mirror `resolve_step`, so a recipe the gateway would refuse is
//! refused here before anything is uploaded. The request digest is computed
//! over the bytes the gateway computes it over: `canonical_recipe`, which is
//! Python's `json.dumps` with sorted keys, no spaces, ASCII only, and Python's
//! own spelling of a float ([`canonical_json`]).
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::cloud_wire::{validate_png_header, PngColorMode};

pub const VERSION: &str = "1.0.0";
/// The grant and journal name of a page denoise upload.
pub const CAPABILITY: &str = "page_denoise@1";
pub const RECIPE_SCHEMA: u32 = 1;
pub const MAX_STEPS: usize = 3;
pub const MAX_PNG_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_BODY_BYTES: usize = 24_000_000;
pub const MAX_RESPONSE_BYTES: usize = 24_000_000;
pub const MAX_PNG_B64_BYTES: usize = 4 * MAX_PNG_BYTES.div_ceil(3);
/// The gateway's page ceiling (`MAX_PAGE_PIXELS`).
pub const MAX_PAGE_PIXELS: u64 = 40_000_000;
/// The gateway's ceiling on a sharpen step's upscaled page (`MAX_UPSCALED_PIXELS`).
pub const MAX_UPSCALED_PIXELS: u64 = 160_000_000;
const DIGEST_DOMAIN: &[u8] = b"mc-denoise-v1\0";

/// waifu2x engines: name, architecture, domain, and whether it has 1x
/// (`noise{N}`) and 4x models. swin_unet `art` ships 2x models only.
const W2X_ENGINES: [(&str, &str, &str, bool, bool); 3] = [
    ("waifu2x-art-scan", "swin_unet", "art_scan", true, true),
    ("waifu2x-art", "swin_unet", "art", false, false),
    ("waifu2x-cunet-art", "cunet", "art", true, false),
];
/// Real-CUGAN (`CUGAN_LEVELS`): no level is conservative, 0 no denoise, 1 to 3
/// denoise strength. 3x and 4x ship conservative, no-denoise and denoise3x only.
const CUGAN_ENGINE: &str = "realcugan";
/// MangaJaNai: one model per trained page height, picked by the gateway per page.
const MANGAJANAI_ENGINE: &str = "mangajanai";
/// `MANGAJANAI_HEIGHTS`: the heights a `mangajanai.<s>x.auto` step may resolve to.
const MANGAJANAI_HEIGHTS: [u32; 7] = [1200, 1300, 1400, 1500, 1600, 1920, 2048];
const JPEG_ENGINES: [(&str, &str); 4] = [
    ("mangajpeg-hqplus", "omdb.1x-MangaJPEGHQPlus"),
    ("mangajpeg-hq", "omdb.1x-MangaJPEGHQ"),
    ("mangajpeg-mq", "omdb.1x-MangaJPEGMQ"),
    ("mangajpeg-lq", "omdb.1x-MangaJPEGLQ"),
];
const SHARPEN_SPANDREL_ENGINES: [(&str, &str); 1] = [("digimanga-bw-4x", "omdb.4x-eula-digimanga-bw-v2-nc1")];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DenoiseOp {
    Denoise,
    Sharpen,
    Jpeg,
}

impl DenoiseOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Denoise => "denoise",
            Self::Sharpen => "sharpen",
            Self::Jpeg => "jpeg",
        }
    }
}

/// One recipe step. Absent fields stay absent on the wire, so the digest is
/// over the step as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenoiseStep {
    pub op: DenoiseOp,
    pub engine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u8>,
    /// Blend of the step's output with its input, 0 to 1. Absent is 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<f64>,
}

/// The pinned model a step runs, and the factor it upscales by before the
/// gateway scales back down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStep {
    pub model_id: String,
    pub scale: u32,
}

impl DenoiseStep {
    /// `resolve_step`: validate the step and name its model.
    pub fn resolve(&self) -> Result<ResolvedStep, String> {
        if self.strength.is_some_and(|strength| !(0.0..=1.0).contains(&strength)) {
            return Err("strength must be a number from 0 to 1".into());
        }
        if self.level.is_some_and(|level| level > 3) {
            return Err("level must be an integer from 0 to 3".into());
        }
        let engine = self.engine.as_str();
        if self.op == DenoiseOp::Jpeg {
            let model = JPEG_ENGINES.iter().find(|(name, _)| *name == engine)
                .filter(|_| self.level.is_none() && self.scale.is_none())
                .ok_or("jpeg steps take a mangajpeg engine and no level or scale")?;
            return Ok(ResolvedStep { model_id: model.1.into(), scale: 1 });
        }
        if self.op == DenoiseOp::Sharpen && engine == MANGAJANAI_ENGINE {
            let scale = self.scale.unwrap_or(2);
            if self.level.is_some() || !matches!(scale, 2 | 4) {
                return Err("mangajanai takes scale 2 or 4 and no level".into());
            }
            return Ok(ResolvedStep { model_id: format!("mangajanai.{scale}x.auto"), scale: scale.into() });
        }
        if self.op == DenoiseOp::Sharpen && engine == CUGAN_ENGINE {
            let scale = self.scale.unwrap_or(2);
            if !matches!(scale, 2..=4) { return Err("realcugan scale must be 2, 3 or 4".into()); }
            let level = match self.level {
                None => "conservative".to_string(),
                Some(0) => "no-denoise".to_string(),
                Some(level) if scale == 2 || level == 3 => format!("denoise{level}x"),
                Some(level) => return Err(format!("realcugan has no {scale}x model at level {level}")),
            };
            return Ok(ResolvedStep { model_id: format!("realcugan.up{scale}x-latest-{level}"), scale: scale.into() });
        }
        if self.op == DenoiseOp::Sharpen {
            if let Some((_, model)) = SHARPEN_SPANDREL_ENGINES.iter().find(|(name, _)| *name == engine) {
                if self.level.is_some() || !matches!(self.scale, None | Some(4)) {
                    return Err(format!("{engine} is a fixed 4x model with no level"));
                }
                return Ok(ResolvedStep { model_id: (*model).into(), scale: 4 });
            }
        }
        let &(_, arch, domain, has_1x, has_4x) = W2X_ENGINES.iter().find(|(name, ..)| *name == engine)
            .ok_or_else(|| format!("engine {engine:?} is not available for {}", self.op.as_str()))?;
        let (stem, scale) = if self.op == DenoiseOp::Denoise {
            let (Some(level), None) = (self.level, self.scale) else {
                return Err("denoise steps take a level and no scale".into());
            };
            if !has_1x { return Err(format!("{engine} has no noise{level} model")); }
            (format!("noise{level}"), 1)
        } else {
            let scale = self.scale.unwrap_or(2);
            if !matches!(scale, 2 | 4) { return Err("sharpen scale must be 2 or 4".into()); }
            let stem = match self.level {
                None => format!("scale{scale}x"),
                Some(level) => format!("noise{level}_scale{scale}x"),
            };
            if scale == 4 && !has_4x { return Err(format!("{engine} has no {stem} model")); }
            (stem, u32::from(scale))
        };
        Ok(ResolvedStep { model_id: format!("waifu2x.{arch}.{domain}.{stem}"), scale })
    }
}

/// `{"schema": 1, "steps": [...]}`, 1 to 3 steps, run in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenoiseRecipe {
    pub schema: u32,
    pub steps: Vec<DenoiseStep>,
}

impl DenoiseRecipe {
    /// `resolve_recipe`: every step's model, in order.
    pub fn resolve(&self) -> Result<Vec<ResolvedStep>, String> {
        if self.schema != RECIPE_SCHEMA { return Err(format!("recipe schema must be {RECIPE_SCHEMA}")); }
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            return Err(format!("a recipe has 1 to {MAX_STEPS} steps"));
        }
        self.steps.iter().map(DenoiseStep::resolve).collect()
    }

    /// Whether a page of this size is inside the gateway's pixel limits for
    /// this recipe, the page's own and each sharpen step's upscaled one.
    pub fn fits(&self, width: u32, height: u32) -> Result<(), String> {
        let pixels = u64::from(width) * u64::from(height);
        if pixels == 0 || pixels > MAX_PAGE_PIXELS { return Err("page is too large".into()); }
        let largest = self.resolve()?.iter().map(|step| u64::from(step.scale)).max().unwrap_or(1);
        if pixels * largest * largest > MAX_UPSCALED_PIXELS {
            return Err("page is too large for this sharpen scale".into());
        }
        Ok(())
    }

    /// `canonical_recipe`, byte for byte.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        if self.steps.iter().any(|step| step.strength.is_some_and(|s| !s.is_finite())) {
            return Err("strength must be finite".into());
        }
        let value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        Ok(canonical_json(&value)?.into_bytes())
    }
}

/// Where a preset may run, and where a recorded denoise ran
/// ([`crate::project::DenoisedPage::target`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetTarget {
    Cloud,
    Local,
}

/// One shipped recipe (`PRESETS` in `denoise.py`, user choice 2026-09-28).
#[derive(Debug, Clone, Copy)]
pub struct DenoisePreset {
    pub id: &'static str,
    /// The recipe as `denoise.py` writes it: `{"schema": 1, "steps": [...]}`.
    pub recipe_json: &'static str,
    /// Cloud always; local only where the desktop has the engine.
    pub targets: &'static [PresetTarget],
}

impl DenoisePreset {
    pub fn recipe(&self) -> DenoiseRecipe {
        serde_json::from_str(self.recipe_json).expect("a shipped preset is a recipe")
    }

    pub fn runs_locally(&self) -> bool {
        self.targets.contains(&PresetTarget::Local)
    }
}

const CLOUD: &[PresetTarget] = &[PresetTarget::Cloud];

/// The six presets, in `denoise.py`'s order. Only the waifu2x one runs locally:
/// its model is ONNX, and the rest are PyTorch files the cloud worker loads.
pub const PRESETS: [DenoisePreset; 6] = [
    DenoisePreset {
        id: "waifu2x-scan-4x-n2",
        recipe_json: r#"{"schema":1,"steps":[{"op":"sharpen","engine":"waifu2x-art-scan","scale":4,"level":2}]}"#,
        targets: &[PresetTarget::Cloud, PresetTarget::Local],
    },
    DenoisePreset {
        id: "realcugan-2x-conservative",
        recipe_json: r#"{"schema":1,"steps":[{"op":"sharpen","engine":"realcugan","scale":2}]}"#,
        targets: CLOUD,
    },
    DenoisePreset {
        id: "realcugan-3x-conservative",
        recipe_json: r#"{"schema":1,"steps":[{"op":"sharpen","engine":"realcugan","scale":3}]}"#,
        targets: CLOUD,
    },
    DenoisePreset {
        id: "realcugan-3x-denoise3",
        recipe_json: r#"{"schema":1,"steps":[{"op":"sharpen","engine":"realcugan","scale":3,"level":3}]}"#,
        targets: CLOUD,
    },
    DenoisePreset {
        id: "mangajanai-2x",
        recipe_json: r#"{"schema":1,"steps":[{"op":"sharpen","engine":"mangajanai","scale":2}]}"#,
        targets: CLOUD,
    },
    DenoisePreset {
        id: "mangajanai-4x",
        recipe_json: r#"{"schema":1,"steps":[{"op":"sharpen","engine":"mangajanai","scale":4}]}"#,
        targets: CLOUD,
    },
];

pub fn preset(id: &str) -> Option<&'static DenoisePreset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

/// Whether a result's step names the model the request resolved to. A
/// MangaJaNai step resolves to `mangajanai.<s>x.auto` and the gateway reports
/// the height it picked for the page, `mangajanai.<s>x.<h>p`.
fn ran_as(reported: &str, resolved: &str) -> bool {
    match resolved.strip_suffix("auto") {
        Some(prefix) if resolved.starts_with("mangajanai.") => reported.strip_prefix(prefix)
            .and_then(|height| height.strip_suffix('p'))
            .and_then(|height| height.parse::<u32>().ok())
            .is_some_and(|height| MANGAJANAI_HEIGHTS.contains(&height)),
        _ => reported == resolved,
    }
}

/// `request_digest`: SHA-256 over the domain, the canonical recipe and the page.
pub fn request_digest(recipe: &DenoiseRecipe, page_png: &[u8]) -> Result<String, String> {
    let mut digest = Sha256::new();
    digest.update(DIGEST_DOMAIN);
    digest.update(recipe.canonical_bytes()?);
    digest.update(b"\0");
    digest.update(page_png);
    Ok(format!("{:x}", digest.finalize()))
}

/// Python's `json.dumps(value, sort_keys=True, separators=(",", ":"),
/// ensure_ascii=True)`. Integers print as integers and floats as `repr`
/// prints them, so `1.0` stays `1.0` and `1e-05` keeps its two-digit exponent.
pub fn canonical_json(value: &Value) -> Result<String, String> {
    let mut out = String::new();
    write_canonical(value, &mut out)?;
    Ok(out)
}

fn write_canonical(value: &Value, out: &mut String) -> Result<(), String> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                out.push_str(&integer.to_string());
            } else if let Some(integer) = number.as_u64() {
                out.push_str(&integer.to_string());
            } else {
                let float = number.as_f64().filter(|f| f.is_finite()).ok_or("non-finite number")?;
                out.push_str(&python_float(float));
            }
        }
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 { out.push(','); }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 { out.push(','); }
                write_string(key, out);
                out.push(':');
                write_canonical(&map[key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// `repr(float)`: the shortest digits that round-trip (which Rust's `{:e}`
/// also gives), fixed notation for exponents from -4 to 15, and otherwise
/// scientific with a sign and at least two exponent digits.
fn python_float(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let body = if (-4..16).contains(&exponent) {
        if exponent >= 0 {
            let whole = exponent as usize + 1;
            if digits.len() <= whole {
                format!("{digits}{}.0", "0".repeat(whole - digits.len()))
            } else {
                format!("{}.{}", &digits[..whole], &digits[whole..])
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
        }
    } else {
        let mantissa = if digits.len() == 1 { digits } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{mantissa}e{}{:02}", if exponent < 0 { '-' } else { '+' }, exponent.abs())
    };
    format!("{sign}{body}")
}

fn hex_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The request's `metadata`: exactly the three fields the gateway accepts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenoiseMetadata {
    pub protocol_version: String,
    pub request_digest: String,
    pub recipe: DenoiseRecipe,
}

impl DenoiseMetadata {
    pub fn new(recipe: DenoiseRecipe, page_png: &[u8]) -> Result<Self, String> {
        let request_digest = request_digest(&recipe, page_png)?;
        let metadata = Self { protocol_version: VERSION.into(), request_digest, recipe };
        metadata.validate(page_png)?;
        Ok(metadata)
    }

    /// `validate_denoise_request`, in the gateway's order.
    pub fn validate(&self, page_png: &[u8]) -> Result<Vec<ResolvedStep>, String> {
        if self.protocol_version != VERSION { return Err(format!("protocol_version must be {VERSION}")); }
        if !page_png.starts_with(PNG_SIGNATURE) { return Err("page must be a PNG".into()); }
        if page_png.len() > MAX_PNG_BYTES { return Err("page PNG is too large".into()); }
        let steps = self.recipe.resolve()?;
        if self.request_digest != request_digest(&self.recipe, page_png)? {
            return Err("request_digest does not match the recipe and page".into());
        }
        Ok(steps)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenoiseStepTiming {
    pub model_id: String,
    pub ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenoiseInfo {
    pub width: u32,
    pub height: u32,
    pub gray: bool,
    pub steps: Vec<DenoiseStepTiming>,
}

/// A 200 answer. `page_png_b64` is decoded by the caller and checked with
/// [`DenoiseResult::validate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenoiseResult {
    pub protocol_version: String,
    pub request_digest: String,
    pub page_png_b64: String,
    pub info: DenoiseInfo,
}

impl DenoiseResult {
    /// The answer is bound to the request (version, digest echo, the recipe's
    /// models in order) and its PNG is the uploaded page's size. The upload is
    /// an 8-bit page without alpha, so the gateway answers 8-bit grey when it
    /// found the page grey and 8-bit RGB otherwise.
    pub fn validate(&self, request: &DenoiseMetadata, width: u32, height: u32, png: &[u8]) -> Result<(), String> {
        if self.protocol_version != VERSION || self.request_digest != request.request_digest {
            return Err("denoise result identity mismatch".into());
        }
        let models = request.recipe.resolve()?;
        if self.info.width != width || self.info.height != height || self.info.steps.len() != models.len()
            || self.info.steps.iter().zip(&models).any(|(timing, step)| !ran_as(&timing.model_id, &step.model_id))
        {
            return Err("denoise result does not describe the request".into());
        }
        if png.len() > MAX_PNG_BYTES { return Err("denoise result PNG too large".into()); }
        let color = if self.info.gray { PngColorMode::Gray8 } else { PngColorMode::Rgb8 };
        validate_png_header(png, width, height, color).map_err(|e| e.to_string())
    }
}

/// An error answer: `_denoise_error` in the gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenoiseRejection {
    pub protocol_version: String,
    pub error_code: String,
    pub message: String,
    pub request_digest: Option<String>,
}

impl DenoiseRejection {
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != VERSION
            || !matches!(self.error_code.as_str(), "unauthorized" | "invalid_request" | "payload_too_large"
                | "page_too_large" | "unsupported_page" | "capability_unavailable" | "inference_failed")
            || self.message.is_empty() || self.message.len() > 1024
            || self.request_digest.as_ref().is_some_and(|digest| !hex_hash(digest))
        { return Err("invalid denoise rejection".into()); }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenoiseEngines {
    pub denoise: Vec<String>,
    pub sharpen: Vec<String>,
    pub jpeg: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenoiseModelInfo {
    pub model_id: String,
    pub license: String,
    pub author: String,
    pub seeded: bool,
}

/// `GET /mc/denoise/v1/capabilities`: which engines have their models on the
/// weights volume, per op.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenoiseCapabilities {
    pub protocol_version: String,
    pub available: bool,
    /// The shipped presets whose every model is seeded. Absent from an older gateway.
    #[serde(default)]
    pub presets: Vec<String>,
    pub engines: DenoiseEngines,
    pub models: Vec<DenoiseModelInfo>,
}

impl DenoiseCapabilities {
    pub fn validate(&self) -> Result<(), String> {
        let known = |op: DenoiseOp, names: &[String]| names.len() <= 8 && names.iter().all(|name| match op {
            DenoiseOp::Jpeg => JPEG_ENGINES.iter().any(|(known, _)| known == name),
            DenoiseOp::Denoise => W2X_ENGINES.iter().any(|&(known, _, _, has_1x, _)| known == name && has_1x),
            DenoiseOp::Sharpen => W2X_ENGINES.iter().any(|(known, ..)| known == name)
                || [MANGAJANAI_ENGINE, CUGAN_ENGINE].contains(&name.as_str())
                || SHARPEN_SPANDREL_ENGINES.iter().any(|(known, _)| known == name),
        });
        let text = |value: &str| !value.is_empty() && value.len() <= 128 && value.bytes().all(|c| c.is_ascii_graphic() || c == b' ');
        if self.protocol_version != VERSION
            || !known(DenoiseOp::Denoise, &self.engines.denoise)
            || !known(DenoiseOp::Sharpen, &self.engines.sharpen)
            || !known(DenoiseOp::Jpeg, &self.engines.jpeg)
            || self.presets.len() > PRESETS.len() || !self.presets.iter().all(|id| preset(id).is_some())
            || self.models.len() > 64
            || !self.models.iter().all(|model| text(&model.model_id) && text(&model.license) && text(&model.author))
        { return Err("invalid denoise capabilities".into()); }
        Ok(())
    }

    /// Whether every step's engine is advertised for its op.
    pub fn supports(&self, recipe: &DenoiseRecipe) -> bool {
        self.available && recipe.steps.iter().all(|step| {
            let names = match step.op {
                DenoiseOp::Denoise => &self.engines.denoise,
                DenoiseOp::Sharpen => &self.engines.sharpen,
                DenoiseOp::Jpeg => &self.engines.jpeg,
            };
            names.contains(&step.engine)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_PNG: &[u8] = include_bytes!("../../../deploy/cloud/fixtures/tiny_image.png");

    fn recipe(json: &str) -> DenoiseRecipe {
        serde_json::from_str(json).unwrap()
    }

    fn step(json: Value) -> Result<ResolvedStep, String> {
        serde_json::from_value::<DenoiseStep>(json).map_err(|e| e.to_string())?.resolve()
    }

    fn png(width: u32, height: u32, color: png::ColorType) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            let samples = if color == png::ColorType::Rgb { 3 } else { 1 };
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&vec![128; (width * height * samples) as usize]).unwrap();
        }
        bytes
    }

    // Golden values from deploy/cloud/common/denoise.py over tiny_image.png:
    // canonical_recipe(r).decode(), request_digest(r, png), and each step's model_id.
    const THREE_STEPS: &str = r#"{"schema":1,"steps":[
        {"op":"denoise","engine":"waifu2x-art-scan","level":1,"strength":0.5},
        {"strength":1e-05,"op":"sharpen","engine":"waifu2x-cunet-art","scale":2,"level":3},
        {"op":"jpeg","engine":"mangajpeg-hq","strength":1.0}]}"#;

    #[test]
    fn denoise_digest_matches_python_byte_for_byte() {
        let three = recipe(THREE_STEPS);
        assert_eq!(
            String::from_utf8(three.canonical_bytes().unwrap()).unwrap(),
            r#"{"schema":1,"steps":[{"engine":"waifu2x-art-scan","level":1,"op":"denoise","strength":0.5},{"engine":"waifu2x-cunet-art","level":3,"op":"sharpen","scale":2,"strength":1e-05},{"engine":"mangajpeg-hq","op":"jpeg","strength":1.0}]}"#,
        );
        assert_eq!(request_digest(&three, TINY_PNG).unwrap(),
            "1a8a64640123deab2849e07a756e77313ccb4de358a85d7ef851642cd189125f");
        let models: Vec<String> = three.resolve().unwrap().into_iter().map(|step| step.model_id).collect();
        assert_eq!(models, ["waifu2x.swin_unet.art_scan.noise1", "waifu2x.cunet.art.noise3_scale2x", "omdb.1x-MangaJPEGHQ"]);

        let fixed = recipe(r#"{"schema":1,"steps":[{"op":"sharpen","engine":"digimanga-bw-4x"}]}"#);
        assert_eq!(fixed.canonical_bytes().unwrap(), br#"{"schema":1,"steps":[{"engine":"digimanga-bw-4x","op":"sharpen"}]}"#);
        assert_eq!(request_digest(&fixed, TINY_PNG).unwrap(),
            "e09fcb3dd611f11b43d9d3b4b5255cc602481bf515f65fa3759d62e32f28ffdb");
    }

    #[test]
    fn denoise_canonical_json_spells_values_as_python_does() {
        for (value, python) in [(0.1, "0.1"), (1.0 / 3.0, "0.3333333333333333"), (1e-5, "1e-05"), (1e-4, "0.0001"),
            (9.999e-5, "9.999e-05"), (2.5e-7, "2.5e-07"), (1.0, "1.0"), (0.0, "0.0"), (0.5, "0.5"), (123.0, "123.0"),
            (1234567890123456.0, "1234567890123456.0"), (1e16, "1e+16"), (12345678901234567.0, "1.2345678901234568e+16")] {
            assert_eq!(python_float(value), python, "{value}");
        }
        let value = serde_json::json!({"b": "\u{e9}\n\"\\\u{7f}\u{1F600}", "a": [1, true, null]});
        assert_eq!(canonical_json(&value).unwrap(), r#"{"a":[1,true,null],"b":"\u00e9\n\"\\\u007f\ud83d\ude00"}"#);
    }

    #[test]
    fn denoise_recipe_rules_mirror_resolve_step() {
        use serde_json::json;
        // Each case was run through resolve_step in deploy/cloud/common/denoise.py.
        for (case, model) in [
            (json!({"op": "denoise", "engine": "waifu2x-art-scan", "level": 0}), "waifu2x.swin_unet.art_scan.noise0"),
            (json!({"op": "denoise", "engine": "waifu2x-cunet-art", "level": 3, "strength": 0}), "waifu2x.cunet.art.noise3"),
            (json!({"op": "sharpen", "engine": "waifu2x-art-scan"}), "waifu2x.swin_unet.art_scan.scale2x"),
            (json!({"op": "sharpen", "engine": "waifu2x-art-scan", "scale": 4, "level": 2}), "waifu2x.swin_unet.art_scan.noise2_scale4x"),
            (json!({"op": "sharpen", "engine": "waifu2x-cunet-art", "scale": 2}), "waifu2x.cunet.art.scale2x"),
            (json!({"op": "sharpen", "engine": "digimanga-bw-4x", "scale": 4}), "omdb.4x-eula-digimanga-bw-v2-nc1"),
            (json!({"op": "jpeg", "engine": "mangajpeg-lq", "strength": 0.25}), "omdb.1x-MangaJPEGLQ"),
            (json!({"op": "sharpen", "engine": "realcugan"}), "realcugan.up2x-latest-conservative"),
            (json!({"op": "sharpen", "engine": "realcugan", "scale": 2, "level": 0}), "realcugan.up2x-latest-no-denoise"),
            (json!({"op": "sharpen", "engine": "realcugan", "scale": 2, "level": 1}), "realcugan.up2x-latest-denoise1x"),
            (json!({"op": "sharpen", "engine": "realcugan", "scale": 2, "level": 2}), "realcugan.up2x-latest-denoise2x"),
            (json!({"op": "sharpen", "engine": "realcugan", "scale": 3, "level": 3}), "realcugan.up3x-latest-denoise3x"),
            (json!({"op": "sharpen", "engine": "realcugan", "scale": 3, "level": 0}), "realcugan.up3x-latest-no-denoise"),
            (json!({"op": "sharpen", "engine": "realcugan", "scale": 4}), "realcugan.up4x-latest-conservative"),
            (json!({"op": "sharpen", "engine": "realcugan", "scale": 4, "level": 0}), "realcugan.up4x-latest-no-denoise"),
            (json!({"op": "sharpen", "engine": "realcugan", "scale": 4, "level": 3, "strength": 0.5}), "realcugan.up4x-latest-denoise3x"),
            (json!({"op": "sharpen", "engine": "mangajanai"}), "mangajanai.2x.auto"),
            (json!({"op": "sharpen", "engine": "mangajanai", "scale": 4}), "mangajanai.4x.auto"),
            (json!({"op": "sharpen", "engine": "waifu2x-art"}), "waifu2x.swin_unet.art.scale2x"),
            (json!({"op": "sharpen", "engine": "waifu2x-art", "level": 3}), "waifu2x.swin_unet.art.noise3_scale2x"),
            (json!({"op": "sharpen", "engine": "waifu2x-art", "scale": 2, "level": 0}), "waifu2x.swin_unet.art.noise0_scale2x"),
        ] {
            assert_eq!(step(case.clone()).unwrap().model_id, model, "{case}");
        }
        for case in [
            json!({"op": "sharpen", "engine": "waifu2x-cunet-art", "scale": 4}),
            json!({"op": "sharpen", "engine": "digimanga-bw-4x", "level": 1}),
            json!({"op": "sharpen", "engine": "digimanga-bw-4x", "scale": 2}),
            json!({"op": "denoise", "engine": "waifu2x-art-scan"}),
            json!({"op": "denoise", "engine": "waifu2x-art-scan", "level": 1, "scale": 2}),
            json!({"op": "denoise", "engine": "digimanga-bw-4x", "level": 1}),
            json!({"op": "jpeg", "engine": "mangajpeg-hq", "level": 1}),
            json!({"op": "jpeg", "engine": "waifu2x-art-scan"}),
            json!({"op": "denoise", "engine": "waifu2x-art-scan", "level": 4}),
            json!({"op": "denoise", "engine": "waifu2x-art-scan", "level": 1, "strength": 1.5}),
            json!({"op": "sharpen", "engine": "waifu2x-art-scan", "scale": 3}),
            json!({"op": "denoise", "engine": "nope", "level": 1}),
            json!({"op": "sharpen", "engine": "realcugan", "scale": 3, "level": 1}),
            json!({"op": "sharpen", "engine": "realcugan", "scale": 3, "level": 2}),
            json!({"op": "sharpen", "engine": "realcugan", "scale": 4, "level": 2}),
            json!({"op": "sharpen", "engine": "realcugan", "scale": 5}),
            json!({"op": "sharpen", "engine": "realcugan", "scale": 1}),
            json!({"op": "sharpen", "engine": "realcugan", "level": 4}),
            json!({"op": "denoise", "engine": "realcugan", "level": 1}),
            json!({"op": "jpeg", "engine": "realcugan"}),
            json!({"op": "sharpen", "engine": "mangajanai", "scale": 3}),
            json!({"op": "sharpen", "engine": "mangajanai", "level": 1}),
            json!({"op": "denoise", "engine": "mangajanai", "level": 1}),
            json!({"op": "sharpen", "engine": "waifu2x-art", "scale": 4}),
            json!({"op": "sharpen", "engine": "waifu2x-art", "scale": 4, "level": 1}),
            json!({"op": "denoise", "engine": "waifu2x-art", "level": 1}),
            // Refused by the types before the rules: an unknown op or field.
            json!({"op": "upscale", "engine": "waifu2x-art-scan"}),
            json!({"op": "denoise", "engine": "waifu2x-art-scan", "level": 1, "tile": 256}),
        ] {
            assert!(step(case.clone()).is_err(), "{case}");
        }
        let mut nan = recipe(THREE_STEPS);
        nan.steps[0].strength = Some(f64::NAN);
        assert!(nan.resolve().is_err() && nan.canonical_bytes().is_err());
        assert!(recipe(r#"{"schema":2,"steps":[{"op":"sharpen","engine":"waifu2x-art-scan"}]}"#).resolve().is_err());
        assert!(recipe(r#"{"schema":1,"steps":[]}"#).resolve().is_err());
        let four = r#"{"op":"sharpen","engine":"waifu2x-art-scan"}"#;
        assert!(recipe(&format!(r#"{{"schema":1,"steps":[{four},{four},{four},{four}]}}"#)).resolve().is_err());
        assert!(serde_json::from_str::<DenoiseRecipe>(r#"{"schema":1,"steps":[],"extra":1}"#).is_err());

        let sharpen4 = recipe(r#"{"schema":1,"steps":[{"op":"sharpen","engine":"digimanga-bw-4x"}]}"#);
        assert!(sharpen4.fits(3000, 3000).is_ok());
        assert!(sharpen4.fits(4000, 4000).is_err(), "16 MP at 4x is past the upscaled ceiling");
        assert!(recipe(THREE_STEPS).fits(8000, 5001).is_err(), "past the page ceiling");
    }

    #[test]
    fn denoise_result_is_bound_to_request_and_page() {
        let page = png(16, 16, png::ColorType::Rgb);
        let request = DenoiseMetadata::new(recipe(THREE_STEPS), &page).unwrap();
        assert_eq!(request.validate(&page).unwrap().len(), 3);
        assert!(request.validate(&png(16, 15, png::ColorType::Rgb)).is_err(), "digest binds the page");
        let answer = png(16, 16, png::ColorType::Grayscale);
        let result = DenoiseResult {
            protocol_version: VERSION.into(),
            request_digest: request.request_digest.clone(),
            page_png_b64: String::new(),
            info: DenoiseInfo {
                width: 16, height: 16, gray: true,
                steps: request.recipe.resolve().unwrap().into_iter()
                    .map(|step| DenoiseStepTiming { model_id: step.model_id, ms: 5 }).collect(),
            },
        };
        result.validate(&request, 16, 16, &answer).unwrap();

        let mut echo = result.clone();
        echo.request_digest = "0".repeat(64);
        assert!(echo.validate(&request, 16, 16, &answer).is_err(), "a digest that is not the request's");
        let mut version = result.clone();
        version.protocol_version = "1.0.1".into();
        assert!(version.validate(&request, 16, 16, &answer).is_err());
        let mut steps = result.clone();
        steps.info.steps.pop();
        assert!(steps.validate(&request, 16, 16, &answer).is_err());
        assert!(result.validate(&request, 16, 16, b"GIF89a not a png at all, long enough to pass").is_err());
        assert!(result.validate(&request, 16, 16, &png(16, 16, png::ColorType::Rgb)).is_err(), "grey said, RGB sent");
        assert!(result.validate(&request, 17, 16, &answer).is_err());
        let mut oversize = answer.clone();
        oversize.resize(MAX_PNG_BYTES + 1, 0);
        assert!(result.validate(&request, 16, 16, &oversize).is_err());
        let mut extra = serde_json::to_value(&result).unwrap();
        extra["reported_cost_usd"] = serde_json::json!(0.01);
        assert!(serde_json::from_value::<DenoiseResult>(extra).is_err());
    }

    #[test]
    fn denoise_rejection_and_capabilities_are_strict() {
        let rejection: DenoiseRejection = serde_json::from_str(
            r#"{"protocol_version":"1.0.0","error_code":"unsupported_page","message":"P pages are not denoised","request_digest":null}"#,
        ).unwrap();
        rejection.validate().unwrap();
        let mut unknown = rejection.clone();
        unknown.error_code = "teapot".into();
        assert!(unknown.validate().is_err());

        let capabilities: DenoiseCapabilities = serde_json::from_value(serde_json::json!({
            "protocol_version": "1.0.0", "available": true,
            "engines": {"denoise": ["waifu2x-art-scan"], "sharpen": ["waifu2x-art-scan", "digimanga-bw-4x"], "jpeg": []},
            "models": [{"model_id": "waifu2x.swin_unet.art_scan.noise1", "license": "MIT", "author": "nagadomi (nunif)", "seeded": true}],
        })).unwrap();
        capabilities.validate().unwrap();
        assert!(!capabilities.supports(&recipe(THREE_STEPS)), "no cunet sharpen, no jpeg");
        assert!(capabilities.supports(&recipe(r#"{"schema":1,"steps":[{"op":"sharpen","engine":"digimanga-bw-4x"}]}"#)));
        let mut bad = capabilities.clone();
        bad.engines.jpeg.push("waifu2x-art-scan".into());
        assert!(bad.validate().is_err());

        // What the gateway answers now: presets, and the PyTorch sharpen engines.
        let current: DenoiseCapabilities = serde_json::from_value(serde_json::json!({
            "protocol_version": "1.0.0", "available": true,
            "presets": ["realcugan-2x-conservative", "mangajanai-4x"],
            "engines": {"denoise": [], "sharpen": ["waifu2x-art", "mangajanai", "realcugan"], "jpeg": []},
            "models": [],
        })).unwrap();
        current.validate().unwrap();
        assert!(current.supports(&PRESETS[1].recipe()));
        let mut unknown = current.clone();
        unknown.presets.push("sharpest".into());
        assert!(unknown.validate().is_err());
        let mut no_1x = current.clone();
        no_1x.engines.denoise.push("waifu2x-art".into());
        assert!(no_1x.validate().is_err(), "waifu2x-art has no 1x models");
    }

    #[test]
    fn denoise_presets_validate_and_match_python() {
        // canonical_recipe(PRESETS[id]) and request_digest(PRESETS[id], tiny_image.png)
        // from deploy/cloud/common/denoise.py, with each step's model_id.
        let golden = [
            ("waifu2x-scan-4x-n2", r#"{"schema":1,"steps":[{"engine":"waifu2x-art-scan","level":2,"op":"sharpen","scale":4}]}"#,
                "51a4fcfff58569f19cbc95ba6cf72f7c19e1f70fe5ada9a49e70adba7622b879", "waifu2x.swin_unet.art_scan.noise2_scale4x", 4),
            ("realcugan-2x-conservative", r#"{"schema":1,"steps":[{"engine":"realcugan","op":"sharpen","scale":2}]}"#,
                "82accc426e9d552ed1dbcd3baa7fb5efed51c9cf8395d2e76ab7fb2975f61980", "realcugan.up2x-latest-conservative", 2),
            ("realcugan-3x-conservative", r#"{"schema":1,"steps":[{"engine":"realcugan","op":"sharpen","scale":3}]}"#,
                "69ea899e1b36bda3f3bf9fcaa90389cd49b4791215ba393d01780d1c1f7e4fe5", "realcugan.up3x-latest-conservative", 3),
            ("realcugan-3x-denoise3", r#"{"schema":1,"steps":[{"engine":"realcugan","level":3,"op":"sharpen","scale":3}]}"#,
                "dc16291efd06bcde8191e1839152f458eade81caa7812c97a0278e7718cb0f21", "realcugan.up3x-latest-denoise3x", 3),
            ("mangajanai-2x", r#"{"schema":1,"steps":[{"engine":"mangajanai","op":"sharpen","scale":2}]}"#,
                "d6369d2839ea7c19908ca3e1d0c39972b0d2c9e3eca1c4a628e62b813a5ac721", "mangajanai.2x.auto", 2),
            ("mangajanai-4x", r#"{"schema":1,"steps":[{"engine":"mangajanai","op":"sharpen","scale":4}]}"#,
                "c2f364a61b03ba3424769c0fd26eedc2c817a74e4dbafbbcbcf1be72564ebe2d", "mangajanai.4x.auto", 4),
        ];
        assert_eq!(PRESETS.len(), golden.len());
        for (preset, (id, canonical, digest, model, scale)) in PRESETS.iter().zip(golden) {
            assert_eq!(preset.id, id);
            let recipe = preset.recipe();
            assert_eq!(recipe.resolve().unwrap(), [ResolvedStep { model_id: model.into(), scale }], "{id}");
            assert_eq!(String::from_utf8(recipe.canonical_bytes().unwrap()).unwrap(), canonical, "{id}");
            assert_eq!(request_digest(&recipe, TINY_PNG).unwrap(), digest, "{id}");
            assert!(preset.targets.contains(&PresetTarget::Cloud), "{id}");
            assert_eq!(super::preset(id).map(|found| found.id), Some(id));
        }
        let local: Vec<&str> = PRESETS.iter().filter(|preset| preset.runs_locally()).map(|preset| preset.id).collect();
        assert_eq!(local, ["waifu2x-scan-4x-n2"]);
        assert!(super::preset("custom").is_none());
    }

    #[test]
    fn denoise_result_may_name_the_mangajanai_height_it_picked() {
        assert!(ran_as("mangajanai.2x.1920p", "mangajanai.2x.auto"));
        assert!(ran_as("mangajanai.4x.1200p", "mangajanai.4x.auto"));
        assert!(!ran_as("mangajanai.4x.1920p", "mangajanai.2x.auto"), "the wrong scale");
        assert!(!ran_as("mangajanai.2x.1700p", "mangajanai.2x.auto"), "no model at that height");
        assert!(!ran_as("mangajanai.2x.auto", "mangajanai.2x.auto"), "the gateway names what ran");
        assert!(ran_as("realcugan.up2x-latest-conservative", "realcugan.up2x-latest-conservative"));
        assert!(!ran_as("realcugan.up2x-latest-denoise1x", "realcugan.up2x-latest-conservative"));
    }
}
