/**
 * The cloud state the whole interface shares.
 *
 * Three things, each with one owner here so no component keeps a copy of its
 * own:
 *
 * 1. **Readiness.** Whether a cloud render can be offered now, and if not the
 *    first thing missing (`api/backend.js#readCloudReadiness`). The tool bar
 *    gates the Cloud engine on it and Settings > Cloud prints it. It is read
 *    again whenever the permission changes, and by anything that changes the
 *    endpoint or its token.
 * 2. **Jobs.** The cloud renders in flight, from the `cloud://attempt` event
 *    (IC-3) and from the calls that start them, so the status element can show
 *    each one's phase and elapsed time and offer Cancel from the first moment.
 *    Every render ends in exactly one notice, whichever of the event and the
 *    command's own answer arrives first.
 * 3. **Recovery.** Renders a previous session left behind are settled once,
 *    when the app starts with cloud allowed, or the first time it is allowed
 *    after that. It never sends anything new: an unknown submission is
 *    reported, not resubmitted. Waiting on an accepted job does contact the
 *    provider, which is why it waits for the permission.
 */

import { dismissQwenReview, reviewQwen } from '../editor/qwenflow.js'
import { untrack } from 'svelte'
import { getBackend, readCloudReadiness } from '../api/backend.js'
import { notify, pushModal } from './app.svelte.js'
import { editor, replaceRegion } from './editor.svelte.js'
import { backendSettingsPatch, session, setCloudAllowed } from './session.svelte.js'
import { writeSettingsSerialized } from './settingswrite.js'
import { currentCloudModelId } from '../model/model-names.js'
import { providerPaused } from '../model/providers.js'
import { checkConfiguration, configurationKey, configurationFailed, loadCloudConfiguration, rememberedConfiguration, resetCloudConfiguration } from './cloudconfig.svelte.js'

/**
 * A render the status element shows.
 *
 * @typedef {Object} CloudJob
 * @property {string} attemptId
 * @property {{provider: 'modal'|'beam', profileId: string}|null} target
 * @property {string|null} configurationKey - the deployment revision this job started on
 * @property {string|null} regionId
 * @property {string|null} chapterId
 * @property {number|null} pageIndex
 * @property {string} phase - a non-terminal IC-3 phase
 * @property {number} startedAt - `Date.now()` when the render started
 * @property {boolean} cold - no render has finished recently, so a GPU may be starting
 * @property {boolean} cancelling
 * @property {boolean} background - no command is waiting on it: recovery resumed it,
 *   so a commit has to be read back into the open chapter
 */

/**
 * How an ending is reported. `quiet` endings say nothing: the backend already
 * said it (`notice.cloud.blocked`).
 *
 * @typedef {Object} CloudOutcome
 * @property {'committed'|'failed'|'cancelled'|'unknown'} phase
 * @property {string|null} [errorCode]
 * @property {boolean} [quiet]
 */

/** How recently a render must have finished for the GPU to count as warm. */
export const WARM_MS = 5 * 60 * 1000

/**
 * How long a command's answer waits for the event that carries its code. The
 * native side emits the terminal event before the command answers, so this is
 * only ever spent when an event was lost.
 */
export const SETTLE_GRACE_MS = 1500

const RUNNING_PHASES = Object.freeze([
  'preparing',
  'submitting',
  'queued',
  'running',
  'downloading',
  'reviewing',
  'compositing',
])
const ENDING_PHASES = Object.freeze(['committed', 'failed', 'cancelled', 'unknown'])

/** The phase line the status element shows, one literal key per IC-3 phase. */
const PHASE_KEYS = new Map([
  ['preparing', 'notice.cloud.phase.preparing'],
  ['submitting', 'notice.cloud.phase.submitting'],
  ['queued', 'notice.cloud.phase.queued'],
  ['running', 'notice.cloud.phase.running'],
  ['downloading', 'notice.cloud.phase.downloading'],
  ['reviewing', 'qwen.review.title'],
  ['compositing', 'notice.cloud.phase.compositing'],
])

/**
 * Why a render did not finish, per stable error code. Codes that mean the same
 * thing to the person reading the notice share a sentence. Anything not listed
 * reads `notice.cloud.error.generic`.
 */
