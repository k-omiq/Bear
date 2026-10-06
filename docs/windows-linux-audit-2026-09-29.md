# Windows and Linux support audit, 2026-09-29

## Scope and method

Question: what breaks, degrades, or behaves differently on a fresh Windows or
Linux machine, from download to export. The app has been developed and used
almost only on macOS (Apple Silicon, WKWebView).

Code read: commit `91edc3b` on `codex/cloud-integration`. Every `path:line`
below is from that commit. Nothing was built for or run on Windows or Linux:
cross-target `cargo check` on the Mac failed in C build scripts (`lcms2-sys`,
`ring`, `aws-lc-sys`) for want of a cross toolchain, so compile-level proof
comes from CI (`.github/workflows/ci.yml` runs check, clippy and test on
`windows-latest` and `ubuntu-22.04`, but only on `main` and pull requests).

Ten read-only agents split the app:

| Part | Agent |
| --- | --- |
| Windows install-to-export walkthrough | Opus |
| Linux install-to-export walkthrough | Opus |
| ONNX Runtime, execution providers, GPU and memory, downloads | Codex |
| Filesystem, paths, persistence, export | Codex |
| Svelte frontend on WebView2 and WebKitGTK | Sonnet |
| Cloud path, sidecar spawn, credentials, TLS, Python provisioner | Sonnet |
| cleaner-core pipeline portability | Gemini (agy) |
| Inventory of every `cfg(target_os/unix/windows)` gate | Gemini (agy) |
| Docs, README, dev scripts | Gemini (agy) |
| Test and CI coverage | Gemini (agy) |

The top claims were re-checked by hand against the code. Claims found wrong
are listed at the end.

Earlier evidence: `docs/windows-linux-install-validation-2026-09-26.md`. CI
runners built and inspected the NSIS, deb and AppImage packages, silently
installed and uninstalled the Windows installer, and installed the deb in an
emulated Ubuntu 22.04 container. No run has opened the app window, loaded ONNX
Runtime, or run a model on Windows or Linux.

Severity: BLOCKER means the user cannot install, launch, or finish import,
detect, clean, export. MAJOR means a feature breaks, data can be lost, or a
security guarantee is missing. MINOR means degraded, cosmetic, or an edge case.
Proof: `code` (traced in code), `inferred` (platform knowledge plus code),
`real-machine` (only a real machine can tell).

## Blockers

1. **Linux, NVIDIA: blank window.** Nothing sets `WEBKIT_DISABLE_DMABUF_RENDERER`
   before `run()` (`src-tauri/src/main.rs:4`; grep over `src-tauri`, `scripts`,
   `.github` is empty). WebKitGTK's DMABUF renderer gives a white window on the
   NVIDIA proprietary driver, worst on Wayland. Fix: set it in `main()` on
   Linux when unset. Proof: inferred, real-machine.
2. **Linux, Ubuntu 24.04, Fedora, Arch: AppImage does not start.** Tauri's
   AppImage still needs FUSE2 (`libfuse.so.2`), absent by default on Ubuntu
   24.04 and Arch. Bundle targets are `deb` and `appimage` only
   (`src-tauri/tauri.conf.json:36`), so Fedora and Arch users have nothing
   else. Fix: add `rpm`, document `libfuse2t64` / `fuse2` and
   `--appimage-extract-and-run`, or repack with a static runtime. Proof:
   inferred, real-machine.
3. **Windows 11 with Smart App Control on: installer blocked.** Authenticode
   signing runs only when `WINDOWS_CERTIFICATE` is set
   (`.github/workflows/release.yml:222`); today the installer, the app, and
   both sidecars (`manga-cleaner-provisioner.exe`, a PyInstaller one-file, and
   `manga-cleaner-uv.exe`) ship unsigned. Smart App Control blocks unsigned
   apps with no bypass; elsewhere SmartScreen warns, and Defender often
   quarantines unsigned PyInstaller bootloaders. Fix: OV or EV certificate, or
   Azure Trusted Signing, and confirm the sidecars get signed. Proof: inferred.
