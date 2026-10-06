#!/usr/bin/env python3
"""Local MTSv3 detection-only chapter probe; outputs are not erase masks."""

import argparse
import hashlib
import importlib.metadata
import json
import math
import os
import platform
import resource
import statistics
import sys
import threading
import time
from pathlib import Path

os.environ["PYTORCH_ENABLE_MPS_FALLBACK"] = "0"

import cv2
import numpy as np
import pyclipper
import torch
from PIL import Image
from safetensors.torch import load_file
from torch import nn
from torch.nn import functional as F

SOURCE_REV = "d8028f015b8ce99a4dd798427342f97087529357"
WEIGHT_REV = "b5d31460573b6f61c1d4bdaea5fe4e18425e6a61"
WEIGHT_SHA256 = "49a448ec19e4e3894b726553fe6f47916827b10d642109aa5f41b96aeeb88e30"
MEAN_BGR = (102.9801, 115.9465, 122.7717)


class MpsMemorySampler:
    """Sample PyTorch Metal allocation; sampled maxima can miss short peaks."""

    def __init__(self):
        self.stop_event = threading.Event()
        self.samples = 0
        self.maximum = {"current_allocated_bytes": 0, "driver_allocated_bytes": 0}
        self.thread = None

    def sample(self):
        memory = {
            "current_allocated_bytes": int(torch.mps.current_allocated_memory()),
            "driver_allocated_bytes": int(torch.mps.driver_allocated_memory()),
        }
        self.samples += 1
        for name, value in memory.items():
            self.maximum[name] = max(self.maximum[name], value)

    def start(self):
        self.sample()
        self.thread = threading.Thread(target=self._poll, daemon=True)
        self.thread.start()

    def _poll(self):
        while not self.stop_event.wait(0.02):
            self.sample()

    def stop(self):
        self.stop_event.set()
        if self.thread is not None:
            self.thread.join()
        self.sample()


class FrozenBN(nn.Module):
    def __init__(self, channels):
        super().__init__()
        for name, value in (("weight", 1), ("bias", 0), ("running_mean", 0), ("running_var", 1)):
            self.register_buffer(name, torch.full((channels,), value, dtype=torch.float32))

    def forward(self, x):
        scale = self.weight * self.running_var.rsqrt()
        bias = self.bias - self.running_mean * scale
        return x * scale[None, :, None, None] + bias[None, :, None, None]


class Bottleneck(nn.Module):
    def __init__(self, channels_in, channels_mid, channels_out, stride):
        super().__init__()
        self.conv1 = nn.Conv2d(channels_in, channels_mid, 1, stride, bias=False)
        self.bn1 = FrozenBN(channels_mid)
        self.conv2 = nn.Conv2d(channels_mid, channels_mid, 3, padding=1, bias=False)
        self.bn2 = FrozenBN(channels_mid)
        self.conv3 = nn.Conv2d(channels_mid, channels_out, 1, bias=False)
        self.bn3 = FrozenBN(channels_out)
        if channels_in != channels_out:
            self.downsample = nn.Sequential(nn.Conv2d(channels_in, channels_out, 1, stride, bias=False), FrozenBN(channels_out))
        else:
            self.downsample = None

    def forward(self, x):
        residual = x if self.downsample is None else self.downsample(x)
        x = F.relu(self.bn1(self.conv1(x)))
        x = F.relu(self.bn2(self.conv2(x)))
        return F.relu(self.bn3(self.conv3(x)) + residual)


class Body(nn.Module):
    def __init__(self):
        super().__init__()
        self.stem = nn.Module()
        self.stem.conv1 = nn.Conv2d(3, 64, 7, stride=2, padding=3, bias=False)
        self.stem.bn1 = FrozenBN(64)
        in_channels = 64
        for layer_index, (mid, out, blocks) in enumerate(((64, 256, 3), (128, 512, 4), (256, 1024, 6), (512, 2048, 3)), 1):
            layer = nn.Sequential()
            for block in range(blocks):
                layer.add_module(str(block), Bottleneck(in_channels, mid, out, 2 if layer_index > 1 and block == 0 else 1))
                in_channels = out
            setattr(self, f"layer{layer_index}", layer)

    def forward(self, x):
        x = F.max_pool2d(F.relu(self.stem.bn1(self.stem.conv1(x))), 3, 2, 1)
        outputs = []
        for layer in (self.layer1, self.layer2, self.layer3, self.layer4):
            x = layer(x)
            outputs.append(x)
        return outputs


