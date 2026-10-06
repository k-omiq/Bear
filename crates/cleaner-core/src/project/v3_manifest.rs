//! Test-only manifest shape from HEAD, before text-shaped and revision fields.
#![allow(dead_code)]

use std::path::PathBuf;

use super::StripMode;
use crate::image::{BitDepth, ColorMode};
use crate::mask::Rect;
use crate::patch::{Engine, Provenance};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectSource {
    pub rel_path: PathBuf,
    pub sha256: String,
    pub mtime: Option<u64>,
    pub w: u32,
    pub h: u32,
    pub mode: ColorMode,
    pub bit_depth: BitDepth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converted_from: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Strip {
    pub mode: StripMode,
    pub order: Vec<usize>,
    #[serde(default)]
    pub splits: Vec<crate::strip::Split>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PatchRecord {
    pub id: String,
    pub source_idx: usize,
    pub bbox: Rect,
    pub mask_ref: String,
    pub buffer_ref: String,
    pub engine: Engine,
    pub order: u32,
    pub visible: bool,
    pub review_state: Option<String>,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegionUntouched {
    pub source_idx: usize,
    pub bbox: Rect,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkippedInput {
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InputReport {
    pub skipped: Vec<SkippedInput>,
    pub junk_skipped: u32,
    pub duplicate_basenames: Vec<String>,
    pub converted: Vec<ConvertedInput>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ConvertedInput {
    pub file: String,
    pub from: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Counters {
    pub refused: u32,
    pub errored: u32,
    pub param_reject: u32,
    pub residual_reject: u32,
    pub structural_reject: u32,
    pub declined: u32,
    pub gate_dropped: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Project {
    pub version: u32,
    pub created: u64,
    pub app_version: String,
    pub sources: Vec<ProjectSource>,
    pub strip: Strip,
    pub patches: Vec<PatchRecord>,
    pub regions_untouched: Vec<RegionUntouched>,
    pub counters: Counters,
    #[serde(default)]
    pub input_report: InputReport,
    #[serde(default)]
    pub examined: Vec<usize>,
    #[serde(default)]
    pub errored: Vec<usize>,
    #[serde(default)]
    pub interrupted_at: Option<u32>,
    pub settings: serde_json::Value,
}
