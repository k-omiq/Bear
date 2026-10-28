"""Fetch pinned Koharu sources and export the app's local SAM-TS-L graphs.

Runs under a managed Python environment started by the desktop command. The
checkpoint and converted graphs stay in the user's app data directory.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request
import zipfile
from pathlib import Path

MODEL_REVISION = "5dd97423e0fbf2404264979136d47e8101144046"
MODEL_SHA256 = "bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45"
MODEL_BYTES = 1355824988
MODEL_URL = f"https://huggingface.co/mayocream/koharu-text-sam-ts-l/resolve/{MODEL_REVISION}/model.safetensors"
DOWNLOAD_ATTEMPTS = 6
HI_SAM_REVISION = "69009434d4dba5541f228d8f5acb0754c333d417"
HI_SAM_ARCHIVE_SHA256 = "f18fb049813f9b9319ac074449f4539fbe5b58f4e3f450ce331578d74bc3e327"
ENCODER = "koharu_samts_encoder.onnx"
ATTENTION_HEADS = 16
ATTENTION_SIDE = 64


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def progress(step: str, **fields: int) -> None:
    """One JSON line on stdout; the desktop turns these into its progress bar."""
    print(json.dumps({"step": step, **fields}), flush=True)


def checkpoint(root: Path) -> Path:
    """Download the pinned checkpoint, resuming a partial file across retries."""
    target = root / "checkpoint" / "model.safetensors"
    if target.is_file() and target.stat().st_size == MODEL_BYTES and digest(target) == MODEL_SHA256:
        progress("weights", done=MODEL_BYTES, total=MODEL_BYTES)
        return target
    target.parent.mkdir(parents=True, exist_ok=True)
    partial = target.with_name(target.name + ".part")
    for attempt in range(DOWNLOAD_ATTEMPTS):
        have = partial.stat().st_size if partial.is_file() else 0
        if have > MODEL_BYTES:
            partial.unlink()
            have = 0
        if have == MODEL_BYTES:
            break
        request = urllib.request.Request(MODEL_URL, headers={"Range": f"bytes={have}-"} if have else {})
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                # A server that ignores the range sends the whole file again.
                if have and response.status != 206:
                    have = 0
                with partial.open("ab" if have else "wb") as output:
                    shown = -1
                    while chunk := response.read(1024 * 1024):
                        output.write(chunk)
                        have += len(chunk)
                        percent = have * 100 // MODEL_BYTES
                        if percent != shown:
                            shown = percent
                            progress("weights", done=have, total=MODEL_BYTES)
        except (OSError, http.client.HTTPException) as error:
            if attempt + 1 == DOWNLOAD_ATTEMPTS:
                raise RuntimeError(f"Koharu checkpoint download failed: {error}") from error
            time.sleep(min(30, 2 ** attempt))
    if not partial.is_file() or partial.stat().st_size != MODEL_BYTES or digest(partial) != MODEL_SHA256:
        partial.unlink(missing_ok=True)
        raise RuntimeError("Koharu checkpoint SHA-256 mismatch")
    os.replace(partial, target)
    return target


def extract_hi_sam_archive(archive: Path, source: Path) -> None:
    """Extract the pinned source snapshot without accepting paths outside it."""
    expected_root = f"Hi-SAM-{HI_SAM_REVISION}"
    with zipfile.ZipFile(archive) as bundle:
        for member in bundle.infolist():
            parts = Path(member.filename).parts
            if parts == (expected_root,) and member.is_dir():
                continue
            if len(parts) < 2 or ".." in parts or parts[0] != expected_root:
                raise RuntimeError("Unexpected Hi-SAM archive entry")
            destination = source.joinpath(*parts[1:])
            if member.is_dir():
                destination.mkdir(parents=True, exist_ok=True)
            else:
                destination.parent.mkdir(parents=True, exist_ok=True)
                with bundle.open(member) as stream, destination.open("wb") as output:
                    while chunk := stream.read(1024 * 1024):
                        output.write(chunk)


def hi_sam(root: Path) -> Path:
    source = root / "Hi-SAM"
    archive = root / "Hi-SAM.zip"
    if not archive.is_file() or digest(archive) != HI_SAM_ARCHIVE_SHA256:
        url = f"https://codeload.github.com/ymy-k/Hi-SAM/zip/{HI_SAM_REVISION}"
        with urllib.request.urlopen(url, timeout=120) as response, archive.open("wb") as output:
            shutil.copyfileobj(response, output)
    if digest(archive) != HI_SAM_ARCHIVE_SHA256:
        raise RuntimeError("Hi-SAM source archive SHA-256 mismatch")
    if source.exists():
        shutil.rmtree(source)
    source.mkdir(parents=True, exist_ok=True)
    extract_hi_sam_archive(archive, source)
    (source / ".pinned-revision").write_text(HI_SAM_REVISION + "\n")
    (source / ".archive-sha256").write_text(HI_SAM_ARCHIVE_SHA256 + "\n")
    return source


def split_global_attention(model, heads: int, side: int) -> None:
    """Run the encoder's global-attention blocks one head at a time.

    Each global block builds a heads x tokens x tokens score tensor (1 GiB at
    16 x 4096 x 4096), adds the relative-position terms and softmaxes it, with
    several such tensors alive at once. One Slice per head keeps one 64 MiB
    slice alive instead (WebGPU refuses a 16-way Split). Every head runs the
    same ops in the same order, so the output is bit-identical.
    """
    import numpy as np
    from onnx import helper, numpy_helper

    graph = model.graph
    tokens = side * side
    blocks = sorted({node.name.split("/")[2] for node in graph.node
                     if node.name.startswith("/image_encoder/blocks.")},
                    key=lambda name: int(name.split(".")[1]))
    # Windowed blocks pad the page to whole windows; global blocks never do.
    global_blocks = [block for block in blocks if not any(
        node.op_type == "Pad" and node.name.startswith(f"/image_encoder/{block}/") for node in graph.node)]

    def constant(name: str, values: list[int]) -> str:
        graph.initializer.append(numpy_helper.from_array(np.asarray(values, dtype=np.int64), name))
        return name

    constant("head_axis", [0])
    for head in range(heads):
        constant(f"head_start_{head}", [head])
        constant(f"head_end_{head}", [head + 1])
    constant("head_shape5", [1, side, side, side, side])
    constant("head_shape3", [1, tokens, tokens])

    # Keyed by name: protobuf may hand out a new wrapper, with a new id(), on
    # every access to a repeated field.
    dropped: set[str] = set()
    replacements: dict[str, list] = {}
    for block in global_blocks:
        prefix = f"/image_encoder/{block}/attn/"
        names = ("MatMul", "Reshape_3", "Add_2", "Add_3", "Reshape_4", "Softmax", "MatMul_1")
        nodes = {node.name[len(prefix):]: node for node in graph.node
                 if node.name.startswith(prefix) and node.name[len(prefix):] in names}
        if len(nodes) != len(names) or nodes["Add_2"].input[0] != nodes["Reshape_3"].output[0] \
                or nodes["Add_3"].input[0] != nodes["Add_2"].output[0]:
            raise RuntimeError(f"Unexpected SAM-TS-L attention layout in {block}")
        query, key = nodes["MatMul"].input
        sources = {"q": query, "kt": key, "v": nodes["MatMul_1"].input[1],
                   "rh": nodes["Add_2"].input[1], "rw": nodes["Add_3"].input[1]}
        made = []
        parts: dict[str, list[str]] = {}
        for label, tensor in sources.items():
            parts[label] = [f"{prefix}head/{label}_{head}" for head in range(heads)]
            made += [helper.make_node("Slice", [tensor, f"head_start_{head}", f"head_end_{head}", "head_axis"],
                                      [output], name=f"{prefix}head/slice_{label}_{head}")
                     for head, output in enumerate(parts[label])]
        results = []
        for head in range(heads):
            at = f"{prefix}head/{head}/"
            made += [
                helper.make_node("MatMul", [parts["q"][head], parts["kt"][head]], [at + "a"], name=at + "MatMul"),
                helper.make_node("Reshape", [at + "a", "head_shape5"], [at + "a5"], name=at + "Reshape5"),
                helper.make_node("Add", [at + "a5", parts["rh"][head]], [at + "b5"], name=at + "AddH"),
                helper.make_node("Add", [at + "b5", parts["rw"][head]], [at + "c5"], name=at + "AddW"),
                helper.make_node("Reshape", [at + "c5", "head_shape3"], [at + "c"], name=at + "Reshape3"),
                helper.make_node("Softmax", [at + "c"], [at + "s"], name=at + "Softmax", axis=-1),
                helper.make_node("MatMul", [at + "s", parts["v"][head]], [at + "o"], name=at + "MatMul_1"),
            ]
            results.append(at + "o")
        made.append(helper.make_node("Concat", results, [nodes["MatMul_1"].output[0]],
                                     name=f"{prefix}head/concat", axis=0))
        dropped.update(node.name for node in nodes.values())
        replacements[nodes["MatMul_1"].name] = made

    rebuilt = []
    for node in graph.node:
        if node.name in replacements:
            rebuilt += replacements[node.name]
        elif node.name not in dropped:
            rebuilt.append(node)
    # Drop what nothing reads any more: the old blocks' shape plumbing.
    outputs = {output.name for output in graph.output}
    while True:
        used = outputs | {name for node in rebuilt for name in node.input}
        kept = [node for node in rebuilt if any(name in used for name in node.output)]
        if len(kept) == len(rebuilt):
            break
        rebuilt = kept
    del graph.node[:]
    graph.node.extend(rebuilt)
    used = {name for node in graph.node for name in node.input}
    initializers = [tensor for tensor in graph.initializer if tensor.name in used]
    del graph.initializer[:]
    graph.initializer.extend(initializers)


def per_head_encoder(proof: Path) -> None:
    """Rewrite the exported encoder in place, its weights in a side file.

    ONNX Runtime copies every initializer embedded in the graph file but maps a
    side file, which saves about 0.9 GiB at load. A stale side file is never
    appended to: the new pair is written in a fresh directory first.
    """
    import onnx

    encoder = proof / ENCODER
    staged = proof / "per-head"
    shutil.rmtree(staged, ignore_errors=True)
    staged.mkdir()
    model = onnx.load(str(encoder))
    split_global_attention(model, ATTENTION_HEADS, ATTENTION_SIDE)
    onnx.save_model(model, str(staged / ENCODER), save_as_external_data=True,
                    all_tensors_to_one_file=True, location=ENCODER + ".data")
    del model
    # Checked from disk so the weights are not serialized a second time.
    onnx.checker.check_model(str(staged / ENCODER))
    os.replace(staged / (ENCODER + ".data"), proof / (ENCODER + ".data"))
    os.replace(staged / ENCODER, encoder)
    staged.rmdir()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    # The desktop passes the pinned table so every platform exports the same
    # head bytes. The cloud seed omits it and keeps its own Linux pin.
    parser.add_argument("--dense-pe", type=Path)
    # The desktop runs global attention one head at a time with the weights in
    # a side file. The cloud seed omits it and keeps its own single-file pin.
    parser.add_argument("--per-head-attention", action="store_true")
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    weights = checkpoint(root)
    progress("source")
    architecture = hi_sam(root)
    progress("export")
    proof = root / "proof"
    subprocess.run([
        sys.executable, str(arguments.source / "export_mask.py"),
        "--input", str(arguments.source / "synthetic_page.png"),
        "--weights", str(weights),
        "--hi-sam-root", str(architecture),
        "--output-dir", str(proof),
        "--phase", "export",
        *(["--dense-pe", str(arguments.dense_pe)] if arguments.dense_pe else []),
    ], check=True)
    if arguments.per_head_attention:
        per_head_encoder(proof)


if __name__ == "__main__":
    main()
