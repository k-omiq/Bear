# Developer guide

How to set up, run, test, change and release Manga Cleaner. For how the pieces fit together, read [architecture.md](architecture.md) next.

Manga Cleaner is a Tauri 2 desktop app. The interface is Svelte 5 with Vite in [src/](../src/). The backend is Rust in [src-tauri/](../src-tauri/), on a pure-Rust core crate, [crates/cleaner-core/](../crates/cleaner-core/). ONNX Runtime loads dynamically and model weights are downloaded separately. The Python pieces are optional.

## 1. Prerequisites

Every platform needs:

- **Rust**: current stable. [Cargo.toml](../Cargo.toml) declares `rust-version = "1.82"`, and CI builds with stable.
- **Node.js**: 20.19 or newer. CI uses Node 20, and Vite 8 accepts 20.19 or 22.12 and up.
- **Tauri system dependencies**: see the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/).
- **bash, curl, tar, unzip and shasum**, which the scripts in [scripts/](../scripts/) use.
- **uv**. `npm run helpers` stops with "uv is missing" without it, because it stages `uv` as the managed Python installer.
- **Python 3.11 or newer** for the helper and the Python tests. `npm run helpers` builds a Python 3.12 virtual environment, through `uv` if present, otherwise it needs `python3.12` on your path.

**macOS, Apple Silicon.** Install the Xcode command line tools. **Intel Macs are unsupported**: ONNX Runtime 1.28 publishes no Intel archive, `scripts/fetch-runtime.sh` exits with an error on one, and the release workflow has no Intel target.

**Windows, x64.** Install the Microsoft C++ build tools and the `x86_64-pc-windows-msvc` Rust target. Use Git for Windows so the shell scripts run in git-bash. WebView2 is part of Windows 10 1803 and later. Windows on ARM is not shipped.

**Linux, x64.** CI installs these on Ubuntu 22.04 before it builds:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev libdbus-1-dev build-essential curl wget file patchelf
```

`libdbus-1-dev` is for the `keyring` crate. GPU use at run time needs the system Vulkan loader, `libvulkan.so.1`, which graphics driver packages install.

**Optional tools:** `gh` for `scripts/ship.sh`, `wrangler` (through `npx`) for [infra/](../infra/), `psd-tools` for `scripts/check-psd.py`, and Modal or Beam accounts to try the cloud path against a real provider.

## 2. First-time setup

Run these from the repository root, in order.

```bash
npm install
```

```bash
scripts/fetch-runtime.sh
```

This downloads the ONNX Runtime build for your platform, checks its SHA-256, and unpacks it into `runtimes/` (ignored by git). The assembled libraries land in `runtimes/lib`. The script mirrors [package.rs](../crates/cleaner-core/src/runtime/package.rs), which is the authority. On Windows and Linux x64, `ORT_FLAVOUR=cuda12` or `cuda13` selects the NVIDIA build. Read the trade-offs in the script header first.

The app does not look in `runtimes/`. It searches `ORT_DYLIB_PATH`, then the app-data `runtimes` folder, then the folders beside the executable. For `npx tauri dev`, point `ORT_DYLIB_PATH` at the library in `runtimes/lib`, or download the runtime from Settings.

```bash
scripts/fetch-models.sh
```

This downloads the pinned weights into `models/` (ignored by git) and verifies each SHA-256. It skips files that are already correct. Manga OCR (460 MB), Hayai OCR (600 MB) and the denoise models are optional. For a development run, set `MANGA_CLEANER_MODELS` to that folder, or download from Settings.

```bash
npm run helpers
```

This runs [update-helpers.sh](../scripts/update-helpers.sh). It creates `target/cloud-build-venv` with the hash-checked [requirements.lock](../provisioner/requirements.lock), freezes the cloud setup helper with PyInstaller when its stamp no longer matches (`--force` freezes anyway), and stages `uv` if missing. The outputs are `src-tauri/binaries/manga-cleaner-provisioner-<target-triple>`, its `.sha256` stamp, and `manga-cleaner-uv-<target-triple>`.

The helper is a frozen copy of [provisioner/](../provisioner/) and [deploy/](../deploy/), and release builds bundle it. [build.rs](../src-tauri/build.rs) hashes every frozen source against the stamp. A **release build panics if the stamp is missing or stale**. A development build only warns and runs the live Python (`python -m provisioner`). Run `npm run helpers` after any edit under `provisioner/` or `deploy/`. [cloud-provisioning.md](cloud-provisioning.md) has the detail. `npm run helpers:install` also copies changed helpers into an installed macOS app and re-signs it ad hoc.

**Optional: the local FLUX sidecar.** A packaged app installs it from Settings. For a hand-managed environment, follow [sidecar/README.md](../sidecar/README.md): create `.sidecar-venv`, install `sidecar/requirements.txt` (Apple MLX) or `sidecar/requirements-sdnq.txt` (CUDA, XPU or Metal), then `pip install -e sidecar`.

## 3. Running

### The full app

```bash
npx tauri dev
```

[tauri.conf.json](../src-tauri/tauri.conf.json) runs `npm run dev` first and points the window at `http://localhost:5173`. [vite.config.js](../vite.config.js) does not change the port, so free it before you start.

