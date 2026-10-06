# Cloud implementation work log

Worktree: `/Users/caved/.codex/worktrees/cloud-integration/manga-cleaner`.
Base: `03fcaba708d80835f0f5591b6d0cffe9154ad021`.
All changes remain uncommitted. The original checkout is unchanged.

## Acceptance status

Milestone count: **2 of 12 complete** (P0 and P2), **3 partial** (P1, P3,
P4), **7 not complete** (P5 to P11). This is an unweighted milestone count,
not an estimate of effort spent. Offline subtasks do not satisfy live staging
or installed-platform exit evidence.

- P0: baseline recorded; the model-dependent bubble-routing failure remains
  undiagnosed and is not treated as a cloud regression.
- P1 offline preparation: reviewed by two external reviewers. Nineteen local
  tests pass (`/tmp/manga-cloud-p1-orchestrator-tests.log`). PyPI metadata and
  hashes were independently checked; template hashes in the report were
  synchronized with the actual bytes. Live account, token-scope, cleanup, and
  installed-platform packaging evidence remains outstanding. P1 is incomplete.
- P2a: previously approved pure preparation/composition extraction retained.
- P2b: both final source reviews approved. The initial frontend run passed 932 tests and
  failed one new dialog test because DOM cleanup was missing. Cleanup was
  fixed; all four dialog tests then passed and the frontend build passed.
  A subsequent full frontend run passes all 933 tests (`/tmp/manga-cloud-integrated-frontend.log`). Migration tests accidentally removed during formatting cleanup were restored;
  all 24 project tests pass. The first full core run failed the obsolete v2
  assertion; that assertion now follows `FORMAT_VERSION`. The identifier and
  public provenance converter tests each passed. Integrated check/Clippy will
  also cover subsequent P2c/P3a changes and is not claimed complete here.
- P2c: both source reviewers approve the offline contract. Orchestrator checks
  pass 24 Python tests and 16 Rust wire tests. Digest collision vectors now
  assert the actual shared fixture bytes and hashes. No deployed gateway exists.
- P3a: both final source reviewers approve the foundation. Eighteen inference
  tests and five settings tests pass. All 45 handlers match the app manifest
  and local main-window permission allowlists. Credential deletion fails closed
  when persistent deletion cannot be confirmed; errors suppress raw store text.
  Region-bound consent is implemented in P3b2; real dispatch coordination remains later integration work.
- P3b1: module integration is present; eight real loopback HTTP tests and ten
  decoder tests pass. The combined cloud-focused core filter passes 31 tests.
  An IPv6 documentation-prefix omission, self-bound handle helper, allocation
  ceilings, strict PNG framing, and a wrong crop-size test were corrected during
  orchestration. Both final source reviewers approve. No submit/cancel POST exists.
- P3c1: both final source reviews approve the five real profile/credential API
  mappings, canonical DTO shape, strict mock projection, and immediate mock secret
  refusal. All 953 frontend tests and the production build pass.
- P4a: both final source reviews approve the offline attempt journal and bounded,
  fully validated result cache. Fourteen journal tests and strict app-library
  Clippy pass. Real dispatch, project attachment, and restart UI are not wired.
- P3b2: accepted after two final source re-reviews. Epoch validation and grant insertion share the `GrantService` state lock; proposal preparation rechecks its starting epoch before cache insertion; poisoned authorization locks recover instead of silently skipping invalidation; malformed public configuration can be replaced after global invalidation; and the grant cache remains bounded at 256 entries. The focused inference suite passes 69 tests, all 42 region tests pass, and strict app-library Clippy passes. No consent IPC, remote submission, or paid path is exposed.
- P3c2: profile settings UI is complete and both final external reviews approve.
  All 967 frontend tests and the production build pass. This is the next completed
  task requested by the user; work stopped here on 2026-09-20. No new tasks started.
- P4 to P11: not complete. On 2026-09-24 P5 to P9 were implemented offline;
  P10 is not implemented and P11 is partial. Nothing has run live, so the
  count above is unchanged. See the last section.

No cloud deployment, account mutation, GPU operation, or model download has
been authorized or performed in this continuation. A question requesting
staging profile/workspace names (not tokens) remains pending.

## External-agent review evidence

- P1 final reviews: `/tmp/manga-cloud-p1-final-review1.json` and
  `/tmp/manga-cloud-p1-final-review2.json`.
- P2b frontend/build: `/tmp/manga-cloud-p2b-orchestrator-frontend.log`,
  `/tmp/manga-cloud-p2b-dialog-final.log`, and
  `/tmp/manga-cloud-p2b-build-final.log`.
- P2b migration: `/tmp/manga-cloud-p2b-project-final.log`.
- P2b converter: `/tmp/manga-cloud-p2b-converter-final.log`.
- P2b public converter: `/tmp/manga-cloud-p2b-public-converter-final.log`.
- P2b final reviews: `/tmp/manga-cloud-p2b-final-review1.json` and
  `/tmp/manga-cloud-p2b-final-review2.json`.
- P3b2 initial reviews: `/tmp/manga-cloud-p3b2-final-review-vesper.json` and
  `/tmp/manga-cloud-p3b2-final-review-onyx.json`; the security review found the
  epoch-check/issuance TOCTOU and poisoned-lock/config-repair gaps now fixed.
- P3b2 post-fix re-reviews: `/tmp/manga-cloud-p3b2-rereview-vesper.json` and
  `/tmp/manga-cloud-p3b2-rereview-onyx.json` (both approved with no P0 to P3 findings).

Agent summaries are not acceptance evidence by themselves. Several agent runs
returned early with background checks unfinished; those runs are not counted
as passing verification. Formatting-only cleanup must preserve semantic edits
and tests, and is verified against the actual diff.

## Integrated checks after P3b1

- Full core: 569 unit tests passed (1 ignored), 14 integration tests passed,
  2 doc tests passed (1 ignored): `/tmp/manga-cloud-integrated-core-tests.log`.
- Strict core all-target Clippy passed:
  `/tmp/manga-cloud-integrated-core-clippy2.log`.
- Strict app-library Clippy passed before journal module integration:
  `/tmp/manga-cloud-integrated-app-clippy2.log`.
- P3c1 final frontend run: 953 passed, no unhandled rejections. Production build
  passed: `/tmp/manga-cloud-p3c1-frontend-tests3.log` and
  `/tmp/manga-cloud-p3c1-build3.log`. Both final reviews are in
  `/tmp/manga-cloud-p3c1-final-review-{nimbus,tundra}.json`.
- P4a initial compile and source-review findings were corrected, including
  foreign lock use, cache bounds/validation, parent directory durability on Unix,
  attachment snapshot checks, and ambiguous dispatch handling. Windows directory
  flush/power-loss evidence remains outstanding. Final reviews are in
  `/tmp/manga-cloud-p4a-final-review-{lumen,onyx}.json`.
