/**
 * The Tauri adapter - the real backend, as far as it goes.
 *
 * The seam contract fixes thirty-six methods and six events, and the
 * core cannot yet answer all of them. That is not a reason to wait: `setBackend`
 * exists so a partial adapter can serve what it implements while the mock
 * serves the rest, which is what lets the pipeline be built bottom-up against a
 * finished interface rather than integrated in one jump at the end.
 *
 * So this is a **delegating** adapter. Every method is either
 *
 *   - `invoke`'d against a Tauri command, or
 *   - forwarded to `fallback` unchanged,
 *
 * and `IMPLEMENTED` below is the list of which - one place to read, and one
 * place to edit when a command lands. A method missing from both is a bug the
 * tests catch: the adapter must satisfy the whole interface or it is not one.
 *
 * Nothing here imports `@tauri-apps/api`. `withGlobalTauri` puts `invoke` on the
 * window, and taking it from there keeps this module loadable - and testable -
 * in a plain browser and in vitest, where a Tauri import would throw at load.
 */

import { createEventStream } from './tauri-events.js'

/**
 * The Tauri commands this adapter uses, by seam method name.
 *
 * Everything not in here is the mock's, and stays the mock's until the core can
 * answer it honestly.
 *
 * All six region-level edits are commands now. `deleteMask` and `restoreRegion`
 * landed first because they need **no engine** - a mask is deleted by turning a
 * persisted `visible` flag off, which the composite already honours - and the
 * other four each run a rung over one region in `src-tauri/src/region.rs`.
 * Leaving any of them with the mock
 * meant the control silently did nothing in a real window: the mock holds
 * fixture chapters, so a region id from the library matches nothing in it and
 * every call answered `null`.
 *
 * `sidecarAvailable` is the one method the seam grew for rung 3a. It is a
 * question about the machine rather than about a project, and it is asked once
 * so that a rung nobody installed is never offered rather than offered and
 * refused.
 */
const IMPLEMENTED = Object.freeze({
  about: 'about',
  diagnostics: 'diagnostics',
  readSettings: 'read_settings',
  writeSettings: 'write_settings',
  readInferenceConfig: 'read_inference_config',
  writeInferenceConfig: 'write_inference_config',
  selectCloudProfile: 'select_cloud_profile',
  storeCloudSecret: 'store_cloud_secret',
  deleteCloudSecret: 'delete_cloud_secret',
  getCloudSecretSummary: 'get_cloud_secret_summary',
  forgetCloudSecretDenials: 'forget_cloud_secret_denials',
  checkCloudConnection: 'check_cloud_connection',
  getCloudModelInfo: 'get_cloud_model_info',
  checkCloudRelease: 'check_cloud_release',
  getCloudGpuStatus: 'get_cloud_gpu_status',
  stopCloudGpu: 'stop_cloud_gpu',
  listRemoteAnalysisCapabilities: 'list_remote_analysis_capabilities',
  proposeRemoteAnalysis: 'propose_remote_analysis',
  confirmRemoteAnalysis: 'confirm_remote_analysis',
  cancelRemoteAnalysis: 'cancel_remote_analysis',
  getRemoteAnalysisStatus: 'get_remote_analysis_status',
  proposeRunAnalysis: 'propose_run_analysis',
  confirmRunAnalysis: 'confirm_run_analysis',
  cancelRunAnalysis: 'cancel_run_analysis',
  resolveQwenReview: 'resolve_qwen_review',
  prepareCloudClean: 'prepare_cloud_clean',
  confirmCloudClean: 'confirm_cloud_clean',
  startCloudClean: 'start_cloud_clean',
  cancelCloudClean: 'cancel_cloud_clean',
  denoisePresets: 'denoise_presets',
  benchmarkDenoiseLocal: 'benchmark_denoise_local',
  denoiseChapterLocal: 'denoise_chapter_local',
  cancelDenoiseLocal: 'cancel_denoise_local',
  replaceWithDenoised: 'replace_with_denoised',
  denoiseHistory: 'denoise_history',
  denoiseCompareImage: 'denoise_compare_image',
  cloudDenoisePresets: 'cloud_denoise_presets',
  prepareCloudDenoise: 'prepare_cloud_denoise',
  confirmCloudDenoise: 'confirm_cloud_denoise',
  startCloudDenoise: 'start_cloud_denoise',
  cancelCloudDenoise: 'cancel_cloud_denoise',
  prepareCloudConsent: 'prepare_cloud_consent',
  confirmCloudConsent: 'confirm_cloud_consent',
  submitCloudAttempt: 'submit_cloud_attempt',
  getCloudAttemptStatus: 'get_cloud_attempt_status',
  getCloudAttemptResult: 'get_cloud_attempt_result',
  cancelCloudAttempt: 'cancel_cloud_attempt',
  reconcileCloudRecovery: 'reconcile_cloud_recovery',
  runCloudProvisioner: 'run_cloud_provisioner',
  cancelCloudProvisioner: 'cancel_cloud_provisioner',
  listProjects: 'list_projects',
  createProject: 'create_project',
  createChapter: 'create_chapter',
  openChapter: 'open_chapter',
  loadPages: 'load_pages',
  historyLoad: 'history_load',
  historyPush: 'history_push',
  historyMove: 'history_move',
  renameProject: 'rename_project',
  deleteProject: 'delete_project',
  deleteChapter: 'delete_chapter',
  exportChapter: 'export_chapter',
  planExportChapter: 'plan_export_chapter',
  deleteMask: 'delete_mask',
  restoreRegion: 'restore_region',
  setLayerStyle: 'set_layer_style',
  keepDependencyResult: 'keep_dependency_result',
  setDetectionType: 'set_detection_type',
  applyTool: 'apply_tool',
  createRegion: 'create_region',
  editDetectionMask: 'edit_detection_mask',
  setDetectionPadding: 'set_detection_padding',
  previewPaint: 'preview_paint',
  rerunMask: 'rerun_mask',
  cleanAnyway: 'clean_anyway',
  sidecarAvailable: 'sidecar_available',
  listSidecarModels: 'list_sidecar_models',
  installFluxHelper: 'install_flux_helper',
  runClean: 'run_clean',
  cancelRun: 'cancel_run',
  resumeJob: 'resume_job',
  listLoadedModels: 'list_loaded_models',
  unloadModel: 'unload_model',
  listAccelerators: 'list_accelerators',
  listModels: 'list_models',
  downloadModel: 'download_model',
  downloadModelGroup: 'download_model_group',
  cancelDownload: 'cancel_download',
  deleteModel: 'delete_model',
  deleteModelGroup: 'delete_model_group',
  discardPartial: 'discard_partial',
  verifyModel: 'verify_model',
  verifyModelGroup: 'verify_model_group',
  downloadRuntime: 'download_runtime',
  deleteRuntime: 'delete_runtime',
  listWorkflowCapabilities: 'list_workflow_capabilities',
  importFullRt: 'import_full_rt',
  removeFullRt: 'remove_full_rt',
  importSamTs: 'import_sam_ts',
  installSamTs: 'install_sam_ts',
  removeSamTs: 'remove_sam_ts',
  verifySamTs: 'verify_sam_ts',
  analyzeCapabilities: 'analyze_capabilities',
  analyzeChapterPage: 'analyze_chapter_page',
  cancelCapabilityAnalysis: 'cancel_capability_analysis',
  prepareComponentWrite: 'prepare_component_write',
  loadComponentCorrection: 'load_component_correction',
  applyComponentWrite: 'apply_component_write',
  listJobs: 'list_jobs',
  confirmQuit: 'confirm_quit',
  hideToTray: 'hide_to_tray',
})

