# Milestone ledger, M0 to M8

Status on 25 September 2026, branch `codex/cloud-integration`, uncommitted work tree. The criteria are the
deliverables and exits in [phased-implementation-plan.md](phased-implementation-plan.md); nothing here
widens them. Test names are Rust or Vitest tests in this tree. "Verified" means the code was read against
the criterion and the named test passed in the integration run below; it does not mean a model-quality
claim. "Blocked" means the remaining step needs something this machine or session cannot supply.

Owner is who landed the work, or who must act on what remains.

## Integration run

Run on 25 September 2026 on the MacBook Air Mac17,3 (Apple M5, macOS 27.0 arm64), after every fix below
had landed. The commands are the ones CI runs, run locally.

| Command | Result |
| --- | --- |
| `cargo clippy -p cleaner-core -p manga-cleaner --no-deps --all-targets -- -D warnings` | Passed |
| `cargo test --workspace --exclude spike-strip-memory -- --test-threads=1` | 1184 passed, 0 failed, 20 ignored |
| `npx vitest run` | 68 files, 1438 passed, 0 failed |
| `npm run build` | Built; only the existing chunk-size warning |
| `python3 -m unittest discover -s deploy/cloud/tests` | 200 passed, 12 skipped (Starlette and provider SDKs not installed) |
| `python3 -m unittest discover -s provisioner` | 133 passed, 7 skipped |
| `python3 -m unittest test_measure` in `spikes/holdout-metrics` | 3 passed |
| `python3 -m compileall deploy/cloud provisioner` | Passed |

Not run here: the Windows `x86_64-pc-windows-msvc` clippy and test jobs, and any live cloud, Windows,
Linux or WKWebView check. Hands-on checks used the Vite mock in Chromium only.

## M0: pin, reproduce, classify

| Criterion | Evidence | Status | Owner | Remaining issue |
| --- | --- | --- | --- | --- |
| Build identity, model and runtime hashes, project format, installed models | [m0-m1-evidence.md](m0-m1-evidence.md) table; installed weights rechecked against the catalogue SHA-256 values | Verified | orchestrator | Provider inventory is macOS arm64 only |
| Rights-cleared unmarked Japanese mono, Korean colour, SFX, no-text, long strip and synthetic pages | Synthetic fixtures in the test suites; saved 45-page Korean colour chapter used for candidate agreement only | Blocked | user | No rights ledger and no unmarked labeled corpus |
| Per-region ledger with category and artifacts | `spikes/m0-ledger` on 3 pages of the user's scans: 22 regions, 21 cleaned, 1 held by policy; every reason key mapped explicitly, unknown keys reported as `unknown:<key>`; proxy artifacts carry a `_proxy` name | Partial | C5, C5fix | Missed discovery, bad inpainting and weak mask need human labels; empty detector seeds get no row |
| Baseline recall, false candidates, correction time, exposure, latency, peak memory | `spikes/holdout-metrics` computes every metric from labels; CPU page times 61.1 s, 19.7 s, 27.4 s, cumulative peak RSS about 2.49 GB | Blocked | user | No labeled holdout, no human review |
| Test and benchmark commands, macOS labeled as macOS only | Integration run above | Verified | orchestrator | None |
| Parity tolerances, quality promotion thresholds and per-device budgets set after the baseline | SAM parity is exact-mask on a fixed corpus ([provider-qualification.md](provider-qualification.md)); no quality threshold or device budget has been set | Blocked | user | Needs the labeled baseline; measured memory figures are recorded as observations, not budgets |
| Exit: every reported sample failure has a category and a reproducible artifact; no published score presented as an app benchmark | The synthetic late-edit failure is classified with its fixture (M1); the ledger harness writes per-region artifacts; docs label published scores as external | Partial | user | Real-page failures need human categories |

## M1: repeated edits and cloud crop identity