### The browser-only mock

```bash
npm run dev
```

Open the address Vite prints. Outside a Tauri window, `getBackend()` in [backend.js](../src/lib/api/backend.js) returns the mock in [mock.js](../src/lib/api/mock.js). It simulates projects, runs, cloud flows and models with fixture data and injected delays. Use it for interface work and for telling an interface bug from a backend bug. Page images fall back to placeholder art, because there is no `tile://` protocol.

- `globalThis.__MANGA_CLEANER_FORCE_MOCK__ = true`, set before the first backend call, forces the mock even inside a Tauri window.
- `__MANGA_CLEANER_MOCK_QUIT__({ canHide: true })` in the console raises the quit question a browser tab never gets.

The shipped app uses WKWebView on macOS, so check focus and observer behaviour there too.

### Environment variables

| Variable | Effect |
|---|---|
| `ORT_DYLIB_PATH` | The ONNX Runtime library to load. First in the search order. Read-only in Settings. |
| `MANGA_CLEANER_MODELS` | A folder of weights, searched first. Settings cannot delete weights found outside app data. |
| `MANGA_CLEANER_SIDECAR` | A folder that holds the local FLUX Python environment. |
| `MANGA_CLEANER_SAM_TS` | A folder holding the SAM-TS-L graphs. Never removed by the interface. |
| `MANGA_CLEANER_RT_FULL` | A path to the RT-DETR Full graph. |
| `MANGA_CLEANER_PROVISIONER_BIN` | Debug builds only. A helper executable to use instead of the bundled one. |
| `MANGA_CLEANER_JOURNAL_ROOT` | Debug builds only. Where the cloud setup journal is kept. |
| `MANGA_CLEANER_SMOKE_REPORT` | Run the launch check instead of a session, write a JSON report here, and exit. See [smoke.rs](../src-tauri/src/smoke.rs). |
| `MANGA_CLEANER_SMOKE_RUNTIME` | With the above, also download and load ONNX Runtime. |
| `MC_SIDECAR_*` | Set by the app when it starts the sidecar. Set `MC_SIDECAR_HOST`, `_PORT` and `_TOKEN` by hand only to run it standalone. `MC_SIDECAR_WEIGHTS_DIR` overrides the weights lookup. |
| `MC_CLOUD_CODE_DIGEST`, `MC_FROZEN_HELPER_FRESH`, `MC_TARGET_TRIPLE` | Set by `build.rs`. Do not set them. |

Many `#[ignore]` tests read variables of their own; each test's doc comment names them.

## 4. Testing

### Frontend

```bash
npm test
```

Vitest runs two projects, defined in [vite.config.js](../vite.config.js). The **node** project runs `src/**/*.test.js`, because most tests are arithmetic over plain objects. The **jsdom** project runs only files named `*.dom.test.js`, which mount components and resolve Svelte through its browser build. Name a test `*.dom.test.js` whenever it mounts a component. A pragma in the file is not enough.