/** Every method the seam contract fixes, so the adapter can be checked against it. */
export const SEAM_METHODS = Object.freeze([
  'subscribe',
  'listProjects',
  'createProject',
  'createChapter',
  'openChapter',
  'loadPages',
  'historyLoad',
  'historyPush',
  'historyMove',
  'renameProject',
  'deleteProject',
  'deleteChapter',
  'resumeJob',
  'runClean',
  'cancelRun',
  'applyTool',
  'createRegion',
  'editDetectionMask',
  'setDetectionPadding',
  'previewPaint',
  'deleteMask',
  'restoreRegion',
  'setLayerStyle',
  'keepDependencyResult',
  'setDetectionType',
  'rerunMask',
  'cleanAnyway',
  'exportChapter',
  'planExportChapter',
  'sidecarAvailable',
  'listSidecarModels',
  'installFluxHelper',
  'readSettings',
  'writeSettings',
  'readInferenceConfig',
  'writeInferenceConfig',
  'selectCloudProfile',
  'storeCloudSecret',
  'deleteCloudSecret',
  'getCloudSecretSummary',
  'forgetCloudSecretDenials',
  'checkCloudConnection',
  'getCloudModelInfo',
  'checkCloudRelease',
  'getCloudGpuStatus',
  'stopCloudGpu',
  'listRemoteAnalysisCapabilities',
  'proposeRemoteAnalysis',
  'confirmRemoteAnalysis',
  'cancelRemoteAnalysis',
  'getRemoteAnalysisStatus',
  'proposeRunAnalysis',
  'confirmRunAnalysis',
  'cancelRunAnalysis',
  'prepareCloudClean',
  'confirmCloudClean',
  'startCloudClean',
  'cancelCloudClean',
  'denoisePresets',
  'benchmarkDenoiseLocal',
  'denoiseChapterLocal',
  'cancelDenoiseLocal',
  'replaceWithDenoised',
  'denoiseHistory',
  'denoiseCompareImage',
  'cloudDenoisePresets',
  'prepareCloudDenoise',
  'confirmCloudDenoise',
  'startCloudDenoise',
  'cancelCloudDenoise',
  'prepareCloudConsent',
  'confirmCloudConsent',
  'submitCloudAttempt',
  'getCloudAttemptStatus',
  'getCloudAttemptResult',
  'cancelCloudAttempt',
  'reconcileCloudRecovery',
  'runCloudProvisioner',
  'cancelCloudProvisioner',
  'onProvisionProgress',
  'onQwenReview',
  'resolveQwenReview',
  'onCloudAttempt',
  'onRemoteAnalysis',
  'onDenoiseProgress',
  'onDenoiseRecorded',
  'about',
  'diagnostics',
  'listLoadedModels',
  'unloadModel',
  'listAccelerators',
  'listModels',
  'downloadModel',
  'downloadModelGroup',
  'cancelDownload',
  'deleteModel',
  'deleteModelGroup',
  'discardPartial',
  'verifyModel',
  'verifyModelGroup',
  'downloadRuntime',
  'deleteRuntime',
  'listWorkflowCapabilities',
  'importFullRt',
  'removeFullRt',
  'importSamTs',
  'installSamTs',
  'removeSamTs',
  'verifySamTs',
  'analyzeCapabilities',
  'analyzeChapterPage',
  'cancelCapabilityAnalysis',
  'prepareComponentWrite',
  'loadComponentCorrection',
  'applyComponentWrite',
  'listJobs',
  'confirmQuit',
  'hideToTray',
  'onQuitRequested',
])