const ERROR_KEYS = new Map([
  ['cloud_disabled', 'notice.cloud.error.disabled'],
  ['target_invalid', 'notice.cloud.error.target'],
  ['target_changed', 'notice.cloud.error.target'],
  ['profile_missing', 'notice.cloud.error.target'],
  ['endpoint_invalid', 'notice.cloud.error.target'],
  ['credential_missing', 'notice.cloud.error.credential'],
  ['consent_invalid', 'notice.cloud.error.consent'],
  ['region_not_found', 'notice.cloud.error.region'],
  ['region_changed', 'notice.cloud.error.region'],
  ['region_unsupported', 'notice.cloud.error.regionUnsupported'],
  ['attempt_busy', 'notice.cloud.error.busy'],
  ['attempt_exists', 'notice.cloud.error.busy'],
  ['journal_error', 'notice.cloud.error.local'],
  ['project_error', 'notice.cloud.error.local'],
  ['config_unreadable', 'notice.cloud.error.local'],
  ['invalid_request', 'notice.cloud.error.local'],
  ['result_invalid', 'notice.cloud.error.result'],
  ['gateway_protocol', 'notice.cloud.error.result'],
  ['gateway_unauthorized', 'notice.cloud.error.unauthorized'],
  ['gateway_error', 'notice.cloud.error.gateway'],
  ['gateway_unreachable', 'notice.cloud.error.unreachable'],
  ['submission_unknown', 'notice.cloud.error.submission'],
  ['edit_not_applied', 'qwen.review.failed'],
  ['qwen_review_required', 'qwen.review.required'],
  ['remote_failed', 'notice.cloud.error.remote'],
  ['weights_unavailable', 'notice.cloud.error.weights'],
  ['remote_cancelled', 'notice.cloud.error.remoteCancelled'],
  ['cancelled', 'notice.cloud.error.cancelled'],
  ['poll_timeout', 'notice.cloud.error.timeout'],
])

/**
 * @param {string|null|undefined} code
 * @returns {string} the i18n key for why a render did not finish
 */
export function cloudErrorKey(code) {
  return (typeof code === 'string' && ERROR_KEYS.get(code)) || 'notice.cloud.error.generic'
}

/**
 * @param {string} phase
 * @returns {string} the i18n key for a running phase
 */
export function cloudPhaseKey(phase) {
  return PHASE_KEYS.get(phase) ?? 'notice.cloud.phase.preparing'
}

/** @returns {import('../api/backend.js').CloudReadiness} */
function unknownReadiness() {
  return {
    allowed: false,
    configured: false,
    ready: false,
    reason: 'unknown',
    target: null,
    profile: null,
    endpoints: [],
  }
}

export const cloud = $state({
  /** @type {import('../api/backend.js').CloudReadiness} */
  readiness: unknownReadiness(),
  /** False until one read has answered, so nothing says "not ready" before it is known. */
  checked: false,
  /** @type {CloudJob[]} */
  jobs: [],
  /** When a render last committed, for the first-run hint. */
  lastCommitAt: 0,
  /** @type {import('../api/backend.js').CloudRecoveryReport|null} */
  recovery: null,
  /** Model metadata read for the current endpoint during a consent proposal. */
  model: null,
  /**
   * The model each saved profile runs, by `profileModelKey`, so the engine
   * pickers can list every profile's model at once (`cloudProfileChoices`).
   *
   * @type {Record<string, {state: 'checking'|'ready'|'failed', id: string|null, at: number}>}
   */
  profileModels: {},
})

/* ------------------------------------------------------------------ */
/* Readiness                                                           */
/* ------------------------------------------------------------------ */

let readinessSeq = 0
let modelEpoch = 0
const modelFetches = new Map()

function modelTargetKey(readiness) {
  const target = readiness?.target
  if (!target || !readiness?.profile) return null
  return `${target.type}:${target.profile_id}:${readiness.profile.updatedAtMs ?? ''}:${readiness.profile.endpointUrl ?? ''}`
}

/** Read only endpoint metadata; a failed lookup leaves the ordinary Cloud label. */
function refreshCloudModel(readiness, backend) {
  const key = modelTargetKey(readiness)
  if (!session.cloudAllowed || !readiness.allowed || !readiness.configured || !key || currentCloudModelId(cloud)) return
  if (modelFetches.has(key)) return
  const target = readiness.target
  const remember = (/** @type {string} */ id) => {
    cloud.model = { id, provider: target.type, profileId: target.profile_id,
      updatedAtMs: readiness.profile.updatedAtMs ?? null }
  }
  // Read already while it was another profile in the list.
  const listed = cloud.profileModels[selectedProfileModelKey(readiness)]
  if (listed?.state === 'ready' && listed.id) {
    remember(listed.id)
    return
  }
  const epoch = modelEpoch
  const promise = Promise.resolve()
    .then(async () => {
      const configKey = configurationKey(readiness)
      const known = await checkConfiguration(backend, configKey, 'clean', async () => {
        const info = await backend.getCloudModelInfo({ provider: target.type, profileId: target.profile_id })
        return { state: 'ready', modelId: info.pinnedModelId, presets: [] }
      })
      return { pinnedModelId: known?.modelId }
    })
    .then((info) => {
      if (epoch !== modelEpoch || modelTargetKey(cloud.readiness) !== key || !session.cloudAllowed) return
      if (typeof info?.pinnedModelId !== 'string' || !info.pinnedModelId) return
      remember(info.pinnedModelId)
      // And for the list, once another profile is the default.
      cloud.profileModels = { ...cloud.profileModels,
        [selectedProfileModelKey(readiness)]: { state: 'ready', id: info.pinnedModelId, at: Date.now() } }
    })
    .catch(() => {})
    .finally(() => { if (modelFetches.get(key) === promise) modelFetches.delete(key) })
  modelFetches.set(key, promise)
}