class FPN(nn.Module):
    def __init__(self):
        super().__init__()
        for i, channels in enumerate((256, 512, 1024, 2048), 1):
            setattr(self, f"fpn_inner{i}", nn.Conv2d(channels, 256, 1))
            setattr(self, f"fpn_layer{i}", nn.Conv2d(256, 256, 3, padding=1))

    def forward(self, features):
        last = self.fpn_inner4(features[3])
        results = [self.fpn_layer4(last)]
        for i in (3, 2, 1):
            last = getattr(self, f"fpn_inner{i}")(features[i-1]) + F.interpolate(last, scale_factor=2, mode="nearest")
            results.insert(0, getattr(self, f"fpn_layer{i}")(last))
        results.append(F.max_pool2d(results[-1], 1, 2))
        return results


class ProposalHead(nn.Module):
    def __init__(self):
        super().__init__()
        for i in (3, 4, 5):
            setattr(self, f"fpn_out{i}", nn.Sequential(nn.Conv2d(256, 64, 3, padding=1, bias=False)))
        self.fpn_out2 = nn.Conv2d(256, 64, 3, padding=1, bias=False)
        self.seg_out = nn.Sequential(
            nn.Sequential(nn.Conv2d(256, 64, 3, padding=1, bias=False), nn.BatchNorm2d(64), nn.ReLU()),
            nn.ConvTranspose2d(64, 64, 2, 2), nn.BatchNorm2d(64), nn.ReLU(),
            nn.ConvTranspose2d(64, 1, 2, 2), nn.Sigmoid(),
        )

    def forward(self, x):
        p5 = F.interpolate(self.fpn_out5(x[3]), scale_factor=8, mode="nearest")
        p4 = F.interpolate(self.fpn_out4(x[2]), scale_factor=4, mode="nearest")
        p3 = F.interpolate(self.fpn_out3(x[1]), scale_factor=2, mode="nearest")
        p2 = self.fpn_out2(x[0])
        return self.seg_out(torch.cat((p5, p4, p3, p2), dim=1))


class Detector(nn.Module):
    def __init__(self):
        super().__init__()
        self.module = nn.Module()
        self.module.backbone = nn.Module()
        self.module.backbone.body = Body()
        self.module.backbone.fpn = FPN()
        self.module.proposal = nn.Module()
        self.module.proposal.head = ProposalHead()

    def forward(self, x):
        features = self.module.backbone.fpn(self.module.backbone.body(x))
        return self.module.proposal.head(features)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def array_sha256(array):
    return hashlib.sha256(np.ascontiguousarray(array).tobytes()).hexdigest()


def peak_rss_bytes():
    value = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return int(value if sys.platform == "darwin" else value * 1024)


def prepare(image, mode="eval_pil"):
    if mode not in ("eval_pil", "feature_cv2", "demo_pil_800"):
        raise ValueError(f"unknown preprocessing mode {mode}")
    width, height = image.size
    size = 800 if mode == "demo_pil_800" else 1440
    if mode != "demo_pil_800" and max(width, height) / min(width, height) * size > 4000:
        size = round(4000 * min(width, height) / max(width, height))
    if mode == "feature_cv2":
        # Pinned extract_features_vmb.py::_image_transform: float32 BGR,
        # Caffe subtraction, then cv2.INTER_LINEAR with an isotropic scale.
        rgb = np.asarray(image.convert("RGB"), dtype=np.float32)
        arr = rgb[:, :, ::-1].copy()
        arr -= np.array(MEAN_BGR, dtype=np.float32)
        scale = 1440 / min(width, height)
        if np.round(scale * max(width, height)) > 4000:
            scale = 4000 / max(width, height)
        arr = cv2.resize(arr, None, fx=scale, fy=scale, interpolation=cv2.INTER_LINEAR)
        new_height, new_width = arr.shape[:2]
    else:
        if width < height:
            new_width, new_height = int(size), int(size * height / width)
        else:
            new_width, new_height = int(size * width / height), int(size)
        resized = image.convert("RGB").resize((new_width, new_height), Image.Resampling.BILINEAR)
        arr = np.asarray(resized, dtype=np.float32)[:, :, ::-1].copy()
        arr -= np.array(MEAN_BGR, dtype=np.float32)
    padded_height = math.ceil(new_height / 32) * 32
    padded_width = math.ceil(new_width / 32) * 32
    tensor = np.zeros((3, padded_height, padded_width), dtype=np.float32)
    tensor[:, :new_height, :new_width] = arr.transpose(2, 0, 1)
    return torch.from_numpy(tensor[None]), (new_width, new_height, padded_width, padded_height)