/**
 * The seam methods that are listeners rather than calls.
 *
 * `subscribe` is the run stream, merged with the fallback's. The others are
 * Tauri events the core emits while a helper, a render or a local denoise is
 * running (`provision://progress`, `cloud://attempt`, `denoise://progress`),
 * and each answers with a promise of its unlisten function, the shape Tauri's
 * own `listen` has. None of them is a command, so none is in `IMPLEMENTED`.
 */
export const EVENT_METHODS = Object.freeze(['subscribe', 'onProvisionProgress', 'onQwenReview', 'onCloudAttempt', 'onRemoteAnalysis', 'onDenoiseProgress', 'onDenoiseRecorded', 'onQuitRequested'])

/** The event names the listeners attach to. */
export const CLOUD_EVENTS = Object.freeze({
  provisionProgress: 'provision://progress',
  qwenReview: 'qwen://review',
  cloudAttempt: 'cloud://attempt',
  remoteAnalysis: 'cloud://analysis',
  denoiseProgress: 'denoise://progress',
  denoiseRecorded: 'denoise://recorded',
})

/**
 * The application's own events. `app://quit-requested` is the quit guard in
 * `src-tauri/src/jobs.rs`: a close, the tray's Quit or Cmd+Q while jobs run,
 * held until the interface answers with `confirmQuit` or `hideToTray`.
 */
export const APP_EVENTS = Object.freeze({
  quitRequested: 'app://quit-requested',
})

/** Whether this page is running inside a Tauri window. */
export function isTauri() {
  return Boolean(globalThis.__TAURI_INTERNALS__ ?? globalThis.__TAURI__)
}

/**
 * `invoke`, from wherever Tauri put it.
 *
 * @returns {(command: string, args?: Object) => Promise<any>}
 */
function globalInvoke() {
  const fn = globalThis.__TAURI__?.core?.invoke ?? globalThis.__TAURI_INTERNALS__?.invoke
  if (typeof fn !== 'function') {
    throw new Error('the Tauri adapter was constructed outside a Tauri window')
  }
  return fn
}

/**
 * `listen`, from wherever Tauri put it, answering with the event's payload
 * only. Outside a window there is nothing to listen to, and the answer is an
 * unlisten that does nothing rather than a rejection: a listener is attached
 * at startup, and a browser tab has no events to miss.
 *
 * @param {string} event
 * @param {(payload: any) => void} handler
 * @returns {Promise<() => void>}
 */
function globalListen(event, handler) {
  const listen = globalThis.__TAURI__?.event?.listen
  if (typeof listen !== 'function') return Promise.resolve(() => {})
  return Promise.resolve(listen(event, (message) => handler(message?.payload)))
}

/**
 * @param {Object} options
 * @param {import('./backend.js').Backend} options.fallback - serves every method not yet implemented
 * @param {(command: string, args?: Object) => Promise<any>} [options.invoke] - injected for tests
 * @param {(event: string, handler: (payload: any) => void) => Promise<() => void>} [options.listen] - injected for tests
 * @returns {import('./backend.js').Backend}
 */