4. **Branch CI: Windows `cargo test` will not compile.** Two files new on this
   branch use Unix-only APIs in tests without a gate:
   `src-tauri/src/mask_stages.rs:838` (`std::os::unix::fs::symlink`) and
   `src-tauri/src/underlay.rs:1283` (`libc::getrusage`; `libc` is a
   `cfg(unix)` dependency). CI has never run on this branch. Fix: gate both
   with `#[cfg(unix)]`, then run CI on the branch. Proof: code. Fixed the same
   day: the three symlink cases in `mask_stages.rs` and the `getrusage` read
   in `underlay.rs` are gated; nothing has compiled them for Windows yet.

Checked since, not a blocker on current WebKitGTK: **tile images in the
editor.** `src/lib/api/boundedimage.js:131` loads layers and masks with
`fetch('tile://localhost/...')` from origin `tauri://localhost`, and wry
registers the scheme as secure but not CORS-enabled. The launch check below
ran the app in an Ubuntu 22.04 arm64 container under Xvfb with WebKitGTK
2.50.4 (the current 22.04 package): the fetch was answered (HTTP 404 for a
chapter that does not exist), not refused. Still unchecked: WebKitGTK 2.36, what
an Ubuntu 22.04 install without updates has.

## Major

### Windows

- **Console windows.** A GUI app (`windows_subsystem = "windows"`) spawns
  console programs without `CREATE_NO_WINDOW`; only
  `src-tauri/src/provision.rs:619` sets it. Missing at
  `crates/cleaner-core/src/sidecar/client.rs:398` (FLUX sidecar, long-lived:
  closing its window kills the engine), `src-tauri/src/flux_install.rs:41,62,84,111`,
  `src-tauri/src/model_workflows.rs:794,798,802`, and
  `crates/cleaner-core/src/accel.rs:542` (`nvidia-smi`). Fix: one
  `no_console(&mut Command)` helper at every spawn site. Proof: code.
- **FLUX sidecar exits at start.** `sidecar/manga_cleaner_sidecar/memory.py:38`
  imports the Unix-only `resource` module at top level; `server.py:21` and
  `self_test.py:24` import it. The bootstrap import check
  (`sidecar/bootstrap.py:113`) misses it. Fix: guard the import, read Windows
  process memory another way. Proof: code.
- **Uninstaller can delete every project.** The library, models, runtimes and
  Python envs live under `app_data_dir` = `%APPDATA%\com.mangacleaner.studio`
  (`src-tauri/src/library.rs:1473`, `src-tauri/src/weights.rs:428-438`). The
  Tauri NSIS "Delete the application data" checkbox removes that folder with no
  warning. Fix: keep the library outside it, or add an NSIS pre-uninstall hook
  that warns or keeps `library/`. Proof: code.
- **Cloud cancel leaves the helper running.** `kill_helper`
  (`src-tauri/src/provision.rs:529-545`) kills the process tree only on Unix.
  On Windows `child.kill()` hits the PyInstaller bootloader; the real Python
  child keeps deploying and downloading weights in the user's Modal account, and
  keeps the exe locked for updates. Fix: Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Proof: code plus platform knowledge.
- **Rename-publish has no retry.** Atomic writes rename once
  (`crates/cleaner-core/src/project/buffers.rs:343`, also
  `provisioner/journal.py:630`, `src-tauri/src/inference/journal.rs:387`).
  Defender, the indexer, or an image viewer holding a file without delete
  sharing makes the rename fail. Fix: bounded retry on sharing and access
  errors. Proof: inferred.

### Linux

- **deb users: update fails every time.** `latest.json` gets one entry per
  platform directory (`.github/workflows/release.yml:480-505`); on Linux that
  is the AppImage. The updater downloads it for deb installs and rejects it.
  Fix: publish `linux-x86_64-deb` plus the deb signature, or hide the updater
  for deb installs. Proof: code.
- **FLUX install fails on stock Ubuntu.** `system_python()`
  (`src-tauri/src/flux_install.rs:30-52`) accepts `python3` by version only;
  `sidecar/bootstrap.py:92` then runs `python -m venv`, which needs the separate
  `python3-venv` package. stderr is discarded, so the user sees only "FLUX setup
  failed". Fix: probe `import venv, ensurepip`, else use the bundled uv Python.
  Proof: code plus platform knowledge.
- **SAM install fails without system CUDA.** `spikes/sam-ts-l/environment.txt`
  is a macOS `pip freeze`, installed with `uv pip sync`
  (`src-tauri/src/model_workflows.rs:798`). On Linux x86_64 PyPI torch is the
  CUDA build and needs `nvidia-*` wheels that are not listed. Fix: per-platform
  lock, Linux torch from the CPU index. Proof: inferred.