- Combined app inference checks: 40 passed, plus 5 settings tests passed:
  `/tmp/manga-cloud-inference-integrated-tests.log` and
  `/tmp/manga-cloud-settings-final.log`.

Remaining gates include backend region consent/dispatch integration, project
attachment and restart UI, P1 authorized staging verification and physical
platform packaging, real Modal/Beam GPU runs, provisioning and lifecycle UX,
explicit chapter batching, and release verification. Local foundations do not
constitute completion of these milestones.

## User-requested stop point: 2026-09-20

The Inference settings tab supports both provider profile maps, endpoint editing,
default target selection, and removal with selected-target fallback to Local.
It handles read retries, failed-save selection rollback, plain DTO snapshots,
and accessible profile actions. It explicitly states remote execution is unavailable.
No runtime credential entry or connection test is advertised.

- Final frontend: `/tmp/manga-cloud-p3c2-frontend-final.log` (967 passed).
- Production build: `/tmp/manga-cloud-p3c2-build-final.log` (passed).
- Final reviews: `/tmp/manga-cloud-p3c2-final-review-nimbus.json` and
  `/tmp/manga-cloud-p3c2-final-review-tundra.json` (both approved).
- Backend stabilization before the latest review fixes: 62 inference tests passed; 42 region tests and strict app-library/core Clippy passed. After the fixes, 69 inference tests and 42 region tests pass; strict app-library Clippy also passes.
- Region guards now reject explicit remote/malformed targets before auto-clean
  dispatch; saved remote FLUX selection cannot silently invoke the local sidecar.
- HTTP over-limit test now consumes the request before responding, avoiding a
  connection reset caused by closing a socket with unread request bytes.
- `git diff --check` passes. Original checkout remains unchanged apart from its
  original two untracked planning documents. Worktree changes remain uncommitted.

P3b2 is accepted after the post-fix two-reviewer cycle. No consent IPC or paid path is exposed. See `docs/cloud-consent.md`.

P0 and P2 are complete; P1/P3/P4 are partial; P5 to P11 are incomplete. There have
been no live deployments, account mutations, GPU requests, or model downloads.

## Continuation audit: 2026-09-22 (in progress; not a release checkpoint)

The earlier status above is historical, not a current acceptance claim. The
current branch is `codex/cloud-integration`; the complete pre-existing dirty
worktree is preserved. The master plan defines P0 to P11 and **does not define
P12**. No checkpoint commit, push, deployment, GPU run, provider-account action,
credential use, or packaging-platform verification has occurred in this turn.

### Agent evidence and disposition

- Initial lifecycle implementer `1f89a549-4d51-493b-bbc9-e79029f877af`
  (quasar): failed run: progress-only/background cargo operation. Retry
  `33e3649f-a9bd-4ecf-92da-3c5499a8eca9` (zephyr): failed run: background
  tasks and unverified counts. Solaris: authentication failure before a usable
  result. None is counted as verification. Implementer
  `ceb816cd-e0b9-423f-8cf1-badbccb4e80a` (nimbus) made synchronous source
  edits; its claim of no compile risk was rejected after a local compile found
  three missing imports and three warnings.
- Parallel independent lifecycle reviews: security reviewer
  `3776bd05-87a4-4429-acac-e261fdad9e97` (tundra, rejected patch),
  concurrency reviewer `50f37b34-afda-49d7-ac2c-11876ce9b7a0` (lumen,
  conditional approval). The latter's approval is not acceptance: security
  findings were verified against source. Accepted: missing imports, grant
  consumed before validation, client construction after dispatch, source/page
  revalidation, malformed supplied snapshot, fabricated recipe fallback,
  caller handle echo, recovery identity, unsanitized IPC errors, and lack of
  handler-level tests. Rejected as overbroad: the claim that an *omitted*
  optional snapshot bypasses validation (the actual issue was a malformed
  supplied snapshot), and using caller page metadata as an authoritative
  recovery fallback.
- Separate fixers: imports `82b19a90-b1c1-4c1d-935f-61290e6bd9bf7`;
  grant/client ordering `57c79e08-43aa-4264-ba5f-5eb396b4e99f`;
  overlapping ordering finding `44cab683-0c92-405f-b805-ae79c16c51da`
  required no further edit; page/source `e85c023b-ad9e-4068-8aab-119bd9bdd54e`;
  malformed snapshot `c6a4b255-84eb-4343-a0d8-84b5a1af756a`;
  recipe `8bf41a33-54ae-4471-87c2-fd8ec9e6b183`;
  handle `c23baef8-a137-483c-b50d-5c27a2fb534a`;
  recovery `ee9823ff-8284-4094-86d3-e1b53817926a`;
  errors `e3e6dffc-7bd1-4b6a-9ed9-e61004551b0f` (failed verification:
  34 compiler errors); handler tests/compile repair
  `4b720841-f1d8-49da-b2ba-1610c854d972`. The latter corrected two
  test-fixture failures after the focused run; their rerun remains pending.
  The page/source fixer is working separately on service-test failures.
  Both original reviewers must still re-review the resulting patch.
- Follow-up service fixture/product ordering fix in
  `e85c023b-ad9e-4068-8aab-119bd9bdd54e`: moved local page/source
  validation before DNS/client setup and corrected strict fake wire response;
  personal source inspection and focused tests verified it. Original security
  reviewer `3776bd05-87a4-4429-acac-e261fdad9e97` and original concurrency
  reviewer `50f37b34-afda-49d7-ac2c-11876ce9b7a0` re-reviewed in parallel,
  both approved with zero new actionable findings. Their bounded IPC approval
  is **not** deployment, platform, paid-path, or release acceptance; the
  security reviewer's stronger deployment-readiness wording is rejected.
- Strict-Clippy fixer `49521a77-ac96-45ab-a81c-9e085cccb595` narrowly
  annotated four existing multi-argument handler seams and used
  `std::io::Error::other` in a test; source inspection and strict rerun passed.
- Security-lockfile fixer `6071236b-118d-4cd8-8909-27fa48e19ad7` updated
  only `rustls` 0.23.43 to 0.23.45 with foreground
  `cargo update -p rustls --precise 0.23.45` (reported exit 0; verified by
  subsequent `cargo audit`).

### Commands observed locally in this continuation

