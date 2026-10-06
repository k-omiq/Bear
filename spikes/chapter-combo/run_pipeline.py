#!/usr/bin/env python3
"""Run RT-DETR, SAM-TS-L and COO, then fuse their chapter detections.

RT CPU and SAM MPS may run concurrently; COO uses MPS after SAM exits so the
two large GPU models do not contend for Metal memory. This is a local proof,
not the desktop runtime.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[2]
RT_MODEL = ROOT / "spikes/chapter-rtdetr/artifacts/models/detector.onnx"
RT_PYTHON = ROOT / "spikes/sam-ts-l/artifacts/venv/bin/python"
SAM_PYTHON = RT_PYTHON
COO_PYTHON = ROOT / "spikes/coo-mtsv3/artifacts/venv/bin/python"
RT_SHA256 = "065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195"
COO_SHA256 = "49a448ec19e4e3894b726553fe6f47916827b10d642109aa5f41b96aeeb88e30"
SAM_SHA256 = "bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def sampled_rss_bytes(pids: list[int]) -> dict[int, int]:
    if not pids:
        return {}
    try:
        result = subprocess.run(
            ["ps", "-o", "pid=,rss=", "-p", ",".join(map(str, pids))],
            capture_output=True, text=True, check=False, timeout=2,
        )
        values = [line.split() for line in result.stdout.splitlines() if line.strip()]
        return {int(pid): int(rss_kib) * 1024 for pid, rss_kib in values}
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return {}


def run_group(output: Path, specs: list[tuple[str, list[str]]]) -> dict:
    processes = {}
    streams = {}
    started = time.perf_counter()
    start_times = {}
    end_times = {}
    peak_sampled_rss = 0
    peak_process_rss = {name: 0 for name, _ in specs}
    try:
        for name, command in specs:
            log_path = output / f"{name}.log"
            stream = log_path.open("w")
            streams[name] = stream
            start_times[name] = time.perf_counter()
            print(f"starting {name}: {' '.join(command)}", flush=True)
            processes[name] = subprocess.Popen(
                command, cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT,
                env=os.environ.copy(),
            )
        pending = set(processes)
        while pending:
            sample = sampled_rss_bytes([processes[name].pid for name in pending])
            peak_sampled_rss = max(peak_sampled_rss, sum(sample.values()))
            for name in pending:
                peak_process_rss[name] = max(peak_process_rss[name], sample.get(processes[name].pid, 0))
            for name in tuple(pending):
                if processes[name].poll() is not None:
                    end_times[name] = time.perf_counter()
                    pending.remove(name)
                    if processes[name].returncode:
                        for other in pending:
                            processes[other].terminate()
                        raise RuntimeError(f"{name} failed with exit {processes[name].returncode}; see {output / (name + '.log')}")
            if pending:
                time.sleep(0.1)
        return {
            "group_wall_seconds": round(time.perf_counter() - started, 3),
            "peak_sampled_aggregate_child_rss_bytes": peak_sampled_rss,
            "rss_sample_interval_seconds": 0.1,
            "rss_scope": "sum of currently running stage process RSS; excludes GPU driver allocation, parent and ps sampler",
            "stages": {name: {
                "command": command,
                "wall_seconds": round(end_times[name] - start_times[name], 3),
                "peak_sampled_process_rss_bytes": peak_process_rss[name],
                "log": str(output / f"{name}.log"),
                "exit_code": processes[name].returncode,
            } for name, command in specs},
        }
    finally:
        for process in processes.values():
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        for stream in streams.values():
            stream.close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, default=Path("/Users/caved/Downloads/apocalypse/109"))
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--pages", nargs="*", help="page stems for a smoke run; omit for the full chapter")
    parser.add_argument("--sam-device", choices=("cpu", "mps", "cuda"), default="mps")
    parser.add_argument("--coo-device", choices=("cpu", "mps"), default="mps")
    parser.add_argument("--coo-min-score", type=float, default=0.4)
    parser.add_argument("--sequential", action="store_true", help="run RT and SAM serially for a controlled comparison")
    args = parser.parse_args()
    source_dir = args.source_dir.resolve(strict=True)
    if not source_dir.is_dir():
        parser.error("source must be a directory")
    output = args.output_dir.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error(f"output directory is not empty: {output}")
    if source_dir == output or source_dir in output.parents:
        parser.error("output must be outside the source directory")
    output.mkdir(parents=True, exist_ok=True)
    paths = sorted(p for p in source_dir.iterdir() if p.is_file() and p.suffix.lower() in {".jpg", ".jpeg", ".png"})
    if args.pages:
        wanted = set(args.pages)
        paths = [path for path in paths if path.stem in wanted]
        if {path.stem for path in paths} != wanted:
            parser.error("one or more requested page stems are missing")
    if not paths or len({p.stem for p in paths}) != len(paths):
        parser.error("source has no pages or duplicate page stems")
    if sha256(RT_MODEL) != RT_SHA256:
        parser.error("full RT-DETR checkpoint SHA-256 mismatch")
    for executable in (RT_PYTHON, SAM_PYTHON, COO_PYTHON):
        if not executable.is_file():
            parser.error(f"missing Python environment: {executable}")

    input_dir = source_dir
    if args.pages:
        input_dir = output / "input_pages"
        input_dir.mkdir()
        for path in paths:
            (input_dir / path.name).symlink_to(path)
    source_hashes = {path.name: sha256(path) for path in paths}
    rt_out, sam_out, coo_out, fusion_out = (
        output / "rt.json", output / "sam", output / "coo", output / "fusion"
    )
    rt_command = [str(RT_PYTHON), str(ROOT / "spikes/chapter-rtdetr/compare_variants.py"),
                  "--source", str(input_dir), "--model", str(RT_MODEL), "--model-id", "detector-fp32",
                  "--layout", "halves", "--output", str(rt_out), "--threads", "4"]
    sam_command = [str(SAM_PYTHON), str(ROOT / "spikes/sam-ts-l/run_chapter_torch.py"),
                   "--source-dir", str(input_dir), "--output-dir", str(sam_out),
                   "--device", args.sam_device]
    coo_command = [str(COO_PYTHON), str(ROOT / "spikes/coo-mtsv3/probe.py"),
                   "--source-dir", str(input_dir), "--output-dir", str(coo_out),
                   "--device", args.coo_device, "--preprocess", "eval_pil"]
    fusion_command = [sys.executable, str(ROOT / "spikes/chapter-combo/fuse.py"),
                      "--rt-json", str(rt_out), "--coo-dir", str(coo_out),
                      "--sam-dir", str(sam_out), "--output-dir", str(fusion_out),
                      "--coo-min-score", str(args.coo_min_score), "--write-labels"]
    record = {
        "description": "One-command three-model chapter inference and source-coordinate fusion",
        "status": "running", "source_dir": str(source_dir),
        "input_dir": str(input_dir), "pages": source_hashes,
        "model_sha256": {"rt": RT_SHA256, "coo": COO_SHA256, "sam": SAM_SHA256},
        "execution": "RT CPU and SAM run concurrently; COO MPS follows" if not args.sequential
                     else "RT, SAM and COO run sequentially",
        "groups": [],
    }
    started = time.perf_counter()
    try:
        if args.sequential:
            record["groups"].append(run_group(output, [("rt", rt_command)]))
            record["groups"].append(run_group(output, [("sam", sam_command)]))
        else:
            record["groups"].append(run_group(output, [("rt", rt_command), ("sam", sam_command)]))
        record["groups"].append(run_group(output, [("coo", coo_command)]))
        record["groups"].append(run_group(output, [("fusion", fusion_command)]))
        rt = json.loads(rt_out.read_text())
        coo = json.loads((coo_out / "summary.json").read_text())
        sam = json.loads((sam_out / "summary.json").read_text())
        fusion = json.loads((fusion_out / "summary.json").read_text())
        if (len(rt["pages"]) != len(paths) or len(coo["pages"]) != len(paths)
                or sam["completed_pages"] != len(paths) or len(fusion["pages"]) != len(paths)):
            raise ValueError("one stage did not process every requested page")
        if (rt["model_sha256"] != RT_SHA256 or coo["checkpoint_sha256"] != COO_SHA256
                or sam["model"]["checkpoint_sha256"] != SAM_SHA256):
            raise ValueError("stage model identity mismatch")
        record["artifact_sha256"] = {
            "rt": sha256(rt_out), "coo_summary": sha256(coo_out / "summary.json"),
            "coo_manifest": sha256(coo_out / "manifest.json"),
            "sam_summary": sha256(sam_out / "summary.json"),
            "fusion_summary": sha256(fusion_out / "summary.json"),
        }
        record["sam_peak_process_rss_bytes"] = sam.get("peak_process_rss_bytes")
        record["sam_mps_sampled_max_bytes"] = sam.get("mps_sampled_max_bytes")
        record["coo_peak_process_rss_bytes"] = coo["summary"].get("max_rss_bytes")
        record["coo_mps_sampled_max_bytes"] = coo["summary"].get("mps_sampled_max_bytes")
        record["fusion_totals"] = fusion.get("totals")
        record["status"] = "complete"
    except Exception as error:
        record["status"] = "failed"
        record["error"] = str(error)
        raise
    finally:
        record["whole_run_seconds"] = round(time.perf_counter() - started, 3)
        (output / "run.json").write_text(json.dumps(record, indent=2) + "\n")
        print(f"{record['status']}: {output / 'run.json'}", flush=True)


if __name__ == "__main__":
    main()