| Criterion | Evidence | Status | Owner | Remaining issue |
| --- | --- | --- | --- | --- |
| Two-stroke and neighbour fixtures for automatic, AI, paint, clone, alpha/depth and strip joins | `a_later_ai_stroke_reads_the_visible_first_stroke`, `later_ai_stroke_reads_a_real_paint_patch`, `clone_source_dependency_tracks_visible_input_and_ignores_unrelated_change`, `detected_region_reads_prior_visible_patch_and_records_input`, `rgba8_and_sixteen_bit_renderers_receive_visible_composite`, `prepared_text_shape_near_a_strip_join_reads_the_same_lower_context_at_apply` | Verified | C1a | None |
| One bounded composited underlay for every local operation; undersized window refused | `underlay.rs` `insufficient_model_context_is_refused`; `clean_anyway_held_back_region_reads_visible_hand_patch` | Verified | C1a | Rerun and Clean anyway commands are tested through helpers, not the command path |
| Cloud consent, dispatch and attach from the same underlay with digest checks | `consent_rejects_a_changed_visible_predecessor_even_when_target_is_unchanged`, `changed_underlay_refuses_dispatch_without_spending_grant`, `changed_predecessor_inside_read_window_refuses_paid_dispatch_and_preserves_grant`, `predecessor_change_mid_poll_stales_one_paid_job_without_attaching` | Verified | C7, C7fix, SW-E | Fixed: the dispatch-time hash check was missing before C7; recovery now also checks both underlay hashes (`test_recovery_detects_underlay_and_predecessor_drift`) |
| Gestures serialized per page; cancel, delete and page switch cannot misplace a patch; cloud stroke is one undo | `drawing.svelte.test.js` (one undo step per gesture, cloud stroke deleted or undone while pending, render landing after its page left memory), `lateresults.test.js` (tool, re-run, Clean anyway, delete and undo answers after eviction or a chapter switch land on their own chapter and can be undone after reopening) | Verified | U3, F1, F1c | Fixed: `createMask` selected a region after a page switch (U3), and late answers were dropped or recorded on the wrong chapter (F1c). A plain undo pressed while a cloud render is pending undoes the previous entry |
| Provenance, Needs review with Keep, Rebuild, Undo; Try again replaces, new stroke refines; retry disabled on paint and clone | `retry_refuses_non_replayable_paint_and_clone_patches`, `dependency_keep_preserves_pixels_and_unrelated_edits_leave_b_alone`, `legacy_retry_restores_exact_pixels_after_reopen`, `MaskRow.dom.test.js` | Verified | C1a, C1b, U3 | Needs review Undo reverses the last chapter action, and is labeled so |
| Koharu and repository LaMa and boundary ramps compared on identical inputs before any model or blend change | No model or blend change was made on this branch, so the comparison was not triggered | Not triggered | none | Required before a future LaMa or blend change |
| Underlay cache decided from measurement | `bounded_4000x6000_dense_history_benchmark`: Gray8 window 2,611,456 B, RGB8 window 7,834,368 B; no cache added | Verified | C1a | Numbers are not budgets |
| Exit: pixels outside each write mask equal the underlay; preview equals lossless export | `each_layer_preserves_its_immediate_underlay_outside_the_mask_and_export_matches_preview` | Verified | C1a | Synthetic only |

## M2: exact models and ONNX parity

| Criterion | Evidence | Status | Owner | Remaining issue |
| --- | --- | --- | --- | --- |
| SAM-TS-L pinned, strictly loaded, reference reproduced, mask-only graph exported | `spikes/sam-ts-l` (PROVENANCE, export script, `results/initializer-key-map.json`); `prepared_rgb_matches_saved_pytorch_input`, `restored_mask_matches_saved_reference`, `native_cpu_graph_matches_saved_mask`; [model-workflow-benchmarks.md](model-workflow-benchmarks.md) page 07 table | Verified | earlier work, C2 | FP32 only; page 27 JPEG decode differs from Pillow by 666 mask pixels |
| RT-DETR-v2 ONNX parity | `chapter_reference_pages_keep_regions_and_context`, `numpy_box_rounding_is_even` | Partial | earlier work | Full graph does not merge boxes across its two-tile seam |
| COO MTSv3 export or named blocker | `spikes/coo-mtsv3` research; [model-rights-decision.md](model-rights-decision.md) excludes COO from the product | Blocked | rights reviewer | Noncommercial terms; no product path |
| Exit: reproducible manifests, export scripts and reports, fixed-input parity tables and error examples | `spikes/sam-ts-l` (PROVENANCE, export script, manifests, parity results), [model-workflow-benchmarks.md](model-workflow-benchmarks.md), [provider-qualification.md](provider-qualification.md) page 27 error example | Verified | earlier work | SAM only; COO research only |
| Rights reviewed before downloads or bundling are promoted | [model-rights-decision.md](model-rights-decision.md): local hash-pinned user import only | Blocked | rights reviewer | No hosting or bundling of RT or SAM graphs |

