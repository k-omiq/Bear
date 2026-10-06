/**
 * The cloud setup that is running, or last ran, kept outside the component that
 * started it.
 *
 * A first setup builds an image and pulls about 5.5 GB of weights, which is
 * minutes, and Settings can be closed in the meantime. The native command keeps
 * running whatever the interface does, so the run's state lives here, where a
 * `CloudProvisioner` mounted again finds it: the checklist carries on, and a
 * failure is still there to resume.
 *
 * **No secret ever enters this module's state.** The provider keys travel in
 * the `params` of the one call that uses them and are not kept; the component
 * that collected them holds them, and wipes them. What is kept, and persisted
 * so a restart can offer Resume, is the provider and the installation id,
 * which are names, not keys.
 *
 * Everything here is labelled through literal i18n keys, one per helper code,
 * step, resource type and health status, so the catalogue test can see every
 * one of them.
 */

import { getBackend } from '../api/backend.js'
import { notify } from '../state/app.svelte.js'
import { refreshCloudReadiness } from '../state/cloud.svelte.js'
import { readRecord, removeRecord, writeRecord } from '../state/persist.js'

/** The persisted marker of a setup that has not finished. */
const RECORD = 'cloudSetup.v1'

const PROVIDERS = new Set(['modal', 'beam'])
const INSTALLATION_ID = /^[a-z0-9][a-z0-9-]{2,40}$/
const STEP_STATES = new Set(['start', 'done', 'fail', 'skip'])
const ERROR_CODE = /^ERR_[A-Z0-9_]{1,48}$/

/**
 * Codes the helper answers before it changes anything, so a refused `apply`
 * leaves nothing to resume and a refused `resume` leaves the setup as it was.
 */
const NOTHING_RAN = new Set([
  'ERR_UNAPPROVED_PLAN',
  'ERR_PROVIDER_UNAVAILABLE',
  'ERR_HELPER_MISSING',
  'ERR_INVALID_REQUEST_PAYLOAD',
  'ERR_INVALID_PROTOCOL_VERSION',
  'ERR_UNSUPPORTED_OPERATION',
  'ERR_UNSUPPORTED_PROVIDER',
  'ERR_PAYLOAD_TOO_LARGE',
])

/**
 * One line of the live checklist.
 *
 * @typedef {Object} SetupStep
 * @property {string} id - an IC-2 step id
 * @property {'running'|'done'|'fail'|'skip'} state
 * @property {number|null} startedAt
 * @property {number|null} endedAt
 * @property {number|null} pct - only the weights download reports one
 */

/**
 * @typedef {Object} SetupRun
 * @property {number} id - which run this is, since a run is replaced, never edited
 * @property {'apply'|'resume'|'cleanup_apply'} op
 * @property {'modal'|'beam'} provider
 * @property {string} installationId
 * @property {'running'|'done'|'failed'} status
 * @property {string|null} errorCode - an `ERR_*` code when it failed
 * @property {SetupStep[]} steps - in the order the helper first reported them
 * @property {number} startedAt
 * @property {number|null} endedAt
 * @property {any} data - the helper's answer when it succeeded
 */

/**
 * A setup that started and has not finished: which account, which
 * installation, and the choices its plan was made with. `resume` has to be
 * asked with the same choices, or the helper regenerates a different plan and
 * refuses it as drift.
 *
 * @typedef {Object} UnfinishedSetup
 * @property {'modal'|'beam'} provider
 * @property {string} installationId
 * @property {SetupOptions} options
 */

/**
 * The plan choices the Review step offers. Absent means the helper's default.
 *
 * @typedef {Object} SetupOptions
 * @property {string} [gpu]
 * @property {number} [idle_seconds]
 * @property {string} [model_id]
 * @property {string[]} [analysis_models]
 * @property {boolean} [denoise] - page denoise; Modal only
 * @property {string} [routing_region] - where requests enter the provider; Modal only
 */

/** Where Modal's proxy for a gateway may sit (`deploy/cloud/common/deployment.py`). */
export const ROUTING_REGIONS = Object.freeze(['us-east', 'us-west', 'eu-west', 'ap-south'])

