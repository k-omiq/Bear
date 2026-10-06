/**
 * Named outcomes for the text-shaped review, local and cloud.
 *
 * The native commands in `src-tauri/src/model_workflows.rs` refuse with plain
 * English strings, and the cloud analysis in
 * `src-tauri/src/inference/analysis.rs` with snake_case codes. The review
 * never prints either as its message: each one is classified here into a
 * named outcome the catalogue can say in the user's language, with the
 * recovery that fits it. The raw text survives only as `detail`, for the
 * collapsed "Technical detail" line a support request needs.
 *
 * Every `Err(...)` text those two files can return has a row below, and
 * `workflowoutcome.test.js` pins each one with the backend's exact wording. A
 * text this table does not know (an I/O error, a transport error) falls
 * through to `failed` for the phase it happened in, which still names what
 * did not happen ("Nothing was written", "Nothing was sent") rather than
 * showing a stack.
 *
 * Pure: no component, no store.
 */

import { runtimeState } from '../model/pipelines.js'

/**
 * @typedef {'cancelled'|'unavailable'|'modelMissing'|'modelFailed'|'memory'|'sourceLimit'
 *   |'undiscovered'|'held'|'declined'|'needsCorrection'|'stale'|'expired'
 *   |'badReconstruction'|'applied'|'failed'|'notReady'
 *   |'invalidRequest'|'wrongModel'|'sourceUnreadable'|'exhausted'|'existingGeometry'|'unconfirmed'|'system'
 *   |'cloudDisabled'|'cloudProfile'|'capabilityMissing'|'consentRequired'|'proposalExpired'
 *   |'proposalConsumed'|'proposalLimit'|'remoteCancelled'|'remoteStale'|'remoteUnknown'|'remoteInvalid'} OutcomeKind
 *
 * @typedef {'analyze'|'prepare'|'apply'|'load'|'models'|'refresh'|'capabilities'|'propose'|'confirm'} Phase
 *
 * @typedef {Object} Outcome
 * @property {OutcomeKind} kind
 * @property {string} [reason] - a sub-reason, for the kinds whose body depends on one
 * @property {string} [detail] - the native text, never the message itself
 * @property {Record<string, unknown>} [params]
 */

const WRONG_MODEL = { kind: 'wrongModel' }
const MODEL_MISSING = { kind: 'modelMissing' }

/**
 * `[pattern, outcome]`, first match wins. Order matters where wordings
 * overlap. An outcome may be a function of the phase, for the one text whose
 * meaning depends on whether a file was being imported or used.
 *
 * @type {Array<[RegExp, Omit<Outcome, 'detail'> | ((phase: Phase) => Omit<Outcome, 'detail'>)]>}
 */