## M3: versioned text-shaped mask plan

| Criterion | Evidence | Status | Owner | Remaining issue |
| --- | --- | --- | --- | --- |
| `legacy` or `text_shape` policy; missing means legacy; unknown fails | `unknown_geometry_fails_while_missing_defaults_to_legacy`, `an_unknown_geometry_policy_is_not_migrated_to_legacy`, `legacy_edits_keep_v3_and_text_shape_plan_upgrades_to_v4` | Verified | earlier work, C1b | None |
| Round source-pixel padding from base plus corrections; 2, 5, 2 reproduces 2 | `padding_rebuild_2_5_2_is_byte_identical`, `round_center_distance_and_edges_preserve_topology`, `zero_padding_is_exact_corrected_base_and_hole_is_independent` | Verified | earlier work | None |
| Every engine writes only W or declines | `every_enabled_local_engine_is_clamped_to_exact_support_on_fake_output`, `exact_engines_decline_unsupported_source_modes`, `text_shaped_region_is_refused_at_proposal_without_mutation` | Verified | earlier work, C1a | Cloud FLUX declines text-shaped regions |
| Versioned records; apply needs the same source and support hash; W checked again at load and export | `text_shape_commit_refuses_a_patch_mask_different_from_preview_support`, `tampered_text_shape_mask_is_rejected_on_patch_load`, `tampered_text_shape_sidecar_refuses_export_before_any_output` | Verified | C1b | Windows WebView2 preview budget not measured |
| Mixed patches, undo and redo, long strips, mode, depth, ICC, layered export | `mixed_legacy_and_text_shape_patches_reopen_and_export_the_same_composite`, `text_shape_saved_patches_preserve_source_samples_mode_depth_and_icc_on_lossless_export`, `layered_text_shape_uses_sparse_write_support_as_its_mask` | Verified | earlier work, C1b | None |
| Exit: bounded 4000x6000 page measured | `benchmark_4000_by_6000_decoded_page_and_prepared_mask`, `a_4000_by_6000_page_keeps_a_small_plan_region_bounded` | Verified | earlier work | macOS only |

## M4: workflow, policies and review UI

| Criterion | Evidence | Status | Owner | Remaining issue |
| --- | --- | --- | --- | --- |
| Per-language picks and skip affect a native run | `native_run_language_selection_and_optional_reader_are_explicit`, `skipped_language_is_held_in_the_page_clean_path`, `interrupted_run_restores_its_captured_policy_instead_of_current_defaults`, `session-detection.test.js` | Verified | C3 | None |
| Capability graph in setup and Settings; one row per model; files, checks and delete in details | `logical_model_groups_contain_every_dependency_but_no_shared_detector`, `pipelines.test.js`, Settings Detection | Verified | U1, F1, F1b | Fixed in F1: Settings readiness now needs the runtime to load (`diagnostics`), and single-file models have per-file Check and Delete. First-launch setup and the review's readiness still count an installed runtime as ready |
| No-recognition path never opens gate or OCR files | Code read: `src-tauri/src/model_workflows.rs` never references the script gate or manga-ocr; `each_workflow_opens_only_its_requested_models` pins which of RT and SAM each workflow opens | Verified by code read | earlier work | No test observes file opens; an all-text Auto clean does not exist (below) |
| An explicit all-text run bypasses recognition | All-text cleaning is only the supervised review: `run_clean` refuses `all_text` ("All-text cleaning requires the RT-DETR + SAM-TS review workflow") and Auto clean opens the review instead | Partial by decision | user | Automatic text-shaped cleaning is a No go until M5 quality evidence exists ([local-text-shape-release.md](local-text-shape-release.md)) |
| Prepared-mask preview in the editor with padding, corrections, components | `WorkflowAnalysis.dom.test.js`, prepare and apply bind the support SHA-256 | Verified in Chromium | U2, U4 | Fixed in U4: tints are canvases (`MaskTint.svelte`), exact pixel counts checked at fit, 1:1 and 400% in the mock. Not run in WKWebView |
| Specific outcomes | `workflowoutcome.test.js` | Verified | U2, U4 | All eight named outcomes in the plan have their own text (legacy script rejection is the legacy review reason). File errors and three internal tile-planner errors still show the generic failure |
| Background download failures shown outside Settings | `model-download-notices.test.js`, wired in `App.svelte` | Verified | U3 | None |
| New stroke versus Try again, dependency review, one gesture one undo | `MaskRow.dom.test.js` (Try again semantics and blocked retry, keyboard included), `drawing.svelte.test.js` (one undo step per gesture, including a cloud stroke deleted or undone while its render is pending) | Verified | U3, F1 | A plain undo while a cloud render is pending undoes the previous entry |
| Engine stars re-rated only from one common benchmark | No common benchmark exists; Settings labels the ratings as provisional estimates | Blocked | user | Needs a shared benchmark run |
| Exit: same hash, zoom/DPR, keyboard, return to old workflow | Chromium mock walk-through and DOM tests | Partial | U2 | Not run in WKWebView |

