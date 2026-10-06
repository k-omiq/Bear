# Manga Cleaner Sidecar (`manga_cleaner_sidecar`)

The Manga Cleaner sidecar is an out-of-process engine that executes FLUX.2 Klein 4B image inpainting (Rung 3a). It has **two rendering backends** and they do not share a dependency, a weights format or a platform:

| Backend | Runtime | Platforms | Requirements file |
|---|---|---|---|
| `mflux` | Apple MLX + `mflux` | macOS, Apple Silicon | `requirements.txt` |
| `sdnq` | `torch` + `diffusers` + `sdnq`, 4-bit | CUDA, Intel XPU, Metal - Windows, Linux, macOS | `requirements-sdnq.txt` |
| `sdcpp` | `stable-diffusion.cpp` / ggml | none - declared only, declines every open | - |

A virtual environment may hold either backend's dependencies or both. The sidecar reports which ones it can import on `GET /v1/health` (`"backends": ["mflux", "sdnq"]`), because the application finds an install by looking for a `pyvenv.cfg` and cannot see inside it; a backend that is not in that list is refused before any model load with `decline.reason.sidecarBackend`. Which backend is used is the **AI redraw engine** row in the application's Settings - `Automatic` (MLX on Apple Silicon, SDNQ elsewhere), `MLX (Apple)`, or `SDNQ (any GPU)`.

---

## 1. Architectural Purpose and Design

Manga Cleaner adopts a multi-rung cleaning ladder. Most text and sound-effect removal operations are handled quickly and deterministically by in-process local models (such as LaMa and Manga-OCR). However, complex artwork reconstruction across intricate textures, screentones, and multi-layered backgrounds benefits from generative reference-guided inpainting.

To satisfy the application's stability and memory boundaries:
1. **Optional installation**: The application bundles the helper source and a managed-Python installer, and downloads Python, dependencies and pinned diffusion weights when the user selects **Install FLUX helper and 4B model**. An existing compatible helper can also be selected by folder.
2. **Out-of-process isolation**: Generative diffusion models consume substantial memory and compute. By running in an independent process over loopback HTTP/1.1 (`127.0.0.1`), a runtime failure or memory spike in Python never crashes the desktop GUI application.
3. **Memory bounds, and what each one is actually worth** - `mflux`: memory limits (`mx.set_memory_limit`) and buffer cache ceilings (`mx.set_cache_limit`) are applied before model loading and are the two guards this backend echoes at `POST /v1/open`. Two more used to be here and both were withdrawn, each on its own measurement. **Text encoder eviction** (`MemorySaver`) made a *second* render through one opened model fail, because `mflux` cannot rebuild the encoder it nulled - so the callback is not registered, the encoder is kept, and the price is declared instead: **8.92 GB** of peak physical footprint across two renders and **9.77 GB** for the largest untiled shape, against a declaration of 10 GiB, where the evicting path peaked at 6.58 GB. **VAE spatial tiling** (`TilingConfig`) bounds a large crop's decode and seams a small one - `mflux` tiles the encode above 512 px and the decode above a 512 px output - so it is applied per render on the crop's working resolution and echoed at open by nobody. What is left is thinner than four guards sounded: `mx.set_memory_limit` is documented by MLX as *a guideline*, raising only once RAM and swap are both exhausted, and the real backstop is the parent's per-region peak check.
4. **Memory bounds on `sdnq`**: one setter and one behaviour, both applied at `POST /v1/open` and both echoed. `torch.cuda.set_per_process_memory_fraction` / `torch.mps.set_per_process_memory_fraction` (echoed as `memory_fraction`) caps what the caching allocator may **reserve** - live allocations and its own cache together - so rule 9's "cap the cache, do not only purge it" is met by one knob rather than two, and `empty_cache()` between renders is a sweep on top of a cap rather than instead of one. `diffusers`' `enable_model_cpu_offload` (echoed as `cpu_offload`) keeps one pipeline component on the accelerator at a time; `enable_sequential_cpu_offload` replaces it where the card cannot hold a component, selected automatically or by `MC_SIDECAR_LOW_VRAM=1`. **VAE tiling and text-encoder eviction are deliberately not echoed**: `diffusers` tiles below the 768 px working resolution small crops are upscaled into, which would seam exactly the shape this backend edits, and the encoder's eviction is what the offload hook already performs rather than a second promise. **A machine with no CUDA, XPU or Metal device is refused with `501 unbounded_backend`** rather than served: with no accelerator neither guard can be applied, and a backend that echoes a guard it did not apply is the failure this guards against.
5. **Lifecycle and watchdog**: The child process is tied to the parent's process identifier (`MC_SIDECAR_PARENT_PID`). A background watchdog terminates the sidecar immediately if the parent crashes or closes.

---

## 2. Prerequisites and Environment Setup

### Install from the application

Open **Settings → Cleaning → Install FLUX helper and 4B model**, or use the
same action during onboarding. The installer creates a private environment in
the application's data directory, downloads its pinned checkpoint, and selects
the resulting helper folder. A system Python or terminal is not required in a
packaged release. Interrupted downloads can be retried from the same action.

Automatic selection uses MLX on Apple Silicon, CUDA when NVIDIA's driver is
available, or Intel XPU when its runtime is detected. The settings also provide
an explicit GPU-runtime choice. GPU drivers remain a platform prerequisite;
CPU-only machines can use LaMa Manga locally or provision a cloud FLUX model.
Local installation currently supplies FLUX.2 Klein 4B. Cloud setup separately
offers FLUX.2 Klein 4B and 9B.