/**
 * Read readiness again. Overlapping reads keep only the newest answer.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<import('../api/backend.js').CloudReadiness>}
 */
export async function refreshCloudReadiness(backend = getBackend()) {
  readinessSeq += 1
  const mine = readinessSeq
  const verdict = await readCloudReadiness(backend)
  await loadCloudConfiguration(backend)
  if (mine === readinessSeq) {
    cloud.readiness = verdict
    cloud.checked = true
    if (!currentCloudModelId(cloud)) cloud.model = null
    refreshCloudModel(verdict, backend)
    refreshProfileModels(verdict, backend)
    const key = configurationKey(verdict)
    if (session.cloudAllowed && verdict.allowed && verdict.configured && key) {
      void checkConfiguration(backend, key, 'denoise', async () => ({ state: 'ready', modelId: null,
        presets: await backend.cloudDenoisePresets({ provider: verdict.target.type, profileId: verdict.target.profile_id }) }))
    }
  }
  return verdict
}

/* ------------------------------------------------------------------ */
/* Every profile's model                                               */
/* ------------------------------------------------------------------ */

/** A failed read is asked again after this long, never on every refresh. */
const PROFILE_MODEL_RETRY_MS = 60_000

/** @param {{provider: string, id: string, endpointUrl?: string, updatedAtMs?: number}} endpoint */
function profileModelKey(endpoint) {
  return `${endpoint.provider}:${endpoint.id}:${endpoint.updatedAtMs ?? ''}:${endpoint.endpointUrl ?? ''}`
}

/** @param {import('../api/backend.js').CloudReadiness} readiness */
function selectedProfileModelKey(readiness) {
  const target = readiness?.target
  return target ? profileModelKey({ provider: target.type, id: target.profile_id, updatedAtMs: readiness.profile?.updatedAtMs, endpointUrl: readiness.profile?.endpointUrl }) : ''
}

let profileModelsReading = false

/**
 * Read the model of every saved profile not read yet: the gateway's model
 * info, metadata only, so no page is sent and no GPU starts. One profile at a
 * time, so a keychain asks for one token at once, and again only when an
 * endpoint changes or a failed read is a minute old. A profile that cannot
 * answer is listed by its name alone.
 *
 * @param {import('../api/backend.js').CloudReadiness} readiness
 * @param {import('../api/backend.js').Backend} backend
 */
function refreshProfileModels(readiness, backend) {
  if (profileModelsReading || !session.cloudAllowed || !readiness.allowed) return
  const now = Date.now()
  const target = readiness.target
  const wanted = (readiness.endpoints ?? []).filter((endpoint) => {
    // The default's model is `refreshCloudModel`'s to read.
    if (providerPaused(endpoint.provider) || (target?.type === endpoint.provider && target?.profile_id === endpoint.id)) return false
    const known = cloud.profileModels[profileModelKey(endpoint)]
    return !known || (known.state === 'failed' && now - known.at >= PROFILE_MODEL_RETRY_MS)
  })
  if (!wanted.length) return
  profileModelsReading = true
  // A stop (`stopCloud`) forgets the list; an answer from before it is dropped.
  const epoch = modelEpoch
  void (async () => {
    try {
      for (const endpoint of wanted) {
        if (!session.cloudAllowed || epoch !== modelEpoch) return
        const key = profileModelKey(endpoint)
        cloud.profileModels = { ...cloud.profileModels, [key]: { state: 'checking', id: null, at: Date.now() } }
        /** @type {string|null} */
        let id = null
        try {
          const known = await checkConfiguration(backend, configurationKey(readiness, { provider: endpoint.provider, profileId: endpoint.id }), 'clean', async () => {
            const info = await backend.getCloudModelInfo({ provider: endpoint.provider, profileId: endpoint.id })
            return { state: 'ready', modelId: info.pinnedModelId, presets: [] }
          })
          id = typeof known?.modelId === 'string' && known.modelId ? known.modelId : null
        } catch {
          // Listed by name; asked again later.
        }
        if (epoch !== modelEpoch) return
        cloud.profileModels = { ...cloud.profileModels, [key]: { state: id ? 'ready' : 'failed', id, at: Date.now() } }
      }
    } finally {
      profileModelsReading = false
    }
    // An endpoint saved while this was reading.
    refreshProfileModels(cloud.readiness, backend)
  })()
}