## M5: local ONNX integration and benchmark

| Criterion | Evidence | Status | Owner | Remaining issue |
| --- | --- | --- | --- | --- |
| Segmenter independent of CTD; RT and SAM association; unassociated components reviewable | `fusion.rs` tests, `associated_detector_bounds_are_kept_as_the_candidate_locator` | Verified | earlier work | None |
| SAM graphs pinned and loaded on demand; residency and footprint measured | `sam_admission_releases_only_parked_competing_sessions`, `managed_sam_removal_hides_both_graphs_together`, `macos_task_footprint_is_measured`, `cancel_before_and_between_stages_never_commits_review_evidence` | Verified | earlier work, C2 | Apple M5 only; eviction reload cost not measured |
| Useful without COO; COO compared | Workflow runs RT plus SAM without COO | Partial | rights reviewer | COO comparison blocked by rights |
| Partial package against full reference | Key map and page 07 parity | Verified | C2 | Fixed corpus only |
| Exit: holdout metrics by slice, human mask and reconstruction review | Tooling only | Blocked | user | No labeled holdout |

## M6: local release candidate and rollback

| Criterion | Evidence | Status | Owner | Remaining issue |
| --- | --- | --- | --- | --- |
| Feature-gated optional mode, legacy default | Editor control only for `all_text`; legacy default in settings | Verified | earlier work, U2 | None |
| Model-missing and model-failure messages | `absent_optional_reader_reports_rescue_off_before_cleaning`, `notice.run.ocrRescueUnavailable`, download notices | Verified | C3, U3 | None |
| Artifact permission decision | [model-rights-decision.md](model-rights-decision.md) | Verified | earlier work | Local import only |
| Migration and rollback fixtures | `a_version_one_manifest_still_opens_and_is_rewritten_at_the_legacy_floor`, `legacy_edits_keep_v3_and_text_shape_plan_upgrades_to_v4` (reads through the `v3_manifest.rs` schema), `an_unknown_geometry_policy_is_not_migrated_to_legacy` | Verified | C1b | Opening a v4 manifest in an older binary is not qualified |
| User documentation and known-failure gallery | [local-text-shape-release.md](local-text-shape-release.md), [features.md](features.md) | Verified | orchestrator | features.md now covers source languages and the optional review; findings.md stale bullets replaced |
| Providers exercised on their machines | [provider-qualification.md](provider-qualification.md): Apple M5 WebGPU | Blocked | hardware | Windows and Linux not run |
| Lossless export, mixed chapters, formats, cancel, memory pressure | Tests named under M3 and M5 | Verified | earlier work | Synthetic and M5 only |
| Padding default, threshold, overlap, provider limits from M5 evidence | Not decided | Blocked | user | Needs the labeled holdout |
| Exit: disabling leaves patches viewable, old output unchanged, no silent swap, enlargement or upload | Migration and export tests; component write qualified only for PNG plus M5 WebGPU | Verified | earlier work | Release claim is narrow by design |

