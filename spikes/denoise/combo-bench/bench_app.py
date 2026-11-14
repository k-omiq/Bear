"""Denoise combo bench (2026-11-14): pinned app models plus new community models, chained.

Results and plan: docs/research/denoise-combos-2026-11.md. combos.py holds the models (EXTRA) and the
combos; its last block (round 2) replaces COMBOS, so edit that block to choose what runs.

    modal volume put mc-denoise-bench-2611 <pages dir> /in
    MC_DENOISE_PY=deploy/cloud/common/denoise.py modal run --detach bench_app.py::main --crops <boxes.json>

Own ephemeral app and own volume, so the deployed mc-* apps are untouched (their weights volume is mounted
read only, to copy the app models). A Modal function drives the runs: streaming pages from the laptop
dropped the link. Pages go to /out/<combo>/, crops to /crops/<combo>/, run info to /info/<combo>/.
Delete the volume afterwards.
"""

from __future__ import annotations

import hashlib
import io
import json
import os
import time
from pathlib import Path

import modal

HERE = Path(__file__).resolve().parent
RUNTIME = Path(os.environ.get("MC_DENOISE_PY", "/nonexistent")) if modal.is_local() else None
VOLUME = os.environ.get("MC_BENCH_VOLUME", "mc-denoise-bench-2611")
ROOT = "/weights"
EXTRA_DIR = f"{ROOT}/extra"

image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("torch==2.13.0", "torchvision==0.28.0", index_url="https://download.pytorch.org/whl/cu129")
    .pip_install("onnxruntime-gpu==1.23.2", "numpy==2.4.6", "pillow==12.3.0", "spandrel==0.4.2",
                 "spandrel_extra_arches==0.2.0")
)
if modal.is_local():
    image = image.add_local_file(RUNTIME, "/root/mc_denoise.py").add_local_file(
        HERE / "combos.py", "/root/combos.py")
volume = modal.Volume.from_name(VOLUME, create_if_missing=True)
# The live installation's weights, read only: the app models are already there.
LIVE = modal.Volume.from_name(os.environ.get("MC_LIVE_VOLUME", "mc-acb01f-weights")).read_only()
app = modal.App("mc-denoise-bench-2611")