const GPU_NAME = /^[A-Za-z0-9_-]{1,32}$/
const MODEL_ID = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]{1,100}$/

/**
 * Only the two known choices, and only well-formed values of them: this is
 * read back from storage and sent to the helper.
 *
 * @param {unknown} value
 * @returns {SetupOptions}
 */
export function cleanOptions(value) {
  /** @type {SetupOptions} */
  const options = {}
  if (!value || typeof value !== 'object') return options
  const { gpu, idle_seconds: idle, model_id, analysis_models, denoise, routing_region: region } = /** @type {Record<string, unknown>} */ (value)
  if (typeof gpu === 'string' && GPU_NAME.test(gpu)) options.gpu = gpu
  if (typeof idle === 'number' && Number.isInteger(idle) && idle >= 0 && idle <= 86_400) options.idle_seconds = idle
  if (typeof model_id === 'string' && MODEL_ID.test(model_id)) options.model_id = model_id
  if (Array.isArray(analysis_models) && analysis_models.length <= 2 &&
      analysis_models.every((item) => item === 'text_mask_sam_ts@1' || item === 'text_regions_rt@1') &&
      new Set(analysis_models).size === analysis_models.length) options.analysis_models = analysis_models
  if (typeof denoise === 'boolean') options.denoise = denoise
  if (typeof region === 'string' && ROUTING_REGIONS.includes(region)) options.routing_region = region
  return options
}

/** @returns {UnfinishedSetup|null} */
function readUnfinished() {
  const record = /** @type {Record<string, unknown>|null} */ (readRecord(RECORD, null))
  if (!record) return null
  const provider = record.provider
  const installationId = record.installationId
  if (typeof provider !== 'string' || !PROVIDERS.has(provider)) return null
  if (typeof installationId !== 'string' || !INSTALLATION_ID.test(installationId)) return null
  return {
    provider: /** @type {'modal'|'beam'} */ (provider),
    installationId,
    options: cleanOptions(record.options),
  }
}

export const setup = $state({
  /** @type {SetupRun|null} */
  run: null,
  /**
   * A setup that started and never finished, from this run of the app or an
   * earlier one. Settings offers to resume it or clean it up.
   *
   * @type {UnfinishedSetup|null}
   */
  unfinished: readUnfinished(),
  /** How many provisioners are on screen, so a run nobody watches can say how it ended. */
  watchers: 0,
})

/* ------------------------------------------------------------------ */
/* Labels                                                              */
/* ------------------------------------------------------------------ */

const STEP_KEYS = new Map([
  ['inspect', 'settings.cloud.setup.step.inspect'],
  ['validate', 'settings.cloud.setup.step.validate'],
  ['volume', 'settings.cloud.setup.step.volume'],
  ['state', 'settings.cloud.setup.step.state'],
  ['secret', 'settings.cloud.setup.step.secret'],
  ['image', 'settings.cloud.setup.step.image'],
  ['deploy', 'settings.cloud.setup.step.deploy'],
  ['weights', 'settings.cloud.setup.step.weights'],
  ['token', 'settings.cloud.setup.step.token'],
  ['endpoint', 'settings.cloud.setup.step.endpoint'],
  ['health', 'settings.cloud.setup.step.health'],
  ['cleanup', 'settings.cloud.setup.step.cleanup'],
])