const RULES = [
  // Cloud analysis codes, `analysis.rs`. The code leads; a detail may follow a colon.
  [/^cloud_disabled\b/, { kind: 'cloudDisabled' }],
  [/^analysis_profile_not_active\b/, { kind: 'cloudProfile', reason: 'inactive' }],
  [/^analysis_profile_missing\b/, { kind: 'cloudProfile', reason: 'missing' }],
  [/^credential missing:/i, { kind: 'cloudProfile', reason: 'credential' }],
  [/^configuration error:/i, { kind: 'cloudProfile', reason: 'config' }],
  [/^capability_unavailable: gateway advertisement is invalid/, { kind: 'capabilityMissing', reason: 'invalid' }],
  [/^capability_unavailable: gateway analysis is not configured/, { kind: 'capabilityMissing', reason: 'notConfigured' }],
  [/^capability_unavailable: gateway tile limits/, { kind: 'capabilityMissing', reason: 'limits' }],
  [/^capability_unavailable: model identity changed/, { kind: 'capabilityMissing', reason: 'modelChanged' }],
  [/^capability_unavailable\b/, { kind: 'capabilityMissing', reason: 'absent' }],
  [/^rights_attestation_required\b/, { kind: 'consentRequired', reason: 'rights' }],
  [/^retention_acknowledgement_required\b/, { kind: 'consentRequired', reason: 'retention' }],
  [/^(analysis_proposal_(expired|missing)|proposal_expired)\b/, { kind: 'proposalExpired' }],
  [/^analysis_proposal_consumed\b/, { kind: 'proposalConsumed' }],
  [/^analysis_proposal_limit\b/, { kind: 'proposalLimit' }],
  [/^analysis_cancelled\b/, { kind: 'remoteCancelled' }],
  [/^analysis_stale\b/, { kind: 'remoteStale' }],
  [/^review_evidence_expired\b/, { kind: 'expired' }],
  [/^(analysis result lies outside page|analysis mask missing|invalid analysis box class|analysis page too large)/i,
    { kind: 'remoteInvalid' }],
  [/Remote analysis is review-only/i, { kind: 'declined', reason: 'remote' }],
  [/Remote analysis currently requires a paginated chapter/i, { kind: 'sourceLimit', reason: 'longstrip' }],
  // The tile planner, `cleaner-core/src/cloud_tiles.rs`, refusing a page before anything is sent.
  [/^analysis (tile count exceeded|upload extent exceeded|page grid too large)/i, { kind: 'sourceLimit', reason: 'upload' }],

  // Local analysis and component writes, `model_workflows.rs`.
  [/analysis cancelled/i, { kind: 'cancelled' }],
  [/desktop runtime/i, { kind: 'unavailable' }],
  [/indexed palette/i, { kind: 'declined', reason: 'indexed' }],
  [/sub-8-bit/i, { kind: 'declined', reason: 'sub8Bit' }],
  [/JPEG parity|require a PNG/i, { kind: 'declined', reason: 'jpeg' }],
  [/Chapter model analysis currently requires a paginated chapter/i, { kind: 'sourceLimit', reason: 'longstrip' }],
  [/paginated chapter/i, { kind: 'declined', reason: 'longstrip' }],
  [/not qualified|qualification on this operating system|unavailable for this analysis path|Unsupported SAM backend/i,
    { kind: 'declined', reason: 'host' }],
  [/Only a SAM component can grant write support/i, { kind: 'declined', reason: 'box' }],
  [/held until explicitly permitted/i, { kind: 'held' }],
  [/Empty component has no write support/i, { kind: 'needsCorrection', reason: 'empty' }],
  [/overlaps an existing visible edit/i, { kind: 'needsCorrection', reason: 'overlap' }],
  [/16 megapixel plan limit/i, { kind: 'needsCorrection', reason: 'tooLarge' }],
  [/does not use text-shaped geometry/i, { kind: 'existingGeometry' }],
  [/Analysis expired|Analysis changed|does not match the analyzed source|Saved correction|Saved SAM base mask changed|absent from this analysis|no longer in chapter|Invalid SAM component/i,
    { kind: 'expired' }],
  [/Prepared write expired|Source changed since approval|Approved support raster changed|Region order changed|Mask plan revision changed|Visible page changed|without a new correction revision|Approval does not match/i,
    { kind: 'stale' }],
  [/No surrounding pixels|outside the composited underlay/i, { kind: 'badReconstruction' }],
  [/does not match the pinned manifest|copied .*graph failed verification/i, WRONG_MODEL],
  [/has \d+ bytes; expected \d+|SHA-256 mismatch/i, (phase) => (phase === 'models' ? WRONG_MODEL : MODEL_MISSING)],
  [/not installed|failed SHA-256|failed verification|graphs are missing/i, MODEL_MISSING],
  [/10 GB|memory room/i, { kind: 'memory' }],
  [/source geometry changed/i, { kind: 'sourceLimit', reason: 'geometry' }],
  [/tile PNG exceeds transfer limit/i, { kind: 'sourceLimit', reason: 'tile' }],
  [/20 MB|24 megapixel|review-preview limit|preview format is unsupported/i, { kind: 'sourceLimit' }],
  [/Analysis request id|Choose a supported detection model combination|Choose Regions, Mask, or Text-shaped review|Choose the Full \(tiled\) or installed Small Ogkalu detector profile/i,
    { kind: 'invalidRequest' }],
  [/Chapter source is missing|Chapter source path is not UTF-8/i, { kind: 'sourceUnreadable' }],
  [/Patch order is exhausted|Mask plan revision is exhausted/i, { kind: 'exhausted' }],
  [/Saved component could not be found in chapter/i, { kind: 'unconfirmed' }],
  [/Random source unavailable/i, { kind: 'system' }],
]