| Command | Exit | Observed result |
| --- | ---: | --- |
| `cargo test -p manga-cleaner --lib inference:: -- --test-threads=1` | 101 | 110 run, 105 passed, 5 failed: two command fixtures and three service fixtures; later fixes not yet rerun. |
| `cargo test -p manga-cleaner --lib inference:: -- --test-threads=1` (after fixes) | 0 | 110 passed, 0 failed; 308 filtered out. |
| `cargo test --workspace --exclude spike-strip-memory -- --test-threads=1` | 0 | Core 569 passed/1 ignored; app 418 passed; integration suites 4+4+5+1 passed; spike 2 passed; core docs 2 passed/1 ignored; app doc 1 ignored. |
| `cargo clippy -p cleaner-core -p manga-cleaner --all-targets --no-deps -- -D warnings` (first) | 101 | Four `too_many_arguments` and one test `io_other_error` in commands.rs. |
| `cargo clippy -p cleaner-core -p manga-cleaner --all-targets --no-deps -- -D warnings` (after fixer) | 0 | Strict all-target check passed. |
| `cargo check --workspace --exclude spike-strip-memory` | 0 | Workspace check passed. |
| `npm test` | 0 | 52 files, 999 tests passed. |
| `npm run build` | 0 | Vite production build, 336 modules transformed. |
| `python3 -m unittest discover -v -s deploy/cloud/tests` | 0 | 71 tests passed, including fake-gateway/adversarial faults. |
| `python3 -m unittest discover -v -s provisioner` | 0 | 105 tests passed, including fake drivers, interruptions and ownership gates. |
| `python3 -m compileall deploy/cloud provisioner` | 0 | Both trees compiled. |
| `npm audit --audit-level=high` | 0 | No high advisories; 3 moderate vulnerabilities in `@vitest/mocker`/`devalue`, not silently dismissed. |
| `cargo audit` before lockfile fix | 1 | RUSTSEC-2026-0285 in `rustls` 0.23.43, plus 8 warning advisories. |
| `cargo audit` after lockfile fix | 0 | No vulnerability; 8 allowed warnings remain (unmaintained/unsound/yanked transitive crates). |
| `git diff --check` (early snapshot) | 0 | No whitespace errors at that point; rerun after final edits. |
| `git diff --check` (post-lifecycle) | 0 | Tracked diff clean; untracked files not covered until staged. |
| Bounded `rg` secret-pattern scan (exact command below) | 1 | No matching candidate key patterns; this is not proof that no secrets exist. |

```sh
rg -l -i --glob '!*.png' --glob '!Cargo.lock' '(-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----|AKIA[0-9A-Z]{16}|sk-[A-Za-z0-9]{32,}|ghp_[A-Za-z0-9]{36}|xox[baprs]-[A-Za-z0-9-]{20,})' .
```

The P3/P4 IPC re-review is bounded acceptance only; P1 and P5 to P8 still have
external provider/staging/physical-platform acceptance gaps. Remaining local
gateway/provisioner source reviews, final post-change reruns, staged diff/secret
scan, and checkpoint assessment remain open. Do not call the product ready.

### Follow-on offline audits and failed runs (still in progress)

- P5 implementer `f5e95e6b-f6bb-424b-9d44-566eb7b24b2d`
  (vesper) first returned **empty response on 240s timeout: failed run**.
  It had edited gateway/Modal/test files; a bounded follow-up reported the
  edits. The claimed 71-test count was stale: orchestrator rerun of
  `python3 -m unittest discover -v -s deploy/cloud/tests` exited 0 with **75**
  tests. P5 original parallel reviewers: `10a8a5e4-a096-4614-aa70-fb50a9eb728a`
  (zephyr) timed out with partial output (failed review run), then returned
  a completed bounded follow-up; `7b582425-3f6b-4bc8-9f7d-38df08388998`
  (onyx) completed. Accepted findings verified personally in `api.py` /
  `handle_mapping.py`: concurrent duplicate-digest registration, non-atomic
  PENDING→RUNNING worker claim permitting duplicate execution, cancellation
  terminal TOCTOU/ignored `request_cancel` result, and Modal's
  success-shaped non-inferencing GPU builder. Separate fixers: duplicate
  digest `e379125a-5b23-4909-a276-eb9823507ba5` (lumen); Modal builder
  `0f7a19a6-49b3-4363-870e-d93a98a2c0f0` (quasar); worker and cancel
  fixes/re-reviews pending. Neither reviewer approval implies live readiness.
- P7/P8 offline implementer `9c68c129-604e-40db-b661-b31f4ab79009`
  (axiom) first returned **empty response on 240s timeout: failed run**,
  then summarized touched provisioner files. Its reported 105/105 pass was
  false for the edited tree: orchestrator
  `python3 -m unittest discover -v -s provisioner` exited **1**, 105 tests,
  one `test_nested_dict_and_list_redaction` mismatch. Separate redaction
  fixer `827591ca-27fe-4f76-b28e-d0b5cb7fa915` (tundra) first timed out
  empty (failed run), then completed bounded follow-up: fully redacts exact
  and non-exact `*_token_id`, updates old partial-mask expectation, preserves
  benign `profile_id`. Orchestrator rerun exited **0**, **106** tests.
  Independent parallel reviewers `256935f9-8af2-42c3-894f-a1898dbbd5a8`
  (vesper) and `8014d00f-510a-41bd-8b0f-c24a35604d7f` (axiom)
  reported no further offline findings; their assertion that P7/P8 are
  complete is rejected: real drivers explicitly hard-stop and no bundled
  helper/physical platform or account tests exist.
- Frontend readiness implementer `59cfea9e-b9cc-4a66-b8b0-1c399e18c839`
  (quasar) initially replaced a premature constant `true` with an untrusted
  caller-supplied prerequisite object able to return `true`; that assertion
  was rejected after source inspection. Separate fixer
  `a67e36bb-8907-40c3-aea8-853dec2e4f4e` (nimbus) keeps readiness
  unconditionally `false` until authoritative backend evidence exists,
  separate from truthful IPC registration. Orchestrator `npm test` exited 0,
  **999** tests. Parallel independent reviewers
  `92309f4b-3930-4a3b-b351-0ad709006f2b` (nimbus) and
  `4234250c-62c1-45cb-931a-5f0c52f781b1` (onyx) reported no new
  actionable frontend findings. Personally checked both readiness functions.
- PyPI candidate metadata rechecked on 2026-09-22 with
  `curl -fsSL https://pypi.org/pypi/{package}/{version}/json | jq` for
  `modal/1.5.5`, `beam-client/0.2.211`, `beta9/0.1.268` (loop exit 0):
  wheel SHA-256 and Python version constraints match `docs/cloud-feasibility.md`.
  This does not verify hosted behavior, account permissions or current
  compatibility on packaged platforms.

P5 and P6 cannot be called real cloud crops: `FluxWorker` is a synthetic
RGB8 PNG simulator, native handle state is in-memory, and live builders are
being made fail-closed. P7/P8 real drivers refuse live calls. P9 to P11 depend
on live/provider/physical-platform evidence; safe offline work remains under
assessment. The plan contains **no P12 definition**.