export function createTauriBackend({ fallback, invoke, listen }) {
  // `async` so that being constructed outside Tauri surfaces as a rejected
  // promise like any other backend failure, rather than as a synchronous throw
  // from a method the seam declares async.
  const call = invoke ?? (async (command, args) => globalInvoke()(command, args))
  const on = listen ?? globalListen
  let nextAnalysisRequest = 0
  const requestIdFor = (requestId) => requestId ?? `analysis-${Date.now()}-${++nextAnalysisRequest}`

  /**
   * Settings are stored by the core and *defaulted* by the interface.
   *
   * The core deliberately does not know what a setting means - it round-trips
   * opaque JSON - so on a first launch it
   * has nothing to return. The defaults are the fallback's, and the persisted
   * snapshot is merged over them. When the settings vocabulary gets a home
   * outside `fixtures.js`, this is the one line that moves.
   *
   * @param {Object} stored
   */
  const withDefaults = async (stored) => ({ ...(await fallback.readSettings()), ...stored })

  const implementations = {
    about: () => call(IMPLEMENTED.about),
    // The one command whose answer still crosses in snake_case:
    // `src-tauri/src/diagnostics.rs` predates the camelCase rule and has no
    // `rename_all`. Renamed here so the seam is camelCase like the rest; the
    // camelCase spelling is read too, so the struct can gain its `rename_all`
    // without this line having to move in the same change.
    diagnostics: async () => {
      const answer = await call(IMPLEMENTED.diagnostics)
      return {
        appVersion: answer?.appVersion ?? answer?.app_version ?? '',
        components: (answer?.components ?? []).map((/** @type {any} */ component) => ({
          name: component?.name ?? '',
          available: component?.available === true,
          detail: component?.detail ?? null,
          reasonKey: component?.reasonKey ?? component?.reason_key ?? null,
        })),
      }
    },
    readSettings: async () => withDefaults(await call(IMPLEMENTED.readSettings)),
    writeSettings: async (patch) => withDefaults(await call(IMPLEMENTED.writeSettings, { patch })),
    readInferenceConfig: () => call(IMPLEMENTED.readInferenceConfig),
    writeInferenceConfig: ({ config }) => call(IMPLEMENTED.writeInferenceConfig, { config }),
    selectCloudProfile: ({ provider, profileId }) => call(IMPLEMENTED.selectCloudProfile, { provider, profileId }),
    storeCloudSecret: ({ provider, profileId, role, secret, tokenId, sessionOnly = false }) =>
      call(IMPLEMENTED.storeCloudSecret, {
        provider,
        profileId,
        role,
        secret,
        ...(tokenId !== undefined ? { tokenId } : {}),
        sessionOnly,
      }),
    deleteCloudSecret: ({ provider, profileId, role }) =>
      call(IMPLEMENTED.deleteCloudSecret, { provider, profileId, role }),
    getCloudSecretSummary: ({ provider, profileId, role }) =>
      call(IMPLEMENTED.getCloudSecretSummary, { provider, profileId, role }),
    forgetCloudSecretDenials: () => call(IMPLEMENTED.forgetCloudSecretDenials),
    checkCloudConnection: ({ provider, profileId }) =>
      call(IMPLEMENTED.checkCloudConnection, { provider, profileId }),
    checkCloudRelease: ({ provider, profileId }) => call(IMPLEMENTED.checkCloudRelease, { provider, profileId }),
    getCloudModelInfo: ({ provider, profileId }) =>
      call(IMPLEMENTED.getCloudModelInfo, { provider, profileId }),
    getCloudGpuStatus: ({ provider, profileId }) =>
      call(IMPLEMENTED.getCloudGpuStatus, { provider, profileId }),
    stopCloudGpu: ({ provider, profileId, role }) => call(IMPLEMENTED.stopCloudGpu, { provider, profileId, role }),
    listRemoteAnalysisCapabilities: ({ provider, profileId }) =>
      call(IMPLEMENTED.listRemoteAnalysisCapabilities, { provider, profileId }),
    proposeRemoteAnalysis: (spec) => call(IMPLEMENTED.proposeRemoteAnalysis, { regions: [], companion: null, ...spec }),
    confirmRemoteAnalysis: ({ proposalId, rightsAttested, retentionAcknowledged }) =>
      call(IMPLEMENTED.confirmRemoteAnalysis, { proposalId, rightsAttested, retentionAcknowledged }),
    cancelRemoteAnalysis: ({ proposalId }) => call(IMPLEMENTED.cancelRemoteAnalysis, { proposalId }),
    getRemoteAnalysisStatus: ({ proposalId }) => call(IMPLEMENTED.getRemoteAnalysisStatus, { proposalId }),
    proposeRunAnalysis: ({ chapterId, scope = null, pageIndices = null, capabilities, provider, profileId }) =>
      call(IMPLEMENTED.proposeRunAnalysis, { chapterId, scope, pageIndices, capabilities, provider, profileId }),
    confirmRunAnalysis: ({ proposalId, rightsAttested, retentionAcknowledged }) =>
      call(IMPLEMENTED.confirmRunAnalysis, { proposalId, rightsAttested, retentionAcknowledged }),
    cancelRunAnalysis: ({ proposalId }) => call(IMPLEMENTED.cancelRunAnalysis, { proposalId }),
    // The batch cloud clean (docs/detect-clean.md §3). Arguments are named
    // here rather than passed through, so nothing the interface happens to
    // carry reaches a command that spends money.
    // Strictly the cloud unless mixed execution is asked for by name.
    resolveQwenReview: ({ attemptId, choice }) => call(IMPLEMENTED.resolveQwenReview, { attemptId, choice }),
    prepareCloudClean: ({ chapterId, scope, pageIndices = null, regionIds = null, localFirst = false,
      bubbleEngine = null, outsideEngine = null, qwenEdit }) =>
      call(IMPLEMENTED.prepareCloudClean, { chapterId, scope, pageIndices, regionIds, localFirst,
        bubbleEngine, outsideEngine, ...(qwenEdit ? { qwenEdit } : {}) }),
    // `planDigest` is the plan the consent dialog showed: the grant is minted
    // only for that plan.
    confirmCloudClean: ({ proposalId, planDigest, rightsAttested, retentionAcknowledged }) =>
      call(IMPLEMENTED.confirmCloudClean, { proposalId, planDigest, rightsAttested, retentionAcknowledged }),
    startCloudClean: ({ grantId }) => call(IMPLEMENTED.startCloudClean, { grantId }),
    cancelCloudClean: ({ proposalId }) => call(IMPLEMENTED.cancelCloudClean, { proposalId }),
    // Page denoise. Named arguments for the same reason as the clean above:
    // the cloud three spend money, and the local one writes files.
    denoisePresets: () => call(IMPLEMENTED.denoisePresets),
    benchmarkDenoiseLocal: ({ presetId } = {}) => call(IMPLEMENTED.benchmarkDenoiseLocal, { presetId: presetId ?? null }),
    denoiseChapterLocal: ({ runId, chapterId, pageIndices = null, presetId, outDir }) =>
      call(IMPLEMENTED.denoiseChapterLocal, { runId, chapterId, pageIndices, presetId, outDir }),
    cancelDenoiseLocal: ({ runId }) => call(IMPLEMENTED.cancelDenoiseLocal, { runId }),
    replaceWithDenoised: ({ chapterId }) => call(IMPLEMENTED.replaceWithDenoised, { chapterId }),
    denoiseHistory: ({ chapterId }) => call(IMPLEMENTED.denoiseHistory, { chapterId }),
    // Raw bytes over IPC (`tauri::ipc::Response`): an ArrayBuffer, not JSON.
    denoiseCompareImage: ({ chapterId, pageIndex, run, side }) =>
      call(IMPLEMENTED.denoiseCompareImage, { chapterId, pageIndex, run, side }),
    cloudDenoisePresets: (target = {}) => call(IMPLEMENTED.cloudDenoisePresets, target),
    prepareCloudDenoise: ({ chapterId, pageIndices = null, recipe }) =>
      call(IMPLEMENTED.prepareCloudDenoise, { chapterId, pageIndices, recipe }),
    confirmCloudDenoise: ({ proposalId, planDigest, rightsAttested, retentionAcknowledged }) =>
      call(IMPLEMENTED.confirmCloudDenoise, { proposalId, planDigest, rightsAttested, retentionAcknowledged }),
    // `runId` is the interface's name for the run, as for a local denoise:
    // `denoise://progress` carries it and `cancelDenoiseLocal` takes it.
    startCloudDenoise: ({ grantId, outDir, runId }) =>
      call(IMPLEMENTED.startCloudDenoise, { grantId, outDir, ...(runId ? { runId } : {}) }),
    cancelCloudDenoise: ({ proposalId }) => call(IMPLEMENTED.cancelCloudDenoise, { proposalId }),
    prepareCloudConsent: (spec) => call(IMPLEMENTED.prepareCloudConsent, spec),
    confirmCloudConsent: (spec) => call(IMPLEMENTED.confirmCloudConsent, spec),
    submitCloudAttempt: (spec) => call(IMPLEMENTED.submitCloudAttempt, spec),
    getCloudAttemptStatus: (spec) => call(IMPLEMENTED.getCloudAttemptStatus, spec),
    getCloudAttemptResult: (spec) => call(IMPLEMENTED.getCloudAttemptResult, spec),
    cancelCloudAttempt: (spec) => call(IMPLEMENTED.cancelCloudAttempt, spec),
    reconcileCloudRecovery: (spec = {}) => call(IMPLEMENTED.reconcileCloudRecovery, spec),
    // `inspect` answers `ProvisionInspectDiscovery` (backend.js) beside the
    // account fields; `provision.rs` has rebuilt that list from allowlisted fields.
    runCloudProvisioner: (spec = {}) => {
      const { op = 'inspect', provider = 'modal', params = {} } = spec ?? {}
      return call(IMPLEMENTED.runCloudProvisioner, { op, provider, params })
    },
    // Kills the running helper. Its journal is what makes a later `resume`
    // safe, so stopping is never a loss of what was already created.
    cancelCloudProvisioner: () => call(IMPLEMENTED.cancelCloudProvisioner),

    // Straight passthrough. The commands answer in the seam's own shapes - the
    // conversions that used to justify a mapping layer happen in Rust, where
    // the data is: epoch seconds become ISO 8601 and pixel boxes become
    // percentages of the page, both in `src-tauri/src/library.rs`.
    listProjects: () => call(IMPLEMENTED.listProjects),
    createProject: (spec) => call(IMPLEMENTED.createProject, spec),
    createChapter: (spec) => call(IMPLEMENTED.createChapter, spec),
    openChapter: (spec) => call(IMPLEMENTED.openChapter, spec),
    // The resident window. Whole pages come back, so the page's
    // status travels with its regions.
    loadPages: ({ chapterId, indices }) =>
      call(IMPLEMENTED.loadPages, { chapterId, indices: indices ?? [] }),
    // The persisted undo journal. `historyLoad` answers the index
    // (cursor plus one `{seq, label}` per entry); `historyMove` moves the
    // cursor on disk and answers with the one delta to replay. The payloads
    // never all exist at once on this side, which is the whole point.
    historyLoad: ({ chapterId }) => call(IMPLEMENTED.historyLoad, { chapterId }),
    historyPush: ({ chapterId, entry }) => call(IMPLEMENTED.historyPush, { chapterId, entry }),
    historyMove: ({ chapterId, direction }) =>
      call(IMPLEMENTED.historyMove, { chapterId, direction }),
    renameProject: (spec) => call(IMPLEMENTED.renameProject, spec),
    deleteProject: (spec) => call(IMPLEMENTED.deleteProject, spec),
    // The command's `source_files` is required; the seam leaves it optional.
    deleteChapter: ({ projectId, chapterId, sourceFiles = false }) =>
      call(IMPLEMENTED.deleteChapter, { projectId, chapterId, sourceFiles }),
    exportChapter: (spec) => call(IMPLEMENTED.exportChapter, spec),
    planExportChapter: (spec) => call(IMPLEMENTED.planExportChapter, spec),

    // Named arguments rather than the spec object, because these two are the
    // only calls whose spec carries a field the command has no parameter for:
    // `restoreRegion`'s `pageStatus` is the *caller's* snapshot, and the
    // manifest's own record is the better copy of it.
    deleteMask: ({ maskId }) => call(IMPLEMENTED.deleteMask, { maskId }),
    restoreRegion: ({ regionId, region }) =>
      call(IMPLEMENTED.restoreRegion, { regionId, region: region ?? null }),
    // A deliberate layer change, judged by the layer's capabilities; history
    // replay goes through `restoreRegion` instead.
    setLayerStyle: ({ regionId, layer }) => call(IMPLEMENTED.setLayerStyle, { regionId, layer }),
    keepDependencyResult: ({ regionId }) => call(IMPLEMENTED.keepDependencyResult, { regionId }),
    setDetectionType: ({ regionId, inside }) => call(IMPLEMENTED.setDetectionType, { regionId, inside }),

    // The four region edits that run an engine. Named arguments rather than
    // the spec object wherever the command's parameters are not the spec's:
    // `rerunMask` addresses a mask and the command takes the three fields it
    // acts on, and `cleanAnyway` carries the engine pick the tool window holds
    // for text outside a balloon, and this is where that row
    // finally bites.
    applyTool: ({ tool, params, chapterId, pageIndex, regionId }) =>
      call(IMPLEMENTED.applyTool, {
        tool,
        params: params ?? null,
        chapterId: chapterId ?? null,
        pageIndex: pageIndex ?? null,
        regionId: regionId ?? null,
      }),
    previewPaint: (spec) => call(IMPLEMENTED.previewPaint, spec),
    createRegion: ({ chapterId, pageIndex, sourceIndex, sourceSha, bbox, tool, params }) =>
      call(IMPLEMENTED.createRegion, {
        chapterId,
        pageIndex,
        expectedSourceIdx: sourceIndex ?? null,
        expectedSourceSha: sourceSha ?? null,
        bbox,
        tool,
        params: params ?? null,
      }),
    // Bound to the source the gesture was drawn over, as `createRegion` is.
    // Exactly one of `stroke` and `painted` carries the gesture; the other
    // crosses as `null` so the command's two optional parameters are always
    // named.
    editDetectionMask: ({ chapterId, pageIndex, sourceIndex, sourceSha, mode, stroke, painted }) =>
      call(IMPLEMENTED.editDetectionMask, {
        chapterId,
        pageIndex,
        expectedSourceIdx: sourceIndex ?? null,
        expectedSourceSha: sourceSha ?? null,
        mode,
        stroke: stroke ?? null,
        painted: painted ?? null,
      }),
    // `pageIndex` crosses as `null` for the whole chapter, so the command's
    // optional parameter is always named.
    setDetectionPadding: ({ chapterId, pageIndex, regionId, paddingPx }) =>
      call(IMPLEMENTED.setDetectionPadding, { chapterId, pageIndex: pageIndex ?? null, regionId: regionId ?? null, paddingPx }),
    // `params` carries a cloud run's grant - `grantNonce`, `executionTarget`,
    // `recipe`, `intent`, the four `applyTool` already sends - and is left off
    // entirely for a local rung, so the command sees exactly what it saw
    // before the cloud could reach it.
    rerunMask: ({ maskId, kind, engine, params }) =>
      call(IMPLEMENTED.rerunMask, {
        maskId,
        kind,
        engine: engine ?? null,
        ...(params ? { params } : {}),
      }),
    cleanAnyway: ({ regionId, engine, params }) =>
      call(IMPLEMENTED.cleanAnyway, {
        regionId,
        engine: engine ?? null,
        ...(params ? { params } : {}),
      }),
    sidecarAvailable: () => call(IMPLEMENTED.sidecarAvailable),
    listSidecarModels: () => call(IMPLEMENTED.listSidecarModels),
    installFluxHelper: ({ backend, accelerator }) =>
      call(IMPLEMENTED.installFluxHelper, { backend, accelerator }),

    // The loaded-models tab. `listLoadedModels` is a **poll**, so it takes no
    // argument and is deliberately the cheapest command in this table: it walks
    // a handful of rows under one mutex in `src-tauri/src/models.rs` and
    // touches no session. `unloadModel` records a request the run acts on at
    // its next region boundary, which is why it answers a boolean about the
    // row rather than about the memory.
    listLoadedModels: () => call(IMPLEMENTED.listLoadedModels),
    unloadModel: ({ id }) => call(IMPLEMENTED.unloadModel, { id }),

    // The accelerator setting. Not a poll: the answer changes when the runtime
    // is downloaded or the setting is written, and both are events the panel
    // already knows about. It carries the *reasons* as keys - which providers
    // this machine has, which one each model landed on, and why a forced one
    // was refused - because the earlier complaint was that the core
    // decided all of that and told nobody.
    listAccelerators: () => call(IMPLEMENTED.listAccelerators),

    // The model catalogue. `listModels` is the cheap one - a
    // `metadata` call per search path per weight and no digests, because a
    // dialog that hashed 200 MB to decide whether to draw a row would take
    // most of a minute to open.
    //
    // The four that *change* something answer as soon as the work is under
    // way, never when it is finished: a 207 MB download reports on the event
    // channel (`model-progress`) and is cancellable while it does, and a
    // command that resolved at the end could offer neither. `verifyModel` is
    // the one exception and is deliberately slow - it is the explicit
    // re-digest of a file somebody suspects.
    //
    // What they answer *with* is an id rather than a boolean: `downloadModel`
    // says `started` or which of the two races refused it, and `deleteModel`
    // says whether the file went, was never there, or belongs to somebody else.
    // The commands serialise those as camelCase
    // strings, so there is nothing to translate here.
    //
    // `retryStore` is the one argument any of them takes and it is not a
    // preference: it says "Settings › Models has just been opened", which is
    // the one moment worth re-offering the token to a credential store that
    // refused this process's first attempt. Absent on every
    // other call, including the refreshes this dialog makes when a download
    // ends, or the prompt-per-poll that row removed would be back.
    listModels: ({ retryStore } = {}) => call(IMPLEMENTED.listModels, { retryStore }),
    downloadModel: ({ id }) => call(IMPLEMENTED.downloadModel, { id }),
    downloadModelGroup: ({ id }) => call(IMPLEMENTED.downloadModelGroup, { id }),
    cancelDownload: ({ id }) => call(IMPLEMENTED.cancelDownload, { id }),
    deleteModel: ({ id }) => call(IMPLEMENTED.deleteModel, { id }),
    deleteModelGroup: ({ id }) => call(IMPLEMENTED.deleteModelGroup, { id }),
    // The bytes a cancelled transfer left behind, given back.
    discardPartial: ({ id }) => call(IMPLEMENTED.discardPartial, { id }),
    verifyModel: ({ id }) => call(IMPLEMENTED.verifyModel, { id }),
    verifyModelGroup: ({ id }) => call(IMPLEMENTED.verifyModelGroup, { id }),
    downloadRuntime: () => call(IMPLEMENTED.downloadRuntime),
    deleteRuntime: () => call(IMPLEMENTED.deleteRuntime),
    listWorkflowCapabilities: () => call(IMPLEMENTED.listWorkflowCapabilities),
    importFullRt: ({ sourcePath }) => call(IMPLEMENTED.importFullRt, { sourcePath }),
    removeFullRt: () => call(IMPLEMENTED.removeFullRt),
    importSamTs: ({ sourceDir }) => call(IMPLEMENTED.importSamTs, { sourceDir }),
    installSamTs: () => call(IMPLEMENTED.installSamTs),
    removeSamTs: () => call(IMPLEMENTED.removeSamTs),
    verifySamTs: () => call(IMPLEMENTED.verifySamTs),
    analyzeCapabilities: ({ sourcePath, workflow, rtProfile, rtBackend, samBackend, requestId }) => call(IMPLEMENTED.analyzeCapabilities, { sourcePath, workflow, rtProfile, rtBackend, samBackend, requestId: requestIdFor(requestId) }),
    analyzeChapterPage: ({ chapterId, pageIndex, workflow, rtProfile, rtBackend, samBackend, requestId }) => call(IMPLEMENTED.analyzeChapterPage, { chapterId, pageIndex, workflow, rtProfile, rtBackend, samBackend, requestId: requestIdFor(requestId) }),
    cancelCapabilityAnalysis: (requestId) => call(IMPLEMENTED.cancelCapabilityAnalysis, { requestId }),
    prepareComponentWrite: ({ analysisId, chapterId, pageIndex, componentId, allowOutsideBubbles, paddingPx, additions, removals, correctionRevision }) => call(IMPLEMENTED.prepareComponentWrite, { analysisId, chapterId, pageIndex, componentId, allowOutsideBubbles, paddingPx, additions, removals, correctionRevision }),
    loadComponentCorrection: ({ analysisId, chapterId, pageIndex, componentId }) => call(IMPLEMENTED.loadComponentCorrection, { analysisId, chapterId, pageIndex, componentId }),
    applyComponentWrite: ({ planId, approvedSupportSha256 }) => call(IMPLEMENTED.applyComponentWrite, { planId, approvedSupportSha256 }),

    runClean: (spec) => call(IMPLEMENTED.runClean, spec),
    cancelRun: (spec = {}) => call(IMPLEMENTED.cancelRun, spec),
    resumeJob: (spec) => call(IMPLEMENTED.resumeJob, spec),

    // Background jobs (`src-tauri/src/jobs.rs`): the running runs and denoises,
    // for a reloaded window or a reopened chapter to pick up, and the two
    // answers to a held quit.
    listJobs: () => call(IMPLEMENTED.listJobs),
    confirmQuit: () => call(IMPLEMENTED.confirmQuit),
    hideToTray: () => call(IMPLEMENTED.hideToTray),
  }

  const backend = {}
  for (const method of SEAM_METHODS) {
    if (EVENT_METHODS.includes(method)) continue
    backend[method] = Object.hasOwn(implementations, method)
      ? implementations[method]
      : (...args) => /** @type {any} */ (fallback)[method](...args)
  }

  /**
   * `subscribe` is both streams at once.
   *
   * The events a handler receives have to come from whichever implementation is
   * actually running the job. `runClean` is a command now and the mock still
   * serves the six region-level edits, so both produce events and a handler
   * that saw only one of them would leave the Pages list frozen with a run
   * apparently in progress. The seam's ordering rules still hold per run,
   * because a run belongs entirely to one implementation.
   */
  backend.subscribe = createEventStream({ call, fallback })

  // Only the core emits these: every command that could produce one - the
  // helper, `applyTool`, `rerunMask`, `cleanAnyway` - is served here, never by
  // the fallback, so there is no second stream to merge.
  backend.onProvisionProgress = (handler) => on(CLOUD_EVENTS.provisionProgress, handler)
  backend.onQwenReview = (handler) => on(CLOUD_EVENTS.qwenReview, handler)
  backend.onCloudAttempt = (handler) => on(CLOUD_EVENTS.cloudAttempt, handler)
  backend.onRemoteAnalysis = (handler) => on(CLOUD_EVENTS.remoteAnalysis, handler)
  backend.onDenoiseProgress = (handler) => on(CLOUD_EVENTS.denoiseProgress, handler)
  backend.onDenoiseRecorded = (handler) => on(CLOUD_EVENTS.denoiseRecorded, handler)
  backend.onQuitRequested = (handler) => on(APP_EVENTS.quitRequested, handler)

  return /** @type {import('./backend.js').Backend} */ (backend)
}