/**
 * @typedef {Object} CloudProfileChoice
 * @property {'modal'|'beam'} provider
 * @property {string} profileId
 * @property {string} name
 * @property {string|null} modelId - null until read, or when it cannot be
 * @property {boolean} selected - the default: where a run with Cloud goes
 */

/**
 * Every saved cloud profile an engine picker can offer, with the model it
 * runs: choosing a model is choosing its profile. In name order, since the
 * native config keeps no order. Empty while cloud is off.
 *
 * @returns {CloudProfileChoice[]}
 */
export function cloudProfileChoices() {
  if (!session.cloudAllowed) return []
  const target = cloud.readiness.target
  return (cloud.readiness.endpoints ?? [])
    .filter((endpoint) => !providerPaused(endpoint.provider))
    .map((endpoint) => {
      const selected = target?.type === endpoint.provider && target?.profile_id === endpoint.id
      return {
        provider: endpoint.provider,
        profileId: endpoint.id,
        name: endpoint.name || endpoint.id,
        modelId: (selected ? currentCloudModelId(cloud) : null) ?? cloud.profileModels[profileModelKey(endpoint)]?.id ?? null,
        selected,
      }
    })
    .sort((a, b) => a.name.localeCompare(b.name) || a.profileId.localeCompare(b.profileId))
}

const CHOICE_PREFIX = 'cloud@'

/**
 * The cloud entries an engine picker shows in place of its one Cloud entry,
 * once there is more than one profile to choose from; null otherwise, and the
 * picker keeps its single entry.
 *
 * The selected profile's entry keeps the value `cloud`, so what a picker
 * stores stays `cloud` and a run goes to the default. Another profile's is
 * `cloud@<provider>:<id>`, which `pickCloudChoice` turns into a switch of the
 * default. With no profile selected a plain `cloud` entry leads, so a stored
 * `cloud` still has its entry.
 *
 * @returns {Array<{value: string, choice: CloudProfileChoice|null}>|null}
 */
export function cloudEntries() {
  const choices = cloudProfileChoices()
  if (choices.length < 2) return null
  const entries = choices.map((choice) => ({
    value: choice.selected ? 'cloud' : `${CHOICE_PREFIX}${choice.provider}:${choice.profileId}`,
    choice: /** @type {CloudProfileChoice|null} */ (choice),
  }))
  return choices.some((choice) => choice.selected) ? entries : [{ value: 'cloud', choice: null }, ...entries]
}

/**
 * The profile a `cloud@<provider>:<id>` entry names, or null for any other
 * value. Profile ids hold no `:` (`config::validate_profile_id`).
 *
 * @param {unknown} value
 * @returns {{provider: 'modal'|'beam', profileId: string}|null}
 */
export function parseCloudChoice(value) {
  if (typeof value !== 'string' || !value.startsWith(CHOICE_PREFIX)) return null
  const rest = value.slice(CHOICE_PREFIX.length)
  const colon = rest.indexOf(':')
  const provider = rest.slice(0, colon)
  const profileId = rest.slice(colon + 1)
  return colon > 0 && profileId && (provider === 'modal' || provider === 'beam')
    ? { provider, profileId }
    : null
}

let picking = Promise.resolve(true)

/**
 * Make the profile a picker entry names the default, then read readiness
 * again. Picks queue behind one another, so the last one chosen is the one
 * that stands. Work already running on another profile goes on. `true` when
 * the entry is a plain `cloud` or the switch was saved.
 *
 * @param {unknown} value - an entry's value from `cloudEntries`
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<boolean>}
 */
export function pickCloudChoice(value, backend = getBackend()) {
  const choice = parseCloudChoice(value)
  if (!choice) return Promise.resolve(value === 'cloud')
  const run = picking.then(async () => {
    try {
      await backend.selectCloudProfile(choice)
      return true
    } catch {
      notify({ key: 'cloud.profile.selectFailed', tone: 'warn' })
      return false
    } finally {
      await refreshCloudReadiness(backend)
    }
  })
  picking = run.catch(() => false)
  return run
}

