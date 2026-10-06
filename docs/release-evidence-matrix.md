# Release evidence matrix

Status on 25 September 2026 for branch `codex/cloud-integration`. This is the M8 exit artifact from
[phased-implementation-plan.md](phased-implementation-plan.md): each operating system and provider
combination is labeled by what was actually run. The only machine available was a MacBook Air Mac17,3
(Apple M5, 32 GB, macOS 27.0 arm64). Release builds target macOS arm64 and Windows x64
(`.github/workflows/release.yml`); Intel macOS is not built.

Labels: **Tested** means run on that combination with recorded evidence. **Tests only** means unit or
integration tests with fakes or synthetic input, no live run. **Unknown** means not run; no claim is made.
**Unsupported** means the app refuses it by design.

## 26 September implementation checks

The onboarding/editor follow-up adds detector combinations, managed FLUX/SAM setup, cloud model selection and usage reporting, and editable layer styles. The release workflow now also targets Linux x64; this supersedes the older packaging statements below, without promoting their hardware evidence labels.

Verification on the macOS development host: 1,468 frontend tests, the production frontend build, the full Rust workspace suite (excluding the memory spike), and strict Clippy passed. The pinned provider SDK environment passed 211 cloud tests and 141 provisioner tests; both local bootstrap suites passed three offline tests each. A browser mock walkthrough covered model selection, tool defaults, layer controls, and reload prevention.

GPT-6 Sol implementers and GPT-6 Luna xhigh reviewers handled the changes. Confirmed review issues were fixed before the final test runs. The requested e-swarm review additionally identified repeated gateway graph hashing, now covered by file/marker invalidation regressions. The e-swarm editor/usage and detection/helper reviews reported no further actionable defects. Review claims were checked against the implementation before applying fixes. Linux release collection and publishing were also exercised with synthetic AppImage/signature/Debian fixtures.

This pass did not run a paid GPU deployment, download the full FLUX models, build Windows/Linux release packages, or validate exact SAM export reproduction on those operating systems. Cloud usage reports known app charges and explicitly marks missing prices; optional Modal workspace billing is delayed provider data, with session change measured since billing was connected.

## Local pipeline and models

| Combination | Label | Evidence | App behaviour that matches it |
| --- | --- | --- | --- |
| Legacy Auto clean, macOS arm64, ORT CPU and CoreML/WebGPU picks | Tested | [findings.md](findings.md) hardware and engine sections; integration run in [milestone-ledger.md](milestone-ledger.md) | Automatic accelerator choice reports measured or guessed per model |
| Legacy Auto clean, Windows x64 (CPU, DirectML, CUDA, WebGPU on Direct3D 12) | Unknown | CI compiles and runs unit tests on `windows-latest`; no GPU run | Every Windows GPU entry is reported as an unmeasured candidate |
| Legacy Auto clean, Linux x64 (CPU, CUDA, WebGPU on Vulkan) | Unknown | No machine | Same as Windows; no Linux release bundle |
| SAM-TS-L analysis, macOS arm64 Apple M5, app ORT 1.28.0 built-in WebGPU | Tested | [provider-qualification.md](provider-qualification.md): 45/45 masks match on Pillow input, about 6.3 s per page, 5.9 GB sampled Metal high-water | Review allowed |
| SAM-TS-L component write, PNG source, same host and runtime SHA-256 | Tested, narrow | [component-write-contract.md](component-write-contract.md) | The only write-qualified path; gated by host identity and runtime hash |
| SAM-TS-L, macOS arm64 ORT CPU | Tested, review only | 44/45 exact masks; page 27 differs by two threshold pixels | Review only, no write |
| SAM-TS-L, JPEG source | Tested, review only | App JPEG decode changes the prepared tensor; 666 mask pixels differ on page 27 | Review only, no write |
| SAM-TS-L, CoreML | Unknown, not usable | Encoder session construction exceeded 180 s | Not offered |
| SAM-TS-L, other Macs, Windows, Linux | Unknown | No hardware | Review may run where the runtime loads; no write |
| Text-shaped review on long strips | Unsupported | Analysis requires a paginated chapter | Refused with a named outcome |
| COO MTSv3 SFX finder | Unsupported | [model-rights-decision.md](model-rights-decision.md) | Not offered anywhere |
| Full RT-DETR managed download and local SAM export | Implemented, release rights unreviewed | Hash-pinned artifact and source acquisition; no Windows/Linux export validation yet | Do not treat availability as redistribution clearance |