### Subsequent offline disposition (2026-09-22)

- P5's worker-claim race was fixed by separate fixer `agy-zephyr`
  `c77015b1-6c05-4457-948a-80ee84373ec2` using an atomic
  PENDING-to-RUNNING claim. Its initial empty timeout was a failed run; its
  bounded follow-up was inspected against the source and deterministic test.
  P5's cancellation race fixer `agy-quasar` was interrupted before a usable
  answer, so that run failed; replacement read-only verifier `agy-nimbus`
  `f48e2c9d-1a73-4fbc-a50b-5f25f3ed0d1b` completed. The route now checks
  `request_cancel`'s result, returns typed 409 on lost/terminal transition,
  and acknowledges only an accepted cancellation request. Original reviewers
  `agy-zephyr` `10a8a5e4-a096-4614-aa70-fb50a9eb728a` and `agy-onyx`
  `7b582425-3f6b-4bc8-9f7d-38df08388998` first encountered transient
  503 failed runs on re-review, then each completed an original-conversation
  re-review with no new offline findings. The gateway suite now passes 84
  tests. This is not real provider acceptance.
- P6's initial implementer `agy-lumen` was interrupted before a usable
  response; the edited code was preserved and replacement `agy-tundra`
  `823ed0d1-62d3-4756-95ba-1f080b5ff010` completed a source check.
  Parallel independent reviewers `agy-axiom`
  `ceda07e3-a9bd-4395-8aed-840c04424f32` and `agy-vesper`
  `7f244d98-6938-4d5f-93af-cc9a119ebb1e` found no further offline
  issue. The Beam builder fails closed before SDK resource creation; no
  hosted queue, token-scope, or GPU parity is verified.
- A stale frontend status string still says lifecycle commands are pending
  registration. `agy-lumen` `35bc7175-3b13-41f9-984f-184bda4ef186`
  and `agy-quasar` `04ab8b49-1305-4ff9-9448-22dec47d8b4b` each returned
  an empty response after a print timeout and made no scoped edit: both are
  failed fixer runs. Fresh `agy-nimbus`
  `8088d491-83e4-439e-a88f-5b58a9422ec0` completed the two-line copy
  correction. The orchestrator inspected both exact lines. Original reviewer
  re-reviews `agy-nimbus` `92309f4b-3930-4a3b-b351-0ad709006f2b`
  and `agy-onyx` `4234250c-62c1-45cb-931a-5f0c52f781b1` completed
  with no new findings; the orchestrator independently checked their
  readiness/registration/copy claims and ran the full frontend suite. An
  extra third reviewer `agy-tundra` `9910ae10-6935-411e-b052-1c73a43e2760`
  timed out with an empty response, so that run failed and is not evidence.