/** Kinds the user asked for: their native text adds nothing to disclose. */
const QUIET = new Set(['cancelled', 'remoteCancelled'])

/**
 * Classify a refusal from one of the review's backend calls.
 *
 * @param {unknown} cause - what the call rejected with: a string from Tauri, an Error from the mock
 * @param {Phase} phase - which call it was, for the fall-through
 * @returns {Outcome}
 */
export function outcomeOf(cause, phase) {
  const detail = textOf(cause)
  for (const [pattern, rule] of RULES) {
    if (!pattern.test(detail)) continue
    const outcome = typeof rule === 'function' ? rule(phase) : rule
    return QUIET.has(outcome.kind) ? { ...outcome } : { ...outcome, detail }
  }
  if (phase === 'analyze') return { kind: 'modelFailed', detail }
  return { kind: 'failed', reason: phase, detail }
}

/** @param {unknown} cause */
function textOf(cause) {
  if (typeof cause === 'string') return cause
  if (cause && typeof cause === 'object' && 'message' in cause) return String(/** @type {any} */ (cause).message)
  return String(cause ?? '')
}

/**
 * The outcome a cloud analysis journal record ends in, or `null` while it is
 * still running or once it attached evidence.
 *
 * A tile that was sent and never answered leaves the record in the unknown
 * phase (`unknown_remote_state` in the journal). If the user had asked to
 * cancel, `cancel_requested` says so, and the outcome says that the request
 * was made rather than that the analysis was cancelled: the last tile may
 * still have run.
 *
 * @param {any} record - the snake_case record from `getRemoteAnalysisStatus` or `cloud://analysis`
 * @returns {Outcome|null}
 */
export function outcomeOfRecord(record) {
  const phase = record?.phase?.phase
  if (phase === 'cancelled') return { kind: 'remoteCancelled' }
  if (phase === 'unknown' || phase === 'unknown_remote_state') {
    return record.cancel_requested === true ? { kind: 'remoteUnknown', reason: 'cancelRequested' } : { kind: 'remoteUnknown' }
  }
  if (phase === 'failed') return outcomeOf(String(record.phase.code ?? ''), 'confirm')
  return null
}

/* ------------------------------------------------------------------ */
/* What an outcome says                                                */
/* ------------------------------------------------------------------ */

const TITLES = {
  cancelled: 'workflow.outcome.cancelled',
  unavailable: 'workflow.outcome.unavailable',
  modelMissing: 'workflow.outcome.modelMissing',
  notReady: 'workflow.outcome.notReady',
  modelFailed: 'workflow.outcome.modelFailed',
  memory: 'workflow.outcome.memory',
  sourceLimit: 'workflow.outcome.sourceLimit',
  undiscovered: 'workflow.outcome.undiscovered',
  held: 'workflow.outcome.held',
  declined: 'workflow.outcome.declined',
  needsCorrection: 'workflow.outcome.needsCorrection',
  stale: 'workflow.outcome.stale',
  expired: 'workflow.outcome.expired',
  badReconstruction: 'workflow.outcome.badReconstruction',
  applied: 'workflow.outcome.applied',
  failed: 'workflow.outcome.failed',
  invalidRequest: 'workflow.outcome.invalidRequest',
  wrongModel: 'workflow.outcome.wrongModel',
  sourceUnreadable: 'workflow.outcome.sourceUnreadable',
  exhausted: 'workflow.outcome.exhausted',
  existingGeometry: 'workflow.outcome.existingGeometry',
  unconfirmed: 'workflow.outcome.unconfirmed',
  system: 'workflow.outcome.system',
  cloudDisabled: 'cloud.analysis.outcome.cloudDisabled',
  cloudProfile: 'cloud.analysis.outcome.cloudProfile',
  capabilityMissing: 'cloud.analysis.outcome.capabilityMissing',
  consentRequired: 'cloud.analysis.outcome.consentRequired',
  proposalExpired: 'cloud.analysis.outcome.proposalExpired',
  proposalConsumed: 'cloud.analysis.outcome.proposalConsumed',
  proposalLimit: 'cloud.analysis.outcome.proposalLimit',
  remoteCancelled: 'cloud.analysis.outcome.cancelled',
  remoteStale: 'cloud.analysis.outcome.stale',
  remoteUnknown: 'cloud.analysis.outcome.unknown',
  remoteInvalid: 'cloud.analysis.outcome.invalid',
}

