#!/usr/bin/env python3
"""Reproduce and export Koharu SAM-TS-L's FP32 lettering-mask path.

This is a conversion/proof tool, not a desktop inference dependency. The two
static-batch ONNX graphs consume RGB 0..255 pixels and encoder embeddings.
The image preparation and binary restoration here follow the checkpoint's
pinned inference.py, including PIL's resize and rounding behavior.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import subprocess
import sys
import zipfile
from pathlib import Path
from types import SimpleNamespace

import numpy as np
import torch
import torch.nn.functional as F
import torch.utils.checkpoint
from PIL import Image
from safetensors.torch import load_file

IMAGE_SIZE = 1024
CHECKPOINT_SHA256 = "bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45"
HF_REVISION = "5dd97423e0fbf2404264979136d47e8101144046"
HI_SAM_REVISION = "69009434d4dba5541f228d8f5acb0754c333d417"
HI_SAM_ARCHIVE_SHA256 = "f18fb049813f9b9319ac074449f4539fbe5b58f4e3f450ce331578d74bc3e327"
OPSET = 17


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_initializer_keys(weights: Path, output: Path, result: Path) -> None:
    """Map exported weights by exact value, including Linear transposes."""
    import onnx
    from onnx import numpy_helper
    from safetensors import safe_open

    manifest = json.loads((output / "manifest.json").read_text())
    if manifest["checkpoint"]["sha256"] != CHECKPOINT_SHA256 or sha256(weights) != CHECKPOINT_SHA256:
        raise ValueError("checkpoint SHA-256 differs from export manifest")
    for graph_name in ("encoder", "text_head"):
        for package in manifest["graphs"][graph_name]["package"]:
            graph_path = output / package["file"]
            if graph_path.stat().st_size != package["bytes"] or sha256(graph_path) != package["sha256"]:
                raise ValueError(f"{graph_name} package differs from export manifest: {graph_path}")
    expected = set(manifest["mask_path_checkpoint_keys"])

    def fingerprint(array: np.ndarray) -> tuple:
        contiguous = np.ascontiguousarray(array)
        return (tuple(contiguous.shape), contiguous.dtype.str,
                hashlib.sha256(contiguous.tobytes()).hexdigest())

    checkpoint_index = {}
    with safe_open(str(weights), framework="np", device="cpu") as checkpoint:
        for key in checkpoint.keys():
            array = checkpoint.get_tensor(key)
            checkpoint_index.setdefault(fingerprint(array), []).append((key, "direct"))
            if array.ndim == 2:
                checkpoint_index.setdefault(fingerprint(array.T), []).append((key, "transpose"))
            if array.ndim == 1:
                checkpoint_index.setdefault(fingerprint(array[:, None, None]), []).append((key, "broadcast_reshape"))
            del array

    special = {}
    with safe_open(str(weights), framework="np", device="cpu") as checkpoint:
        prefix = "modal_aligner.transformer_layers.0.cross_attn."
        for suffix in ("in_proj_weight", "in_proj_bias"):
            key = prefix + suffix
            chunks = np.split(checkpoint.get_tensor(key), 3, axis=0)
            for chunk in chunks:
                if suffix == "in_proj_weight":
                    chunk = chunk.T
                special[fingerprint(chunk)] = ([key], "qkv_split_transpose" if suffix == "in_proj_weight" else "qkv_split")
        token_keys = ["mask_decoder.iou_token.weight", "mask_decoder.mask_tokens.weight"]
        combined = np.concatenate([checkpoint.get_tensor(key) for key in token_keys], axis=0)[None]
        special[fingerprint(combined)] = (token_keys, "concatenate_and_expand")

    mapping = {}
    unmatched_initializers = []
    ambiguous_initializers = {}
    for graph_name, filename in (("encoder", "koharu_samts_encoder.onnx"),
                                 ("text_head", "koharu_samts_text_head.onnx")):
        graph_path = output / filename
        graph = onnx.load(str(graph_path), load_external_data=True)
        for initializer in graph.graph.initializer:
            name = f"{graph_name}:{initializer.name}"
            matches = checkpoint_index.get(fingerprint(numpy_helper.to_array(initializer)), [])
            keys = sorted(set(key for key, _ in matches))
            if len(keys) == 1:
                mapping[name] = {"checkpoint_key": keys[0],
                                 "transform": next(transform for key, transform in matches
                                                   if key == keys[0])}
            elif fingerprint(numpy_helper.to_array(initializer)) in special:
                source_keys, transform = special[fingerprint(numpy_helper.to_array(initializer))]
                mapping[name] = {"checkpoint_keys": source_keys, "transform": transform}
            elif keys:
                ambiguous_initializers[name] = keys
            else:
                unmatched_initializers.append(name)
        del graph

    consumed = {key for entry in mapping.values()
                for key in entry.get("checkpoint_keys", [entry["checkpoint_key"]] if "checkpoint_key" in entry else [])}
    # This checkpoint buffer is converted into a fixed spatial encoding by
    # TextHead.__init__. Verify the derivation against the graph value.
    derived_key = "prompt_encoder.pe_layer.positional_encoding_gaussian_matrix"
    if derived_key in expected:
        sys.path.insert(0, str((Path(__file__).resolve().parent / "artifacts/Hi-SAM").resolve()))
        with safe_open(str(weights), framework="pt", device="cpu") as checkpoint:
            gaussian = checkpoint.get_tensor(derived_key)
        from hi_sam.modeling.prompt_encoder import PositionEmbeddingRandom
        positional = PositionEmbeddingRandom(gaussian.shape[1])
        with torch.no_grad():
            positional.positional_encoding_gaussian_matrix.copy_(gaussian)
            derived = positional((64, 64)).unsqueeze(0).numpy()
        derived_fingerprint = fingerprint(derived.transpose(0, 2, 3, 1).reshape(1, 4096, 256))
        for graph_name, filename in (("text_head", "koharu_samts_text_head.onnx"),):
            graph = onnx.load(str(output / filename), load_external_data=True)
            for initializer in graph.graph.initializer:
                name = f"{graph_name}:{initializer.name}"
                if name in unmatched_initializers and fingerprint(numpy_helper.to_array(initializer)) == derived_fingerprint:
                    mapping[name] = {"checkpoint_key": derived_key, "transform": "derived_dense_pe"}
                    unmatched_initializers.remove(name)
                    consumed.add(derived_key)
            del graph

    for name in ("encoder:pixel_mean", "encoder:pixel_std"):
        if name in unmatched_initializers:
            unmatched_initializers.remove(name)
            mapping[name] = {"checkpoint_keys": [], "transform": "fixed_normalization_constant"}

    report = {
        "checkpoint_sha256": manifest["checkpoint"]["sha256"],
        "graph_sha256": {name: sha256(output / filename) for name, filename in
                         (("encoder", "koharu_samts_encoder.onnx"),
                          ("text_head", "koharu_samts_text_head.onnx"))},
        "initializer_to_checkpoint_key": mapping,
        "direct_or_transpose_initializers": sum(entry["transform"] in ("direct", "transpose")
                                                for entry in mapping.values()),
        "non_checkpoint_initializers": sorted(name for name, entry in mapping.items()
                                               if entry["transform"] == "fixed_normalization_constant"),
        "consumed_keys_from_graph": sorted(consumed),
        "unmatched_initializers": sorted(unmatched_initializers),
        "ambiguous_initializers": ambiguous_initializers,
        "unmatched_manifest_keys": sorted(expected - consumed),
        "consumed_keys_equal_manifest": consumed == expected,
    }
    result.parent.mkdir(parents=True, exist_ok=True)
    result.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"result": str(result), "mapped_initializers": len(mapping),
                      "direct_or_transpose_initializers": report["direct_or_transpose_initializers"],
                      "unmatched_initializers": len(unmatched_initializers),
                      "unmatched_manifest_keys": len(expected - consumed),
                      "consumed_keys_equal_manifest": consumed == expected}))


def prepare(image: Image.Image) -> tuple[torch.Tensor, tuple[int, int], tuple[int, int]]:
    """Exact prepare() from pinned Koharu inference.py, plus original size."""
    original_size = image.size
    image = image.convert("RGB")
    scale = IMAGE_SIZE / max(image.size)
    resized_size = tuple(max(1, round(axis * scale)) for axis in image.size)
    resized = image.resize(resized_size, Image.Resampling.BILINEAR)
    canvas = Image.new("RGB", (IMAGE_SIZE, IMAGE_SIZE), (128, 128, 128))
    canvas.paste(resized, (0, 0))
    array = np.asarray(canvas, dtype=np.float32).copy()
    return torch.from_numpy(array).permute(2, 0, 1), resized_size, original_size


def restore(logits: np.ndarray, resized_size: tuple[int, int], original_size: tuple[int, int]) -> np.ndarray:
    """Threshold before crop and nearest resize, exactly as inference.py."""
    width, height = resized_size
    mask = logits[0, 0, :height, :width] > 0
    binary = Image.fromarray(mask.astype(np.uint8) * 255, mode="L")
    return np.asarray(binary.resize(original_size, Image.Resampling.NEAREST), dtype=np.uint8)


def install_checkpoint_compatibility() -> None:
    # Upstream ViT blocks use torch.utils.checkpoint even during evaluation.
    original = torch.utils.checkpoint.checkpoint

    def checkpoint(function, *args, **kwargs):
        kwargs.setdefault("use_reentrant", False)
        return original(function, *args, **kwargs)

    torch.utils.checkpoint.checkpoint = checkpoint


def verify_hi_sam_source(hi_sam_root: Path) -> dict:
    """Reject a moved commit or modified tracked source before importing it."""
    root = hi_sam_root.resolve()
    archive = root.parent / "Hi-SAM.zip"
    if (root / ".pinned-revision").is_file() and archive.is_file():
        if (root / ".pinned-revision").read_text().strip() != HI_SAM_REVISION:
            raise ValueError("Hi-SAM source revision mismatch")
        if (root / ".archive-sha256").read_text().strip() != HI_SAM_ARCHIVE_SHA256:
            raise ValueError("Hi-SAM archive marker mismatch")
        if sha256(archive) != HI_SAM_ARCHIVE_SHA256:
            raise ValueError("Hi-SAM archive SHA-256 mismatch")
        with zipfile.ZipFile(archive) as bundle:
            for member in bundle.infolist():
                if member.is_dir():
                    continue
                parts = Path(member.filename).parts
                if len(parts) < 2 or parts[0] != f"Hi-SAM-{HI_SAM_REVISION}" or ".." in parts:
                    raise ValueError("Unexpected Hi-SAM archive entry")
                installed = root.joinpath(*parts[1:])
                if not installed.is_file():
                    raise ValueError(f"Hi-SAM source file is missing: {installed}")
                if hashlib.sha256(bundle.read(member)).digest() != bytes.fromhex(sha256(installed)):
                    raise ValueError(f"Hi-SAM source file differs from pinned archive: {installed}")
        return {"root": str(root), "commit": HI_SAM_REVISION,
                "archive_sha256": HI_SAM_ARCHIVE_SHA256, "tracked_tree_clean": True}

    def git(*arguments: str) -> str:
        try:
            return subprocess.check_output(
                ["git", "-C", str(root), *arguments], text=True, stderr=subprocess.PIPE
            ).strip()
        except (OSError, subprocess.CalledProcessError) as error:
            raise RuntimeError(f"cannot verify Hi-SAM Git source at {root}: {error}") from error

    actual_root = Path(git("rev-parse", "--show-toplevel")).resolve()
    if actual_root != root:
        raise ValueError(f"Hi-SAM source root mismatch: expected {root}, found {actual_root}")
    commit = git("rev-parse", "HEAD")
    if commit != HI_SAM_REVISION:
        raise ValueError(f"Hi-SAM source revision mismatch: {commit} != {HI_SAM_REVISION}")
    tracked_changes = git("status", "--porcelain", "--untracked-files=no")
    if tracked_changes:
        raise ValueError(f"Hi-SAM tracked source is modified:\n{tracked_changes}")
    return {"root": str(root), "commit": commit, "tracked_tree_clean": True}


def load_model(hi_sam_root: Path, weights: Path):
    actual = sha256(weights)
    if actual != CHECKPOINT_SHA256:
        raise ValueError(f"full checkpoint SHA-256 mismatch: {actual}")
    source = verify_hi_sam_source(hi_sam_root)
    sys.path.insert(0, str(hi_sam_root.resolve()))
    install_checkpoint_compatibility()
    from hi_sam.modeling.build import model_registry

    args = SimpleNamespace(checkpoint=None, model_type="vit_l", attn_layers=1,
                           prompt_len=12, hier_det=False)
    model = model_registry["vit_l"](args=args)
    state = load_file(str(weights), device="cpu")
    model.load_state_dict(state, strict=True)
    checkpoint_elements = sum(tensor.numel() for tensor in state.values())
    model.eval()
    return model, sorted(state.keys()), checkpoint_elements, source


class Encoder(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.image_encoder = model.image_encoder
        self.register_buffer("pixel_mean", model.pixel_mean.detach().clone())
        self.register_buffer("pixel_std", model.pixel_std.detach().clone())

    def forward(self, prepared_rgb: torch.Tensor) -> torch.Tensor:
        # The input already includes the gray-128 square pad. Normalize once.
        normalized = (prepared_rgb - self.pixel_mean) / self.pixel_std
        return self.image_encoder(normalized)


class TextHead(torch.nn.Module):
    """The live dependency slice of MaskDecoder.predict_masks for hr_masks.

    The transformer uses the IoU token and all mask tokens as context, so
    they remain. Low-resolution hypernetwork MLPs, both IoU prediction heads,
    and hierarchical decoder cannot feed the high-resolution mask and are
    intentionally absent from this wrapper's forward graph.
    """

    def __init__(self, model):
        super().__init__()
        self.modal_aligner = model.modal_aligner
        decoder = model.mask_decoder
        self.iou_token = decoder.iou_token
        self.mask_tokens = decoder.mask_tokens
        self.transformer = decoder.transformer
        self.output_upscaling = decoder.output_upscaling
        self.output_upscaling_hr = decoder.output_upscaling_hr
        self.output_hypernetworks_mlps_hr = decoder.output_hypernetworks_mlps_hr
        # The positional encoding is fixed for static 64x64 embeddings. It
        # depends on prompt_encoder.pe_layer's learned Gaussian matrix.
        self.register_buffer("image_pe", model.prompt_encoder.get_dense_pe().detach().clone())

    def forward(self, image_embeddings: torch.Tensor) -> torch.Tensor:
        sparse = self.modal_aligner(image_embeddings)
        output_tokens = torch.cat((self.iou_token.weight, self.mask_tokens.weight), dim=0)
        output_tokens = output_tokens.unsqueeze(0).expand(sparse.size(0), -1, -1)
        tokens = torch.cat((output_tokens, sparse), dim=1)
        # Static batch one. This matches the source's repeat_interleave, whose
        # repeat count is one for the reference call.
        hs, src = self.transformer(image_embeddings, self.image_pe, tokens)
        mask_token = hs[:, 1, :]
        b, c, h, w = image_embeddings.shape
        upscaled = self.output_upscaling(src.transpose(1, 2).reshape(b, c, h, w))
        upscaled = self.output_upscaling_hr(upscaled)
        hyper = self.output_hypernetworks_mlps_hr(mask_token)
        b, c, h, w = upscaled.shape
        return (hyper @ upscaled.reshape(b, c, h * w)).reshape(b, 1, h, w)


def differences(a: np.ndarray, b: np.ndarray) -> dict:
    if a.shape != b.shape:
        return {"shape_a": list(a.shape), "shape_b": list(b.shape), "error": "shape mismatch"}
    delta = np.abs(a.astype(np.float64) - b.astype(np.float64))
    return {
        "shape": list(a.shape),
        "max_abs": float(delta.max()),
        "mean_abs": float(delta.mean()),
        "rmse": float(np.sqrt(np.mean(delta * delta))),
        "changed_elements": int(np.count_nonzero(delta)),
    }


def export_graph(module: torch.nn.Module, sample: torch.Tensor, path: Path,
                 input_name: str, output_name: str) -> None:
    # Legacy exporter is pinned explicitly. Dynamo may select a different
    # decomposition without changing this proof's on-disk format.
    torch.onnx.export(module, (sample,), str(path), input_names=[input_name],
                      output_names=[output_name], opset_version=OPSET,
                      dynamo=False, external_data=True, do_constant_folding=True)


def graph_inventory(path: Path) -> dict:
    import onnx

    graph = onnx.load(str(path), load_external_data=False)
    onnx.checker.check_model(str(path), full_check=True)
    operators: dict[str, int] = {}
    for node in graph.graph.node:
        key = f"{node.domain or 'ai.onnx'}::{node.op_type}"
        operators[key] = operators.get(key, 0) + 1
    external = sorted({entry.value for tensor in graph.graph.initializer
                       for entry in tensor.external_data if entry.key == "location"})
    if any(Path(name).is_absolute() or ".." in Path(name).parts for name in external):
        raise ValueError(f"ONNX graph has non-relative external data locations: {external}")
    package = [path] + [path.parent / name for name in external]
    missing = [str(p) for p in package if not p.is_file()]
    if missing:
        raise FileNotFoundError(f"missing ONNX external data: {missing}")
    return {"onnx_ir": graph.ir_version,
            "opsets": {item.domain or "ai.onnx": item.version for item in graph.opset_import},
            "operators": dict(sorted(operators.items())),
            "initializer_names": sorted(tensor.name for tensor in graph.graph.initializer),
            "package": [{"file": p.name, "bytes": p.stat().st_size, "sha256": sha256(p)}
                        for p in package]}


def write_json(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def run(args: argparse.Namespace) -> None:
    # Limit transient allocation during the 24-block ViT export on desktops.
    torch.set_num_threads(args.torch_threads)
    weights = args.weights.resolve()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    with Image.open(args.input) as image:
        prepared, resized_size, original_size = prepare(image)
    prepared_batch = prepared.unsqueeze(0).contiguous()
    np.save(output / "prepared_rgb_f32.npy", prepared_batch.numpy())
    model, checkpoint_keys, checkpoint_elements, source = load_model(args.hi_sam_root, weights)
    encoder = Encoder(model).eval()
    head = TextHead(model).eval()
    live_prefixes = ("image_encoder.", "modal_aligner.",
                     "mask_decoder.iou_token.", "mask_decoder.mask_tokens.",
                     "mask_decoder.transformer.", "mask_decoder.output_upscaling.",
                     "mask_decoder.output_upscaling_hr.",
                     "mask_decoder.output_hypernetworks_mlps_hr.",
                     "prompt_encoder.pe_layer.positional_encoding_gaussian_matrix")
    live_keys = sorted(key for key in checkpoint_keys
                       if any(key.startswith(prefix) for prefix in live_prefixes))
    unused_keys = sorted(set(checkpoint_keys) - set(live_keys))
    manifest = {
        "model": "mayocream/koharu-text-sam-ts-l", "revision": HF_REVISION,
        "checkpoint": {"file": weights.name, "sha256": CHECKPOINT_SHA256,
                       "bytes": weights.stat().st_size,
                       "tensor_count": len(checkpoint_keys), "tensor_elements": checkpoint_elements,
                       "model_parameter_elements": sum(p.numel() for p in model.parameters()),
                       "strictly_loaded_keys": checkpoint_keys},
        "mask_path_checkpoint_keys": live_keys,
        "omitted_checkpoint_keys": unused_keys,
        "derived_dense_positional_encoding":
            "prompt_encoder.pe_layer.positional_encoding_gaussian_matrix -> fixed 1x256x64x64 buffer",
        "hi_sam_revision": HI_SAM_REVISION,
        "hi_sam_source": source,
        "configuration": {"model_type": "vit_l", "attn_layers": 1,
                          "prompt_len": 12, "hier_det": False,
                          "batch": 1, "canvas": IMAGE_SIZE, "dtype": "float32"},
        "input": {"file": str(args.input.resolve()), "sha256": sha256(args.input),
                  "original_wh": list(original_size), "resized_wh": list(resized_size)},
        "environment": {"python": sys.version, "platform": platform.platform(),
                        "torch": torch.__version__, "numpy": np.__version__,
                        "pillow": Image.__version__},
    }
    with torch.inference_mode():
        # Record normalization separately so any future Rust preprocessor can
        # be compared against both the external and internal graph boundary.
        normalized = (prepared_batch - model.pixel_mean) / model.pixel_std
        np.save(output / "normalized_f32.npy", normalized.numpy())
        embedding = encoder(prepared_batch)
        np.save(output / "encoder_embedding_f32.npy", embedding.numpy())
        head_logits = head(embedding)
        np.save(output / "head_logits_f32.npy", head_logits.numpy())
        # Independent reference call through the checkpoint's full forward.
        reference = model([{"image": prepared.contiguous(),
                            "original_size": (IMAGE_SIZE, IMAGE_SIZE)}],
                          multimask_output=False)
        reference_logits = reference[3]
        np.save(output / "reference_logits_f32.npy", reference_logits.numpy())
        reference_mask = restore(reference_logits.numpy(), resized_size, original_size)
        np.save(output / "reference_restored_mask_u8.npy", reference_mask)
        Image.fromarray(reference_mask, mode="L").save(output / "reference_restored_mask.png")
    results = {"source_vs_mask_only_head_logits": differences(
        reference_logits.numpy(), head_logits.numpy()),
        "source_vs_mask_only_head_restored_mask_pixels": int(np.count_nonzero(
            reference_mask != restore(head_logits.numpy(), resized_size, original_size)))}
    del reference, normalized
    if args.phase in ("export", "export-head", "all"):
        encoder_path = output / "koharu_samts_encoder.onnx"
        head_path = output / "koharu_samts_text_head.onnx"
        if args.phase == "export-head":
            if not encoder_path.is_file():
                raise FileNotFoundError(f"--phase export-head needs existing {encoder_path}")
        else:
            # The upstream evaluation path calls checkpoint() in every ViT block.
            # Check that removing the recomputation wrapper changes no value,
            # then use direct calls for ONNX tracing (checkpoint is not an op).
            original_checkpoint = torch.utils.checkpoint.checkpoint
            torch.utils.checkpoint.checkpoint = lambda function, *values, **_kwargs: function(*values)
            try:
                with torch.inference_mode():
                    direct_embedding = encoder(prepared_batch)
                results["checkpoint_wrapper_vs_direct_encoder"] = differences(
                    embedding.numpy(), direct_embedding.numpy())
                export_graph(encoder, prepared_batch, encoder_path, "prepared_rgb", "embedding")
            finally:
                torch.utils.checkpoint.checkpoint = original_checkpoint
        # inference_mode tensors cannot be captured by an exporter trace that
        # saves intermediates for backward. A clone made outside that context
        # is an ordinary tensor with identical FP32 values.
        embedding_for_export = embedding.clone()
        # PyTorch's eval fastpath lowers MultiheadAttention to a private
        # aten::_native_multi_head_attention op. Disable that optimization
        # during tracing so ONNX sees ordinary supported tensor operators.
        original_mha_fastpath = torch.backends.mha.get_fastpath_enabled()
        torch.backends.mha.set_fastpath_enabled(False)
        try:
            with torch.inference_mode():
                decomposed_head_logits = head(embedding)
            results["mha_fastpath_vs_decomposed_head_logits"] = differences(
                head_logits.numpy(), decomposed_head_logits.numpy())
            np.save(output / "head_logits_mha_decomposed_f32.npy",
                    decomposed_head_logits.numpy())
            with torch.no_grad():
                export_graph(head, embedding_for_export, head_path,
                             "embedding", "high_res_logits")
        finally:
            torch.backends.mha.set_fastpath_enabled(original_mha_fastpath)
        manifest["graphs"] = {"encoder": graph_inventory(encoder_path),
                              "text_head": graph_inventory(head_path)}
    if args.phase in ("parity", "all"):
        import onnxruntime as ort

        def infer(path: Path, name: str, value: np.ndarray) -> np.ndarray:
            session = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
            return session.run(None, {name: value})[0]

        ort_embedding = infer(output / "koharu_samts_encoder.onnx", "prepared_rgb",
                              prepared_batch.numpy())
        np.save(output / "onnx_encoder_embedding_f32.npy", ort_embedding)
        ort_logits_same_embedding = infer(output / "koharu_samts_text_head.onnx", "embedding",
                                          embedding.numpy())
        np.save(output / "onnx_head_logits_same_embedding_f32.npy", ort_logits_same_embedding)
        ort_logits = infer(output / "koharu_samts_text_head.onnx", "embedding", ort_embedding)
        np.save(output / "onnx_head_logits_f32.npy", ort_logits)
        ort_mask = restore(ort_logits, resized_size, original_size)
        np.save(output / "onnx_restored_mask_u8.npy", ort_mask)
        Image.fromarray(ort_mask, mode="L").save(output / "onnx_restored_mask.png")
        results.update({
            "prepared_input_identity": differences(prepared_batch.numpy(),
                                                     np.load(output / "prepared_rgb_f32.npy")),
            "encoder_embedding": differences(embedding.numpy(), ort_embedding),
            "text_head_same_embedding_logits": differences(head_logits.numpy(),
                                                            ort_logits_same_embedding),
            "end_to_end_logits": differences(head_logits.numpy(), ort_logits),
            "source_vs_onnx_logits": differences(reference_logits.numpy(), ort_logits),
            "source_vs_onnx_restored_mask_pixels": int(np.count_nonzero(reference_mask != ort_mask)),
            "onnxruntime_version": ort.__version__,
            "providers": ort.get_available_providers(),
        })
    if args.phase == "parity":
        # A parity-only run must retain export evidence rather than replace
        # it with a smaller manifest. Reject a stale proof from another input
        # or model before merging stage diagnostics.
        previous_manifest_path = output / "manifest.json"
        previous_results_path = output / "parity.json"
        if previous_manifest_path.is_file():
            previous_manifest = json.loads(previous_manifest_path.read_text())
            for field in ("model", "revision", "hi_sam_revision", "configuration", "input"):
                if previous_manifest.get(field) != manifest[field]:
                    raise ValueError(f"existing manifest has different {field}; use a fresh output directory")
            if "hi_sam_source" in previous_manifest:
                if previous_manifest["hi_sam_source"] != source:
                    raise ValueError("existing manifest has different Hi-SAM source identity")
            else:
                results["previous_source_pin_missing"] = True
            if previous_manifest.get("checkpoint", {}).get("sha256") != CHECKPOINT_SHA256:
                raise ValueError("existing manifest has different checkpoint hash")
        else:
            previous_manifest = None
        current_graphs = {
            "encoder": graph_inventory(output / "koharu_samts_encoder.onnx"),
            "text_head": graph_inventory(output / "koharu_samts_text_head.onnx"),
        }
        if previous_manifest is not None and "graphs" in previous_manifest:
            if current_graphs != previous_manifest["graphs"]:
                raise ValueError("ONNX graph inventory or package hashes changed since export; "
                                 "use a fresh output directory or re-export")
        elif previous_manifest is not None:
            results["previous_graph_inventory_missing"] = True
        manifest["graphs"] = current_graphs
        if previous_results_path.is_file():
            previous_results = json.loads(previous_results_path.read_text())
            for diagnostic in ("checkpoint_wrapper_vs_direct_encoder",
                               "mha_fastpath_vs_decomposed_head_logits"):
                if diagnostic in previous_results:
                    results[diagnostic] = previous_results[diagnostic]
        # Reconstruct the MHA diagnostic from its saved fixed-input tensor if
        # a previous parity-only run had already overwritten parity.json.
        decomposed_path = output / "head_logits_mha_decomposed_f32.npy"
        if ("mha_fastpath_vs_decomposed_head_logits" not in results
                and decomposed_path.is_file()):
            results["mha_fastpath_vs_decomposed_head_logits"] = differences(
                head_logits.numpy(), np.load(decomposed_path))
    write_json(output / "manifest.json", manifest)
    write_json(output / "parity.json", results)
    print(json.dumps({"manifest": str(output / "manifest.json"),
                      "results": str(output / "parity.json"), "summary": results}, indent=2))


def main() -> None:
    root = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path)
    parser.add_argument("--weights", type=Path, default=root / "artifacts/model.safetensors")
    parser.add_argument("--hi-sam-root", type=Path, default=root / "artifacts/Hi-SAM")
    parser.add_argument("--output-dir", type=Path, default=root / "artifacts/proof")
    parser.add_argument("--phase", choices=("reference", "export", "export-head", "parity", "all", "verify-keys"),
                        default="all")
    parser.add_argument("--key-map-output", type=Path,
                        default=root / "results/initializer-key-map.json")
    parser.add_argument("--torch-threads", type=int, default=4)
    args = parser.parse_args()
    if args.torch_threads < 1:
        parser.error("--torch-threads must be positive")
    if args.phase == "verify-keys":
        verify_initializer_keys(args.weights, args.output_dir, args.key_map_output)
        return
    if args.input is None:
        parser.error("--input is required except for --phase verify-keys")
    run(args)


if __name__ == "__main__":
    main()