The final local verification rerun to date (foreground commands with completed
results) is:

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo test --workspace --exclude spike-strip-memory -- --test-threads=1` | 0 | Core 569 passed/1 ignored; app 418 passed; integration suites 4+4+5+1 passed; spike 2 passed; core docs 2 passed/1 ignored; app doc 1 ignored. |
| `cargo clippy -p cleaner-core -p manga-cleaner --all-targets --no-deps -- -D warnings` | 0 | Strict all-target check passed. |
| `cargo check --workspace --exclude spike-strip-memory` | 0 | Workspace check passed. |
| `npm test` | 0 | 52 files, 999 tests passed after the copy correction. |
| `npm run build` | 0 | Vite production build, 336 modules transformed after the copy correction. |
| `python3 -m unittest discover -v -s deploy/cloud/tests` | 0 | 84 passed. |
| `python3 -m unittest discover -v -s provisioner` | 0 | 106 passed. |
| `python3 -m compileall deploy/cloud provisioner` | 0 | Both trees compiled. |
| `npm audit --audit-level=high` | 0 | Three moderate advisories remain; no high advisory. |
| `cargo audit` | 0 | No vulnerability; eight allowed warnings remain. |
| `git diff --check` | 0 | Tracked patch clean; staged/untracked verification pending. |

Staging the previously untracked P1 to P8 files exposed 59 whitespace findings:
`git diff --cached --check` exited **2**. Separate formatting fixer
`agy-axiom` `56feb16c-4978-4dc4-91f1-4a692d4a6639` completed a
whitespace-only pass on 17 non-frontend files, verified with `git diff -w`;
it intentionally left the frontend test outside its allowed scope. A separate
bounded frontend-EOF fixer is in progress. No checkpoint commit may occur
until all edits are restaged and `git diff --cached --check` exits 0. The
bounded secret-pattern scan reran after copy correction and exited 1 with no
matching paths.

The one-file whitespace fixer `agy-vesper`
`51ec3619-5c52-4146-9c69-2e69cc5b33bb` failed before execution on a
provider 503; replacement `agy-zephyr`
`5a1eada0-5efd-4cad-a689-7fb4ff31788a` completed, removing only the
extra EOF blank line in `src/lib/api/cloud-inference.test.js`. The orchestrator
verified the 17-file formatter's unstaged diff with
`git diff -w --ignore-blank-lines -- deploy/cloud src-tauri/src/inference docs/cloud-api.md docs/cloud-contract.md docs/cloud-feasibility.md docs/cloud-journal.md docs/cloud-security.md docs/cloud-verification.md manga-cleaner-cloud-master-plan.md`
(exit 0, empty output), then restaged the scoped tree. Both
`git diff --cached --check` and `git diff --check` exited 0. A staged
`git grep -l --cached -I -E '(-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----|AKIA[0-9A-Z]{16}|sk-[A-Za-z0-9]{32,}|ghp_[A-Za-z0-9]{36}|xox[baprs]-[A-Za-z0-9-]{20,})' -- .`
scan for private-key headers and common cloud, GitHub, and Slack token shapes
exited 1 with no matching paths. This is a
bounded pattern scan, not a complete credential audit.

### P1 to P8 checkpoint boundary

The locally testable portion is stable under the commands above, but the
milestones are **not all complete** under the master plan's acceptance tests:

| Milestone | Verified locally | Missing external/release evidence |
| --- | --- | --- |
| P1 | Candidate SDK/wheel hash matrix and CPU-only probes | Eligible provider accounts, hosted permission and token-scope experiments, Apple Silicon and Windows installed-helper tests |
| P2 | Rendering seam, format migration, and local regression tests | Model-dependent GPU/performance measurements |
| P3 | Profile/secret/consent backend tests, registered IPC, readiness false | Provider credential handling in a packaged app and a safe live paid path |
| P4 | Durable local attempt/fault tests and known-handle recovery | Real provider-native durable handles, restart/staging and power-loss evidence |
| P5 | Modal contract/gateway simulator and fail-closed builder | Real pinned FLUX weights, GPU crop, staging cancellation/recovery |
| P6 | Beam contract/gateway simulator and fail-closed builder | Real Beam queue, token permissions, GPU parity and recovery |
| P7 | Modal fake-driver provisioner plan/journal/redaction tests | Authorized account, real driver, packaged helper, deployment and cleanup |
| P8 | Beam fake-driver provisioner tests and Windows gating | Authorized account, native/WSL packaged path, real driver and cleanup |

The next external action for P1/P5 to P8 is to supply explicitly authorized
staging accounts/credentials and supported test platforms; then run the
master plan's CPU-only account/permission experiments and packaged-helper
probe **before** any real provider resource creation or GPU work. No such
authorization or evidence is present, so live calls remain disabled. The
P1 feasibility gate was not satisfied before offline P7/P8 source work;
those sources are preparation, not approved deployment functionality.

P9's real installation lifecycle requires working, ownership-verified P7/P8
drivers. P10's 20/60-page staged sessions require working P5/P6 crops and
staging authorization. P11 can still gain CPU-only CI checks locally, but
its packaged/GPU/security release gates depend on the same external inputs.
There is **no P12 specification** in the supplied master plan, so no P12
acceptance claim or invented implementation is warranted. Do not expose
the paid path, deploy, push, or assert release readiness at this checkpoint.

P1 still lacks authorized provider-account, hosted permission, and physical
packaged-helper evidence. P5/P6 lack genuine GPU crop/staging and durable
provider-native handle recovery. P7/P8 lack live drivers, packaged helper
integration, provider-account approvals, and tested target platforms. No
fallback code may convert those missing proofs into a ready/paid path.

## Post-checkpoint local work (2026-09-22; uncommitted)

The P1 to P8 offline-foundations checkpoint was committed on
`codex/cloud-integration` as `b16664a`, then amended for the backend
fail-closed safety correction to `7afe0f1`, retaining message
`feat(cloud): checkpoint offline P1-P8 foundations` (both commit commands
exited 0). It was not pushed. The checkpoint is **not** evidence that P1,
P3 to P8 meet their external acceptance gates.

P11 CPU-only CI increment: initial e-swarm implementer `agy-lumen`
`03b1f5b3-b650-4347-81e5-24fa6b95ea60` returned an empty response after
a denied command and made no edit: failed run. Replacement implementer
`agy-nimbus` `c579a96d-c8fb-42ba-8e77-6c6cdd6a6fe6` added only a Python
3.11 job to `.github/workflows/ci.yml`, with three separate unittest and
compileall steps; no secrets, GPU, model downloads or deployment. Parallel
reviewers: `agy-onyx` `15d45a13-6baa-46e6-9529-ffd5970d8e98`
(completed, no findings; original review verified by pinned follow-up) and
`agy-tundra` `44fea54f-3f2a-48c5-9bf9-59377cb49318`
(empty response after denied command: failed review). Replacement independent
reviewer `agy-axiom` `09690082-a951-4beb-b0c1-05ad48acc86f` completed
with no findings. The orchestrator personally inspected the 15-line YAML diff
and parsed its five-step job with
`ruby -e 'require "yaml"; d=YAML.load_file(".github/workflows/ci.yml"); p d.fetch("jobs").fetch("python").fetch("steps").length'`
(exit 0, result 5). Local verification on installed Python 3.12:
`python3.12 -m unittest discover -v -s deploy/cloud/tests` (exit 0, 84),
`python3.12 -m unittest discover -v -s provisioner` (exit 0, 106), and
`python3.12 -m compileall deploy/cloud provisioner` (exit 0). The host's
default `python3` is 3.9.6, below the helper's declared >=3.10 floor;
this is not a GitHub-hosted CI run or a packaged-platform check. The workflow
is uncommitted under the user's post-checkpoint commit rule.

Post-checkpoint source audit found a high-risk inconsistency: frontend
`isCloudExecutionReady()` is false, but the registered production
`submit_cloud_attempt` Tauri handler could still reach `CloudHttpClient::submit_job`
after caller configuration, consent and grant. Those checks do not constitute
the missing real-provider release gate. No real submission was attempted.
Initial hard-stop implementer `agy-quasar`
`df6dad6b-c615-4615-b5c5-588c77e13604` returned empty after 10 minutes
and reported terminating a background task: failed run, no edit accepted.
A narrower one-file replacement was assigned. Amending the unpushed
checkpoint was justified solely to correct this newly discovered safety
defect; P11 workflow work remains uncommitted.

Replacement `agy-vesper` `90ef9596-4f61-4232-a8b2-f141b332fa4b`
returned a completed one-file edit adding a hardcoded guard and a focused
test. Personal diff inspection rejected the patch as
written: it deleted the original `spawn_blocking` production handler body,
added `unused_variables` suppression and a dummy tuple, and duplicated the
error after the guaranteed guard return. The intended fail-closed gate can
be placed before the original body without discarding it. Separate fixer
`agy-zephyr` `7939a069-535b-4dfe-af45-8122de8640ba` restored that body,
retaining a first-statement, hardcoded, caller/config/environment-independent
guard and its test. The orchestrator checked the complete final diff:
19 additive lines only, no original production code removed. Independent
parallel security reviewers `agy-lumen`
`402ae0a4-bbf3-41c2-a3cb-c800600a73c2` and `agy-nimbus`
`551dc483-6327-4548-9297-adc86dc1efb8` each completed with no findings;
their claims were checked against command registration, the direct IPC entry,
and all other job-creating calls in `commands.rs`. Review scope does not
establish live provider readiness.

Post-guard local checks (foreground, completed):

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo test -p manga-cleaner --lib inference::commands::tests -- --test-threads=1` | 0 | 28 passed; 391 filtered out. |
| `cargo clippy -p cleaner-core -p manga-cleaner --all-targets --no-deps -- -D warnings` | 0 | Strict all-target check passed. |
| `cargo test --workspace --exclude spike-strip-memory -- --test-threads=1` | 0 | Core 569 passed/1 ignored; app 419 passed; integration suites 4+4+5+1; spike 2; core docs 2 passed/1 ignored; app doc 1 ignored. |
| `set -o pipefail; cargo test --workspace --exclude spike-strip-memory -- --test-threads=1 2>&1 \| rg '^test result:'` | 0 | Summary re-run confirmed the exact counts above. |
| `cargo check --workspace --exclude spike-strip-memory` | 0 | Workspace check passed. |
| `git diff --check` and `git diff --cached --check` (safety patch before amend) | 0 each | No whitespace error. |

