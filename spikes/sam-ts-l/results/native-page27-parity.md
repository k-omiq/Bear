# Native SAM-TS-L page 27 parity decision

25 September 2026, Apple M5/macOS, local CPU only. This is a diagnostic for the pinned Apocalypse 109 page 27 and graphs; it is not a human-label accuracy result. No source image or model weights are checked in.

## Input identity and result

| Comparison | Result |
| --- | ---: |
| Original `27.jpg` SHA-256 | `e1dbaff715b608354af71aa96c369b124ed4ab0d393b3542efe62cd8a49d2400` |
| Pillow-decoded RGB bytes SHA-256 | `102cc0168f2dcd457d81885554c0f0cf8fe2f167044d833f1c5486ae9a1511a3` |
| Saved Pillow RGB PNG SHA-256 | `47648cf9021b030e6ece7b918c8ca00c11dadce9646ad4b393c9f3d7d71dd07a` |
| App Rust JPEG decoder then PNG SHA-256 | `e7b78b013db8775e1788755e23926268109631e17dbf749c12fdb6e1a937a4d5` |
| Rust prepared CHW versus Python/Pillow CHW, using the *same Pillow PNG* | **0 / 3,145,728 differing f32 samples** |
| Rust native ORT 1.28 versus Python ORT 1.23, same Pillow PNG | **0 canvas-logit sign changes; 0 restored-mask pixels** |
| Python ORT CPU versus pinned PyTorch CPU or MPS, same Pillow input | **2 restored pixels**, `(x=253, y=723)` and `(x=253, y=724)` |
| Rust JPEG decoder versus Pillow JPEG decoder | **104,342 / 3,312,000 decoded RGB channel samples differ** |
| Rust JPEG decoder versus Pillow JPEG decoder after SAM preparation | **52,677 / 3,145,728 CHW samples differ** |
| Python ORT CPU mask, Rust-decoded input versus Pillow-decoded input | **666 pixels differ** |
| Rust native ORT 1.28 versus Python ORT 1.23, same Rust-decoded PNG | **0 canvas-logit sign changes; 0 restored-mask pixels** |

The Pillow-input CPU ORT mask has 30,969 positive pixels; the saved PyTorch MPS mask has 30,971. The Rust-decoded-input Python ORT mask has 30,315 positive pixels. The 668-pixel difference between that mask and saved MPS combines JPEG-decoder and model-runtime effects; it must not be described as an ORT parity failure because the inputs differ. The two ORT masks from different JPEG decoders differ by 666 pixels. A newly saved Pillow PNG may have a different *file* hash because PNG encoding options/metadata differ; the decoded RGB hash and prepared tensor are the input identity checks.

## Stage diagnosis with identical Pillow input

Both differing restored pixels read one canvas logit at `(x=162, y=463)` after nearest resize. The zero threshold is unchanged.

| Executor | Logit at canvas `(162,463)` | Sign |
| --- | ---: | --- |
| Rust native ORT 1.28.0 CPU | `-0.0000013949705` | background |
| Python ORT 1.23.2 CPU | `-0.000012132720257795881` | background |
| PyTorch 2.13.0 CPU | `+0.0000025620940959925065` | foreground |
| PyTorch 2.13.0 MPS | `+0.000010013580322265625` | foreground |

Rust ORT 1.28.0 and Python ORT 1.23.2 produced the same mask on identical Pillow input. Their encoder embeddings differ numerically in 1,026,981 float elements (maximum absolute difference `1.5974045e-5`, mean `9.5582914e-7`). Their head logits differ in 1,043,357 elements (maximum `1.7005205e-4`, mean `2.5584743e-5`), but have **zero sign differences** across the 1024-square canvas. Thus Rust preparation, sign threshold, crop, and nearest restoration do not explain the two pixels. PyTorch CPU and MPS agree on their foreground decision; ONNX Runtime CPU makes the opposite decision for one near-zero logit. An epsilon adjustment to force this pixel would change the model's pinned zero-threshold rule and has not been adopted.

As a targeted operation-order check, Python ORT CPU 1.23.2 was run again with `SessionOptions.graph_optimization_level` set to `ORT_DISABLE_ALL` and `ORT_ENABLE_BASIC` instead of the default. The affected logit stayed negative (`-8.0302916e-6` and `-2.5677602e-5`, respectively), with zero canvas-sign changes relative to the default run. Graph optimization changes the floating-point value but does not restore exact PyTorch mask parity on this input.

With the Rust-decoded PNG as input, Rust and Python ORT again had exact prepared tensors and exact masks, with zero canvas-logit sign differences. Their embedding maximum absolute difference was `3.5390258e-5`, logit maximum `1.9550323e-4`. The native logit at `(162,463)` was `-0.03598851`, far from the Pillow-input near-zero value: the JPEG decoder is a material input change.

## Desktop decision and qualification limit

