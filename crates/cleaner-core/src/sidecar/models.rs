//! The models this computer can download and run through the sidecar.
//!
//! One table, so Settings, the installer and a render read one answer: what a
//! model is called, where its weights come from, which runtime it needs, and
//! how it is asked to clean a region.
//!
//! **Which runtime.** FLUX.2 Klein 4B has weights for MLX and for torch, so it
//! follows the Backend setting. Klein 9B, FLUX 3 and Qwen run only on torch: the
//! MLX arm (`backend/mflux.py`) holds Klein 4B alone, and the torch arm reuses
//! the weights and the recipes the cloud worker runs. So on a Mac those three
//! use Metal, whatever the setting says ([`LocalModel::backend`]).

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use super::client::RENDER_TIMEOUT;
use super::Backend;
use crate::engines::render::Preprocessing;

/// A file from another repository that lands inside a model's folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Adapter {
    pub repo: &'static str,
    pub revision: &'static str,
    pub file: &'static str,
}

/// One download: a pinned repository, the folder it lands in under `weights/`,
/// and what it costs on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Checkpoint {
    pub repo: &'static str,
    pub revision: &'static str,
    pub dir: &'static str,
    pub bytes: u64,
    /// A file only a complete download has; the installer checks for it.
    pub expected: &'static str,
    pub adapters: &'static [Adapter],
}

/// The instruction a model gets, and whether it names where the lettering is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// The same words for every region. The hole says where.
    Fixed(&'static str),
    /// An edit model takes no mask, so the words name the area: `{location}`
    /// becomes the lettering's place in its crop ([`hint_location`]).
    Located(&'static str),
}

/// How a model is asked to clean a region. The cloud worker's recipe for the
/// same weights; deploy/cloud/tests/test_recipe_parity.py holds them equal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Recipe {
    pub prompt: Prompt,
    pub steps: u32,
    pub guidance: f32,
    pub preprocessing: Preprocessing,
    /// How long one region may take. A retrying edit model on Metal can pass
    /// [`RENDER_TIMEOUT`] several times over.
    pub render_timeout: Duration,
}

/// One row of the table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalModel {
    pub id: &'static str,
    pub label: &'static str,
    /// Kept for projects that use it, folded away in Settings.
    pub legacy: bool,
    pub mlx: Option<Checkpoint>,
    pub torch: Checkpoint,
    pub recipe: Recipe,
}

impl LocalModel {
    /// The runtime this model runs on, given the one the setting resolved to.
    pub fn backend(&self, preferred: Backend) -> Backend {
        if preferred == Backend::Mflux && self.mlx.is_some() { Backend::Mflux } else { Backend::Sdnq }
    }

    /// The download this model needs on `backend`.
    pub fn checkpoint(&self, backend: Backend) -> Checkpoint {
        match (backend, self.mlx) {
            (Backend::Mflux, Some(mlx)) => mlx,
            _ => self.torch,
        }
    }

    /// Whether its download for `preferred` is on disk under `weights`.
    ///
    /// The installer moves a folder here only once it is complete
    /// (`sidecar/bootstrap.py`), but an older one downloaded straight into it,
    /// so a folder counts only with the file the download ends with. MLX reads
    /// its one folder. The torch arm finds a model by a folder named after its
    /// id (`backend/sdnq.py#find_weights_dir`), so a folder somebody placed by
    /// hand counts, and MLX's own folder, which it cannot read, does not.
    pub fn installed(&self, preferred: Backend, weights: &Path) -> bool {
        let backend = self.backend(preferred);
        let checkpoint = self.checkpoint(backend);
        if backend == Backend::Mflux {
            return weights.join(checkpoint.dir).join(checkpoint.expected).is_file();
        }
        std::fs::read_dir(weights).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                name.starts_with(self.id) && !name.contains("-mflux") && holds(&entry.path(), checkpoint.expected, 2)
            })
        })
    }
}

