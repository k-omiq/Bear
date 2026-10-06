# COO MTSv3 local detection probe

`probe.py` reconstructs only the inference path in the pinned COO MTSv3
R-50-FPN detector: backbone, FPN, segmentation proposal head, and the
published contour/polygon scoring rules. It verifies the full checkpoint
SHA-256 and strictly loads all 300 tensors. The checkpoint contains 281
backbone and 19 proposal-head tensors; there are no recognition tensors to
discard. It does not run TRBA OCR, SAM, or inpainting.

Upstream source: [`ku21fan/COO-Comic-Onomatopoeia` at
`d8028f015b8ce99a4dd798427342f97087529357`](https://github.com/ku21fan/COO-Comic-Onomatopoeia/tree/d8028f015b8ce99a4dd798427342f97087529357/MTSv3).
Checkpoint: [`mayocream/coo-comic-onomatopoeia-safetensors` at
`b5d31460573b6f61c1d4bdaea5fe4e18425e6a61`](https://huggingface.co/mayocream/coo-comic-onomatopoeia-safetensors/tree/b5d31460573b6f61c1d4bdaea5fe4e18425e6a61/mtsv3),
`mtsv3/model.safetensors`, SHA-256
`49a448ec19e4e3894b726553fe6f47916827b10d642109aa5f41b96aeeb88e30`.
This safetensors copy is an unofficial tensor-preserving conversion of the
authors' published `MTSv3.pth`. The model card documents its source and
conversion checks. The upstream MTSv3 implementation is CC BY-NC 4.0, so
distribution or service use needs a separate rights decision.

The probe used Python 3.12.13, PyTorch 2.13.0, NumPy 2.2.6, Pillow 11.3.0,
OpenCV headless 4.12.0.88, pyclipper 1.3.0.post6, and safetensors 0.6.2.
It used Apple MPS with CPU fallback disabled. To reproduce with a compatible
local PyTorch/MPS environment:

```sh
mkdir -p spikes/coo-mtsv3/artifacts
curl -L --fail --retry 3 -o spikes/coo-mtsv3/artifacts/model.safetensors \
  'https://huggingface.co/mayocream/coo-comic-onomatopoeia-safetensors/resolve/b5d31460573b6f61c1d4bdaea5fe4e18425e6a61/mtsv3/model.safetensors'
uv venv spikes/coo-mtsv3/artifacts/venv --python 3.12
uv pip install --python spikes/coo-mtsv3/artifacts/venv/bin/python \
  'torch==2.13.0' 'numpy==2.2.6' 'Pillow==11.3.0' \
  'opencv-python-headless==4.12.0.88' 'pyclipper==1.3.0.post6' \
  'safetensors==0.6.2' 'packaging==25.0'
spikes/coo-mtsv3/artifacts/venv/bin/python spikes/coo-mtsv3/probe.py \
  --source-dir /absolute/path/to/45/pages \
  --output-dir spikes/coo-mtsv3/artifacts/chapter-109 --device mps
spikes/coo-mtsv3/artifacts/venv/bin/python spikes/coo-mtsv3/render_overlays.py \
  --source-dir /absolute/path/to/45/pages \
  --results-dir spikes/coo-mtsv3/artifacts/chapter-109
```

The default `eval_pil` mode follows the published `best_test.yaml` detection
evaluation path. [The evaluation transform](https://github.com/ku21fan/COO-Comic-Onomatopoeia/blob/d8028f015b8ce99a4dd798427342f97087529357/MTSv3/maskrcnn_benchmark/data/transforms/build.py#L62-L70)
resizes the PIL image before conversion and BGR255 normalization; the config
sets minimum side 1440 and maximum side 4000. [The standalone demo](https://github.com/ku21fan/COO-Comic-Onomatopoeia/blob/d8028f015b8ce99a4dd798427342f97087529357/MTSv3/demo.py#L53-L80)
uses the same resize-before-normalize ordering but its main function
[overrides the minimum side to 800](https://github.com/ku21fan/COO-Comic-Onomatopoeia/blob/d8028f015b8ce99a4dd798427342f97087529357/MTSv3/demo.py#L296-L298).
[The separate M4C feature extraction script](https://github.com/ku21fan/COO-Comic-Onomatopoeia/blob/d8028f015b8ce99a4dd798427342f97087529357/MTSv3/extract_features_vmb.py#L195-L225)
converts to float32 BGR and subtracts means before `cv2.INTER_LINEAR`
resizing. This is a different, named preprocessing path, supported with
`--preprocess feature_cv2`. `--preprocess demo_pil_800` makes the demo scale
explicit; it was prepared for the two-page input comparison but not used for
the 45-page quality comparison.

`artifacts/chapter-109-eval-pil/summary.json` contains all 45
source-coordinate detections and timings. `artifacts/chapter-109` is a
compatibility symlink to that preserved result. A two-page sensitivity run
for the feature extractor is in `artifacts/chapter-109-feature-cv2-smoke/`.
`compare_preprocessing.py` saves exact prepared tensors and an input
difference report for pages 02 and 07 in `artifacts/preprocessing/`.
Each page directory contains `detections.json`, a float32 sigmoid
`proposal-map.npz`, and a `proposal-bitmap.png` at the model's padded
resolution. Pages 02 and 07 additionally save the exact prepared CHW input
tensor. `manifest.json` hashes the output artifacts, scripts, source revision,
checkpoint, and package versions. Overlays and the contact sheet are visual
review aids. All comic-derived artifacts remain ignored by Git.

The raw map uses a 0.1 pixel threshold for contours and a 0.1 box score
threshold from `best_test.yaml`. The upstream demo additionally filters
scores below 0.4; both counts are reported. A polygon is a **candidate
onomatopoeia region**, not a text-pixel erase mask. SAM-TS supplies the
lettering mask in the proposed combined workflow. The COO code has no
independent upstream CUDA output on these pages for numerical parity; do not
promote this port as release-qualified based solely on this probe.