const ERROR_KEYS = new Map([
  ['ERR_ACTIONABLE_MISSING_PERMISSION', 'settings.cloud.setup.error.permission'],
  ['ERR_VALIDATION_ERROR', 'settings.cloud.setup.error.validation'],
  ['ERR_UNAPPROVED_PLAN', 'settings.cloud.setup.error.planChanged'],
  ['ERR_PLATFORM_GATED', 'settings.cloud.setup.error.platform'],
  ['ERR_PROVIDER_UNAVAILABLE', 'settings.cloud.setup.error.unavailable'],
  ['ERR_HELPER_MISSING', 'settings.cloud.setup.error.helperMissing'],
  ['ERR_EXECUTION_FAILED', 'settings.cloud.setup.error.failed'],
  ['ERR_EXECUTION_TIMEOUT', 'settings.cloud.setup.error.timeout'],
  ['ERR_SECRET_STORE', 'settings.cloud.setup.error.secretStore'],
  ['ERR_CONFIG_WRITE', 'settings.cloud.setup.error.configWrite'],
  ['ERR_CANCELLED', 'settings.cloud.setup.error.cancelled'],
  // The Modal token step saw a create intent with no recorded token: one may
  // exist that no cleanup can find. `CloudProvisioner` adds the recovery steps.
  ['ERR_ORPHANED_TOKEN', 'settings.cloud.setup.error.orphanedToken'],
  ['ERR_INVALID_REQUEST_PAYLOAD', 'settings.cloud.setup.error.request'],
  ['ERR_INVALID_PROTOCOL_VERSION', 'settings.cloud.setup.error.request'],
  ['ERR_UNSUPPORTED_OPERATION', 'settings.cloud.setup.error.request'],
  ['ERR_UNSUPPORTED_PROVIDER', 'settings.cloud.setup.error.request'],
  ['ERR_PAYLOAD_TOO_LARGE', 'settings.cloud.setup.error.request'],
  ['ERR_SECURITY_VIOLATION', 'settings.cloud.setup.error.request'],
])

const RESOURCE_KEYS = new Map([
  ['volume', 'settings.cloud.setup.resource.volume'],
  ['app', 'settings.cloud.setup.resource.app'],
  ['service', 'settings.cloud.setup.resource.app'],
  ['proxy_token', 'settings.cloud.setup.resource.token'],
  ['restricted_token', 'settings.cloud.setup.resource.token'],
  ['dict', 'settings.cloud.setup.resource.state'],
  ['gateway', 'settings.cloud.setup.resource.gateway'],
  ['worker', 'settings.cloud.setup.resource.worker'],
  ['secret', 'settings.cloud.setup.resource.secret'],
])

const HEALTH_KEYS = new Map([
  ['reachable', 'settings.inference.health.reachable'],
  ['http_error', 'settings.inference.health.httpError'],
  ['unauthorized', 'settings.inference.health.unauthorized'],
  ['credential_missing', 'settings.inference.health.credentialMissing'],
  ['configuration_error', 'settings.inference.health.configuration'],
  ['unreachable', 'settings.inference.health.unreachable'],
])

/** @param {string} step */
export function setupStepKey(step) {
  return STEP_KEYS.get(step) ?? 'settings.cloud.setup.step.working'
}

/**
 * @param {string|null|undefined} code
 * @param {SetupRun['op']} [op] - the run that failed, when a run did
 */
export function setupErrorKey(code, op) {
  // Only `apply` follows a plan the person reviewed. Resume and Update show
  // none, and the helper refuses them with this code when the key is for
  // another account than the one the setup was made in.
  if (code === 'ERR_UNAPPROVED_PLAN' && op === 'resume') return 'settings.cloud.setup.error.wrongAccount'
  return (typeof code === 'string' && ERROR_KEYS.get(code)) || 'settings.cloud.setup.error.generic'
}

/**
 * Why a `cleanup_apply` run did not finish. A key for another account is
 * refused before anything is deleted, which the general sentence would not say.
 *
 * @param {string|null|undefined} code
 */
export function cleanupErrorKey(code) {
  return code === 'ERR_UNAPPROVED_PLAN' ? 'settings.cloud.setup.error.cleanupWrongAccount' : 'settings.cloud.setup.error.cleanup'
}

/** @param {unknown} type */
export function resourceKey(type) {
  return (typeof type === 'string' && RESOURCE_KEYS.get(type)) || 'settings.cloud.setup.resource.unknown'
}

/**
 * The sentence for a connection check's status (`checkCloudConnection`, and
 * the `health` an installation answers with).
 *
 * @param {unknown} status
 */
export function healthKey(status) {
  return (typeof status === 'string' && HEALTH_KEYS.get(status)) || 'settings.inference.health.unknown'
}

/**
 * The stable code a helper answer failed with, or null when it did not name
 * one. The helper's own message is never shown: provider SDK errors can carry
 * a key or a signed URL.
 *
 * @param {any} envelope
 * @returns {string|null}
 */
