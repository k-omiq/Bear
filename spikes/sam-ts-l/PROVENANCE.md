# SAM-TS-L M2 provenance and rights record

Checked 2026-09-25. This records published claims and local integrity evidence for the mask-only export experiment. It is not a distribution clearance decision.

## Exact inputs

| Item | Identity and evidence |
| --- | --- |
| Full checkpoint | [`mayocream/koharu-text-sam-ts-l`](https://huggingface.co/mayocream/koharu-text-sam-ts-l) at immutable revision `5dd97423e0fbf2404264979136d47e8101144046`, file `model.safetensors`. Local `artifacts/model.safetensors` is 1,355,824,988 bytes and its locally computed SHA-256 is `bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45`. This equals the pinned [`SHA256SUMS`](https://huggingface.co/mayocream/koharu-text-sam-ts-l/blob/5dd97423e0fbf2404264979136d47e8101144046/SHA256SUMS) and [`config.json`](https://huggingface.co/mayocream/koharu-text-sam-ts-l/blob/5dd97423e0fbf2404264979136d47e8101144046/config.json) values. |
| Architecture source | [`ymy-k/Hi-SAM`](https://github.com/ymy-k/Hi-SAM/tree/69009434d4dba5541f228d8f5acb0754c333d417) at local Git commit `69009434d4dba5541f228d8f5acb0754c333d417` (2025-05-30). Source is held under `artifacts/Hi-SAM/` for this experiment. The model card describes the checkpoint as fine-tuned from Hi-SAM's SAM-TS-L TextSeg checkpoint; this source commit is the pinned implementation used for the export experiment, not evidence of the original training commit. |
| Published state | The pinned [`README.md`](https://huggingface.co/mayocream/koharu-text-sam-ts-l/blob/5dd97423e0fbf2404264979136d47e8101144046/README.md) calls `model.safetensors` a full merged state (728 tensors, 338,934,897 elements) and `adapter_model.safetensors` a trainable-only state (366 tensors). The latter is not a substitute for the full checkpoint. The card claims strict loading and reproduction of its evaluation metrics; the M2 parity experiment must independently verify its own PyTorch and ONNX path. |

## Published rights and lineage

| Material | Published statement or source | Remaining review |
| --- | --- | --- |
| Koharu checkpoint and reference files | The pinned model card declares `apache-2.0`, includes an [Apache 2.0 LICENSE](https://huggingface.co/mayocream/koharu-text-sam-ts-l/blob/5dd97423e0fbf2404264979136d47e8101144046/LICENSE), and says its model release matches Hi-SAM and Segment Anything. | Verify the publisher's authority over every merged weight component, including the original SAM encoder and Hi-SAM TextSeg initialization, before distributing the checkpoint or derived ONNX weights. A public download and license label alone do not settle that chain. |
| Hi-SAM implementation | The pinned [Hi-SAM LICENSE](https://github.com/ymy-k/Hi-SAM/blob/69009434d4dba5541f228d8f5acb0754c333d417/LICENSE) is Apache 2.0. Several model files retain Meta copyright notices. The [Hi-SAM README](https://github.com/ymy-k/Hi-SAM/blob/69009434d4dba5541f228d8f5acb0754c333d417/README.md) says its released SAM-TS checkpoints omit the original SAM ViT parameters and direct users to obtain those separately. | If implementation code is copied into shipped software, preserve applicable license, copyright and modification notices. The export experiment alone does not establish which code, if any, must ship. |
| Segment Anything origin | The official [Segment Anything repository](https://github.com/facebookresearch/segment-anything) identifies its model license as Apache 2.0. | Confirm the exact original SAM checkpoint and its terms used in the merged Koharu state. Do not infer that the separate SA-1B dataset's research license governs the model or that the model license governs SA-1B data. |
| Fine-tuning masks | The Koharu card attributes the training masks to [del Gobbo and Herrera's Zenodo dataset](https://doi.org/10.5281/zenodo.4511796); the Zenodo record identifies the dataset license as CC BY 4.0. | Retain attribution where relevant and confirm whether mask data conditions impose any obligations for model or documentation distribution. |
| Fine-tuning images | The Koharu card says authorized Manga109 images were paired locally with those masks, and that no Manga109 images are in its model repository. It reports 36 training books, 5 validation books and 4 test books. | Review the exact Manga109 image access/training terms and whether they permit downstream checkpoint redistribution and commercial or hosted inference. The card's word “authorized” is a publisher statement, not independent clearance. |

## Release questions that remain open

1. Can the full merged checkpoint, or a functionally equivalent mask-only ONNX transformation of its weights, be redistributed with the desktop app or through an optional model download? Confirm the rights chain for SAM base weights, Hi-SAM TextSeg weights and Koharu fine-tuning.
2. What license files, attribution, notices and modification statements must accompany the converted graphs and any Hi-SAM code included in a release? An ONNX conversion grants no new rights.
3. Do the Manga109 image terms and training-mask terms permit this specific checkpoint's downstream commercial distribution and remote inference service use? Assess local downloads, bundling and cloud hosting separately.
4. Is there any third-party material embedded in the model repository's reference code or fixtures beyond the listed components? Review the exact exported artifact set before publication.

Until those questions are resolved, keep the checkpoint and generated graphs as research artifacts in the isolated M2 worktree; do not treat their availability or an Apache label as permission to publish them.

### Commands used for this record

```text
git -C spikes/sam-ts-l/artifacts/Hi-SAM rev-parse HEAD
shasum -a 256 spikes/sam-ts-l/artifacts/model.safetensors
stat -f '%z bytes' spikes/sam-ts-l/artifacts/model.safetensors
curl -fsSL https://huggingface.co/api/models/mayocream/koharu-text-sam-ts-l/revision/5dd97423e0fbf2404264979136d47e8101144046
curl -fsSL https://huggingface.co/mayocream/koharu-text-sam-ts-l/raw/5dd97423e0fbf2404264979136d47e8101144046/README.md
curl -fsSL https://huggingface.co/mayocream/koharu-text-sam-ts-l/raw/5dd97423e0fbf2404264979136d47e8101144046/SHA256SUMS
curl -fsSL https://zenodo.org/api/records/4511796
```
