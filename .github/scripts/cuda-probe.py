"""Set up and run the CUDA probe (`.github/workflows/probe-cuda.yml`).

The runner has no NVIDIA card. What it can have is everything else a user's
machine has: CUDA 12 and CUDA 13 installed side by side by NVIDIA's own
installers, a cuDNN 9 for each, and the app's CUDA builds of ONNX Runtime laid
out the way the app unpacks them. A stand-in `nvidia-smi` answers for the card.

    setup   cuDNN for both majors, both runtime builds, a one-node model, the
            stand-in nvidia-smi. The toolkits are installed by the workflow
            before this runs.
    probe   one test process per build (`accel::tests::
            a_cuda_build_loads_the_toolkit_and_cudnn_of_its_own_major`), with
            the *other* major's directories first on the loader path: the order
            that broke the CUDA 12 build for users with both installed.

Paths come from the environment the workflow sets: RUNNER_TEMP for the work
directory, and the toolkit roots below.
"""

import json
import os
import shutil
import subprocess
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path

WINDOWS = sys.platform == "win32"
WORK = Path(os.environ.get("RUNNER_TEMP", "/tmp")) / "cuda-probe"

ORT_VERSION = "1.28.0"
CUDNN_VERSION = "9.14.0"
CUDNN_REDIST = "https://developer.download.nvidia.com/compute/cudnn/redist/"
# The card the stand-in reports. Its driver is above both builds' floors
# (525 for CUDA 12, 580 for CUDA 13), so the verdict turns on the install.
STAND_IN = "NVIDIA GeForce RTX 4070 (CI stand-in), 581.42"

# Where each major's toolkit and cuDNN end up.
if WINDOWS:
    TOOLKIT_ROOT = Path(r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA")
    CUDNN_ROOT = Path(r"C:\Program Files\NVIDIA\CUDNN") / f"v{'.'.join(CUDNN_VERSION.split('.')[:2])}" / "bin"
    BUILDS = {
        12: {"toolkit": TOOLKIT_ROOT / "v12.6", "cudnn": CUDNN_ROOT / "12.9"},
        13: {"toolkit": TOOLKIT_ROOT / "v13.0", "cudnn": CUDNN_ROOT / "13.0"},
    }
else:
    BUILDS = {
        12: {"toolkit": Path("/usr/local/cuda-12.6"), "cudnn": Path("/usr/local/cuda-12.6/targets/x86_64-linux/lib")},
        13: {"toolkit": Path("/usr/local/cuda-13.0"), "cudnn": Path("/usr/local/cuda-13.0/targets/x86_64-linux/lib")},
    }


def fetch(url: str, to: Path) -> Path:
    to.parent.mkdir(parents=True, exist_ok=True)
    if not to.exists():
        print(f"fetching {url}", flush=True)
        with urllib.request.urlopen(url) as response, open(to, "wb") as out:
            shutil.copyfileobj(response, out)
    return to


def extract(archive: Path, into: Path) -> Path:
    """Unpack, and return the archive's single top-level directory."""
    if into.exists():
        shutil.rmtree(into)
    into.mkdir(parents=True)
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as zipped:
            zipped.extractall(into)
    else:
        with tarfile.open(archive) as tarred:
            tarred.extractall(into, filter="data")
    (top,) = list(into.iterdir())
    return top


def library_dirs(major: int) -> list[Path]:
    """The directories a major's libraries are in, for the loader path."""
    build = BUILDS[major]
    if WINDOWS:
        return [build["toolkit"] / "bin" / "x64", build["toolkit"] / "bin", build["cudnn"]]
    return [build["toolkit"] / "targets" / "x86_64-linux" / "lib", build["cudnn"]]


def setup_cudnn() -> None:
    """cuDNN 9 for both majors, where NVIDIA's own instructions put it.

    Windows: the directories the cuDNN exe installer writes
    (`CUDNN\\v9.14\\bin\\<cuda>`), filled from NVIDIA's redistributable zip
    because the exe has no unattended mode. Linux: the toolkit's own library
    directory, which is what the tar install instructions say to copy into.
    """
    manifest = json.loads(urllib.request.urlopen(f"{CUDNN_REDIST}redistrib_{CUDNN_VERSION}.json").read())
    platform = "windows-x86_64" if WINDOWS else "linux-x86_64"
    for major in BUILDS:
        relative = manifest["cudnn"][platform][f"cuda{major}"]["relative_path"]
        archive = fetch(CUDNN_REDIST + relative, WORK / "downloads" / Path(relative).name)
        top = extract(archive, WORK / f"cudnn-{major}")
        source = top / ("bin" if WINDOWS else "lib")
        target = BUILDS[major]["cudnn"]
        target.mkdir(parents=True, exist_ok=True)
        for item in source.iterdir():
            destination = target / item.name
            if item.is_symlink():
                if destination.exists() or destination.is_symlink():
                    destination.unlink()
                destination.symlink_to(os.readlink(item))
            elif item.is_file():
                shutil.copy2(item, destination)
        print(f"cuDNN {CUDNN_VERSION} for CUDA {major}: {target}", flush=True)


def runtime_dir(major: int) -> Path:
    # Named so the path says nothing of the build: the app unpacks every
    # build to the same `runtimes` directory, and the probe must find the
    # major the way the app does, from the install record.
    return WORK / ("rt-a" if major == 12 else "rt-b")


def setup_runtimes() -> None:
    """Both CUDA builds, flat in a directory with the app's install record."""
    base = f"https://github.com/microsoft/onnxruntime/releases/download/v{ORT_VERSION}/"
    for major in BUILDS:
        name = f"onnxruntime-{'win' if WINDOWS else 'linux'}-x64-gpu_cuda{major}-{ORT_VERSION}"
        archive = fetch(base + name + (".zip" if WINDOWS else ".tgz"), WORK / "downloads" / (name + (".zip" if WINDOWS else ".tgz")))
        top = extract(archive, WORK / f"ort-{major}")
        target = runtime_dir(major)
        if target.exists():
            shutil.rmtree(target)
        shutil.copytree(top / "lib", target, symlinks=True)
        record = {"flavour": f"cuda{major}", "version": ORT_VERSION, "files": sorted(p.name for p in target.iterdir())}
        (target / ".installed.json").write_text(json.dumps(record))
        print(f"runtime cuda{major}: {target}", flush=True)


def setup_model() -> None:
    """A one-node graph: a session on CUDA loads the provider and all it links."""
    import onnx
    from onnx import TensorProto, helper

    graph = helper.make_graph(
        [helper.make_node("Relu", ["x"], ["y"])],
        "probe",
        [helper.make_tensor_value_info("x", TensorProto.FLOAT, [1, 4])],
        [helper.make_tensor_value_info("y", TensorProto.FLOAT, [1, 4])],
    )
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])
    model.ir_version = 9
    onnx.checker.check_model(model)
    onnx.save(model, WORK / "probe.onnx")