- **No Secret Service: cloud setup fails after it has billed.** The Linux
  keyring default is Secret Service only; `linux-native` is compiled but never
  used, contrary to the comments at `src-tauri/Cargo.toml:111-115` and
  `src-tauri/src/weights.rs:52-55`. The runtime credential store at
  `src-tauri/src/provision.rs:958` fails with `ERR_SECRET_STORE` after the
  30-minute deploy. Fix: probe the store before `apply`, fall back to
  session-only or keyutils, fix the comments. Proof: code.
- **Locked keyring can hang the UI.** `has_secret` on non-macOS does a full read
  (`src-tauri/src/inference/secrets.rs:344`), which waits on the unlock prompt
  with no timeout; `list_models` is a sync command on the main thread
  (`src-tauri/src/weights.rs:1052`). Fix: bounded wait, async command, no memo
  lock across the read. Proof: inferred.
- **Cloud health check TLS on Fedora and Arch.** `provisioner/endpoint.py:136-155`
  uses the stdlib default TLS context of the libssl bundled from the Ubuntu
  runner, whose CA path does not exist there; the error is swallowed and setup
  ends in a timeout. Modal sign-in itself uses certifi and is not affected. Fix:
  `ssl.create_default_context(cafile=certifi.where())`. Proof: inferred.
- **`/tmp` mounted noexec breaks the cloud helper.** PyInstaller `--onefile`
  (`.github/scripts/build-cloud-provisioner.py:140`) unpacks and dlopens
  libpython under `$TMPDIR`. Fix: set the helper's `TMPDIR` under app data, or
  freeze `--onedir`. Proof: inferred.

### Both

- **FLUX install writes into the install directory.** `sidecar/bootstrap.py:112`
  runs `pip install` on the bundled source, which builds in place. That fails in
  a read-only AppImage and a root-owned deb, leaves `build/` in the Windows
  install folder, and writes into the signed `.app` on macOS. Fix: ship a wheel
  or copy the source to app data first. Proof: inferred.
- **GPU provider can fail silently.** None of the provider builders call
  `error_on_failure()` (`crates/cleaner-core/src/accel.rs:408`, `:1108`), so a
  failed CUDA or DirectML registration runs on CPU while reported as GPU. Proof:
  code.
- **GPU memory is not budgeted.** The budget reads host RAM only
  (`crates/cleaner-core/src/memory.rs:311`); GPU out-of-memory during LaMa
  inference propagates (`crates/cleaner-core/src/engines/lama.rs:459`) with no
  CPU retry. macOS unified memory hid this. Proof: inferred.
- **Proxies.** The cloud client is built with `.no_proxy()` and a local DNS pin
  (`src-tauri/src/inference/http.rs:881`, `:396-432`); reqwest has
  `default-features = false`, so no Windows system proxy for cloud, model
  downloads, or the updater. `198.18.0.0/15` (Clash and similar fake-ip TUN
  modes) and NAT64 are rejected as non-public (`http.rs:297`, `:325-352`).
  Proof: code.
- **Data safety, all OS, more likely on Windows.**
  - Any source read error counts as `Missing`, and resume then drops patches
    and revisions (`crates/cleaner-core/src/project/mod.rs:2745`,
    `src-tauri/src/run.rs:6335`). A sharing violation can erase saved edits.
  - Export names drop the source extension without a collision check
    (`src-tauri/src/exporting.rs:597`, PSD `:627`): `001.png` and `001.tiff`
    overwrite each other.
  - Denoise can write over the original scans: only "absolute" is checked
    (`src-tauri/src/page_denoise.rs:216`).
  - Export truncates the old file before encoding succeeds
    (`src-tauri/src/exporting.rs:663`, CBZ `:689`, stitched `:757`).
  - A failed project lock logs and continues without protection
    (`crates/cleaner-core/src/project/lock.rs:309`).
  - The settings file holding a fallback token is published before it is
    restricted (`src-tauri/src/settings.rs:282`), so it is briefly readable with
    default permissions.