@app.function(image=image, volumes={ROOT: volume, "/live": LIVE}, cpu=2.0, memory=4096, timeout=1800)
def seed() -> dict:
    """App models through the pinned path; new models downloaded and hashed (recorded, not trusted)."""
    import urllib.request
    import mc_denoise as d
    import combos as c

    started = time.time()
    import shutil
    os.makedirs(f"{ROOT}/denoise", exist_ok=True)
    for f in Path("/live/denoise").glob("*"):
        if not (Path(ROOT) / "denoise" / f.name).exists():
            shutil.copy(f, Path(ROOT) / "denoise" / f.name)
    raw_download = d._download

    def retrying(url, directory):
        for attempt in range(6):
            try:
                return raw_download(url, directory)
            except Exception as e:  # noqa: BLE001
                last = repr(e)[:200]
                time.sleep(5 * (attempt + 1))
        raise RuntimeError(f"download failed {url}: {last}")
    d._download = retrying
    d.ensure_models(ROOT, d.models_for(c.app_steps()), download=True, commit=volume.commit)
    os.makedirs(EXTRA_DIR, exist_ok=True)
    import spandrel_extra_arches
    spandrel_extra_arches.install()
    report = {}
    for key, spec in c.EXTRA.items():
        target = c.extra_path(EXTRA_DIR, spec)
        info = {}
        if not target.exists():
            try:
                cache = Path(EXTRA_DIR) / ("dl-" + hashlib.sha1(spec["url"].encode()).hexdigest()[:12])
                if not cache.exists():
                    req = urllib.request.Request(spec["url"], headers={"User-Agent": "Mozilla/5.0"})
                    with urllib.request.urlopen(req, timeout=600) as r, open(str(cache) + ".part", "wb") as fh:
                        for chunk in iter(lambda: r.read(1 << 20), b""):
                            fh.write(chunk)
                    os.replace(str(cache) + ".part", cache)
                if spec.get("archive"):
                    import tarfile, zipfile
                    if zipfile.is_zipfile(cache):
                        z = zipfile.ZipFile(cache)
                        names = [n for n in z.namelist() if not n.endswith("/")]
                        read = lambda n: z.read(n)
                    else:
                        t = tarfile.open(cache)
                        names = [m.name for m in t.getmembers() if m.isfile()]
                        read = lambda n: t.extractfile(n).read()
                    info["members"] = names
                    if spec.get("member") is None and len([n for n in names if n.endswith((".pth", ".safetensors"))]) == 1:
                        spec = {**spec, "member": next(n for n in names if n.endswith((".pth", ".safetensors")))}
                    if not spec.get("member"):
                        report[key] = info
                        continue
                    target = target.with_suffix(Path(spec["member"]).suffix)
                    target.write_bytes(read(spec["member"]))
                    info["member"] = spec["member"]
                else:
                    os.replace(cache, target)
            except Exception as e:  # noqa: BLE001
                report[key] = {**info, "error": repr(e)[:300]}
                continue
        data = target.read_bytes()
        info.update({"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
        try:
            from spandrel import ModelLoader
            m = ModelLoader().load_from_file(str(target))
            info.update(arch=m.architecture.name, scale=m.scale, in_ch=m.input_channels,
                        half=m.supports_half, bf16=m.supports_bfloat16)
        except Exception as e:  # noqa: BLE001
            info["load_error"] = repr(e)[:300]
        report[key] = info
    volume.commit()
    return {"seconds": round(time.time() - started, 1), "extra": report}


@app.cls(image=image, volumes={ROOT: volume}, gpu=["L4", "A10G", "L40S"], cpu=4.0, memory=16384, timeout=3600,
         scaledown_window=60, max_containers=4)
class Bench:
    @modal.enter()
    def load(self) -> None:
        import mc_denoise as d
        import combos as c
        import spandrel_extra_arches
        spandrel_extra_arches.install()
        self.d, self.c = d, c
        self.extra = c.register_extra(d, EXTRA_DIR)
        paths = d.ensure_models(ROOT, d.models_for(c.app_steps()), download=False)
        paths.update(self.extra)
        self.sessions = d.SessionCache(paths)

    @modal.method()
    def page(self, name: str, combo: str, boxes: list) -> dict:
        """One page through one combo. Input from the volume; page, crops and info to the volume."""
        from PIL import Image
        stem = Path(name).stem
        target = Path(ROOT) / "out" / combo / name
        info_path = Path(ROOT) / "info" / combo / f"{stem}.json"
        if info_path.exists():
            return json.loads(info_path.read_text())
        if target.exists():
            out, info = target.read_bytes(), {"resumed": True}
        else:
            steps = self.c.steps_for(self.d, combo)
            started = time.perf_counter()
            try:
                out, info = self.d.denoise_page(self.sessions, steps, (Path(ROOT) / "in" / name).read_bytes())
            except Exception as error:  # noqa: BLE001
                info = {"error": repr(error)[:300]}
                out = b""
            else:
                info["total_ms"] = round((time.perf_counter() - started) * 1000)
                info["gpu"] = os.environ.get("MODAL_GPU", "")
                import torch
                info["gpu"] = torch.cuda.get_device_name(0)
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(out)
        if out:
            im = Image.open(io.BytesIO(out))
            for i, (x, y, w, h) in enumerate(boxes):
                t = Path(ROOT) / "crops" / combo / f"{stem}_{i}.png"
                t.parent.mkdir(parents=True, exist_ok=True)
                im.crop((x, y, x + w, y + h)).save(t)
        info.update(page=name, combo=combo)
        info_path.parent.mkdir(parents=True, exist_ok=True)
        info_path.write_text(json.dumps(info))
        volume.commit()
        return info


@app.function(image=image, volumes={ROOT: volume}, cpu=1.0, memory=2048, timeout=4 * 3600)
def drive(args: list) -> dict:
    """Runs on Modal, so a dropped laptop link does not stop the round."""
    started = time.time()
    done = 0
    for info in Bench().page.starmap(args, order_outputs=False, return_exceptions=True):
        done += 1
        print(done, info if isinstance(info, Exception) else (info.get("combo"), info.get("page"),
              info.get("total_ms", info.get("error", "resumed"))), flush=True)
    summary = {"runs": len(args), "seconds": round(time.time() - started)}
    volume.reload()
    Path(f"{ROOT}/done.json").write_text(json.dumps(summary))
    volume.commit()
    return summary


@app.local_entrypoint()
def seed_only() -> None:
    print(json.dumps(seed.remote(), indent=1))


@app.local_entrypoint()
def main(crops: str, only: str = "") -> None:
    """Pages must already be on the volume under /in. Run with `modal run --detach`."""
    import combos as c

    boxes = json.loads(Path(crops).read_text())
    names = [k for k in c.COMBOS if not only or k in only.split(",")]
    print("seed:", json.dumps(seed.remote())[:300])
    args = [(page, k, boxes[page]) for page in sorted(boxes) for k in names if c.wanted(k, page)]
    print(len(args), "page runs", flush=True)
    # --detach keeps the app and this call alive if the laptop link drops.
    print(drive.remote(args), flush=True)