The mock has tests in [src/lib/api/](../src/lib/api/), named `mock.test.js`, `mock.cloud.test.js`, `mock.detect.test.js`, `mock.gpu.test.js` and `mock.jobs.test.js`. Use `npm run test:watch` while iterating, or run one file:

```bash
npx vitest run src/lib/api/tauri.test.js
```

### Rust

```bash
cargo test --workspace --exclude spike-strip-memory -- --test-threads=1
```

One thread is required: concurrent WebGPU session initialisation on macOS can abort the whole test binary. The `src-tauri` app crate suite is slow, usually several minutes. While iterating, run one crate with a name filter:

```bash
cargo test -p cleaner-core text_groups
```

```bash
cargo test -p manga-cleaner --lib region
```

Tests that need the runtime or weights skip with a message when they are absent. On Apple Silicon, `scripts/fetch-runtime.sh` is what turns the runtime tests on. Tests marked `#[ignore]` need real data and run with `-- --ignored`. Lint as CI does:

```bash
cargo clippy -p cleaner-core -p manga-cleaner --no-deps --all-targets -- -D warnings
```

### Python

The suites use `unittest`; no pytest configuration exists. Run them from the repository root, as CI does:

```bash
python3 -m pip install numpy==2.4.6 pillow==12.3.0
```

```bash
python3 -m unittest discover -v -s deploy/cloud/tests
```

```bash
python3 -m unittest discover -v -s provisioner
```

```bash
python3 -m compileall deploy/cloud provisioner
```

Suites that need a provider SDK, `torch` or a GPU skip when those are missing. CI does not run the sidecar and SAM bootstrap suites:

```bash
.sidecar-venv/bin/python -m unittest discover -s sidecar/tests -t sidecar/tests
```

```bash
python3 -m unittest discover -s sam-bootstrap
```

### What CI runs

[ci.yml](../.github/workflows/ci.yml) runs on pushes to the default branch, pull requests and manual dispatch:

- **Frontend** (Node 20): `npm ci`, `npm test`, `npx vite build`.
- **Python** (3.11): the three Python commands above.
- **Rust, Linux** (Ubuntu 22.04): `cargo check`, strict clippy, and the one-thread `cargo test`.
- **Rust, Windows**: the same three steps for `x86_64-pc-windows-msvc`, plus a check that the Visual C++ runtime files are where the release build expects them.

CI downloads no runtime and no weights, so it does not show that detection or inpainting runs.

## 5. Project layout in brief

Detail is in [architecture.md](architecture.md).

- [src/](../src/): the interface. `lib/api` is the backend seam, the Tauri adapter and the mock. `lib/state` holds the rune stores, `lib/editor` the canvas and tools, `lib/dialogs` the dialogs, `lib/i18n` the catalogues.
- [src-tauri/](../src-tauri/): the host. IPC commands, the `tile://` image protocol, settings, downloads, cloud inference and the helper bridge. Permissions are in `permissions/` and `capabilities/`.
- [crates/cleaner-core/](../crates/cleaner-core/): detection, the script gate, mask fitting, inpainting engines, project files and export.
- [provisioner/](../provisioner/), [deploy/cloud/](../deploy/cloud/): the cloud setup helper, and the Modal and Beam workers and gateway.
- [sidecar/](../sidecar/), [sam-bootstrap/](../sam-bootstrap/): the optional local FLUX server, and the SAM-TS-L graph setup script.
- [infra/](../infra/): the Cloudflare download site and publish worker.
- [scripts/](../scripts/), [.github/](../.github/): setup, release and CI scripts.
- [docs/](../docs/): contracts and reference. [findings.md](findings.md) records what was measured.

## 6. Common changes

### Add an IPC command