/// Whether `file` is in `folder` or up to `depth` levels under it: a cache entry
/// placed by hand keeps its snapshot at `snapshots/<revision>`
/// (`backend/sdnq.py#resolve_pipeline_dir`).
fn holds(folder: &Path, file: &str, depth: u32) -> bool {
    folder.join(file).is_file() || (depth > 0 && std::fs::read_dir(folder).is_ok_and(|entries| {
        entries.flatten().any(|entry| holds(&entry.path(), file, depth - 1))
    }))
}

/// FLUX.2 Klein: the crop with the lettering held out of it.
const KLEIN_RECIPE: Recipe = Recipe {
    prompt: Prompt::Fixed(crate::engines::flux::PROMPT),
    steps: crate::engines::flux::STEPS,
    guidance: crate::engines::flux::GUIDANCE,
    preprocessing: Preprocessing::CURRENT,
    render_timeout: RENDER_TIMEOUT,
};

/// `QWEN_PROMPT` built by `deploy/cloud/common/qwen.py#build_prompt` for the
/// `auto` target.
pub const QWEN_PROMPT: &str = "Remove the text and lettering in the {location} area of this crop. Reconstruct the background and thin lines hidden behind the erased lettering. Preserve all character artwork, hands, faces, hair, poses, speech-bubble outlines, panel borders, surrounding details, and text outside the target area. Match the original line style and tone. Add no new text or symbols.";

/// `FLUX3_PROMPT` in `deploy/cloud/common/manifest.py`.
pub const FLUX3_PROMPT: &str = "Remove all text in the {location} target area of this manga crop, including small Japanese characters, furigana, hand-drawn sound effects and onomatopoeia. Remove every lettering stroke, outline and text shadow so the target is completely free of writing. Preserve character line art, screentones, panel borders and surrounding background details exactly as they appear. Reconstruct only the background hidden behind the removed lettering, continuing existing lines and textures. Inside speech bubbles, restore the original flat fill. Match the original contrast, shading, grain and line weight. Preserve the crop framing and text outside the target area. Add no lettering, symbols, objects or colours to monochrome artwork.";

/// Every model, in the order Settings shows them.
pub const LOCAL_MODELS: &[LocalModel] = &[
    LocalModel {
        id: "flux2-klein-4b",
        label: "FLUX.2 Klein 4B",
        legacy: false,
        mlx: Some(Checkpoint {
            repo: "mflux-community/flux2-klein-4b-mflux-q4",
            revision: "77090341902cb5f9217f05c664ff604236ca18cc",
            dir: "flux2-klein-4b-mflux-q4",
            bytes: 4_620_000_000,
            expected: "transformer/model.safetensors.index.json",
            adapters: &[],
        }),
        torch: Checkpoint {
            repo: "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic",
            revision: "45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd",
            dir: "flux2-klein-4b-sdnq",
            bytes: 5_480_000_000,
            expected: "model_index.json",
            adapters: &[],
        },
        recipe: KLEIN_RECIPE,
    },
    LocalModel {
        id: "flux3-image-klein",
        label: "FLUX 3 Image Klein",
        legacy: false,
        mlx: None,
        torch: Checkpoint {
            repo: "OzzyGT/Flux_3_Image_Klein_sdnq_dynamic_4bit",
            revision: "bf0af016a936bfe422eec96557aee058c3a6174e",
            dir: "flux3-image-klein-sdnq",
            bytes: 6_650_397_934,
            expected: "modular_model_index.json",
            adapters: &[],
        },
        recipe: Recipe {
            prompt: Prompt::Located(FLUX3_PROMPT),
            steps: 16,
            guidance: 1.0,
            preprocessing: Preprocessing::V3,
            render_timeout: Duration::from_secs(30 * 60),
        },
    },
    LocalModel {
        id: "qwen-image-edit-2511",
        label: "Qwen-Image-Edit-2511",
        legacy: false,
        mlx: None,
        torch: Checkpoint {
            repo: "Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32",
            revision: "a285fceef1439d72d60533e83cb6d8921748a666",
            dir: "qwen-image-edit-2511-sdnq",
            bytes: 17_294_421_877 + 849_608_296 + 18_753_872,
            expected: "model_index.json",
            adapters: &[
                Adapter {
                    repo: "lightx2v/Qwen-Image-Edit-2511-Lightning",
                    revision: "d74eba145674fd7e31b949324e148e21e7118abd",
                    file: "Qwen-Image-Edit-2511-Lightning-8steps-V1.0-bf16.safetensors",
                },
                Adapter {
                    repo: "tori29umai/QwenImageEdit2511_LoRA",
                    revision: "3ca05a1ece5606d4de68ebc564d46713fd77e549",
                    file: "QIE2511_FukidashiErase_V1_dim4_1e-3-000080_dim1.safetensors",
                },
            ],
        },
        recipe: Recipe {
            prompt: Prompt::Located(QWEN_PROMPT),
            steps: 8,
            guidance: 1.0,
            preprocessing: Preprocessing::CURRENT,
            render_timeout: Duration::from_secs(30 * 60),
        },
    },
    LocalModel {
        id: "flux2-klein-9b",
        label: "FLUX.2 Klein 9B",
        legacy: true,
        mlx: None,
        torch: Checkpoint {
            repo: "Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32",
            revision: "94ef5985edc982fa93f87df6e1c0faf94d9e9004",
            dir: "flux2-klein-9b-sdnq",
            bytes: 12_606_105_778,
            expected: "model_index.json",
            adapters: &[],
        },
        recipe: KLEIN_RECIPE,
    },
];