The desktop's **canonical SAM input is the app-decoded `Raster`**, including its existing pure Rust JPEG decode at ingest. SAM accepts this raster and never decodes the original JPEG again. A Python parity run must consume a lossless PNG exported from that exact raster, or its prepared float tensor, before outputs are compared. Pillow-decoded JPEG results remain useful spike evidence but cannot be a byte-exact desktop baseline for JPEG source files. Changing the application JPEG codec or shipping Pillow just to recover the exploratory masks is not justified by these data.

For the pinned page27 input, native CPU ORT is mask-exact against Python CPU ORT under either shared decoded raster. The prior 45-page comparison using Pillow-decoded PNGs remains **44/45 exact against PyTorch MPS**, with two differing pixels on page27. Exact PyTorch mask parity across framework runtimes is not established, even though the native implementation follows the reference threshold and image operations. Keep any strict PyTorch exact-mask gate visible; if a release criterion allows numerical parity, it needs an explicit policy for near-zero logits and verification across the whole chapter on identical canonical inputs. The separate GPU/provider, platform, ground-truth, write-support, and licensing gates remain independent.

## Reproduction

Versions: Pillow 11.3.0, NumPy 2.3.3, Python ONNX Runtime 1.23.2 CPU, PyTorch 2.13.0 CPU/MPS; native ONNX Runtime 1.28.0 CPU, Rust `image` 0.25.10. Use the pinned graphs under the ignored `spikes/sam-ts-l/artifacts/proof/` and source `/Users/caved/Downloads/apocalypse/109/27.jpg`. The encoder SHA-256 is `9b3a32f9018008cfd2c7a5b1a7eb6e20822ba43eab58918863f74ac62ecbafbe`; the head SHA-256 is `a2c63ccf54e2e692a281cffd4dcda648f252ae6649dc7d23d0203e5868685281`.

Create the Pillow input, then use the ignored Rust JPEG comparison test to create the native-decoded PNG:

```sh
spikes/sam-ts-l/artifacts/venv/bin/python - <<'PY'
from PIL import Image
Image.open('/Users/caved/Downloads/apocalypse/109/27.jpg').convert('RGB').save('/tmp/sam-page27-pillow.png')
PY
SAM_TS_CHAPTER_DIR=/Users/caved/Downloads/apocalypse/109 \
SAM_TS_DECODED_PNG=/tmp/sam-page27-pillow.png \
SAM_TS_NATIVE_PNG=/tmp/sam-page27-native.png \
cargo test -p cleaner-core diagnose_page27_jpeg_decode -- --ignored --nocapture
```

For either PNG, save Python ORT stage tensors under `/tmp/sam-page27-stage` (change `page` and `stage` for the other PNG):

```sh
spikes/sam-ts-l/artifacts/venv/bin/python - <<'PY'
import sys
from pathlib import Path
import numpy as np
import onnxruntime as ort
from PIL import Image
root = Path('spikes/sam-ts-l').resolve()
sys.path.insert(0, str(root))
from run_real_pages import prepare, restore
page = Path('/tmp/sam-page27-pillow.png')
stage = Path('/tmp/sam-page27-stage')
stage.mkdir(exist_ok=True)
chw, resized, original = prepare(Image.open(page))
batch = chw[None]
options = ort.SessionOptions()
options.intra_op_num_threads = 4
options.inter_op_num_threads = 1
encoder = ort.InferenceSession(str(root/'artifacts/proof/koharu_samts_encoder.onnx'), sess_options=options, providers=['CPUExecutionProvider'])
embedding = encoder.run(None, {'prepared_rgb': batch})[0]
del encoder
head = ort.InferenceSession(str(root/'artifacts/proof/koharu_samts_text_head.onnx'), sess_options=options, providers=['CPUExecutionProvider'])
logits = head.run(None, {'embedding': embedding})[0]
for name, value in [('prepared', batch), ('embedding', embedding), ('logits', logits), ('mask', restore(logits, resized, original))]:
    np.save(stage/f'{name}.npy', value)
print('page27 logit', logits[0, 0, 463, 162])
PY
SAM_TS_RUNTIME=/Users/caved/dev/manga-cleaner/runtimes/onnxruntime-osx-arm64-1.28.0/lib/libonnxruntime.dylib \
SAM_TS_DECODED_PNG=/tmp/sam-page27-pillow.png \
SAM_TS_STAGE_DIR=/tmp/sam-page27-stage \
cargo test -p cleaner-core diagnose_page27_cpu_stages -- --ignored --nocapture
```

For the PyTorch logits, `export_mask.load_model`, `export_mask.prepare`, and `export_mask.restore` are used exactly as in `run_chapter_torch.py`; run once on `cpu` and once on `mps` with `PYTORCH_ENABLE_MPS_FALLBACK=0`, then inspect `result[3][0,0,463,162]`. The saved chapter MPS mask is `spikes/sam-ts-l/artifacts/examples/chapter-109-torch-mps/masks/27.png`.

To repeat the graph-optimization check, set `options.graph_optimization_level` in the Python block above to `ort.GraphOptimizationLevel.ORT_DISABLE_ALL` and then `ort.GraphOptimizationLevel.ORT_ENABLE_BASIC`; use the same prepared input and compare `(logits > 0)` against the default output.