The public production submit command now returns a static sanitized error
before project reads, grant consumption, journal intent, network I/O, or paid
work; the pure inner implementation remains available for offline fault
tests. This is a deliberate release stop, not a working cloud crop.

### Final local/offline status after the checkpoint amendment

The post-checkpoint P11 workflow and this log are intentionally the only
uncommitted files. No push, staging deployment, provider-account operation,
credential use, GPU run, resource creation, or packaged-platform test occurred.
`git diff --check` exited 0 for the current working tree. The amended
checkpoint is `7afe0f1`; the original `b16664a` hash is superseded and was
never pushed. No further commit was made for P11.

| Command | Exit | Observed result |
| --- | ---: | --- |
| `npm test` | 0 | 52 files, 999 tests passed. |
| `npm run build` | 0 | Vite build, 336 modules transformed. |
| `python3 -m unittest discover -v -s deploy/cloud/tests` | 0 | 84 tests passed. |
| `python3 -m unittest discover -v -s provisioner` | 0 | 106 tests passed. |
| `python3 -m compileall deploy/cloud provisioner` | 0 | Both trees compiled. |
| `npm audit --audit-level=high` | 0 | Three moderate advisories remain (`@vitest/mocker`, `devalue`); no high advisory. |
| `cargo audit` | 0 | No vulnerability; eight warning advisories remain. |
| `git diff --check` | 0 | Current two-file unstaged diff has no whitespace errors. |

P9 cannot truthfully progress to account-check, deploy, repair, rotation,
stop, or uninstall UX backed by real resources: both real provisioning
drivers refuse live calls, and resource ownership/provider scopes are
unverified. The safe local UI already offers public profile management,
advanced endpoint entry, and clearly unavailable execution; adding a
success-shaped setup wizard against fake drivers would mislead users.
P10's explicit 20 to 60-page paid second pass depends on verified P5/P6 crops,
durable native handles, provider compatibility and staging authorization;
only local-only and per-region fault foundations are verified. Enabling a
chapter path before those dependencies would expose paid work and undermine
the no-blind-retry guarantee. P11 gained the reviewed CPU-only Python CI job,
but hosted CI execution, signed packaged helpers, staging/GPU/recovery,
token-scope and release evidence remain absent. P12 is not defined by the
supplied master plan; no acceptance criteria can be inferred.

Specific next external action: the owner must provide explicit staging
authorization plus eligible Modal and Beam test accounts/credentials through
a secure mechanism, confirm permission to run **CPU-only** inspection and
resource-scope experiments first, and arrange clean Apple Silicon macOS and
Windows x64 packaged-app test hosts. After those prerequisites, implement
the real drivers and run the P1 feasibility probes in the plan; only then
seek separate approval for billable GPU deployment/inference and the
staged P5 to P11 acceptance tests. The owner must also define P12's scope and
exit criteria. Until then the exact safe action is to leave the backend
paid-submit guard in place and keep the P11 workflow uncommitted for review.

## P9 to P11 offline continuation (2026-09-22; uncommitted)

This continuation started at checkpoint `7afe0f1` on `codex/cloud-integration`.
It preserved the pre-existing uncommitted `.github/workflows/ci.yml` and this
work log. No commit, push, deployment, provider call, credential use, GPU run,
model download, or packaged-platform verification occurred. The registered
`submit_cloud_attempt` handler still returns its hardcoded fail-closed error
before any project read, grant use, journal write, or network operation.

### Bounded local increments

- **P9 local profile removal UX:** An inline confirmation now explains that
  removing a public profile changes local endpoint configuration only; it does
  not revoke tokens, delete secret-store entries, stop services, or uninstall
  provider resources. It warns that active accepted attempts may lose status
  polling/result retrieval until the same profile is restored. Cancel and
  Escape do not write configuration; focus moves to Cancel and returns to the
  Delete trigger on cancellation. Delete is disabled during edits or another
  removal, failed saves retain the form and show a sanitized error, and a
  successfully removed selected target falls back to Local. This does not
  implement provider setup, repair, rotation, stop, or uninstall.
- **P10 local-only regression gate:** `startRun` now has four table-driven
  frontend cases for default/page/chapter/project scopes. Each asserts the
  actual `runClean` argument contains `engineCeiling: 'lama'` even if local
  tool parameters attempt `cloud` or `flux`. This tests frontend argument
  assembly only; it does not simulate remote target routing or staged 20/60-page
  cloud-assisted chapters. No production chapter dispatch path was added.
- **P11 CPU-only CI:** The pre-existing uncommitted Python 3.11 CI job remains
  scoped to gateway/provisioner unit tests and byte compilation. This host has
  Python 3.12, so these local runs do not prove the hosted Python 3.11 job ran.

### e-swarm review disposition

- P9 implementer `agy-quasar` `0a05659e-ff8a-4ada-8309-a09e9d4306f3`
  edited the scoped files but returned a progress-only response; its report is
  not counted as verification. Parallel original reviewers `agy-axiom`
  `10df04c0-269f-40d9-883a-a53e83262af8` and `agy-onyx`
  `d0f844bc-83f6-4159-ad78-b82d09a72d5e` reviewed independently. Axiom
  initially found none; Onyx found edit/removal button overlap, focus
  stranding, and Escape bubbling. Personal source inspection accepted those
  findings and additionally found the accepted-attempt recovery consequence.
  Separate fixers were `agy-vesper` `00c0e8f9-8a36-4546-9df1-55bf1f2c856f`
  (button exclusion), `agy-quasar` `a2ba6e71-e856-4f4d-8e8a-d7a8b60919b6`
  (focus; first overbuilt patch rejected and simplified in a pinned follow-up),
  `agy-tundra` `07a17f05-4370-4217-bbcf-b8f2ae0df86d` (Escape), and
  `agy-lumen` `92026fa7-0299-4fbf-bad0-6065bb7a6ad2` (recovery copy).
  Both original reviewers re-reviewed the final diff in parallel, approved the
  bounded local UX patch, and reported no remaining P0 to P3 findings. Approval
  does not establish provider lifecycle readiness.
- P10 implementer `agy-tundra` `52716064-b352-4fb4-a873-4af29c6c2859`
  timed out with a partial report while background checks were running; its
  claimed verification was rejected. Independent original reviewer
  `agy-zephyr` `21c3420d-3686-4e59-848d-62fdb56288da` found misleading
  unused remote-target mocks, duplicate cases, a stubbed mock-backend
  "integration" case, and manual run-state reset. I accepted these as one
  root finding: the test suite implied remote coverage it did not exercise.
  `agy-lumen` failed with a
  503 eligibility response; replacement independent reviewer `agy-nimbus`
  `8a5c2d0c-4a08-476b-8fe0-802d71bce0d1` found none, but its integration
  claim was rejected because `runClean` was stubbed. Separate fixer
  `agy-axiom` `9713ef8d-a971-4151-a463-e55cc289f179` replaced the entire
  misleading block with a narrow scope matrix. Both original completed
  reviewers, Zephyr and Nimbus, re-reviewed that final patch in parallel and
  found no remaining P0 to P3 issues within the unit-test boundary.

