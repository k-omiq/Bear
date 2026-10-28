# SAM-TS-L memory probe

Tools behind `docs/ram-analysis-2026-10-28.md`. Not shipped code.

- `chunk_attn.py <encoder.onnx> <out.onnx> 16` rewrites the four global-attention blocks so each
  head runs alone (one `Slice` per head; WebGPU refuses a 16-way `Split`). The shipped version is
  `split_global_attention` in `sam-bootstrap/bootstrap.py`.
- `sam_mem.py <variant> <encoder> <head> <out.npz> page...` runs the CPU EP from Python
  (onnxruntime 1.28.0) and prints the lifetime peak footprint and mask hashes.
- `samprobe/` runs either EP through the app's own runtime dylib:
  `samprobe <dylib> <cpu|webgpu> <encoder> <head> <-|off> <out_prefix> <input.f32>...`.
  `-` keeps today's options; `off` turns the CPU arena and memory pattern off. Build it with its
  own `CARGO_TARGET_DIR`. Inputs are prepared 1x3x1024x1024 little-endian f32 files.