export function errorCodeOf(envelope) {
  const code = envelope?.error?.code
  return typeof code === 'string' && ERROR_CODE.test(code) ? code : null
}

/* ------------------------------------------------------------------ */
/* Installation ids                                                    */
/* ------------------------------------------------------------------ */

/**
 * A fresh installation id, `mc-` and six lowercase letters or digits. Every
 * name the helper creates in the account carries it, so a cleanup can find
 * exactly what one setup made and nothing else.
 *
 * @returns {string}
 */
export function newInstallationId() {
  const alphabet = 'abcdefghijklmnopqrstuvwxyz0123456789'
  const bytes = new Uint8Array(6)
  globalThis.crypto.getRandomValues(bytes)
  return `mc-${Array.from(bytes, (byte) => alphabet[byte % alphabet.length]).join('')}`
}

/* ------------------------------------------------------------------ */
/* Installations that already exist                                    */
/* ------------------------------------------------------------------ */

/** The app name setup gives an installation, which is also its id. */
const INSTALLATION_APP_NAME = /^mc-[a-z0-9](?:[a-z0-9-]{0,22}[a-z0-9])?$/
const CAPABILITIES = new Set(['text_mask_sam_ts@1', 'text_regions_rt@1'])
const MAX_EPOCH_SECONDS = 4_102_444_800
/** A model id as the manifest spells one: `owner/name`, each starting with a letter or digit. */
const READY_MODEL_ID = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,59}\/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}$/

/**
 * One installation `inspect` found in the account (the native side has
 * already rebuilt it from allowlisted fields; this is the webview's own check).
 *
 * @typedef {Object} ExistingInstallation
 * @property {string} installationId
 * @property {number|null} createdAt - unix seconds
 * @property {number|null} deployedAt - unix seconds
 * @property {boolean} weightsChecked - whether its storage could be listed
 * @property {string[]} modelsReady - model ids whose download finished
 * @property {string[]} analysisReady - analysis capabilities whose graphs are in place
 * @property {SetupOptions|null} options - the choices it recorded, when it did
 * @property {boolean} onThisComputer - this computer's journal can resume it
 */

/** @param {unknown} value */
const epoch = (value) =>
  typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= MAX_EPOCH_SECONDS ? value : null

/**
 * The installations an `inspect` answer lists, newest first as it sent them,
 * each one well formed or left out.
 *
 * @param {unknown} data - `inspect`'s `data`
 * @returns {ExistingInstallation[]}
 */
export function existingInstallationsOf(data) {
  const list = /** @type {any} */ (data)?.existing_installations
  if (!Array.isArray(list)) return []
  /** @type {ExistingInstallation[]} */
  const found = []
  for (const item of list.slice(0, 20)) {
    if (!item || typeof item !== 'object') continue
    const id = item.installation_id
    if (typeof id !== 'string' || !INSTALLATION_APP_NAME.test(id) || item.app_name !== id) continue
    if (found.some((entry) => entry.installationId === id)) continue
    const strings = (/** @type {unknown} */ value, /** @type {(v: string) => boolean} */ valid) =>
      Array.isArray(value) ? [...new Set(value.filter((v) => typeof v === 'string' && valid(v)))] : []
    const options = item.options && typeof item.options === 'object' ? cleanOptions(item.options) : null
    found.push({
      installationId: id,
      createdAt: epoch(item.created_at),
      deployedAt: epoch(item.deployed_at),
      weightsChecked: item.weights_checked === true,
      modelsReady: strings(item.models_ready, (v) => READY_MODEL_ID.test(v)),
      analysisReady: strings(item.analysis_ready, (v) => CAPABILITIES.has(v)),
      // All four recorded, or none: half a set of choices is not what it runs.
      options: options && options.gpu && options.idle_seconds && options.model_id && options.analysis_models ? options : null,
      onThisComputer: item.on_this_computer === true,
    })
  }
  return found
}

/**
 * Whether reusing `entry` with `options` downloads nothing: the model and
 * every selected analysis graph are already in its storage, and page denoise
 * is off or the installation already recorded it on (discovery lists no
 * denoise models, so only that record says they are on its volume).
 *
 * @param {ExistingInstallation} entry
 * @param {{model_id?: string, analysis_models?: string[], denoise?: boolean}} options
 */