def setup_stand_in() -> None:
    """`nvidia-smi` for the card the runner does not have.

    Built from Rust rather than written as a script: the app runs it through
    `std::process::Command`, which on Windows finds an `.exe` and never a
    `.cmd`.
    """
    source = WORK / "nvidia_smi.rs"
    source.write_text(f'fn main() {{ println!("{STAND_IN}"); }}\n')
    out = WORK / "fakegpu" / ("nvidia-smi.exe" if WINDOWS else "nvidia-smi")
    out.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["rustc", "-O", "-o", str(out), str(source)], check=True)
    with open(os.environ["GITHUB_PATH"], "a") as github_path:
        github_path.write(f"{out.parent}\n")


def report_layout() -> None:
    """What the installers actually wrote, for whoever reads a failed run."""
    for major, build in BUILDS.items():
        for root in {build["toolkit"], build["cudnn"]}:
            if not root.exists():
                print(f"CUDA {major}: {root} does not exist", flush=True)
                continue
            names = sorted(str(p.relative_to(root)) for p in root.rglob("*") if p.suffix in {".dll"} or ".so" in p.name)
            print(f"CUDA {major}: {root}\n  " + "\n  ".join(names[:80]), flush=True)


def probe() -> int:
    test = "accel::tests::a_cuda_build_loads_the_toolkit_and_cudnn_of_its_own_major"
    cargo = ["cargo", "test", "-p", "cleaner-core", "--lib", test, "--", "--ignored", "--exact", "--nocapture"]
    subprocess.run(cargo[:5] + ["--no-run"], check=True)
    loader = "PATH" if WINDOWS else "LD_LIBRARY_PATH"
    failed = []
    for major in BUILDS:
        other = 13 if major == 12 else 12
        env = dict(os.environ)
        # The other major first, then this one: discovery has to pick by
        # version, not by the order the loader would search.
        trap = [str(d) for d in library_dirs(other) + library_dirs(major)]
        env[loader] = os.pathsep.join(trap + ([env[loader]] if env.get(loader) else []))
        env["CUDA_PROBE_RUNTIME"] = str(runtime_dir(major) / ("onnxruntime.dll" if WINDOWS else "libonnxruntime.so"))
        env["CUDA_PROBE_MODEL"] = str(WORK / "probe.onnx")
        print(f"\n=== CUDA {major} build, CUDA {other} first on {loader} ===", flush=True)
        if subprocess.run(cargo, env=env).returncode != 0:
            failed.append(major)
    if failed:
        print(f"\nThe CUDA probe failed for the CUDA {', '.join(map(str, failed))} build.", flush=True)
        return 1
    print("\nBoth CUDA builds loaded the toolkit and cuDNN of their own major.", flush=True)
    return 0


if __name__ == "__main__":
    stage = sys.argv[1] if len(sys.argv) > 1 else ""
    if stage == "setup":
        WORK.mkdir(parents=True, exist_ok=True)
        setup_cudnn()
        setup_runtimes()
        setup_model()
        setup_stand_in()
        report_layout()
    elif stage == "probe":
        sys.exit(probe())
    else:
        sys.exit("usage: cuda-probe.py setup|probe")