## M7: optional cloud compute

Only offline work is possible here: no real Modal or Beam account was used, so M7 cannot exit.

| Criterion | Evidence | Status | Owner | Remaining issue |
| --- | --- | --- | --- | --- |
| FLUX cloud path stays per region and consented; M1 composited-input fix first | M1 cloud row above; per-render consent (`CloudConsentDialog.dom.test.js`); cloud off by default | Verified | C7 | None |
| New versioned capability and result contract, not the FLUX hint contract relabeled | `/mc/analysis/v1` with `text_mask_sam_ts@1` and `text_regions_rt@1`: `cloud_analysis_vectors`, `cloud_analysis_capability_and_rejection_are_strict`, `cloud_analysis_mask_geometry_is_bound_to_tile`, `cloud_analysis_shared_wire_response_is_bounded`, `cloud_analysis_shared_failure_fixtures`; Python `test_analysis_v1.py` (`test_shared_vectors_and_collisions`, `test_sam_mask_is_tile_sized_and_result_bounded`, `test_rt_boxes_and_asgi_round_trip`, `test_beam_stripped_mount_paths_route_and_enforce_auth`); `/mc/v1` FLUX fixtures unchanged | Verified | C6, SW-F, SW-Ffix | Setup deploys no analysis worker, so a live route answers a structured 503 |
| Page and tile consent shows upload extent (with surrounding art), model revision, outputs, coordinates, transfer bounds, and a cost or an honest unknown | `CloudAnalysis.dom.test.js`: "shows exactly what would be sent and sends nothing until both statements are checked", "shows a cost the proposal carries as an estimate"; `CloudAnalysisConsent.svelte` | Verified | C7, U4 | Cost is always unknown without a real account |
| Provider input rights, training and retention confirmed before upload | Consent needs a rights attestation and a retention acknowledgement: `analysis_consent_submit_and_review_evidence_stay_review_only` checks both refusals | Partial | C7, user | The user attests; the actual provider tier terms were not reviewed |
| Authorization, journal, recovery and cancellation | Grants carry a capability (`legacy_grants_default_to_flux_capability`, `expired_proposal_cannot_mint_an_analysis_grant`); journal (`analysis_journal_recovers_submitted_tile_as_unknown_without_changing_flux_schema`, `analysis_journal_recovers_completed_cached_tile_and_persists_unknown`, `recovered_submitted_tile_discard_keeps_remote_outcome_unknown`); cancel (`cancel_before_submitted_mark_prevents_dispatch`, `cancel_after_submitted_mark_reports_one_in_flight_tile`, `live_cancel_between_tiles_never_sends_next_tile`) | Verified with fakes | C7, C7fix, C7fix2, SW-E | One synchronous request per tile: a tile already sent cannot be cancelled remotely |
| Local plan approval and final composition kept; a remote result cannot widen the write mask | `remote_evidence_cannot_prepare_or_apply_a_component_write`; `CloudAnalysis.dom.test.js` "hands the evidence to the review with where it came from" | Verified | C7, U4 | Remote evidence is review only, so it cannot write at all |
| No duplicate uploads, provider racing, automatic paid retries or implicit chapter uploads | One page per consent; `predecessor_change_after_submission_stales_without_second_job`; `CloudAnalysis.dom.test.js` "names the outcome without retrying", "does not send it again" | Verified | C7, U4 | None |
| Local path independent of account availability | `cloud_unconfigured_does_not_affect_local_analysis_contract` | Verified | C7 | None |
| Remote SAM or COO only where measured local memory or latency justifies it; COO excluded | COO never offered (`CloudAnalysis.dom.test.js` "never offers the SFX finder"); no remote measurement | Blocked | user | Needs a real account to measure upload and GPU start against the local path |
| Checkpoint and training-lineage rights reviewed for remote use | [model-rights-decision.md](model-rights-decision.md): no hosted RT or SAM | Blocked | user | Rights chain review |
| Exit: real-account Modal and Beam staging with latency, memory, transfer, bill, recovery, cancel and cleanup | The FLUX path ran live on Modal (see M8); cloud text analysis did not, because setup deploys no analysis worker | Blocked | user | Needs a deployed SAM or RT worker (a rights decision), a Beam account and authorized sample crops |