/// The runtime `id` runs on, given the one the setting resolved to. A model
/// outside the table is a folder somebody placed by hand, which only the torch
/// arm can find by name: MLX would render Klein 4B and call it that model.
pub fn backend_for(id: &str, preferred: Backend) -> Backend {
    local_model(id).map_or(Backend::Sdnq, |model| model.backend(preferred))
}

/// The table's row for `id`.
pub fn local_model(id: &str) -> Option<&'static LocalModel> {
    LOCAL_MODELS.iter().find(|model| model.id == id)
}

/// The recipe for `id`. A model outside the table is one somebody placed by
/// hand, and those have only ever been Klein models.
pub fn recipe_of(id: &str) -> Recipe {
    local_model(id).map_or(KLEIN_RECIPE, |model| model.recipe)
}

/// How far the lettering's box reaches past the hint before it is placed, as
/// the cloud worker grows it (`HOLE_GROWTH` in `backend/base.py`).
const HOLE_GROWTH: usize = 8;

/// Where the lettering sits in its crop, in words ("middle center"), or `None`
/// for an empty hint. `deploy/cloud/common/flux.py#hint_location`, on the grown
/// hint as the worker's FLUX 3 prompt reads it.
pub fn hint_location(hint: &[u8], width: u32, height: u32) -> Option<&'static str> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || hint.len() != w * h {
        return None;
    }
    let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
    for (y, row) in hint.chunks_exact(w).enumerate() {
        if let Some(first) = row.iter().position(|v| *v != 0) {
            let last = row.iter().rposition(|v| *v != 0).unwrap_or(first);
            left = left.min(first);
            right = right.max(last);
            top = top.min(y);
            bottom = y;
        }
    }
    if left == usize::MAX {
        return None;
    }
    let (left, top) = (left.saturating_sub(HOLE_GROWTH), top.saturating_sub(HOLE_GROWTH));
    let (right, bottom) = ((right + HOLE_GROWTH).min(w - 1), (bottom + HOLE_GROWTH).min(h - 1));
    let cy = (top + bottom) as f64 / 2.0 / h as f64;
    let cx = (left + right) as f64 / 2.0 / w as f64;
    let row = if cy < 1.0 / 3.0 { 0 } else if cy > 2.0 / 3.0 { 2 } else { 1 };
    let col = if cx < 1.0 / 3.0 { 0 } else if cx > 2.0 / 3.0 { 2 } else { 1 };
    Some([
        ["upper left", "upper center", "upper right"],
        ["middle left", "middle center", "middle right"],
        ["lower left", "lower center", "lower right"],
    ][row][col])
}