const BODIES = {
  cancelled: 'workflow.explain.cancelled',
  unavailable: 'workflow.explain.unavailable',
  modelMissing: 'workflow.explain.modelMissing',
  modelFailed: 'workflow.explain.modelFailed',
  memory: 'workflow.explain.memory',
  sourceLimit: 'workflow.explain.sourceLimit',
  undiscovered: 'workflow.explain.undiscovered',
  held: 'workflow.explain.held',
  stale: 'workflow.explain.stale',
  expired: 'workflow.explain.expired',
  badReconstruction: 'workflow.explain.badReconstruction',
  applied: 'workflow.explain.applied',
  invalidRequest: 'workflow.explain.invalidRequest',
  wrongModel: 'workflow.explain.wrongModel',
  sourceUnreadable: 'workflow.explain.sourceUnreadable',
  exhausted: 'workflow.explain.exhausted',
  existingGeometry: 'workflow.explain.existingGeometry',
  unconfirmed: 'workflow.explain.unconfirmed',
  system: 'workflow.explain.system',
  cloudDisabled: 'cloud.analysis.explain.cloudDisabled',
  proposalExpired: 'cloud.analysis.explain.proposalExpired',
  proposalConsumed: 'cloud.analysis.explain.proposalConsumed',
  proposalLimit: 'cloud.analysis.explain.proposalLimit',
  remoteCancelled: 'cloud.analysis.cancelled',
  remoteStale: 'cloud.analysis.stale',
  remoteUnknown: 'cloud.analysis.unknown',
  remoteInvalid: 'cloud.analysis.explain.invalid',
}

/** Bodies that depend on the outcome's `reason`; the first entry of each is its default. */
const REASON_BODIES = {
  declined: {
    host: 'workflow.declined.host',
    regions: 'workflow.declined.regions',
    cpu: 'workflow.declined.cpu',
    jpeg: 'workflow.declined.jpeg',
    format: 'workflow.declined.format',
    indexed: 'workflow.declined.indexed',
    sub8Bit: 'workflow.declined.sub8Bit',
    provider: 'workflow.declined.provider',
    longstrip: 'workflow.declined.longstrip',
    box: 'workflow.declined.box',
    remote: 'cloud.analysis.reviewOnly',
  },
  needsCorrection: {
    empty: 'workflow.correctionReason.empty',
    overlap: 'workflow.correctionReason.overlap',
    tooLarge: 'workflow.correctionReason.tooLarge',
  },
  sourceLimit: {
    size: 'workflow.explain.sourceLimit',
    longstrip: 'workflow.explain.longstrip',
    geometry: 'workflow.explain.geometry',
    tile: 'workflow.explain.tile',
    upload: 'cloud.analysis.explain.upload',
  },
  capabilityMissing: {
    absent: 'cloud.analysis.capabilityMissing',
    invalid: 'cloud.analysis.capability.invalid',
    notConfigured: 'cloud.analysis.capability.notConfigured',
    limits: 'cloud.analysis.capability.limits',
    modelChanged: 'cloud.analysis.capability.modelChanged',
  },
  cloudProfile: {
    inactive: 'cloud.analysis.profile.inactive',
    missing: 'cloud.analysis.profile.missing',
    credential: 'cloud.analysis.profile.credential',
    config: 'cloud.analysis.profile.config',
  },
  consentRequired: {
    rights: 'cloud.analysis.consentRequired.rights',
    retention: 'cloud.analysis.consentRequired.retention',
  },
  remoteUnknown: {
    running: 'cloud.analysis.unknown',
    cancelRequested: 'cloud.analysis.unknownCancelRequested',
  },
  failed: {
    prepare: 'workflow.failure.prepare',
    apply: 'workflow.failure.apply',
    load: 'workflow.failure.load',
    models: 'workflow.failure.models',
    refresh: 'workflow.failure.refresh',
    capabilities: 'cloud.analysis.failure.capabilities',
    propose: 'cloud.analysis.failure.propose',
    confirm: 'cloud.analysis.failure.confirm',
  },
}