export function reuseDownloadsNothing(entry, options) {
  if (!entry.weightsChecked) return false
  if (options.denoise === true && entry.options?.denoise !== true) return false
  const model = options.model_id
  if (!model || !entry.modelsReady.includes(model)) return false
  return (options.analysis_models ?? []).every((capability) => entry.analysisReady.includes(capability))
}

export { PAUSED_PROVIDERS, providerPaused } from '../model/providers.js'

/**
 * The Modal account an endpoint runs in, read from its host: Modal serves a
 * web endpoint at `<workspace>[-<environment>]--<label>.modal.run`, so what
 * comes before `--` names the workspace (and its environment when that is not
 * the main one). Null for Beam, for another host, and for one that does not
 * parse. It only labels the endpoint; nothing is checked against it.
 *
 * @param {unknown} provider
 * @param {unknown} endpointUrl
 * @returns {string|null}
 */
export function endpointAccount(provider, endpointUrl) {
  if (provider !== 'modal') return null
  let host
  try {
    host = new URL(String(endpointUrl)).hostname.toLowerCase().replace(/\.+$/, '')
  } catch {
    return null
  }
  if (!host.endsWith('.modal.run')) return null
  const cut = host.indexOf('--')
  return cut > 0 ? host.slice(0, cut) : null
}

/**
 * The endpoints setup made on this computer, from `inference.json`: the ones
 * whose id is an installation id (an endpoint added by hand has an `ep_` id).
 *
 * @param {any} config
 * @returns {Array<{provider: 'modal'|'beam', installationId: string, name: string, account: string|null}>}
 */
export function setupsOnThisComputer(config) {
  /** @type {Array<{provider: 'modal'|'beam', installationId: string, name: string, account: string|null}>} */
  const list = []
  if (!config || typeof config !== 'object') return list
  for (const provider of /** @type {const} */ (['modal', 'beam'])) {
    const profiles = provider === 'modal' ? config.modalProfiles : config.beamProfiles
    if (!profiles || typeof profiles !== 'object') continue
    for (const [id, profile] of Object.entries(profiles)) {
      if (!INSTALLATION_ID.test(id) || !profile || typeof profile !== 'object') continue
      const name = typeof profile.name === 'string' && profile.name.trim() ? profile.name.trim().slice(0, 120) : id
      list.push({ provider, installationId: id, name, account: endpointAccount(provider, profile.endpointUrl) })
    }
  }
  return list
}

/**
 * @param {UnfinishedSetup|null} marker
 */
function setUnfinished(marker) {
  setup.unfinished = marker
  if (marker) {
    writeRecord(RECORD, {
      provider: marker.provider,
      installationId: marker.installationId,
      options: marker.options,
    })
  } else {
    removeRecord(RECORD)
  }
}

/** Forget an unfinished setup without touching the cloud. */
export function forgetUnfinished() {
  setUnfinished(null)
}

/* ------------------------------------------------------------------ */
/* Runs                                                                */
/* ------------------------------------------------------------------ */

let runSeq = 0

/**
 * The call that runs the helper: a test's stand-in, or the backend's.
 *
 * @typedef {(spec: {op: string, provider: string, params: Record<string, unknown>}) => Promise<any>} Runner
 */

/**
 * @param {import('../api/backend.js').Backend} backend
 * @returns {Runner}
 */
export function backendRunner(backend = getBackend()) {
  return (spec) => backend.runCloudProvisioner(spec)
}

/**
 * One IC-2 event. Events for another provider or operation, or of any other
 * shape, change nothing.
 *
 * @param {unknown} payload
 */