## Interface and packaging

| Combination | Label | Evidence | App behaviour that matches it |
| --- | --- | --- | --- |
| Svelte interface in WKWebView (the shipped macOS webview) | Unknown | jsdom tests and a Chromium mock walk-through only | Known WebKit gaps avoided in code: focus on click, and review mask tints drawn on a canvas instead of CSS masks |
| Svelte interface in WebView2 (Windows) | Unknown | No machine; preview memory not measured | None |
| macOS tray template icon | Unknown | Asset generated by script and inspected as a file; no automated test and no live menu bar | Close-to-tray on by user choice |
| Close-to-tray and single instance on Windows | Unknown | No machine | None |
| Windows app-local CRT and runtime loading | Unknown | Arguments from binaries, not runs ([findings.md](findings.md)) | Typed errors for missing runtime or dependency |
| PSD export opened in Photoshop | Unknown | Independent reader opens every fixture; 2 GB ceiling refused before writing | Large files refused, not written |
| Engine star ratings | Tests only | Provisional, from file sizes and per-page timings | Settings shows "Engine ratings are provisional estimates" |

## Cloud

| Combination | Label | Evidence | App behaviour that matches it |
| --- | --- | --- | --- |
| FLUX region render on a user-owned Modal account | Tested, narrow | One live run on 25 September 2026 ([cloud-work-log.md](cloud-work-log.md)): L4, synthetic 512x384 crop, cold 71 s, warm 10 s, cancel, recovery and cleanup, $0.10 to $0.15 metered. Driven by a contract client, not the desktop app | Off by default; consent per render; cost shown as unknown because the gateway reports none |
| FLUX region render on a user-owned Beam account | Tests only | Same as Modal | Same as Modal |
| Cloud setup helper, macOS arm64 | Tested from a checkout | `python -m provisioner` ran inspect, plan, apply, probe and cleanup live on Modal; the frozen helper binary from the release workflow has not run | Setup refuses with a typed error when the helper is missing |
| Cloud setup helper, Windows | Unknown | Helper not built or run on Windows | Same |
| Cloud text analysis (SAM or RT tiles, M7) on Modal or Beam | Tests only | In-process contract fake and in-process Python gateway | Off by default; per-page consent with rights attestation and retention acknowledgement; results are review evidence only and cannot prepare a write |
| Cloud input limits (size, pixels, timeouts) | Unknown | Drafted limits used as host safety ceilings | Requests outside them refused before sending |
| Provider bills, latency, GPU memory, cleanup | Modal: tested once; Beam: Unknown | Modal: $0.10 to $0.15 metered for setup plus four renders, about 7.0 GiB GPU memory on an L4, cleanup verified; no Beam account | Cost always shown as unknown unless the gateway reports one |

## What would change a label

- A labeled holdout and human mask and reconstruction reviews (M0, M5) would allow quality claims and the padding, threshold and overlap defaults.
- A Windows or Linux machine with the named GPU and runtime, run through the plan in [provider-qualification.md](provider-qualification.md), would move those rows from Unknown.
- A Beam account, a run driven by the desktop app itself, and authorized real crops would move the remaining cloud rows (M7 exit, M8 live gates).
- A rights review of the RT-DETR and SAM training and derivative-weight chain would decide hosted downloads.
- Opening fixtures in Photoshop and running the app in WKWebView and WebView2 would settle the interface rows.