def mini_box(contour):
    rect = cv2.minAreaRect(contour)
    points = sorted(list(cv2.boxPoints(rect)), key=lambda p: p[0])
    a, d = (0, 1) if points[1][1] > points[0][1] else (1, 0)
    b, c = (2, 3) if points[3][1] > points[2][1] else (3, 2)
    return np.array([points[a], points[b], points[c], points[d]], dtype=np.float32), min(rect[1])


def unclip(points, ratio):
    area = abs(cv2.contourArea(points.astype(np.float32)))
    perimeter = cv2.arcLength(points.astype(np.float32), True)
    if perimeter <= 0:
        return []
    offset = pyclipper.PyclipperOffset()
    offset.AddPath(points.astype(np.int32).tolist(), pyclipper.JT_ROUND, pyclipper.ET_CLOSEDPOLYGON)
    return offset.Execute(area * ratio / perimeter)


def postprocess(pred, original_size, resized_size):
    # Mirrors upstream SEGPostProcessor.boxes_from_bitmap, with source-pixel polygons.
    height, width = pred.shape
    contours, _ = cv2.findContours((pred > 0.1).astype(np.uint8) * 255, cv2.RETR_LIST, cv2.CHAIN_APPROX_NONE)
    results = []
    sx, sy = original_size[0] / resized_size[0], original_size[1] / resized_size[1]
    for contour in contours:
        box, short_side = mini_box(contour)
        if short_side < 5:
            continue
        xmin = int(np.clip(np.floor(box[:, 0].min()), 0, width - 1))
        xmax = int(np.clip(np.ceil(box[:, 0].max()), 0, width - 1))
        ymin = int(np.clip(np.floor(box[:, 1].min()), 0, height - 1))
        ymax = int(np.clip(np.ceil(box[:, 1].max()), 0, height - 1))
        mask = np.zeros((ymax-ymin+1, xmax-xmin+1), dtype=np.uint8)
        shifted = box.copy(); shifted[:, 0] -= xmin; shifted[:, 1] -= ymin
        cv2.fillPoly(mask, [shifted.astype(np.int32)], 1)
        score = cv2.mean(pred[ymin:ymax+1, xmin:xmax+1], mask)[0]
        if score < 0.1:
            continue
        approx = cv2.approxPolyDP(contour, 0.01 * cv2.arcLength(contour, True), True).reshape(-1, 2)
        if len(approx) <= 2:
            continue
        polygon_paths = unclip(approx, 3.0)
        expanded = unclip(box, 1.5)
        if len(polygon_paths) != 1 or len(expanded) != 1:
            continue
        polygon = np.asarray(polygon_paths[0], dtype=np.float32).reshape(-1, 2)
        expanded_box, _ = mini_box(np.asarray(expanded[0], dtype=np.float32).reshape(-1, 1, 2))
        # Upstream SegmentationMask.resize scales polygons from resized to
        # source coordinates without clipping. Its box path first maps from
        # the padded prediction dimensions into resized image dimensions.
        polygon[:, 0] *= sx
        polygon[:, 1] *= sy
        expanded_box[:, 0] = np.clip(np.round(expanded_box[:, 0] / width * resized_size[0]), 0, resized_size[0]) * sx
        expanded_box[:, 1] = np.clip(np.round(expanded_box[:, 1] / height * resized_size[1]), 0, resized_size[1]) * sy
        bounds = [float(expanded_box[:, 0].min()), float(expanded_box[:, 1].min()), float(expanded_box[:, 0].max()), float(expanded_box[:, 1].max())]
        results.append({"polygon": polygon.tolist(), "rotated_box": expanded_box.tolist(), "bounding_box": bounds, "score": float(score)})
        if len(results) == 1000:
            break
    return results, len(contours)