/**
 * The catalogue keys an outcome is said with.
 *
 * @param {Outcome} outcome
 * @returns {{ title: string, body: string, params?: Record<string, unknown> }}
 */
export function outcomeCopy(outcome) {
  const title = TITLES[outcome.kind] ?? TITLES.failed
  if (outcome.kind === 'notReady') return { title, body: String(outcome.reason ?? ''), params: outcome.params }
  const reasons = REASON_BODIES[outcome.kind]
  if (reasons) {
    const body = (outcome.reason && reasons[outcome.reason]) || Object.values(reasons)[0]
    return { title, body, params: outcome.params }
  }
  return { title, body: BODIES[outcome.kind] ?? BODIES.modelFailed, params: outcome.params }
}

const TONES = {
  applied: 'ok',
  cancelled: 'info',
  undiscovered: 'info',
  declined: 'info',
  held: 'info',
  remoteCancelled: 'info',
  capabilityMissing: 'info',
  cloudDisabled: 'info',
}

/** @param {OutcomeKind} kind @returns {'ok'|'info'|'warn'} */
export function toneOf(kind) {
  return TONES[kind] ?? 'warn'
}

/* ------------------------------------------------------------------ */
/* Readiness and write eligibility                                     */
/* ------------------------------------------------------------------ */

/**
 * Whether an analysis came from a cloud GPU. Cloud evidence is review-only:
 * it never prepares a component write.
 *
 * @param {any} result
 */
export function isRemoteAnalysis(result) {
  return Boolean(result && (result.remoteSource || result.samBackend === 'remote' || result.rtBackend === 'remote'))
}

/**
 * Why a finished analysis cannot be written, or `null` when it can.
 *
 * The backend answers one boolean, `samWriteEligible`. The review says which
 * of the qualified conditions was missing, in the order a user can act on
 * them: where it ran first, then the backend they chose, then the page, then
 * the machine.
 *
 * @param {any} result - the analysis
 * @param {any} capabilities - the readiness the analysis ran under
 * @returns {string|null}
 */
export function declineReasonOf(result, capabilities) {
  if (!result) return null
  if (isRemoteAnalysis(result)) return 'remote'
  if (result.samWriteEligible) return null
  if (!result.samBackend) return 'regions'
  if (result.samBackend !== 'ort-webgpu') return 'cpu'
  const mime = /^data:([^;,]+)/.exec(String(result.sourceDataUrl ?? ''))?.[1] ?? ''
  if (mime === 'image/jpeg') return 'jpeg'
  if (mime && mime !== 'image/png') return 'format'
  if (!capabilities?.samWriteQualified) return 'host'
  const fallback = result.samCpuFallbackNodes
  if (Array.isArray(fallback) && fallback.some((count) => count > 0)) return 'provider'
  return 'host'
}

/** The load failures `diagnostics` names; anything else reads as the generic one. */
const LOAD_REASONS = new Set([
  'diagnostics.runtime.missing',
  'diagnostics.runtime.quarantined',
  'diagnostics.runtime.refused',
  'diagnostics.runtime.missingDependency',
  'diagnostics.runtime.unloadable',
])