/** Which seam methods this adapter answers itself. For tests and for `about`. */
export function implementedMethods() {
  return Object.keys(IMPLEMENTED).sort()
}

/**
 * The cloud commands currently registered in `src-tauri/src/lib.rs` (P3 configuration & secrets).
 */
export const TAURI_REGISTERED_CLOUD_COMMANDS = Object.freeze([
  'read_inference_config',
  'write_inference_config',
  'select_cloud_profile',
  'store_cloud_secret',
  'delete_cloud_secret',
  'get_cloud_secret_summary',
  'forget_cloud_secret_denials',
  'check_cloud_connection',
  'get_cloud_model_info',
  'get_cloud_gpu_status',
  'stop_cloud_gpu',
  'list_remote_analysis_capabilities',
  'propose_remote_analysis',
  'confirm_remote_analysis',
  'cancel_remote_analysis',
  'get_remote_analysis_status',
  'propose_run_analysis',
  'confirm_run_analysis',
  'cancel_run_analysis',
  'prepare_cloud_clean',
  'confirm_cloud_clean',
  'start_cloud_clean',
  'cancel_cloud_clean',
  'prepare_cloud_consent',
  'confirm_cloud_consent',
  'submit_cloud_attempt',
  'get_cloud_attempt_status',
  'get_cloud_attempt_result',
  'resolve_qwen_review',
  'cancel_cloud_attempt',
  'reconcile_cloud_recovery',
])

/**
 * The cloud lifecycle commands defined by backend wire/consent/journal contracts
 * awaiting registration in `src-tauri/src/lib.rs` (P3b consent IPC, P4 durable lifecycle & recovery).
 * Mapped directly to invoke calls to guarantee no mock fake-success in production Tauri windows.
 */
export const TAURI_PENDING_CLOUD_COMMANDS = Object.freeze([])

/**
 * The cloud provisioner helper IPC commands registered in `src-tauri/src/lib.rs`.
 */
export const TAURI_PROVISIONER_COMMANDS = Object.freeze([
  'run_cloud_provisioner',
  'cancel_cloud_provisioner',
  'provision_inspect',
  'provision_plan',
  'provision_apply',
  'provision_resume',
  'provision_cleanup',
  'provision_probe',
])

/**
 * Truthfully reports whether all required remote execution lifecycle commands are registered in Tauri.
 * Registration indicates that genuine backend handlers exist in `src-tauri/src/lib.rs` and are mapped in the adapter.
 *
 * @returns {boolean}
 */
export function isCloudExecutionRegistered() {
  return TAURI_PENDING_CLOUD_COMMANDS.length === 0 && TAURI_REGISTERED_CLOUD_COMMANDS.length > 0
}