### Personally completed local checks

| Command | Exit | Observed result |
| --- | ---: | --- |
| `npx vitest run src/lib/dialogs/InferenceSettings.dom.test.js` | 0 | 27 passed after all P9 fixes. |
| `npx vitest run src/lib/state/editor.svelte.test.js` | 0 | 32 passed after the P10 fixer; four are new scope cases. |
| `npm test` | 0 | 52 files, 1,012 tests passed. |
| `npm run build` | 0 | Vite production build, 336 modules transformed. |
| `python3.12 -m unittest discover -q -s deploy/cloud/tests` | 0 | 84 gateway tests passed. |
| `python3.12 -m unittest discover -q -s provisioner` | 0 | 106 provisioner tests passed. |
| `python3.12 -m compileall -q deploy/cloud provisioner` | 0 | Both trees byte-compiled. |
| `cargo test -p manga-cleaner --lib inference::commands::tests::test_guard_cloud_execution_enabled_fails_closed -- --test-threads=1` | 0 | One hard-stop guard test passed; 418 filtered out. |
| Ruby YAML parse of `.github/workflows/ci.yml` | 0 | Python job parses as `ubuntu-latest`, Python `3.11`, five steps. |
| `git diff --check` | 0 | Current tracked diff has no whitespace errors. |

P9 remains incomplete until real, owned provider installations and lifecycle
actions can be verified. P10 remains incomplete until genuine P5/P6 crops,
durable provider-native recovery, authorization, and staged 20/60-page sessions
exist. P11 remains incomplete until hosted CI, signed packaged helpers on clean
macOS/Windows hosts, staging/GPU/recovery, token-scope, cleanup, and release
evidence exist. The uncommitted CI and UI/test/log changes are reviewable local
work only. The backend paid-submit guard stays closed.

## Cloud unlock and completion pass (2026-09-24; uncommitted)

The user asked for the cloud path to be unlocked, for a real automatic setup
that needs only pasted keys, for full interface wiring, and for a simple
first-launch setup. This pass did all four offline. Nothing ran against a real
Modal or Beam account, and no cloud resource was created, changed or billed.

### What changed

- **Backend unlock:** `guard_cloud_execution_enabled` now checks only the
  user's **Use a cloud GPU** switch (`cloud_disabled` when it is off). The
  paid-submit hard stop is gone. `submit_cloud_attempt` honours its simulate
  mode only in debug builds.
- **Routing:** FLUX is always the local helper, whichever endpoint is
  selected, and Cloud is its own choice. `edit()` refuses only an explicit
  cloud choice. A cloud-provenance re-run goes to the cloud unless the user
  names a local engine (`rerun_needs_cloud` in `src-tauri/src/region.rs`,
  mirrored by `rerunNeedsCloud` in `src/lib/api/tools.js`). Clean anyway
  always renders locally.
- **Automatic setup:** `provisioner/` was rewritten around real Modal and Beam
  drivers (`modal_driver.py`, `beam_driver.py`) with `inspect`, `plan`,
  `apply`, `resume`, `cleanup_plan` and `cleanup_apply`, progress lines on
  stderr, a resumable journal, and cleanup bound to the installation id. The
  old probe, matrix, template and fake-driver code was deleted.
  `test_sdk_fidelity.py` checks the fakes against the pinned SDKs (modal
  1.5.5, beta9 0.1.268, beam-client 0.2.211). See
  [cloud-provisioning.md](cloud-provisioning.md).
- **Gateways:** `deploy/cloud/` holds the CPU gateway for `/mc/v1`, durable
  job state, the GPU worker, weight seeding on CPU, and the Modal and Beam
  adapters.
- **Release:** `.github/scripts/build-cloud-provisioner.py` and a release
  workflow step bundle the helper as an `externalBin`. Without it, setup fails
  closed.
- **Interface:** setup on the Cloud tab (`CloudProvisioner.svelte`), the
  per-render consent dialog, the status card, recovery at start, and the
  six-step first-launch setup. See [cloud-frontend.md](cloud-frontend.md) and
  [features.md](features.md).
- **Docs:** [cloud-provisioning.md](cloud-provisioning.md) is new.
  [features.md](features.md), `README.md` and [findings.md](findings.md)
  (eight new unmeasured items) are updated. The phase records (`cloud-api`,
  `cloud-contract`, `cloud-consent`, `cloud-journal`, `cloud-security`,
  `cloud-verification`, `cloud-plan-validation`) keep their history and now
  say what is implemented since.

### e-swarm and review disposition

- Builders: two Claude subagents, one for the interface and one for the
  provisioner and gateways. Both hit the session limit and were resumed.
- Reviews: eight Gemini 3.8 Flash runs through the agy fleet (r1 to r8), and a
  Claude reviewer for the provisioner core after r4 failed twice with a 503.
  Six runs ran out the 25-minute print timeout with an empty answer; a second
  "write now" turn on the same conversation returned each answer.
  Conversations: r1 `1bb47c1c-2de3-41dd-aebd-fa8f87037906`, r2
  `e6ed0a73-a362-44b4-96ea-deb97994ed0e`, r3
  `f9e0d324-22d1-4cb8-91fb-e212cef48a34`, r5
  `65ba874e-76dd-462a-9a66-e11ac9d2749c`, r6
  `8ea745b0-a41e-4292-888e-b6474ce1f031`, r7
  `6a424f94-0143-49a9-966f-d4fc51718705`, r8
  `a3d90736-9027-4667-ae8e-c3eb6a716d31`. Doc sweep b1
  `8632d1b6-677e-4c98-a7a1-44a4df699773`; its status lines were corrected by
  hand where they claimed P10 and P11 were done.
