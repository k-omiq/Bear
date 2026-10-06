# Color fidelity: wrong-branch handoff

On 2026-10-03 the user identified that implementation had been performed in
`/Users/caved/dev/manga-cleaner`, branch `main`, HEAD `03fcaba`, rather than
`/Users/caved/.codex/worktrees/cloud-integration/manga-cleaner`, branch
`codex/cloud-integration`, HEAD `c5a61f4`. No commits were made. Main's existing
uncommitted work is retained. The intended checkout also contains uncommitted
cloud configuration, detection, denoise, navigation and settings work, which
must be preserved during adaptation.

## Work performed here

The detailed batch A–I implementation evidence remains in
[color-fidelity-fix-plan.md](color-fidelity-fix-plan.md), with the baseline in
[color-fidelity-audit.md](color-fidelity-audit.md).

- A/D: independent libjpeg, Pillow/LCMS, tifffile and PSD reference fixtures;
  native CMYK/YCCK decoding and explicit ambiguous-marker handling.
- B: native color descriptions, PNG/TIFF metadata, ICC validation, alpha
  association, safe descriptive EXIF rebuilding and declared unsupported cases.
- C/G: chapter-owned conversion and byte-exact archives, native PNG/TIFF/JPEG
  retention, shared export preflight, actual formats/notices, staging and
  stale-plan checks across page/stitched/PSD/archive export paths.
- E: managed sRGB previews, native-depth transparency, bounded transform cache
  and linear-light premultiplied downsampling.
- F: managed paint/fill/model conversion, masked native preservation, backend
  paint previews with cancellation and tile handoff, clone/heal and alpha rules.
- H: all eight orientations, native-coordinate patch persistence, display
  mapping, schema compatibility, history/reorder/undo migration and preservation.
- I: regression tests, documentation, release build and installed-app checks.
  An existing duplicate-geometry detection issue was also fixed at DTO hydration
  without rewriting original records, with navigation regression coverage.

## Validation here (not target-branch evidence)

Core release run: 564 unit, 15 integration and two doc tests passed; 17 LaMa tests
passed separately. Manual PSD fixtures passed independent psd-tools 22/22.
Full serial Tauri: 313 passed and one model-routing test failed; the same failure
reproduced on clean main HEAD. A later 242-test non-run Tauri suite and focused
compatibility tests passed. Frontend: 934 tests / 53 files; Vite and macOS bundle
builds passed. Detailed commands and limitations are in the plan. These results
must not be attributed to cloud-integration without rerunning there.

## Installed application and user data

The wrong-checkout build was copied into `/Applications/Manga Cleaner.app` and
ad-hoc signed for verification. The original installed bundle is backed up at
`/tmp/Manga Cleaner before color fidelity.app`; this initial state was superseded by the completed branch correction below. No release signing/notarization was performed. Original settings were
restored from `/tmp/manga-color-installed-before/settings.json`. Isolated test
projects were removed from the library and retained at
`/tmp/manga-color-verification-projects`; exports/fixtures remain under
`/tmp/manga-color-installed-*`. All 2,523 pre-existing files in the inspected
project retained their hashes. Verification covered byte-exact imports, owned
archives, export samples/metadata, brush/clone, undo/redo, and RTL/LTR navigation.
The last duplicate-ID navigation correction was built and installed but its
final real-chapter UI recheck had not completed when the user stopped this work.

## Completed branch correction (2026-10-03)

The implementation has been adapted and validated in
`/Users/caved/.codex/worktrees/cloud-integration/manga-cleaner`, branch
`codex/cloud-integration` (starting at `c5a61f4`). Pre-existing target cloud,
detection, navigation, reading-direction and settings changes remain in place.
Main has only its pre-task mouse-pressure hunk and untracked `promo/`.
Recovery snapshots are in `/tmp/color-fidelity-branch-handoff` and
`/Users/caved/.codex/backups/manga-cleaner/branch-correction-20261003-d339a4e2`.
No commits were made.

The final target implementation uses schema v5 for native parts/orientation,
preserving existing v3/v4 semantics and the target cloud/revision/write-lock
contracts. Full target gates pass: 827 core unit, 16 integration, two doc,
934 Tauri and 2,193 frontend tests; production and macOS bundle builds pass.
Independent PSD decoding passes 22/22. Full details and skipped checks are in
[color-fidelity-fix-plan.md](color-fidelity-fix-plan.md).

The correct cloud-branch bundle is now installed, ad-hoc signed and signature
verified. Native verification covers existing chapter artwork/layers, repeated
RTL/LTR navigation in both page modes, exact native/archive imports, managed
export metadata, edit/undo/redo and restart with the scan folder absent.
Independent decoding confirms exact outside-mask samples, TIFF alpha,
orientation and the WebP first frame. All 3,938 checked existing project files
remain byte-identical. Isolated test projects were removed from the library and
retained at `/tmp/cloud-color-installed-verification-projects`; the original
installed-app backup is still available.

Physical drag automation failed with `noWindowsAvailable` on this target run;
brush/clone live appearance and held-key repeats remain unverified in the native
app, alongside optional live-model/platform/display checks. Backend and mounted
frontend coverage passes. No skills or plugins were invoked or installed.