## M8: platform, packaging, product evidence

The exit artifact is [release-evidence-matrix.md](release-evidence-matrix.md).

| Criterion | Evidence | Status | Owner | Remaining issue |
| --- | --- | --- | --- | --- |
| Live cloud account checks: bills, slow and expired renders, Beam map cleanup, provider auth and retention | Modal, 25 September 2026 ([cloud-work-log.md](cloud-work-log.md)): setup, edge auth refusal, cold and warm render, cancel, recovery after scale to zero, cleanup and a $0.10 to $0.15 metered bill | Partial | orchestrator, user | Beam has no account; slow and expired renders, retention and access-controlled workspaces not run; the desktop app did not drive the run |
| Shipped WKWebView event and consent flow | Chromium mock and jsdom only | Blocked | user | Needs a run of the real app |
| Windows frozen helper build and child-process cancel | Release workflow step exists; not run | Blocked | hardware | Needs Windows |
| Windows and Linux runtime, WebGPU plugin, DirectML, CUDA, protocol throughput, GPU peaks, CRT packaging, tray and single instance | Arguments from binaries only | Blocked | hardware | Needs Windows and Linux machines |
| Modal token creation against journal interruption | `test_kill_after_token_create_reports_orphan_on_resume`, `test_a_probed_token_id_stays_readable_for_cleanup`; `ERR_ORPHANED_TOKEN` view in Settings | Verified with stand-ins | C4, F1 | The user removes the token in the Modal dashboard |
| Settings write races | `concurrent_patches_keep_both_fields` (one lock) | Verified | C4 | None |
| FLUX cloud path off by default with per-render approval | Default `cloudAllowed: false`; `CloudConsentDialog.dom.test.js` | Verified | earlier work | None |
| Rebench stars; replace Soon only after real artifacts | Ratings labeled provisional; Big LaMa and Qwen drawn disabled | Blocked | user | Needs a common benchmark |
| Monochrome macOS tray asset | `src-tauri/assets/tray/` and its generator script | Done, not seen | U5 | Not seen in a real menu bar |
| PSD 2 GB ceiling guarded | `oversized_psd_is_refused_from_a_header`, `too_many_psd_layers_are_refused_before_writing`, `a_large_sixteen_bit_layered_page_is_refused_before_writing` | Verified | C4, SW-B | Photoshop open not run |
| Pre-import chapters, original scan replacement and ingest free space documented | [findings.md](findings.md) library bullet; `jpeg_import_estimate_bounds_png_without_assuming_rgba16`, `cmyk_import_estimate_uses_tiff_output_size` | Verified | C1b, SW-B | The estimate errs high |
| Targeted fixtures: script rescue, adopted box, paper reading, long strip | `punctuation_changes_the_one_kana_rescue_floor`, `the_rescue_thresholds_are_the_ones_the_decision_log_records`, `a_box_just_above_the_adoption_floor_still_needs_gate_review`, `an_enormous_adopted_box_is_flagged_large`, `a_plate_that_is_mostly_frame_is_not_read_as_paper`, `a_box_taller_than_the_strip_overlap_is_held_at_its_full_extent`, `distinct_boxes_meeting_at_a_strip_cut_stay_separate`, `a_duplicated_join_is_reported_on_the_stream` | Verified | C3, B1, SW-D | Thresholds are floors with reasons, not measurements |
| Report when an optional reader or model fails to open | `a_reader_that_is_absent_or_will_not_open_leaves_the_gate_standing`, `absent_optional_reader_reports_rescue_off_before_cleaning` | Verified | C3 | The nonempty corrupt file case is skipped when gate weights are absent |
| Sidecar release floor, CUDA backend, context clamp | Not current product priorities | Not triggered | user | Measure only if they become priorities |
| Exit: evidence matrix labels each OS and provider combination; claims and defaults match | [release-evidence-matrix.md](release-evidence-matrix.md) | Verified | orchestrator | Most rows are Unknown or Tests only |