- **Nothing in CI proves the app runs.** No job launches the app on any OS; the
  Windows install smoke runs only on `validate_only`
  (`.github/workflows/release.yml:310`); Linux packages are only unpacked
  (`:354`). ONNX Runtime loading and inference never run in CI; the loader tests
  hard-code the macOS runtime path (`crates/cleaner-core/src/runtime/mod.rs:488`,
  `crates/cleaner-core/src/accel.rs:1634`). Frontend and Python tests run on
  Ubuntu only; `sidecar/tests` never runs. The Windows helper spawn, kill, and
  DACL tests are all `#[cfg(unix)]` (`src-tauri/src/provision.rs:1581-2098`,
  `src-tauri/src/settings.rs:594`). Proof: code. The launch check (below)
  now opens the app and loads the runtime on Windows and Linux release builds;
  inference and the other gaps here remain.

### Docs

- `README.md` has no Linux requirements (`libwebkit2gtk-4.1-0`, `libfuse2` for
  the AppImage, a Secret Service for cloud), and says WebView2 ships with
  Windows 10 from 1803 (also `.github/workflows/release.yml:83`), which is
  false for some Windows 10 installs. It lists Windows arm64, which is not
  built (`README.md:77`).

## Minor

Windows:
- The window opens at 1440x900 with no `center` or clamp
  (`src-tauri/tauri.conf.json:17`); on 1366x768 or 1920x1080 at 150% it runs
  off-screen.
- Models, runtimes, and multi-GB Python envs sit in Roaming AppData.
- Uninstall leaves uv, pip, and Hugging Face caches plus Credential Manager
  entries behind.
- Startup failure with no console exits silently, with no log file
  (`src-tauri/src/lib.rs:271`).
- The updater exits without stopping helpers (`src-tauri/src/lib.rs:145`).
- The FLUX watchdog `os.kill(pid, 0)` is not a liveness probe on Windows
  (`sidecar/manga_cleaner_sidecar/watchdog.py:32`), so the sidecar outlives an
  app crash.
- Journal index lock contention returns at once instead of waiting 5 s
  (`src-tauri/src/inference/journal.rs:1246`).
- No `.gitattributes`: a CRLF checkout on the Windows runner bakes a different
  cloud code digest (`src-tauri/src/cloud_code_digest.rs:45`), so the app always
  reports "Update available".
- The data-folder guard folds case on macOS only
  (`src-tauri/src/mask_stages.rs:87`).
- There is no directory fsync (`src-tauri/src/inference/journal.rs:327`) and no
  symlink guard on stage files (`mask_stages.rs:281`).
- Classic 17 px scrollbars with `scrollbar-gutter: stable` shift dialogs.
- The password reveal eye (`::-ms-reveal`) shows.
- The EyeDropper button appears for the first time; it is untested.
- Chromium accelerators (Ctrl+F, Ctrl+P, F7, F3) are not all blocked
  (`src/lib/shell/reload-guard.js:4-9`).

Linux:
- The tray build can panic when no appindicator library loads
  (`src-tauri/src/lib.rs:113`).
- Close-to-tray on GNOME without the AppIndicator extension hides the window
  with no way back.
- The AppImage forces `GDK_BACKEND=x11`.
- There is no `bundle.category` and no WebKitGTK version floor.
- The CUDA library search misses `/opt/cuda` and `ld.so.cache`
  (`crates/cleaner-core/src/accel.rs:567`).
- `RTLD_LOCAL` is hard-coded to the Darwin value
  (`crates/cleaner-core/src/runtime/mod.rs:396`).
- Alt+click, the default clone source, is grabbed by KDE and XFCE.
- An IME Enter can submit half-typed Japanese in dialogs on WebKitGTK.
- There is no CJK font fallback beyond `Noto Sans JP`.
- Non-UTF-8 filenames are skipped as junk
  (`crates/cleaner-core/src/ingest.rs:167`).
- Uninstall leaves config, data, and a plaintext token behind when there is no
  Secret Service.

Both:
- The new-project name is taken from `path.split('/')`
  (`src/lib/home/dialogs/NewProjectDialog.svelte:34`), so a Windows path
  becomes the whole name.
