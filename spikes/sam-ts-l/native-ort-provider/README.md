# Native Rust/ORT WebGPU SAM-TS-L probe

This standalone Rust crate uses the application's `ort = 2.0.0-rc.13` version and an installed ONNX Runtime 1.28.0 WebGPU provider library. It registers the plugin with `Environment::register_ep_library`, selects the discovered `WebGpuExecutionProvider` device with `SessionBuilder::with_devices`, runs the pinned 1024² encoder and text head on saved page tensors, and counts provider node events from ORT profiles. The mask verifier uses the exact Pillow nearest resize used by the saved reference.

From the repository root on the M5 host:

```sh
cargo build --release --manifest-path spikes/sam-ts-l/native-ort-provider/Cargo.toml
for page in 07 08 09; do
  spikes/sam-ts-l/native-ort-provider/target/release/sam-ts-native-ort-provider \
    spikes/sam-ts-l/artifacts/ort-webgpu-venv/lib/python3.12/site-packages/onnxruntime/capi/libonnxruntime.1.28.0.dylib \
    spikes/sam-ts-l/artifacts/ort-webgpu-venv/lib/python3.12/site-packages/onnxruntime_ep_webgpu/libonnxruntime_providers_webgpu.dylib \
    spikes/sam-ts-l/artifacts/proof \
    spikes/sam-ts-l/artifacts/examples/apocalypse-109/page-$page \
    spikes/sam-ts-l/results/native-rust-webgpu-page$page.json
  spikes/sam-ts-l/artifacts/ort-webgpu-venv/bin/python \
    spikes/sam-ts-l/native-ort-provider/verify_native_mask.py \
    spikes/sam-ts-l/results/native-rust-webgpu-page$page.json
done
```

Set `SAMTS_NO_PROFILE=1` for unprofiled latency measurements. The probe records process RSS high-water and, on macOS, polls `MTLDevice.currentAllocatedSize` every 20 ms. Metal values are sampled and can miss a transient peak; unified memory can overlap RSS. Profile JSON is counted then deleted, and temporary raw logits are deleted by the Pillow verifier. Session load is cold within a fresh process but the OS file cache is uncontrolled.

The EP library must be explicitly unregistered **after** both sessions are dropped. With that order the plugin probe exits status 0 without the earlier `ReleaseEpFactory failed ... Unknown exception` log. Leaving the registered factory to ONNX Runtime environment teardown produces that log despite successful inference.

The actual app-downloaded macOS runtime has a built-in WebGPU EP and does not need the plugin. Pass `-` as the provider-library argument to use it:

```sh
spikes/sam-ts-l/native-ort-provider/target/release/sam-ts-native-ort-provider \
  /Users/caved/dev/manga-cleaner/runtimes/onnxruntime-osx-arm64-1.28.0/lib/libonnxruntime.dylib \
  - spikes/sam-ts-l/artifacts/proof \
  spikes/sam-ts-l/artifacts/examples/apocalypse-109/page-07 \
  spikes/sam-ts-l/results/native-rust-app-webgpu-page07.json
spikes/sam-ts-l/artifacts/ort-webgpu-venv/bin/python \
  spikes/sam-ts-l/native-ort-provider/verify_native_mask.py \
  spikes/sam-ts-l/results/native-rust-app-webgpu-page07.json
```

The same installed runtime completed a 45-page, one-session corpus with 45 exact masks against saved PyTorch MPS; see `spikes/sam-ts-l/results/native-rust-app-webgpu-45-pillow.json` and `docs/provider-qualification.md`. This corpus uses saved Pillow-prepared original JPEG inputs, separate from the desktop original-JPEG decode path. The ignored desktop `infer_webgpu` test covers app PNG decoding and full WebGPU node assignment on page 07.