export function onSetupProgress(payload) {
  const run = setup.run
  if (!run || run.status !== 'running' || !payload || typeof payload !== 'object') return
  const event = /** @type {Record<string, unknown>} */ (payload)
  const step = event.step
  const state = event.state
  if (typeof step !== 'string' || !STEP_KEYS.has(step)) return
  if (typeof state !== 'string' || !STEP_STATES.has(state)) return
  if (event.provider !== undefined && event.provider !== run.provider) return
  if (event.op !== undefined && event.op !== run.op) return
  const pct = typeof event.pct === 'number' && Number.isFinite(event.pct)
    ? Math.max(0, Math.min(100, Math.round(event.pct)))
    : null
  const now = Date.now()
  const existing = run.steps.find((candidate) => candidate.id === step)
  /** @type {SetupStep} */
  const next = {
    id: step,
    state: state === 'start' ? 'running' : /** @type {'done'|'fail'|'skip'} */ (state),
    startedAt: existing?.startedAt ?? (state === 'skip' ? null : now),
    endedAt: state === 'start' ? null : now,
    pct: pct ?? existing?.pct ?? null,
  }
  setup.run = {
    ...run,
    steps: existing
      ? run.steps.map((candidate) => (candidate.id === step ? next : candidate))
      : [...run.steps, next],
  }
}

/**
 * Run `apply`, `resume` or `cleanup_apply` with a live checklist.
 *
 * Only one runs at a time. The run is kept when it ends so the component can
 * show how it ended; `clearSetupRun` drops it.
 *
 * @param {{op: 'apply'|'resume'|'cleanup_apply', provider: 'modal'|'beam', installationId: string, params: Record<string, unknown>}} spec
 * @param {Runner} runner
 * @param {import('../api/backend.js').Backend} [backend] - for the progress event
 * @returns {Promise<{ok: boolean, data: any, errorCode: string|null}>}
 */
export async function runSetup(spec, runner, backend = getBackend()) {
  if (setup.run?.status === 'running') return { ok: false, data: null, errorCode: 'ERR_EXECUTION_FAILED' }
  runSeq += 1
  const id = runSeq
  /** @type {SetupRun} */
  const run = {
    id,
    op: spec.op,
    provider: spec.provider,
    installationId: spec.installationId,
    status: 'running',
    errorCode: null,
    steps: [],
    startedAt: Date.now(),
    endedAt: null,
    data: null,
  }
  setup.run = run
  const before = setup.unfinished
  if (spec.op !== 'cleanup_apply') {
    setUnfinished({
      provider: spec.provider,
      installationId: spec.installationId,
      options: cleanOptions(spec.params.options),
    })
  }

  const listener = { ended: false, /** @type {(() => void)|null} */ off: null }
  try {
    Promise.resolve(backend?.onProvisionProgress?.(onSetupProgress))
      .then((off) => {
        if (typeof off !== 'function') return
        if (listener.ended) off()
        else listener.off = off
      })
      .catch(() => {})
  } catch {
    // No progress channel: the checklist stays on its one "working" line.
  }

  /** @type {any} */
  let envelope = null
  try {
    envelope = await runner({ op: spec.op, provider: spec.provider, params: spec.params })
  } catch {
    envelope = null
  } finally {
    listener.ended = true
    listener.off?.()
  }

  const ok = envelope?.success === true
  const errorCode = ok ? null : errorCodeOf(envelope) ?? 'ERR_EXECUTION_FAILED'
  const data = ok ? envelope.data ?? null : null
  if (ok && spec.op === 'cleanup_apply') {
    // What the setup saved on this machine points at what was just deleted.
    // The run is not over until that is gone too, so whatever shows the
    // endpoints after it reads them without it.
    if (setup.unfinished?.installationId === spec.installationId) setUnfinished(null)
    try {
      await removeEndpoint({ provider: spec.provider, profileId: spec.installationId }, backend)
    } catch {
      // The endpoint stays listed and fails its check; Remove takes it away.
    }
  } else if (ok) {
    setUnfinished(null)
  } else if (errorCode && NOTHING_RAN.has(errorCode)) {
    // Refused before anything changed: an `apply` left nothing to resume, and
    // an Update of a finished setup did not leave it unfinished.
    setUnfinished(before)
  }
  if (setup.run?.id === id) {
    setup.run = {
      ...setup.run,
      status: ok ? 'done' : 'failed',
      errorCode,
      endedAt: Date.now(),
      data,
    }
  }
  if (ok) void refreshCloudReadiness(backend)
  if (setup.watchers === 0) {
    const cleanup = spec.op === 'cleanup_apply'
    if (ok) notify({ key: cleanup ? 'notice.cloud.cleanupDone' : 'notice.cloud.setupDone' })
    else notify({ key: cleanup ? 'notice.cloud.cleanupFailed' : 'notice.cloud.setupFailed', tone: 'warn' })
  }
  return { ok, data, errorCode }
}