1. Write the function under [src-tauri/src/](../src-tauri/src/) with `#[tauri::command]`.
2. Add it to the `tauri::generate_handler![...]` list in [lib.rs](../src-tauri/src/lib.rs).
3. Add its name to the `.commands(&[...])` list in [build.rs](../src-tauri/build.rs).
4. Add its name to exactly one `commands.allow` list in [cloud.toml](../src-tauri/permissions/cloud.toml): `allow-app-commands`, `allow-inference-commands` or `allow-provision-commands`. The window capabilities in [capabilities/](../src-tauri/capabilities/) already grant those three. A new permission needs an entry there.
5. In [tauri.js](../src/lib/api/tauri.js), add the command to `IMPLEMENTED`, the method to `SEAM_METHODS`, and a wrapper inside `createTauriBackend`. Describe the method in the `Backend` typedef in [backend.js](../src/lib/api/backend.js).
6. Add a counterpart to [mock.js](../src/lib/api/mock.js), so `npm run dev` still works.
7. Test it: an `invoke` stub test in `tauri.test.js` and a mock test in the matching `mock*.test.js`.

`tauri-acl.test.js` fails if steps 2, 3 and 4 disagree. `tauri.test.js` fails if a seam method is neither implemented nor delegated. Pixels never cross IPC; images go through `tile://`.

### Add a UI string

Every user-visible string goes through `t()` in [src/lib/i18n/](../src/lib/i18n/). Keys read `<namespace>.<category>.<camelCaseName>`. Add the key to [en.js](../src/lib/i18n/en.js), then to `ko.js`, `ja.js`, `es.js`, `pt.js` and `fr.js` in the same folder. Tests enforce these rules:

- Every locale has exactly the English keys, with the same `{placeholders}`.
- Every key is used by a shipped `.js`, `.svelte` or `.rs` file, and every key used exists. Keys built at run time are listed by hand in `catalogue.test.js`.
- No em-dash or en-dash in user-visible text.

A placeholder whose name ends in `Key` carries another i18n key. Never build a sentence by concatenation. Rust and the seam send keys and parameters, never English.

### Add or update a model

1. Add or change the row in the `MODELS` table in [weights.rs](../src-tauri/src/weights.rs): id, file name, URL, SHA-256 and exact size in bytes.
2. Edit the matching `fetch <name> <sha256> <url>` block in [fetch-models.sh](../scripts/fetch-models.sh). Get the digest from `shasum -a 256`, and pin Hugging Face URLs to a commit hash, as the existing entries do.
3. Add the file-name constant in [run.rs](../src-tauri/src/run.rs) if the pipeline opens the file.
4. Add the licence row to the model table in [README.md](../README.md).

Edit the script and the table together. The test `the_script_and_the_table_agree` parses the script and fails on drift. The runtime table has the same relationship with `fetch-runtime.sh`.

### Change cloud worker code

1. Edit [deploy/cloud/](../deploy/cloud/) and run the `deploy/cloud` and `provisioner` test suites.
2. Run `npm run helpers`. The frozen helper carries the deploy tree, and `build.rs` rejects a stale one in a release build.
3. Rebuild the app. `build.rs` bakes a digest of the shipped cloud files (`cloud/common/*.py`, `cloud/modal/*.py` and the package inits) into `MC_CLOUD_CODE_DIGEST`. Settings compares a deployment with it and asks for an update when they differ. The Beam files are outside the digest.
4. `npm run helpers:install` refuses an installed app built from other cloud code. Rebuild the app instead.

To change which files are shipped, change both `deploy/cloud/common/release.py` and [cloud_code_digest.rs](../src-tauri/src/cloud_code_digest.rs). `test_release.py` and a Rust test check that they agree.

### Add a setting

A setting has up to two homes.

- **The interface session**, in [session.svelte.js](../src/lib/state/session.svelte.js). Add the field to the typedef and `defaults()`, validate it in `sanitizeSession()`, add it to the persisted record, and add a setter. It is stored in `localStorage` under `session.v1`.
- **The backend file**, `settings.json`, only when Rust reads the value. Add the key to `backendSettingsPatch()` in the same file. The merge is shallow, and Rust stores the JSON without knowing what it means. Keys that look like tokens or secrets are refused: cloud credentials belong in the keyring and `inference.json`. Add a default for the mock in [fixtures.js](../src/lib/api/fixtures.js).