/**
 * Whether the Cloud engine can be chosen: the permission is on and a cloud
 * endpoint with a token is the default.
 *
 * @returns {boolean}
 */
export function cloudUsable() {
  return session.cloudAllowed && cloud.readiness.configured
}

/** Cleaning availability is independent of page denoise on the same profile. */
export function cloudCleanAvailable(target = null) {
  return rememberedConfiguration(configurationKey(cloud.readiness, target))?.clean?.state !== 'missing'
}

/** A connection test explicitly rechecks both operations after setup is repaired. */
export async function recheckCloudConfiguration(target, backend = getBackend()) {
  const key = configurationKey(cloud.readiness, target)
  if (!session.cloudAllowed || !key) return
  await checkConfiguration(backend, key, 'clean', async () => {
    const info = await backend.getCloudModelInfo(target)
    return { state: 'ready', modelId: info.pinnedModelId, presets: [] }
  }, true)
  await checkConfiguration(backend, key, 'denoise', async () => ({ state: 'ready', modelId: null,
    presets: await backend.cloudDenoisePresets(target) }), true)
}

/**
 * The permission switch, saved where the backend reads it. Put back when the
 * save fails, so the switch never shows a state the backend does not have.
 *
 * @param {boolean} allowed
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<boolean>} whether it was saved
 */
export async function setCloudPermission(allowed, backend = getBackend()) {
  const previous = session.cloudAllowed
  setCloudAllowed(allowed)
  try {
    await writeSettingsSerialized(backend, () => backendSettingsPatch())
  } catch {
    setCloudAllowed(previous)
    notify({ key: 'notice.cloud.permissionFailed', tone: 'warn' })
    return false
  }
  await refreshCloudReadiness(backend)
  return true
}

/** Settings, opened on its Cloud tab. */
export function openCloudSettings() {
  pushModal({ kind: 'settings', props: { tab: 'inference' } })
}

/* ------------------------------------------------------------------ */
/* Start and stop                                                      */
/* ------------------------------------------------------------------ */

/** @type {{backend: import('../api/backend.js').Backend, unlisten: Array<() => void>, stopEffect: () => void, recovered: boolean}|null} */
let started = null

/**
 * Listen for renders, keep readiness current with the permission, and settle
 * what the last session left behind. Once per app run; `App.svelte` calls it
 * after the settings have loaded.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 */
export function startCloud(backend = getBackend()) {
  if (started) return
  const run = { backend, unlisten: /** @type {Array<() => void>} */ ([]), stopEffect: () => {}, recovered: false }
  started = run

  try {
    Promise.resolve(backend.onQwenReview?.((preview) => reviewQwen(preview, backend)))
      .then((off) => { if (typeof off === 'function') { if (started === run) run.unlisten.push(off); else off() } })
      .catch(() => {})
    Promise.resolve(backend.onCloudAttempt?.(onAttemptEvent))
      .then((off) => {
        if (typeof off !== 'function') return
        if (started === run) run.unlisten.push(off)
        else off()
      })
      .catch(() => {})
  } catch {
    // A backend without the event: jobs still end from the commands' answers.
  }

  run.stopEffect = $effect.root(() => {
    $effect(() => {
      const allowed = session.cloudAllowed
      untrack(() => {
        void refreshCloudReadiness(backend)
        if (allowed && !run.recovered) {
          run.recovered = true
          void recoverAtStart(run)
        }
      })
    })
  })
}

/** Undo `startCloud`. For tests, and for nothing else. */
export function stopCloud() {
  resetCloudConfiguration()
  modelEpoch += 1
  modelFetches.clear()
  const run = started
  started = null
  if (run) {
    run.stopEffect()
    for (const off of run.unlisten) off()
  }
  for (const timer of graceTimers.values()) clearTimeout(timer)
  graceTimers.clear()
  finished.clear()
  recoverySettled.clear()
  cloud.jobs = []
  cloud.readiness = unknownReadiness()
  cloud.checked = false
  cloud.lastCommitAt = 0
  cloud.recovery = null
  cloud.model = null
  cloud.profileModels = {}
}

/**
 * @param {unknown} entry
 * @returns {entry is {attemptId: string, chapterId: string|null, pageIndex: number|null, regionId: string|null, reason?: string, canAbandon?: boolean}}
 */
function isRecoveredAttempt(entry) {
  return (
    entry !== null &&
    typeof entry === 'object' &&
    typeof (/** @type {any} */ (entry).attemptId) === 'string'
  )
}

/**
 * `reconcileCloudRecovery({apply: true})`, once, with one notice per outcome
 * that needs saying.
 *
 * @param {NonNullable<typeof started>} run
 */