- Accepted and fixed:
  - r1: Clean anyway with the cloud engine ran a local clean before consent.
    Clean anyway is now local only; Clean with > Cloud is the way to the cloud.
  - r1: closing an inline setup task moved focus to the wrong control. Focus
    now returns to the control that opened it, or to the endpoints heading.
  - r2, r3: the mock answered a live cancel with `acknowledged: true`. It now
    matches the Tauri shape (`handle: ''`, `acknowledged: false`).
  - r2, r3: `onconfigured` fired after a failed health check. It now carries
    `healthy`, and both hosts turn the cloud permission on only when it is
    true.
  - r6: a Beam seed task that vanished, or ended without writing `done`,
    made every Resume time out; a failed Beam map listing was reported as
    already deleted. Both drivers now journal a seed's start before starting
    it, wait on a seed that may still run, judge one gone only by the provider
    or a stale heartbeat, and replace it at most once per run. Beam map cleanup
    raises on a refused listing and verifies the map is empty.
  - r7: three weight-seeding log lines wrote raw exception text. They are
    redacted now, and the unused `RedactingLoggingFormatter` is gone.
  - Claude provisioner-core review (8 findings, all accepted): the app passed
    its whole environment to the helper, so `MODAL_SERVER_URL`,
    `MC_BEAM_GATEWAY_*` or `PYTHONPATH` could redirect a pasted key; the helper
    now starts with a cleared environment and an allowlist
    (`HELPER_ENV_ALLOWLIST`, with a test that fails without the clearing). A
    kill between two journal saves could lose the account id, which cleanup
    needs; it is now written in the first save, filled in on resume, and
    required by cleanup. A kill right after a seed started made Resume start a
    second one (fixed with the r6 design). The journal closed a file
    descriptor twice after a failed write. JSON-quoted key/value secrets
    slipped past the redaction pattern. Four tests did not test what their
    names said, and now do.
- Kept by design: pasted tokens stay in memory while the setup dialog is open
  after a failure, because Resume needs them and Modal shows its secret once.
  They are cleared on success, on close and on unmount.
- Dismissed after checking: r8's three Modal claims (`@modal.concurrent`,
  `Dict.put(..., skip_if_exists=True)` and the callable `ignore`) are all
  correct for modal 1.5.5, and Modal's own `_MountDir.get_files_to_upload`
  ships the expected 17 files with `_not_shipped`. r6's `test_drivers.py:280`
  finding: the Beam runtime credential is the user's own key by design. r5
  found nothing.

### Personally completed local checks

| Command | Exit | Observed result |
| --- | ---: | --- |
| `npm test` | 0 | 57 files, 1,094 tests passed. |
| `npm run build` | 0 | Vite production build, 351 modules transformed. |
| `cargo clippy -p cleaner-core -p manga-cleaner --no-deps --all-targets -- -D warnings` | 0 | No warnings. |
| `cargo test -p manga-cleaner` | 0 | 451 passed, 1 doc test ignored. |
| `cargo test -p cleaner-core` | 0 | 586 passed, 2 ignored. |
| `python3 -m unittest discover -q -s deploy/cloud/tests` | 0 | 182 ran, 11 skipped. |
| `python3 -m unittest discover -q -s provisioner` | 0 | 132 ran, 7 skipped without the SDKs. |
| Same, SDK venv (modal 1.5.5, beta9 0.1.268) and a scratch `HOME` | 0 | 132 ran, 0 skipped. |
| `python3 -m compileall -q deploy/cloud provisioner` | 0 | Both trees byte-compiled. |
| Dash scan of every changed and new file | 0 | No U+2014 or U+2013. |

### Acceptance

The milestone count does not change: every phase from P5 on still needs live
evidence. P5 to P9 are implemented offline. P10 is not implemented: chapter
runs stay local only. P11 is partial: CPU-only CI and the release step exist,
and nothing has run end to end against a real account.

## Live Modal check (2026-09-25; uncommitted)

The user signed the Modal CLI in to workspace `k-omiq` and approved one live run on
the FLUX render path, with cleanup afterwards. Beam has no account, so nothing ran
there. Cloud text analysis (M7) did not run: setup deploys no analysis worker.

**How it ran.** The setup helper ran from this checkout (`python -m provisioner`,
Modal SDK 1.5.5) with a scratch journal. A small script played the desktop's part: it
read the API token from `~/.modal.toml` and passed it on stdin, and nothing printed it.
Renders went through a Python client written to the `/mc/v1` contract, not through the
desktop's Rust client, keychain or interface. The crop was synthetic: a 512x384 speech
balloon with two lines of text on a dot tone, and a dilated text mask as the hint.
Installation `mc-lv0925`, GPU L4, idle 120 s.

| Step | Result |
| --- | --- |
| `inspect`, `plan` | Signed in, workspace `k-omiq`, environment `main`; plan listed a volume, a dict, the app and one proxy token |
| `apply` | Completed in 190.7 s: image build 124.4 s, deploy 2.6 s, weights 52.3 s for 5,475,930,180 bytes, token 2.3 s, health 6.5 s |
| `probe_compatibility` | Compatible, 997.6 ms, no GPU started |
| `/health` without the proxy token | HTTP 401 at Modal's edge |
| Cold render | Accepted in 2.39 s (16,560 bytes uploaded); running at 46.3 s; completed at 67.6 s; result 217,384 bytes in 3.29 s; 70.9 s from submit to result |
| Warm render (twice) | Completed at 7.9 s; 10.2 s and 10.0 s from submit to result |
| Determinism | Cold and warm results are the same bytes, and match the digest in the status document |
| Cancel after 1 s on a warm worker | Cancel acknowledged in 1.41 s; status `cancelled` at 5.0 s; no result |
| GPU memory | 7,176 MiB with the pipeline loaded; at most 7,180 MiB sampled every 250 ms during a render; L4 total 23,034 MiB |
| Host memory | Worker process RSS 7,121,456 kB after renders; a torch compile helper 5,213,984 kB (shared pages may count twice). The sandbox has no peak counter |
| Recovery after scale to zero | Status and result of the cold job still answered (HTTP 200, digest matches, 3.91 s). Only a CPU gateway container started, with no GPU |
| Refusals | Unknown handle: 404 `not_found`. Wrong request digest: 400 `invalid_digest`, `enqueued: false`, `retryable: false` |
| `cleanup_plan`, `cleanup_apply` | Deleted the proxy token, app, dict and volume in 3.8 s; none missing. Afterwards the CLI lists no volume, dict or container, the app record reads `stopped`, and the old proxy token gets HTTP 401 |
| Bill (`modal billing`) | Metered $0.15 just after the renders (deployed apps $0.14, ephemeral $0.01) and $0.10 a few minutes after cleanup, as Modal settled its metering; covered by credits; billed $0.00. Per resource for the app: L4 $0.0631, CPU $0.0181, memory $0.0090 |

The gateway reported no cost (`reported_cost_usd` null), so the consent dialog still
shows the cost as unknown. The billing report also listed an A10G line at $0 for the
app; the worker ran on an L4.

**Not covered by this run:** the desktop app driving setup and renders (Rust client,
keychain, WKWebView consent), real manga crops, larger crops, a Modal workspace with
environment access control, how long job records stay readable, Dict size limits, a
render past the 15-minute bound, a setup interrupted live, and every Beam item.
