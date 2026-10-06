# Hayai OCR v2.5 Nova to ONNX

The multi-script reader behind `crates/cleaner-core/src/gate/hayai.rs`.
Upstream ships PyTorch only, so this folder exports it.

- Model: [`JustANormalTinkerer/hayai-ocr-v2.5-nova`](https://huggingface.co/JustANormalTinkerer/hayai-ocr-v2.5-nova),
  revision `e34d7755ed11e626c5ba39544af5d66f20ee57cc`, Apache-2.0. Reads Japanese,
  Chinese (simplified and traditional), Korean and English, horizontal and vertical.
  Its training data includes Manga109-s (non-commercial terms).
- Exported 2026-10-02 with torch 2.14.1, transformers 5.18.0, onnx 1.23.1,
  onnxruntime 1.30.0, opset 18, FP32.

## Files the app expects

Put these in the app's `models` folder under these names:

| File | Size | SHA-256 |
| --- | --- | --- |
| `hayai-ocr-vision.onnx` (`onnx/vision.onnx`) | 343 538 300 | `379ec20e7d5b134e6bd0e7c5cf0a4e705318129bfb710e25e69c6ca027b84021` |
| `hayai-ocr-decoder.onnx` (`onnx/decoder.onnx`) | 255 717 567 | `23342ad16efee65486347b7ac15c98d5f78412eb4c4bee3236d03d8478cb0e20` |
| `hayai-ocr-tokenizer.json` (upstream `tokenizer.json`, unchanged) | 1 247 253 | `f8a0a909c628a684fe463094614e236a8b1d3609e7770f77e7beafaf1056bf13` |

## Re-run

```bash
uv venv -p 3.12 .venv && source .venv/bin/activate
uv pip install torch "transformers>=5" onnx onnxruntime onnxscript pillow huggingface_hub
python export.py onnx
```

`read_torch.py` reads a folder of crops (`crops/index.json`) with the PyTorch
model and writes `torch_results.json`. `check_onnx.py` reads the same crops
through the exported graphs, doing in numpy what the Rust side does (NaFlex
resize, unshuffle, rotary tables, cache), and reports how many readings match.

## Graph split

The vision graph takes the patch grid as an input and resizes the position
embeddings inside the graph (ONNX `Resize`, antialiased). The caller does the
2x2 unshuffle and the 2D rotary tables, so no graph has a shape computed from
tensor values. One decoder graph serves prefill and every step: prefill passes
the vision tokens, `[BOS]` and an empty cache; a step passes no vision tokens,
the last token and the running cache. `hayai.rs` has the tensor contract.

## Results (Black Jack chapter 1, CPU, M-series Mac)

- 64 detected regions: ONNX readings identical to PyTorch on 64 of 64.
  0.23 s per crop in Python ORT, 0.43 s in PyTorch without a cache.
- Rendered Korean, simplified and traditional Chinese, vertical Chinese and
  English lines all read exactly.
- Text check: 30 random crops of bare art all read one or two characters at a
  mean confidence of 0.66 or less; lettering read at 0.8 or more except short
  sound effects. `Reading::is_text` encodes the split.
- Find-all run (SAM-TS-L + CTD + Ogkalu Full, All text policy): 84 regions
  cleaned without the reader, 55 with it. 29 held as not text, nearly all
  art (page 5's 15 regions on a crowd scene, page 9's surgical tools). Two
  real lettering regions were held too (a 含・実・ row with emphasis dots, and a
  シャコーン that one region shared with a shop sign).
- Default run (CTD + Ogkalu Full): 37 regions cleaned before, 40 after.
  The reader rescued オレもさ, はあ…… and で, which the script identifier held
  as low confidence.

## Smaller graphs and other providers (2026-10-02)

`make_variants.py` writes FP16 (inputs and outputs kept FP32) and dynamic int8
(MatMul weights only) copies of both graphs. Readings identical to PyTorch on
the 64 crops, through `check_onnx.py` on the CPU:

| variant | vision + decoder size | identical |
| --- | --- | --- |
| FP32 (shipped) | 599 MB | 64/64 |
| FP16 | 300 MB | 64/64 |
| FP32 vision, int8 decoder | 432 MB | 63/64 |
| int8 | 176 MB | 59/64 |

Per region on the Apple M5, through the app's own session builder (a scratch
bench test; first read excluded):

| vision / decoder | FP32 | FP16 |
| --- | --- | --- |
| CPU / CPU | 190 to 230 ms | 335 ms |
| CPU / WebGPU | 234 ms | 217 ms |
| CoreML / CPU | 618 ms | - |
| CoreML / CoreML | fails on the first decoder step | 342 ms |
| WebGPU / any | every read fails | every read fails |

WebGPU builds the vision session and then refuses its antialiased `Resize`
("The antialias attribute of Resize operator is NOT implemented"). With the
position resize split out to the CPU (`onnx.utils.extract_model`), the rest
of the vision graph ran on WebGPU at 256 ms against 288 ms on the CPU in the
same sitting, inside the spread. CoreML cannot take the decoder's empty vision
input on a step ("has a dynamic shape ... but the runtime shape ({1,0,512})
has zero elements").

`bench_cuda.py` ran the same readings on a Modal NVIDIA L4 (onnxruntime-gpu
1.23.2, `run_core.py` is `check_onnx.py` as a class):

| variant, provider | per region | identical |
| --- | --- | --- |
| FP32, CUDA | 303 ms | 59/64 |
| FP16, CUDA | 167 ms | 58/64 |
| int8, CUDA | 327 ms | 39/64 |
| FP32, CPU (4 cores) | 497 ms | 64/64 |

`read_job.py` prints the reader's text and confidence for the held or
cleaned regions of a run's job file; the thresholds in `Reading::is_lettering`
came from it on four sets of pages (see docs/findings.md).