def build_manifest(output_dir, weights):
    files = {}
    for path in sorted(output_dir.rglob("*")):
        if path.is_file() and path.name != "manifest.json":
            files[str(path.relative_to(output_dir))] = {"sha256": sha256(path), "bytes": path.stat().st_size}
    manifest = {
        "source": "https://github.com/ku21fan/COO-Comic-Onomatopoeia",
        "source_revision": SOURCE_REV,
        "checkpoint": "https://huggingface.co/mayocream/coo-comic-onomatopoeia-safetensors",
        "checkpoint_revision": WEIGHT_REV,
        "checkpoint_file": "mtsv3/model.safetensors",
        "checkpoint_sha256": sha256(weights),
        "probe_sha256": sha256(Path(__file__)),
        "mps_fallback": os.environ["PYTORCH_ENABLE_MPS_FALLBACK"],
        "packages": {name: importlib.metadata.version(name) for name in ("torch", "numpy", "Pillow", "opencv-python-headless", "pyclipper", "safetensors")},
        "files": files,
    }
    (output_dir / "manifest.json").write_text(json.dumps(manifest, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--weights", type=Path, default=Path(__file__).parent / "artifacts/model.safetensors")
    parser.add_argument("--device", choices=("cpu", "mps"), default="mps")
    parser.add_argument("--preprocess", choices=("eval_pil", "feature_cv2", "demo_pil_800"), default="eval_pil")
    parser.add_argument("--pages", nargs="*", default=[])
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    if sha256(args.weights) != WEIGHT_SHA256:
        raise ValueError("MTSv3 full checkpoint SHA-256 mismatch")
    model = Detector()
    state = load_file(str(args.weights))
    model.load_state_dict(state, strict=True)
    model.eval().to(args.device)
    paths = sorted((p for p in args.source_dir.iterdir() if p.suffix.lower() in (".jpg", ".jpeg", ".png", ".webp")), key=lambda p: int(p.stem) if p.stem.isdigit() else p.stem)
    if args.pages:
        wanted = set(args.pages)
        paths = [p for p in paths if p.stem in wanted]
    pages = []
    sampler = MpsMemorySampler() if args.device == "mps" else None
    if sampler:
        sampler.start()
    for index, path in enumerate(paths, 1):
        start = time.perf_counter()
        image = Image.open(path)
        tensor, resized = prepare(image, args.preprocess)
        prepared = time.perf_counter()
        prepared_sha256 = array_sha256(tensor.numpy())
        with torch.inference_mode():
            pred = model(tensor.to(args.device)).squeeze().float().cpu().numpy()
        if args.device == "mps":
            torch.mps.synchronize()
        inferred = time.perf_counter()
        detections, contour_count = postprocess(pred, image.size, resized[:2])
        processed = time.perf_counter()
        page_dir = args.output_dir / path.stem
        page_dir.mkdir(parents=True, exist_ok=True)
        if path.stem in ("02", "07"):
            np.save(page_dir / "prepared-input.npy", tensor.numpy())
        np.savez_compressed(page_dir / "proposal-map.npz", proposal=pred)
        Image.fromarray((pred > 0.1).astype(np.uint8) * 255).save(page_dir / "proposal-bitmap.png")
        page = {"file": path.name, "sha256": sha256(path), "preprocess": args.preprocess, "original_size": list(image.size), "resized_size": list(resized[:2]), "padded_size": list(resized[2:]), "prepared_input_sha256": prepared_sha256, "proposal_map_sha256": array_sha256(pred), "raw_contours": contour_count, "detections": detections, "detections_at_demo_threshold_0_4": sum(d["score"] >= 0.4 for d in detections), "timing_ms": {"prepare": round((prepared-start)*1000, 2), "inference": round((inferred-prepared)*1000, 2), "postprocess": round((processed-inferred)*1000, 2)}, "proposal_positive_pixels": int((pred > 0.1).sum())}
        (page_dir / "detections.json").write_text(json.dumps(page, indent=2))
        pages.append(page)
        print(f"{index}/{len(paths)} {path.name}: {len(detections)} proposals, {page['timing_ms']}", flush=True)
    if sampler:
        sampler.stop()
    report = {"upstream_source_revision": SOURCE_REV, "checkpoint_revision": WEIGHT_REV, "checkpoint_sha256": WEIGHT_SHA256, "checkpoint_keys_strictly_loaded": len(state), "checkpoint_unused_keys": 0, "torch": torch.__version__, "python": platform.python_version(), "device": args.device, "preprocess": args.preprocess, "pages": pages, "summary": {"page_count": len(pages), "proposals": sum(len(p['detections']) for p in pages), "mean_inference_ms": statistics.mean(p['timing_ms']['inference'] for p in pages) if pages else None, "max_rss_bytes": peak_rss_bytes(), "mps_sampled_max_bytes": sampler.maximum if sampler else None, "mps_samples": sampler.samples if sampler else 0}}
    (args.output_dir / "summary.json").write_text(json.dumps(report, indent=2))
    build_manifest(args.output_dir, args.weights)


if __name__ == "__main__":
    main()