After the first run the session wins and the backend is made to agree (`reconcileSettings()`). Add the control to [SettingsDialog.svelte](../src/lib/dialogs/SettingsDialog.svelte), the strings to every locale, and tests beside the existing `session-*.test.js` and `SettingsDialog*.dom.test.js` files.

## 7. Building a release

### A local build

```bash
python -m pip install --require-hashes -r provisioner/requirements.lock
```

```bash
python .github/scripts/build-cloud-provisioner.py aarch64-apple-darwin
```

```bash
python -m pip install uv==0.12.19
```

```bash
python .github/scripts/stage-flux-python.py aarch64-apple-darwin
```

```bash
npx tauri build --target aarch64-apple-darwin
```

The targets are `aarch64-apple-darwin`, `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu`. Freeze on the matching OS and CPU: the script refuses otherwise. The freeze builds with PyInstaller and smoke tests the binary offline (a self check, missing credentials, unreachable credentials). `--online` adds a real Modal sign-in with an unknown token.

- Both the freeze and the `uv` staging **edit** [tauri.conf.json](../src-tauri/tauri.conf.json) to add `bundle.externalBin`. The committed file leaves it out so `cargo check` and `tauri dev` work without generated binaries. Revert those edits before committing; `scripts/ship.sh` refuses a dirty tree. `npm run helpers` passes `--no-config` and leaves the file alone.
- A release build panics unless `externalBin` names both the provisioner and `uv`, both binaries exist for the target, and the provisioner stamp is fresh.
- Update bundles are signed, so set `TAURI_SIGNING_PRIVATE_KEY` (a path or the key content) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The updater public key in `tauri.conf.json` must match.
- On Windows, run `.github/scripts/stage-vc-runtime.ps1 -Destination src-tauri/vendor/windows-crt` first. Without the Visual C++ DLLs, the downloaded ONNX Runtime fails to load with Windows error 126.
- Bundles appear in `target/<target>/release/bundle/` (`macos` and `dmg`, `nsis`, or `deb` and `appimage`).

### Signing

- **macOS.** The release workflow signs with a certificate only if `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD` and `APPLE_SIGNING_IDENTITY` are all set. With none set, the app is ad-hoc signed. A partial set fails the build. An ad-hoc signature changes with every build, so the keychain asks again for saved cloud keys after each update. [entitlements.plist](../src-tauri/entitlements.plist) turns off library validation, which the downloaded ONNX Runtime needs. The workflow has no notarisation step.
- **Windows.** Authenticode runs only when `WINDOWS_CERTIFICATE` and `WINDOWS_CERTIFICATE_PASSWORD` are both set. The workflow patches the thumbprint into the config for that build only. The repository variable `WINDOWS_TIMESTAMP_URL` defaults to the DigiCert server. Without a certificate the installer is unsigned and users see a SmartScreen warning.

### The release workflow

```bash
./scripts/ship.sh 1.0.0 "Release notes shown in the update dialog" --dry-run
```

Drop `--dry-run` to publish. `ship.sh` checks `gh` authentication, a clean tree, the default branch, sync with origin and the required repository secrets. It bumps the version in `tauri.conf.json`, `package.json`, `package-lock.json` and `Cargo.toml`, commits `release: vX.Y.Z`, pushes, and creates an annotated tag whose message becomes the release notes. It then watches the run and checks `latest.json`.

[release.yml](../.github/workflows/release.yml) runs on a `v*` tag, or by hand with `notes` and `validate_only` inputs:

1. **Build**, on macOS Apple Silicon, Windows x64 and Ubuntu 22.04. Each leg freezes and smoke tests the helper, stages `uv`, runs `npx tauri build --target`, and checks the helper is inside the package. Windows also runs a launch check from a non-ASCII path. A validation run adds the online Modal check and a Windows install and reinstall test. Artefacts are renamed `MangaCleaner_<version>_<platform>.<ext>`.
2. **Launch smoke**, on clean Ubuntu images for the `.deb`, and on Ubuntu, Fedora and Arch for the AppImage. The `.deb` legs gate publishing. The AppImage legs only report.
3. **Publish**, skipped on a validation run. It refuses a tag that does not match the `tauri.conf.json` version. It builds `latest.json`, uploads each artefact, then uploads `latest.json` last, all by authenticated `PUT` to the publish worker.