- The export placeholder shows a macOS path (`src/lib/i18n/en.js:972`).
- UI copy says "keychain" (`src/lib/i18n/en.js:226` and others).
- Tooltips are hard-coded to `⇧` (`src/lib/editor/HistoryControls.svelte:45`).
- There is no Ctrl+Z / Ctrl+Y undo.
- Holding `O` sticks after Alt+Tab.
- Right-click is blocked in text inputs (`src/lib/shell/reload-guard.js:13`).
- Ctrl+wheel zoom jumps 25 per cent per notch.
- The paint canvas has no size cap (`src/lib/editor/PaintLayer.svelte:151`).
- CBZ entries lack the UTF-8 name flag
  (`crates/cleaner-core/src/export/cbz.rs:119`).
- Library manifests store native separators
  (`crates/cleaner-core/src/project/mod.rs:876`), so a library moved between
  Windows and Linux breaks.
- Developer paths (`/Users/caved/...`) ship in `pub mod` probe modules
  (`src-tauri/src/live_demo.rs:47`, `src-tauri/src/cloud_attempt_probe.rs:30`,
  others).

Developer setup:
- `scripts/*.sh` need `shasum` and call `xattr`.
- `scripts/release.sh` rejects Linux.
- `scripts/make-fixture-pages.py` hard-codes macOS fonts.
- The README omits `stage-flux-python.py` and `stage-vc-runtime.ps1`, and does
  not say that `.sh` scripts need Git Bash on Windows.

By design, not bugs: SAM WebGPU and qualified writes are macOS only
(`crates/cleaner-core/src/sam_ts.rs:277`); mflux is Apple Silicon only; ROCm and
OpenVINO ship no package.

## Already handled

- Windows:
  - ONNX Runtime discovery per OS
    (`crates/cleaner-core/src/runtime/mod.rs:51`, `:141`).
  - `SetDllDirectoryW` plus a DirectML preload (`:306`).
  - Missing-dependency classification (`:367`).
- Windows packaging and install:
  - The VC++ CRT is staged beside the exe (`src-tauri/tauri.conf.json:52`).
  - Per-user NSIS install, with no admin needed (`tauri.conf.json:68-70`).
- Downloads:
  - Pinned URLs and SHA-256 for every Windows and Linux runtime
    (`crates/cleaner-core/src/runtime/package.rs:289-484`).
  - Quota-aware free-space preflight (`src-tauri/src/weights.rs:539-580`).
  - In-use DLL replacement by rename-aside (`weights.rs:2948-3008`).
- RAM probes: `/proc/meminfo` plus PSI on Linux and `GlobalMemoryStatusEx` on
  Windows (`crates/cleaner-core/src/memory.rs:635-739`).
- Cross-process locks:
  - `flock` and `LockFile`
    (`crates/cleaner-core/src/project/lock.rs:330-414`).
  - Journal locks (`src-tauri/src/inference/journal.rs:278-314`).
- Settings file protection: `0600` on Unix and an owner-only DACL on Windows
  (`src-tauri/src/settings.rs:297-421`).
- Tile protocol origin:
  - The origin comes from `convertFileSrc` (`src/lib/api/tile.js:120-125`), so
    Windows gets `http://tile.localhost`.
  - The CSP lists both origins.
  - CORS headers are set on every tile response (`src-tauri/src/tile.rs:1202`).
- Shortcuts use `metaKey || ctrlKey` with per-platform keycaps
  (`src/lib/shortcuts.js:1194`, `:779`).
- The web features used are all in Chromium 105 and WebKitGTK 2.40.
- Cloud provisioner helper:
  - Helper naming and `.exe` suffix (`src-tauri/src/provision.rs:155-161`).
  - Environment allowlist (`:95-118`).
  - `CREATE_NO_WINDOW` for this helper only (`:614-620`).
  - UTF-8 stdio on both sides.
- Windows CI runs `cargo check`, `clippy -D warnings`, and `cargo test` on
  every pull request to `main`.

## Needs a real machine

Windows:
- Windows 11 clean install, Smart App Control in evaluation mode:
  - Download the installer and record the SmartScreen and Smart App Control
    verdicts.
  - Check whether Defender quarantines either sidecar.
- Windows 10 22H2 fresh, offline, no WebView2: record the installer result.
- Machine with no VC++ redistributable:
  - Install, download the DirectML runtime, and check that Diagnostics shows
    ONNX Runtime as available.
  - Run the installed helper `--self-check`.
- User named `山田`: import, detect, clean on CPU and on DirectML, export, then
  install FLUX and SAM.
- 1366x768 at 100 per cent and 1920x1080 at 150 per cent: does the first window
  fit the screen?