async function recoverAtStart(run) {
  try { await recoverWithBackend(run.backend, () => started === run) }
  catch { if (started === run) notify({ key: 'cloud.recovery.loadError', tone: 'warn' }) }
}

/** @param {import('../api/backend.js').Backend} backend @param {() => boolean} [current] */
async function recoverWithBackend(backend, current = () => true) {
  // A resumed attempt may previously have ended here as unknown or timed out.
  for (const id of finished) if (!recoverySettled.has(id)) finished.delete(id)
  const report = await backend.reconcileCloudRecovery({ apply: true })
  if (!current()) return
  const list = (/** @type {unknown} */ value) => (Array.isArray(value) ? value.filter(isRecoveredAttempt) : [])
  const attached = list(report?.attached)
  const stillRunning = list(report?.stillRunning).filter((entry) => !recoverySettled.has(entry.attemptId))
  const needsAttention = list(report?.needsAttention).filter((entry) => !recoverySettled.has(entry.attemptId))
  cloud.recovery = { attached, stillRunning, needsAttention }
  if (attached.length) {
    notify({ key: 'notice.cloud.recovered', params: { count: attached.length } })
    for (const entry of attached) void reloadRegion(entry, backend)
  }
  if (needsAttention.length) {
    notify({ key: 'notice.cloud.needsAttention', params: { count: needsAttention.length }, tone: 'warn' })
    // The regions these name are flagged natively from the attempt journal
    // once recovery has settled what it could (`review.reason.repairNeeded`
    // and its siblings); read them back so Layers and the page header show
    // the flag without a reopen.
    for (const entry of needsAttention) void reloadRegion(entry, backend)
  }
  for (const entry of stillRunning) {
    trackCloudJob({
      attemptId: entry.attemptId,
      target: entry.provider && entry.profileId ? { provider: entry.provider, profileId: entry.profileId } : null,
      regionId: entry.regionId ?? null,
      chapterId: entry.chapterId ?? null,
      pageIndex: typeof entry.pageIndex === 'number' ? entry.pageIndex : null,
      background: true,
    })
  }
}

/**
 * Read a region a render committed with no command waiting on it back into the
 * open chapter: one recovery attached, or one it resumed in the background.
 * A chapter that is not open has nothing to update; opening it reads the
 * stored page.
 *
 * @param {{chapterId?: string|null, pageIndex?: number|null, regionId?: string|null}} ref
 * @param {import('../api/backend.js').Backend} [backend]
 */
async function reloadRegion(ref, backend = getBackend()) {
  const { chapterId, pageIndex, regionId } = ref
  if (!chapterId || typeof pageIndex !== 'number' || !regionId) return
  if (editor.chapter?.id !== chapterId) return
  /** @type {unknown} */
  let loaded
  try {
    loaded = await backend.loadPages({ chapterId, indices: [pageIndex] })
  } catch {
    return
  }
  if (editor.chapter?.id !== chapterId || !Array.isArray(loaded)) return
  const page = loaded.find((candidate) => candidate?.index === pageIndex)
  const region = page?.regions?.find((/** @type {any} */ candidate) => candidate?.id === regionId)
  if (region) replaceRegion(region, page.status)
}

/**
 * Forget a recovery entry the user has dealt with.
 *
 * @param {string} attemptId
 */
export async function dismissRecovered(attemptId, backend = getBackend()) {
  await backend.reconcileCloudRecovery({ action: 'acknowledge', attemptId })
  const entry = cloud.recovery?.needsAttention.find((candidate) => candidate.attemptId === attemptId)
  forgetRecovered(attemptId)
  // Settling the attempt clears the region's flag natively; read it back.
  if (entry) void reloadRegion(entry, backend)
}

/** @param {string} attemptId */
function forgetRecovered(attemptId) {
  if (!cloud.recovery) return
  cloud.recovery = {
    ...cloud.recovery,
    needsAttention: cloud.recovery.needsAttention.filter((entry) => entry.attemptId !== attemptId),
  }
}

/** Repeat recovery without restarting the app. @param {import('../api/backend.js').Backend} [backend] */
export async function retryCloudRecovery(backend = getBackend()) {
  return recoverWithBackend(backend)
}

/** Explicit confirmation is required, even if a caller bypasses the notice.
 * @param {string} attemptId @param {boolean} acceptDuplicateRisk
 * @param {import('../api/backend.js').Backend} [backend]
 */