impl Recipe {
    /// The words for one crop.
    pub fn prompt_for(&self, hint: &[u8], width: u32, height: u32) -> String {
        match self.prompt {
            Prompt::Fixed(words) => words.to_owned(),
            Prompt::Located(words) => words.replace("{location}", hint_location(hint, width, height).unwrap_or("center")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_klein_4b_can_run_on_mlx() {
        for model in LOCAL_MODELS {
            let expected = if model.id == "flux2-klein-4b" { Backend::Mflux } else { Backend::Sdnq };
            assert_eq!(model.backend(Backend::Mflux), expected, "{}", model.id);
            assert_eq!(model.backend(Backend::Sdnq), Backend::Sdnq, "{}", model.id);
        }
    }

    #[test]
    fn only_the_9b_is_legacy() {
        let legacy: Vec<_> = LOCAL_MODELS.iter().filter(|m| m.legacy).map(|m| m.id).collect();
        assert_eq!(legacy, ["flux2-klein-9b"]);
    }

    #[test]
    fn a_folder_is_a_download_for_the_runtime_that_reads_it() {
        let root = std::env::temp_dir().join(format!("mc-local-models-{}", std::process::id()));
        std::fs::create_dir_all(root.join("flux2-klein-4b-sdnq")).unwrap();
        std::fs::write(root.join("flux2-klein-4b-sdnq/model_index.json"), "{}").unwrap();
        std::fs::create_dir_all(root.join("flux3-image-klein/snapshots/abc")).unwrap();
        std::fs::write(root.join("flux3-image-klein/snapshots/abc/modular_model_index.json"), "{}").unwrap();
        std::fs::create_dir_all(root.join("flux2-klein-9b-mflux-q4")).unwrap();
        let klein = local_model("flux2-klein-4b").unwrap();
        assert!(klein.installed(Backend::Sdnq, &root));
        assert!(!klein.installed(Backend::Mflux, &root), "MLX cannot read the torch weights");
        assert!(local_model("flux3-image-klein").unwrap().installed(Backend::Mflux, &root));
        assert!(!local_model("qwen-image-edit-2511").unwrap().installed(Backend::Sdnq, &root));
        assert!(!local_model("flux2-klein-9b").unwrap().installed(Backend::Sdnq, &root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_model_placed_by_hand_cleans_as_a_klein_model() {
        assert_eq!(recipe_of("flux2-klein-2b"), KLEIN_RECIPE);
        assert_eq!(recipe_of("qwen-image-edit-2511").steps, 8);
        assert_eq!(recipe_of("flux3-image-klein").preprocessing, Preprocessing::V3);
    }

    #[test]
    fn the_prompt_names_the_grown_hint_in_thirds_of_the_crop() {
        let mut hint = vec![0u8; 90 * 90];
        hint[5 * 90 + 45] = 255;
        assert_eq!(hint_location(&hint, 90, 90), Some("upper center"));
        hint = vec![0u8; 90 * 90];
        hint[85 * 90 + 85] = 255;
        assert_eq!(hint_location(&hint, 90, 90), Some("lower right"));
        assert_eq!(hint_location(&[0; 4], 2, 2), None);

        let qwen = recipe_of("qwen-image-edit-2511");
        assert!(qwen.prompt_for(&hint, 90, 90).contains("in the lower right area of this crop"));
        assert_eq!(recipe_of("flux2-klein-4b").prompt_for(&hint, 90, 90), crate::engines::flux::PROMPT);
    }

    /// An older installer downloaded straight into `weights/<dir>` (see
    /// `sidecar/bootstrap.py#fetch_weights`), so a stopped download leaves a folder with a
    /// name and no index.
    #[test]
    fn a_folder_without_the_expected_file_is_not_installed() {
        let root = std::env::temp_dir().join(format!("mc-local-partial-{}", std::process::id()));
        std::fs::create_dir_all(root.join("flux3-image-klein-sdnq/transformer")).unwrap();
        let installed = local_model("flux3-image-klein").unwrap().installed(Backend::Sdnq, &root);
        std::fs::remove_dir_all(root).unwrap();
        assert!(!installed, "no modular_model_index.json: the download did not finish");
    }
}