The following instructions are for a manually managed development environment.

Python 3.10, 3.11 or 3.12. The `mflux` backend additionally requires macOS on Apple Silicon; the `sdnq` backend requires a CUDA, Intel XPU or Metal device (it refuses a CPU-only machine - see §1 point 4).

### Step 1: Create a Dedicated Virtual Environment

Create a dedicated virtual environment in the project directory (defaulting to `.sidecar-venv`):

```bash
cd /Users/caved/dev/manga-cleaner
python3 -m venv .sidecar-venv
source .sidecar-venv/bin/activate
```

### Step 2: Install Dependencies and Sidecar Package

Install **one or both** backends' dependencies, plus the sidecar package itself.

**macOS, Apple Silicon - MLX backend (the platform default):**

```bash
pip install -r sidecar/requirements.txt
pip install -e sidecar
```

**macOS, Apple Silicon - SDNQ backend (Metal through torch):**

```bash
pip install -r sidecar/requirements-sdnq.txt
pip install -e sidecar
```

**Windows or Linux with an NVIDIA GPU - SDNQ backend.** Install the CUDA `torch`
wheel **first**: PyPI's default `torch` on Linux is CPU-only, and this backend
refuses a machine with no accelerator, so a default install would look complete
and then decline every region.

```bash
pip install torch --index-url https://download.pytorch.org/whl/cu124
pip install -r sidecar/requirements-sdnq.txt
pip install -e sidecar
```

**Windows or Linux with an Intel Arc GPU** - the same, with the XPU wheel index
in place of the CUDA one.

Both requirements files may be installed into one environment; the two backends
coexist and `GET /v1/health` then reports both.

---

## 3. Model Weights

**Weight format is chosen by the runtime, so the two backends do not share a download.**

| Backend | Repository | On disk |
|---|---|---|
| `mflux` | `mflux-community/flux2-klein-4b-mflux-q4` | 4.62 GB |
| `sdnq` | `Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic` | 5.48 GB |

Weights live under `<sidecar_root>/weights/<model_dir>/`, which the application's
model dropdown enumerates. The `sdnq` backend accepts either layout:

* a plain `diffusers` snapshot - a directory with `model_index.json` in it;
* a HuggingFace cache entry - `models--Org--Repo/snapshots/<sha>/`, which is what
  `huggingface-cli download` leaves and whose files are all symlinks into a
  sibling `blobs/`. The newest snapshot is used.

`MC_SIDECAR_WEIGHTS_DIR` overrides the lookup and may name either one specific
snapshot or a weights root to resolve the model id inside.
`MC_SIDECAR_WEIGHTS_ROOT` overrides only the root.

---

## 4. Running the Sidecar

### Automatic Spawn (Normal Operation)

When Manga Cleaner runs, the Rust core automatically discovers the virtual environment under `.sidecar-venv` or custom install paths, passes configuration through environment variables, and manages the sidecar lifecycle.

### Manual Standalone Execution

For debugging or developer inspection, the sidecar can be started manually:

```bash
source .sidecar-venv/bin/activate
export MC_SIDECAR_HOST="127.0.0.1"
export MC_SIDECAR_PORT="8080"
export MC_SIDECAR_TOKEN="0123456789abcdef0123456789abcdef0123456789abcdef"
python -m manga_cleaner_sidecar
```

### Self-Test Mode

The package provides a built-in end-to-end self-test that verifies the full wire protocol, health checks, model initialization, synthetic render pass, memory reporting, and clean shutdown:

```bash
python -m manga_cleaner_sidecar --self-test
```

---

## 5. Wire Protocol Summary

The sidecar speaks HTTP/1.1 over loopback with `Connection: close` and JSON payloads. All endpoints (except `/v1/health`) require authentication via the `x-mc-token` header matching the shared token.

- `GET /v1/health`: Non-blocking liveness probe reporting state (`idle`, `loading`, `ready`, `busy`), disk weights size, working set estimates, applied memory guards, **which backends this environment can import** (`backends`), and memory metrics.
- `POST /v1/open`: Applies allocator limits, validates the text encoder configuration, and loads the model into memory. On `mflux` it registers no memory saver and sets no tiling - see §1 point 3; on `sdnq` it caps the allocator fraction and installs the offload hook - see §1 point 4. The reply echoes what was actually applied, and an echo missing anything the caller required fails the open.
- `POST /v1/render`: Receives raw interleaved 8-bit image samples (base64-encoded) and inference parameters, performs inpainting, and returns the reconstructed image maintaining exact request geometry. **The working resolution is the sidecar's own**: a crop whose long side is under 768 px is upscaled to 768 (bicubic, snapped to /16) before the edit and returned by area average at exactly the geometry that was asked for, because a FLUX.2 edit resolves poorly on a hole that is small in its frame. The caller sends the page's own pixels at the page's own scale and never resamples.
- `POST /v1/release`: Frees model weights and clears memory caches, returning the process to the idle floor.
- `POST /v1/shutdown`: Gracefully terminates the sidecar server.

---

## 6. Tests

```bash
.sidecar-venv/bin/python -m unittest discover -s sidecar/tests -t sidecar/tests
```

Seventeen tests, `unittest` rather than pytest so no fourth dependency is needed.
Everything requiring `torch` skips where it is absent, so the suite is green in an
environment holding only the MLX backend's dependencies.