- FLUX: install, run, check the console windows, then kill the app from Task
  Manager and check the sidecar process.
- Cloud: apply, cancel, then check `tasklist` and `%TEMP%\_MEI*`.
- Uninstall with "Delete the application data" ticked: list what is lost.

Linux:
- Ubuntu 22.04 and 24.04 GNOME Wayland, Fedora 40, Arch, KDE Plasma 6:
  - Install the deb and run the AppImage, with and without `libfuse2`.
  - First launch.
  - Check the tray.
  - Confirm the editor shows cleaned patches and masks (the tile fetch above).
- NVIDIA proprietary driver: launch with and without
  `WEBKIT_DISABLE_DMABUF_RENDERER=1`.
- FLUX install with stock `python3` (no `python3-venv`).
- SAM install without the CUDA toolkit.
- No Secret Service (i3 or sway), and a locked GNOME keyring: save a Hugging
  Face token, run cloud apply, open Settings.
- `/tmp` mounted noexec: run the cloud helper.
- Fedora and Arch: cloud endpoint health check.
- deb in-app update: check the failure text.
- AppImage update in `~/Applications`.

Both:
- CUDA with the driver only, the wrong CUDA major, or no cuDNN: does the
  reported placement match the real one?
- Exhaust GPU memory during a clean: check recovery.
- System proxy, TLS-inspecting proxy, and Clash fake-ip: cloud health, model
  download, updater.

## Launch check in CI

Added after this audit so that CI opens the app on every release build.

**How the check runs.** Start the app with
`MANGA_CLEANER_SMOKE_REPORT=<file>` and it checks itself instead of starting
a session (`src-tauri/src/smoke.rs`, page half `src-tauri/src/smoke.js`).
- The page must mount and stay mounted.
- A `diagnostics` command must answer.
- The `tile` scheme must answer a cross-origin fetch.
- With `MANGA_CLEANER_SMOKE_RUNTIME=1`, the ONNX Runtime for the platform must
  download, verify and load. The report then also lists the accelerators it
  offers.
- The app writes a JSON report, holds the window 5 s for a screenshot, and
  exits: 0 when all checks passed, 1 when one failed, 3 when the page never
  reported.
- The single-instance plugin is skipped in this mode, so the check never hands
  itself to a running copy.

**Where it runs** (`.github/workflows/release.yml`):
- **Windows**, step "Launch smoke (Windows)": the NSIS installer installs
  silently to a path with CJK characters. The installed app is launched, and a
  failure fails the build.
- **Linux**, job `smoke-linux`: the release packages run in clean containers
  under Xvfb (`.github/scripts/launch-smoke-linux.sh`).
  - The `.deb` runs on Ubuntu 22.04 and 24.04. These legs gate publishing.
  - The AppImage runs on Ubuntu 24.04, Fedora 40 and Arch, through
    `APPIMAGE_EXTRACT_AND_RUN`. These legs report but do not gate yet.
- Every leg uploads its report and screenshot as an artifact.

**Checked locally** (2026-09-29): a debug build with `tauri/custom-protocol`,
in an Ubuntu 22.04 arm64 container under Xvfb, WebKitGTK 2.50.4.
- Every check passed: the page mounted, `diagnostics` answered, the tile fetch
  was answered, and the aarch64 runtime downloaded and loaded.
- The screenshot shows the first-launch screen.
- The Windows step and the CI Linux legs have not run yet.

**What the check does not prove:**
- A GPU. CI has none, so the NVIDIA blank window cannot show up there.
- A clean Windows image: the hosted runner has the VC++ redistributable and
  WebView2 installed.
- SmartScreen or Smart App Control.
- The AppImage without `APPIMAGE_EXTRACT_AND_RUN`.
- Detect, clean or export: they need the models.

## Claims checked and rejected

- "`std::fs::rename` fails on Windows when the target exists": wrong. Rust
  calls `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`. The real risk is
  sharing violations, kept above.
- "Release build fails because `build-cloud-provisioner.py` overwrites
  `externalBin`": the release workflow runs it (`release.yml:174`) before
  `stage-flux-python.py` (`:180`), which appends `uv`. Only the README dev steps
  miss the second script. Downgraded to a docs item.
- A float-digest test in `sam_ts.rs:860` that "may differ on x86_64": not
  confirmed, and CI on `main` has not reported it. Left out.