`scripts/release.sh "notes"` is the other path. It builds and publishes from your own machine, for that platform only, and merges its entry into the existing `latest.json`. It reads the updater key and publish token from `~/.tauri/`. Prefer `ship.sh`.

### The download site

[infra/](../infra/) holds two Cloudflare workers over one R2 bucket, `cleaner-updates`. **cleaner-site** ([infra/site/](../infra/site/)) serves the landing page from `public/` and reads `latest.json` and `releases/v<version>/<file>` from the bucket, read-only with range requests. **cleaner-publish** ([infra/publish-worker/](../infra/publish-worker/)) accepts `PUT /<key>` with a bearer token from its `PUBLISH_TOKEN` secret and streams the body into the bucket.

```bash
cd infra/site
```

```bash
npx wrangler deploy
```

The installed app polls `https://cleaner.komiq.cc/latest.json`, set in `tauri.conf.json`. Read [infra/site/README.md](../infra/site/README.md) before the first deploy, because it takes the hostname over from the bucket's own custom domain.

## 8. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| "no ONNX Runtime found; looked in: ..." | No runtime in any search path. Run `scripts/fetch-runtime.sh` and set `ORT_DYLIB_PATH`, or download it from Settings. |
| Runtime "quarantined" or "refused by the system" on macOS | A download carries `com.apple.quarantine`, or the signature does not match. The fetch script uses `curl` and strips the attribute. Keep the file name `libonnxruntime.dylib`. |
| Windows error 126 loading the runtime | The Visual C++ runtime DLLs are missing. Stage them with `stage-vc-runtime.ps1`. |
| Release build panics "the frozen cloud helper is out of date" | A frozen source changed. Run `npm run helpers`. |
| Release build panics about `bundle.externalBin`, a helper or `uv` | Run the freeze and `uv` staging steps in section 7. |
| Development build warns that the helper is out of date | Expected. It runs `python -m provisioner`, using the first of `VIRTUAL_ENV`, `.venv`, `venv` or `target/cloud-build-venv` that can import `modal`. |
| Tests abort with a WebGPU crash, or flake | Run with `-- --test-threads=1`. |
| GPU unused on Linux, "no graphics device" | Install the graphics driver and the Vulkan loader, `libvulkan.so.1`. |
| Digest mismatch in a fetch script | The upstream file changed or the download broke. Do not edit a digest without checking the source. |
| `the_script_and_the_table_agree` or `tauri-acl.test.js` fails | A list was edited in one place only. See section 6. |
| `npm run helpers:install` refuses | The app is running, or it was built from other cloud code. After re-signing, click Always Allow on each cloud key prompt. |

## 9. Conventions

- **Commits** use a lowercase type prefix and a short subject: `feat:`, `fix:`, `chore:`, with an optional scope as in `fix(ci):` or `feat(site):`. The release script writes `release: vX.Y.Z`.
- **Spelling** in documentation and user-visible text is British: colour, licence, behaviour. Do not use em-dashes; use a colon, comma or full stop. Tests enforce this for the catalogues and dialogs.
- **Measurements and deferrals** go in [findings.md](findings.md). Record what you did not measure, test or finish under "What has not been measured", with the conditions.
- **Working notes stay local.** Plans, reports, audits, evidence and validation notes, work logs, ledgers, handoffs and dated notes are listed in [.gitignore](../.gitignore) and are not committed. Reference documents such as contracts and `findings.md` are.
- **No secrets in the repository.** Weights, runtimes, helper binaries and `.claude/` are ignored. The Hugging Face token and cloud keys live in the operating system keyring, and the setup helper receives credentials on stdin only. Release keys live in `~/.tauri/` or in repository secrets.
- **Licence:** the app is GPL-3.0-only. Downloaded model weights keep their own licences.