/**
 * Plan a cleanup of what one installation created. Changes nothing.
 *
 * @param {{provider: 'modal'|'beam', installationId: string}} spec
 * @param {Runner} runner
 * @returns {Promise<{ok: boolean, plan: any, errorCode: string|null}>}
 */
export async function planCleanup(spec, runner) {
  /** @type {any} */
  let envelope = null
  try {
    envelope = await runner({
      op: 'cleanup_plan',
      provider: spec.provider,
      params: { installation_id: spec.installationId },
    })
  } catch {
    envelope = null
  }
  if (envelope?.success !== true || !envelope.data) {
    return { ok: false, plan: null, errorCode: errorCodeOf(envelope) ?? 'ERR_EXECUTION_FAILED' }
  }
  return { ok: true, plan: envelope.data, errorCode: null }
}

/**
 * Delete what a cleanup plan lists, and then the endpoint the setup saved on
 * this machine (`runSetup` does that before the run counts as done). The
 * caller has shown the list and the person has confirmed it, weights volume
 * included, which is what `confirm_delete_persistent_storage` records.
 *
 * @param {{provider: 'modal'|'beam', installationId: string, keys: Record<string, unknown>, planHash: string}} spec
 * @param {Runner} runner
 * @param {import('../api/backend.js').Backend} [backend]
 */
export function applyCleanup(spec, runner, backend = getBackend()) {
  return runSetup(
    {
      op: 'cleanup_apply',
      provider: spec.provider,
      installationId: spec.installationId,
      params: {
        ...spec.keys,
        installation_id: spec.installationId,
        approved_cleanup_plan_hash: spec.planHash,
        confirm_delete_persistent_storage: true,
      },
    },
    runner,
    backend,
  )
}

/**
 * Take an endpoint out of the inference config, and its access token out of
 * the keychain. The default falls back to local when it was this one.
 * Nothing in the cloud is touched.
 *
 * @param {{provider: 'modal'|'beam', profileId: string}} spec
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<boolean>} whether there was one to remove
 * @throws when the config cannot be read or written
 */
export async function removeEndpoint(spec, backend = getBackend()) {
  const config = await backend.readInferenceConfig()
  const key = spec.provider === 'beam' ? 'beamProfiles' : 'modalProfiles'
  const profiles = config?.[key] ?? {}
  if (!Object.prototype.hasOwnProperty.call(profiles, spec.profileId)) return false
  const { [spec.profileId]: _removed, ...rest } = profiles
  const selected = config.selectedTarget
  const wasSelected = selected?.type === spec.provider && selected.profile_id === spec.profileId
  // Native secret keys are bound to the profile's origin. Delete while that
  // profile still exists; keep it accessible if credential removal fails.
  for (const role of ['runtime', 'setup']) {
    await backend.deleteCloudSecret({ provider: spec.provider, profileId: spec.profileId, role })
  }
  await backend.writeInferenceConfig({
    config: { ...config, [key]: rest, selectedTarget: wasSelected ? { type: 'local' } : selected },
  })
  void refreshCloudReadiness(backend)
  return true
}

/**
 * Ask the running helper to stop. Its journal keeps what finished, so Resume
 * picks up from there. The run ends with `ERR_CANCELLED`.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<boolean>} whether a stop was delivered
 */
export async function stopSetup(backend = getBackend()) {
  try {
    const answer = await backend.cancelCloudProvisioner()
    return /** @type {any} */ (answer)?.cancelled === true
  } catch {
    return false
  }
}

/** Drop the last run's state, once whatever showed it is done with it. */
export function clearSetupRun() {
  if (setup.run?.status === 'running') return
  setup.run = null
}

/** A provisioner came on screen; it says how the run ended from now on. */
export function watchSetup() {
  setup.watchers += 1
  return () => {
    setup.watchers = Math.max(0, setup.watchers - 1)
  }
}