/**
 * Whether the installed ONNX Runtime loads, from the `diagnostics` command,
 * which makes the same load an analysis makes. A missing answer is
 * `unchecked`, never `loaded`: a runtime file on disk can still fail to load
 * (a CUDA build without CUDA, a quarantined library).
 *
 * @param {any} answer - what `diagnostics()` resolved with
 * @returns {{state: 'loaded'|'unchecked'} | {state: 'failed', reasonKey: string}}
 */
export function runtimeLoadOf(answer) {
  const status = answer?.components?.find?.((component) => component?.name === 'onnxruntime')
  if (!status) return { state: 'unchecked' }
  if (status.available === true) return { state: 'loaded' }
  const reasonKey = String(status.reasonKey ?? '')
  return { state: 'failed', reasonKey: LOAD_REASONS.has(reasonKey) ? reasonKey : 'diagnostics.runtime.unloadable' }
}

/** What each runtime state that is not ready says, from `runtimeState`. */
const RUNTIME_KEYS = {
  unloadable: 'workflow.ready.runtimeUnloadable',
  checking: 'workflow.ready.runtimeChecking',
  unchecked: 'workflow.ready.runtimeUnchecked',
}

/**
 * What keeps the selected workflow from running, or `null` when it can run.
 * Each answer is a catalogue key naming the missing piece and where to fix it.
 *
 * The runtime is read the way Settings reads it (`runtimeState`): installed
 * is not enough, it must also load. `load` is `runtimeLoadOf`'s state, or
 * `checking` while that question is out. `workflow.ready.runtimeUnloadable`
 * takes the load failure as its `reasonKey`.
 *
 * @param {any} capabilities
 * @param {{needs: string[]}|undefined} preset
 * @param {{rtProfile: string, rtBackend: string, samBackend: string, verified: boolean|null,
 *   load?: 'loaded'|'failed'|'checking'|'unchecked'}} choice
 * @returns {string|null}
 */
export function readinessKeyOf(capabilities, preset, { rtProfile, rtBackend, samBackend, verified, load = 'checking' }) {
  if (!capabilities || !preset) return null
  const runtime = runtimeState({ installed: capabilities.runtimeInstalled === true }, preset.needs, load)
  if (runtime !== 'installed' && runtime !== 'notNeeded') return RUNTIME_KEYS[runtime] ?? 'workflow.ready.runtime'
  if (preset.needs.includes('ctd') && !capabilities.ctdInstalled) return 'workflow.ready.ctd'
  if (preset.needs.includes('rt')) {
    const installed = rtProfile === 'full-halves' ? capabilities.fullRtInstalled : capabilities.rtInstalled
    if (!installed) return 'workflow.ready.rt'
    if (rtBackend !== 'auto' && !capabilities.rtBackends?.some((entry) => entry.id === rtBackend && entry.selectable)) return 'workflow.ready.backend'
  }
  if (preset.needs.includes('sam')) {
    if (!capabilities.samInstalled) return 'workflow.ready.sam'
    if (verified !== true) return 'workflow.ready.samUnverified'
    if (!capabilities.samMemoryReady) return 'workflow.ready.memory'
    if (samBackend !== 'auto' && !capabilities.samBackends?.some((entry) => entry.id === samBackend && entry.selectable)) return 'workflow.ready.backend'
  }
  return null
}

/**
 * The component a point on the page lands on: the smallest box that holds it,
 * widened by a display-only tolerance so a one-pixel stroke can be clicked.
 * Components win over detector boxes, which are locators around them.
 *
 * @param {{x: number, y: number}} point - source pixels
 * @param {any} evidence
 * @param {number} tolerance - source pixels
 * @returns {string|null}
 */
export function hitTest(point, evidence, tolerance = 0) {
  if (!point || !evidence) return null
  for (const list of [evidence.components ?? [], evidence.regions ?? []]) {
    let best = null
    let bestArea = Infinity
    for (const entry of list) {
      const { x, y, w, h } = entry.bounds
      if (point.x < x - tolerance || point.y < y - tolerance ||
          point.x >= x + w + tolerance || point.y >= y + h + tolerance) continue
      const area = w * h
      if (area < bestArea) { best = entry.id; bestArea = area }
    }
    if (best) return best
  }
  return null
}