export async function abandonRecovered(attemptId, acceptDuplicateRisk, backend = getBackend()) {
  if (acceptDuplicateRisk !== true) throw new Error('duplicate risk confirmation required')
  await backend.reconcileCloudRecovery({ action: 'abandon', attemptId, acceptDuplicateRisk })
  recoverySettled.add(attemptId)
  finished.delete(attemptId)
  finishJob(attemptId, { phase: 'cancelled', errorCode: null, quiet: true })
  const attention = cloud.recovery?.needsAttention.find((candidate) => candidate.attemptId === attemptId)
  forgetRecovered(attemptId)
  if (cloud.recovery) cloud.recovery = { ...cloud.recovery,
    stillRunning: cloud.recovery.stillRunning.filter((entry) => entry.attemptId !== attemptId) }
  // Abandoning clears the region's flag natively whatever the reason was, so
  // any entry that names a region is read back, not only a repair.
  if (attention) void reloadRegion(attention, backend)
}

/* ------------------------------------------------------------------ */
/* Jobs                                                                */
/* ------------------------------------------------------------------ */

/** Attempt ids already ended, so a late event or answer cannot end one twice. */
const finished = new Set()
/** Commits and explicit abandonment cannot be reopened by a late recovery report. */
const recoverySettled = new Set()
/** @type {Map<string, ReturnType<typeof setTimeout>>} */
const graceTimers = new Map()

const ATTEMPT_ID = /^att-[0-9a-f]{24}$/
const ERROR_CODE = /^[a-z][a-z_]{0,47}$/

/**
 * Show a render that is about to start, before its first event.
 *
 * @param {{attemptId: string, regionId?: string|null, chapterId?: string|null, pageIndex?: number|null, phase?: string, background?: boolean, target?: {provider: 'modal'|'beam', profileId: string}|null}} spec
 * @returns {CloudJob|null}
 */
export function trackCloudJob(spec) {
  if (!ATTEMPT_ID.test(spec.attemptId) || recoverySettled.has(spec.attemptId)) return null
  finished.delete(spec.attemptId)
  const existing = cloud.jobs.find((job) => job.attemptId === spec.attemptId)
  if (existing) return existing
  const now = Date.now()
  /** @type {CloudJob} */
  const job = {
    attemptId: spec.attemptId,
    target: Object.hasOwn(spec, 'target') ? spec.target : (cloud.readiness.target ? { provider: cloud.readiness.target.type, profileId: cloud.readiness.target.profile_id } : null),
    configurationKey: configurationKey(cloud.readiness, spec.target),
    regionId: spec.regionId ?? null,
    chapterId: spec.chapterId ?? null,
    pageIndex: spec.pageIndex ?? null,
    phase: spec.phase && RUNNING_PHASES.includes(spec.phase) ? spec.phase : 'preparing',
    startedAt: now,
    cold: now - cloud.lastCommitAt > WARM_MS,
    cancelling: false,
    background: spec.background === true,
  }
  cloud.jobs = [...cloud.jobs, job]
  return job
}

/**
 * One `cloud://attempt` event. Anything not shaped like IC-3 is ignored.
 *
 * @param {unknown} payload
 */
export function onAttemptEvent(payload) {
  if (!payload || typeof payload !== 'object') return
  const event = /** @type {Record<string, unknown>} */ (payload)
  const attemptId = event.attemptId
  const phase = event.phase
  if (typeof attemptId !== 'string' || !ATTEMPT_ID.test(attemptId) || typeof phase !== 'string') return
  const elapsed = typeof event.elapsedMs === 'number' && Number.isFinite(event.elapsedMs) && event.elapsedMs >= 0
    ? event.elapsedMs
    : 0
  const errorCode = typeof event.errorCode === 'string' && ERROR_CODE.test(event.errorCode) ? event.errorCode : null

  if (ENDING_PHASES.includes(phase)) {
    dismissQwenReview(attemptId)
    finishJob(attemptId, { phase: /** @type {CloudOutcome['phase']} */ (phase), errorCode, quiet: errorCode === 'qwen_retry' }, elapsed, {
      regionId: typeof event.regionId === 'string' ? event.regionId : null,
      chapterId: typeof event.chapterId === 'string' ? event.chapterId : null,
      pageIndex: typeof event.pageIndex === 'number' ? event.pageIndex : null,
    })
    return
  }
  if (!RUNNING_PHASES.includes(phase) || finished.has(attemptId)) return
  const job =
    cloud.jobs.find((candidate) => candidate.attemptId === attemptId) ??
    trackCloudJob({
      attemptId,
      regionId: typeof event.regionId === 'string' ? event.regionId : null,
      chapterId: typeof event.chapterId === 'string' ? event.chapterId : null,
      pageIndex: typeof event.pageIndex === 'number' ? event.pageIndex : null,
      target: typeof event.profileId === 'string' && (event.provider === 'modal' || event.provider === 'beam')
        ? { provider: event.provider, profileId: event.profileId } : null,
      // A render this interface did not start: nothing is waiting on its answer.
      background: true,
    })
  if (!job) return
  cloud.jobs = cloud.jobs.map((candidate) =>
    candidate.attemptId === attemptId
      ? { ...candidate, ...(typeof event.profileId === 'string' && (event.provider === 'modal' || event.provider === 'beam')
          ? { target: { provider: event.provider, profileId: event.profileId } } : {}), phase, startedAt: Math.min(candidate.startedAt, Date.now() - elapsed) }
      : candidate,
  )
}

/**
 * A command that started a render has answered. A known ending ends the job
 * now; `null` means the answer did not say how it ended, and the event that
 * does is given a moment to arrive before the job ends as a failure.
 *
 * @param {string} attemptId
 * @param {CloudOutcome|null} outcome
 */
export function settleCloudJob(attemptId, outcome) {
  if (finished.has(attemptId)) return
  if (outcome) {
    finishJob(attemptId, outcome)
    return
  }
  if (graceTimers.has(attemptId)) return
  graceTimers.set(
    attemptId,
    setTimeout(() => {
      graceTimers.delete(attemptId)
      finishJob(attemptId, { phase: 'failed', errorCode: null })
    }, SETTLE_GRACE_MS),
  )
}

/**
 * End a job, once, with the notice its ending calls for.
 *
 * @param {string} attemptId
 * @param {CloudOutcome} outcome
 * @param {number} [elapsedMs] - from the event, when an event ended it
 * @param {{chapterId: string|null, pageIndex: number|null, regionId: string|null}} [where] -
 *   from the event: where a render nothing was tracking committed
 */
function finishJob(attemptId, outcome, elapsedMs, where) {
  if (finished.has(attemptId)) return
  // Another command holds this attempt right now, so it is already running:
  // that holder's own events end it.
  if (outcome.errorCode === 'attempt_busy') return
  finished.add(attemptId)
  const timer = graceTimers.get(attemptId)
  if (timer !== undefined) {
    clearTimeout(timer)
    graceTimers.delete(attemptId)
  }
  const job = cloud.jobs.find((candidate) => candidate.attemptId === attemptId)
  cloud.jobs = cloud.jobs.filter((candidate) => candidate.attemptId !== attemptId)
  if (outcome.quiet) return
  if (outcome.phase === 'failed' && job) {
    void configurationFailed(started?.backend ?? getBackend(), job.configurationKey, 'clean', outcome.errorCode)
  }

  switch (outcome.phase) {
    case 'committed': {
      recoverySettled.add(attemptId)
      forgetRecovered(attemptId)
      if (cloud.recovery) cloud.recovery = { ...cloud.recovery,
        stillRunning: cloud.recovery.stillRunning.filter((entry) => entry.attemptId !== attemptId) }
      cloud.lastCommitAt = Date.now()
      // Every render the interface starts is tracked before it is sent, so one
      // that ends untracked had no command waiting on it either.
      const unattended = job ? (job.background ? job : null) : (where ?? null)
      if (unattended) void reloadRegion(unattended, started?.backend)
      const ms = elapsedMs ?? (job ? Date.now() - job.startedAt : 0)
      notify({ key: 'notice.cloud.finished', params: { seconds: Math.max(1, Math.round(ms / 1000)) } })
      return
    }
    case 'cancelled':
      notify({ key: 'notice.cloud.cancelled' })
      return
    case 'unknown':
      notify({ key: 'notice.cloud.unknown', tone: 'warn' })
      return
    default:
      notify({
        key: 'notice.cloud.failed',
        params: { reasonKey: cloudErrorKey(outcome.errorCode) },
        tone: 'warn',
      })
  }
}

/**
 * Ask a render to stop. The render ends with its own `cancelled` event.
 *
 * @param {string} attemptId
 * @param {import('../api/backend.js').Backend} [backend]
 */
export async function cancelCloudJob(attemptId, backend = getBackend()) {
  const job = cloud.jobs.find((candidate) => candidate.attemptId === attemptId)
  if (!job || job.cancelling) return
  cloud.jobs = cloud.jobs.map((candidate) =>
    candidate.attemptId === attemptId ? { ...candidate, cancelling: true } : candidate,
  )
  try {
    await backend.cancelCloudAttempt({ attemptId })
  } catch {
    cloud.jobs = cloud.jobs.map((candidate) =>
      candidate.attemptId === attemptId ? { ...candidate, cancelling: false } : candidate,
    )
    notify({ key: 'notice.cloud.cancelFailed', tone: 'warn' })
  }
}
