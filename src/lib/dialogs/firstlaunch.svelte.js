/**
 * The setup's *state*, and the download run it drives.
 *
 * Why a module and not the component: the downloads must outlive the screen.
 * Finishing the setup while files are still arriving leaves them arriving, a
 * dialog raised over the setup unmounts it (`App.svelte`), and Settings reads
 * the same `model-progress` channel. Everything that must outlive a mount
 * lives here: the plan, the choices, each file's status, the run, and what the
 * cloud setup did. `FirstLaunchDialog.svelte` and its steps only draw it.
 *
 * The run is `downloadRuntime`, then individual files and atomic logical model
 * groups. Pause is `cancelDownload`: the backend keeps resumable staging data.
 */

import { writeSettingsSerialized } from '../state/settingswrite.js'
import { getBackend } from '../api/backend.js'
import {
  backendSettingsPatch,
  markFirstLaunchOffered,
  session,
  setCloudAllowed,
  setDetection,
  setFluxModel,
  setOcrRescue,
} from '../state/session.svelte.js'
import { setDialogOutsideStack } from '../shortcuts.js'
import { capabilities, loadCapabilities } from '../state/capabilities.svelte.js'
import { cloud } from '../state/cloud.svelte.js'
import { chooseDetectTarget, detectTarget, targets as detectTargets } from '../state/cloudtargets.svelte.js'
import { HAYAI_FILES, LANGUAGES, OCR_FILES, SCRIPT_GATE_FILES, localDetectorModels } from '../model/pipelines.js'
import {
  RUNTIME_ID,
  FIRST_LAUNCH_STEPS,
  defaultFluxModel,
  firstLaunchPlan,
  missingBytes,
  neededFiles,
  runtimeReady,
} from './firstlaunch.js'

/** @typedef {'waiting'|'active'|'paused'|'done'|'failed'} FileStatus */

export const firstLaunch = $state({
  open: false,
  /** @type {string} */
  step: 'welcome',
  /** @type {import('./firstlaunch.js').FirstLaunchPlan|null} */
  plan: null,
  /** Language id → detector id, or null to skip the language. @type {Record<string, string|null>} */
  detection: {},
  /** Cleaner id → wanted. @type {Record<string, boolean>} */
  cleaners: {},
  /** The files the run is fetching, in order. @type {string[]} */
  queue: [],
  /** @type {Record<string, FileStatus>} */
  status: {},
  /** @type {Record<string, {downloaded: number, total: number|null}>} */
  progress: {},
  /** @type {Record<string, string>} */
  errors: {},
  running: false,
  /** The id being fetched. @type {string|null} */
  current: null,
  /** The Hugging Face key being typed. Never stored here once it is saved. */
  tokenDraft: '',
  /** Whether the last save of that key was refused. */
  tokenFailed: false,
  /** Whether the cloud step is showing the provisioner rather than its two lines. */
  provisioning: false,
  /**
   * Whether the provisioner is creating or removing things in the user's
   * account. The setup offers no way out then but the provisioner's own: the
   * helper would carry on with nothing left to read its answer.
   */
  provisionerBusy: false,
  /**
   * The cloud GPU a setup in this offer finished, as much of it as the step
   * says back, and whether it answered its first check. Never the endpoint or
   * a credential: the provisioner keeps those.
   *
   * @type {{provider: 'modal'|'beam', name: string|null, healthy: boolean}|null}
   */
  cloud: null,
  /** Whether that setup finished and the permission it turns on could not be saved. */
  cloudSaveFailed: false,
  /** Whether the last Detect on choice could not be saved, and was put back. */
  detectSaveFailed: false,
  /** The runtime's answer to `listAccelerators`, once it can be asked. @type {import('../api/backend.js').Accelerators|null} */
  accelerators: null,
  /** Whether that question was asked and refused, which `null` alone cannot say. */
  acceleratorsFailed: false,
  /** What the AI redraw helper lists, when there is one. @type {Array<{id: string, label: string}>} */
  sidecarModels: [],
})

/** @type {(() => void) | null} */
let unsubscribe = null
/** @type {Map<string, (error: string|null) => void>} */
const waiting = new Map()
/** Ids the user paused while their start was still in flight. */
const pauseRequested = new Set()
/** The native model groups, with the member lists `model/pipelines.js` owns. */
const MODEL_GROUPS = Object.freeze({
  scriptGate: SCRIPT_GATE_FILES,
  mangaOcr: OCR_FILES,
  hayaiOcr: HAYAI_FILES,
})
const GROUP_FOR_MODEL = Object.freeze(Object.fromEntries(
  Object.entries(MODEL_GROUPS).flatMap(([groupId, members]) => members.map((member) => [member, groupId])),
))

/**
 * Open the setup over a catalogue answer.
 *
 * Idempotent while open. `force` is Settings' "Run setup again": without it
 * an offer already made is not made twice. A replay while an earlier run is
 * still fetching keeps that run's queue and statuses.
 *
 * @param {import('../api/backend.js').ModelsView} view
 * @param {{force?: boolean}} [options]
 * @returns {boolean} whether it is now open
 */
export function offerFirstLaunch(view, { force = false } = {}) {
  if (firstLaunch.open) return true
  if (!force && session.firstLaunchOffered) return false
  firstLaunch.plan = firstLaunchPlan(view)
  firstLaunch.plan.files.samTs = {
    id: 'samTs', labelKey: 'settings.detection.model.samTs', bytes: 0, installed: false,
  }
  Promise.resolve(getBackend().listWorkflowCapabilities?.()).then((caps) => {
    if (firstLaunch.plan) firstLaunch.plan.files.samTs.installed = caps?.samInstalled === true
  }).catch(() => {})
  firstLaunch.step = FIRST_LAUNCH_STEPS[0]
  firstLaunch.tokenDraft = ''
  firstLaunch.tokenFailed = false
  firstLaunch.provisioning = false
  firstLaunch.provisionerBusy = false
  firstLaunch.cloud = null
  firstLaunch.cloudSaveFailed = false
  firstLaunch.detectSaveFailed = false
  firstLaunch.accelerators = null
  firstLaunch.acceleratorsFailed = false
  firstLaunch.sidecarModels = []
  // Detection starts from what is stored. The cleaners keep an earlier
  // offer's answer while its run is still fetching, so a replay does not tick
  // back a model that run was told to leave out.
  firstLaunch.detection = Object.fromEntries(
    LANGUAGES.map((language) => [language.id, session.detection?.[language.id] ?? null]),
  )
  if (!firstLaunch.running) {
    firstLaunch.cleaners = { 'lama-manga': true }
    firstLaunch.queue = []
    firstLaunch.status = {}
    firstLaunch.progress = {}
    firstLaunch.errors = {}
  }
  firstLaunch.open = true
  // The setup is not on the modal stack, so the shortcut table cannot see it.
  // Telling it is what stops `,` opening Settings underneath.
  setDialogOutsideStack(true)
  listen()
  return true
}

/** @param {string} step - one of `FIRST_LAUNCH_STEPS`; anything else is ignored */
export function setFirstLaunchStep(step) {
  if (FIRST_LAUNCH_STEPS.includes(/** @type {any} */ (step))) firstLaunch.step = step
}

export function nextFirstLaunchStep() {
  const index = FIRST_LAUNCH_STEPS.indexOf(/** @type {any} */ (firstLaunch.step))
  if (index >= 0 && index < FIRST_LAUNCH_STEPS.length - 1) firstLaunch.step = FIRST_LAUNCH_STEPS[index + 1]
}

export function prevFirstLaunchStep() {
  const index = FIRST_LAUNCH_STEPS.indexOf(/** @type {any} */ (firstLaunch.step))
  if (index > 0) firstLaunch.step = FIRST_LAUNCH_STEPS[index - 1]
}

/**
 * Close the setup and remember it was offered. Every way out comes here. A
 * run in flight keeps going: the transfers belong to the backend, and
 * Settings watches the same events.
 */
export function dismissFirstLaunch() {
  markFirstLaunchOffered()
  firstLaunch.open = false
  firstLaunch.tokenDraft = ''
  firstLaunch.tokenFailed = false
  firstLaunch.provisioning = false
  firstLaunch.provisionerBusy = false
  setDialogOutsideStack(false)
  if (!firstLaunch.running) stopListening()
}

/**
 * Choose a language's detector, or `null` to skip it. Stored at once, as
 * Settings stores it: a setup whose files are all here never reaches a
 * Download press, and the choice must not wait for one.
 *
 * @param {string} language
 * @param {string|null} detectorId
 */
export function chooseDetector(language, detectorId) {
  firstLaunch.detection = { ...firstLaunch.detection, [language]: detectorId }
  setDetection(language, detectorId)
}

/** @param {string} id @param {boolean} wanted */
export function chooseCleaner(id, wanted) {
  firstLaunch.cleaners = { ...firstLaunch.cleaners, [id]: wanted }
}

/**
 * Opt in to, or out of, the optional Japanese OCR rescue. Stored at once, for
 * `chooseDetector`'s reason: it is the same switch Settings > Models shows,
 * and the run reads it from the session.
 *
 * @param {boolean} enabled
 */
export function chooseOcrRescue(enabled) {
  setOcrRescue(enabled === true)
}

/**
 * Whether setup can offer *Detect on: Cloud GPU*: cloud engines are on (the
 * Cloud step's setup turns them on), a cloud endpoint is already set up, or
 * this offer's Cloud step saved one. A stored cloud choice is always shown as
 * it is, so a replay never shows a place it is not.
 *
 * @returns {boolean}
 */
export function cloudDetectOffered() {
  return session.cloudAllowed || cloud.readiness?.configured === true || firstLaunch.cloud !== null ||
    detectTarget() === 'cloud'
}

/**
 * Where Text cleanup detects, chosen in setup. Written through the one owner
 * the editor's Detect on uses (`state/cloudtargets.svelte.js`), straight
 * away: the native side stores a cloud choice whether or not an endpoint is
 * ready, and a run with it waits, saying so, until one is
 * (`tools.target.stranded`). So there is no separate intent to hold and
 * apply later. A refused write is put back and said on the step.
 *
 * @param {'local'|'cloud'} target
 * @returns {Promise<boolean>} whether it was saved
 */
export async function chooseFirstLaunchDetectTarget(target) {
  if (target === 'cloud' && !cloudDetectOffered()) return false
  firstLaunch.detectSaveFailed = false
  const saved = await chooseDetectTarget(target)
  if (!saved && detectTargets.detectSaveFailed) firstLaunch.detectSaveFailed = true
  return saved
}

/**
 * The workflow the downloads follow: the stored text policy, the detection
 * models and the rescue switch, and where Text cleanup detects. Setup never
 * changes the policy or Detect on; legacy and this computer are the defaults
 * for a new machine, and a replay keeps whatever was chosen since. With
 * detection on the cloud GPU the run's own combination decides
 * (`pipelines.js#runDetection`): CTD and the text reader are fetched here, the
 * cloud stages never.
 */
function workflow() {
  return {
    textPolicy: session.textPolicy,
    ocrRescue: session.ocrRescue,
    detectorModels: session.detectorModels,
    analysisTargets: session.analysisTargets,
  }
}

/** The files the current choices need, runtime first. @returns {string[]} */
export function chosenFiles() {
  const plan = firstLaunch.plan
  if (!plan) return []
  const files = neededFiles(plan, firstLaunch.detection, firstLaunch.cleaners, workflow())
  if (localDetectorModels(session).includes('samTs') &&
      (session.textPolicy === 'all_text' || LANGUAGES.some((language) => firstLaunch.detection[language.id]))) {
    if (plan.files[RUNTIME_ID] && !files.includes(RUNTIME_ID)) files.unshift(RUNTIME_ID)
    files.push('samTs')
  }
  // Local page denoise is an optional package: queued only when the denoise
  // step chose this computer. Cloud and off download nothing.
  const denoise = session.denoiseTarget === 'local' ? (plan.denoise ?? []).filter((id) => plan.files[id]) : []
  if (denoise.length) {
    if (plan.files[RUNTIME_ID] && !files.includes(RUNTIME_ID)) files.unshift(RUNTIME_ID)
    for (const id of denoise) if (!files.includes(id)) files.push(id)
  }
  return files
}

/** Bytes the current choices still cost. */
export function chosenBytes() {
  const plan = firstLaunch.plan
  return plan ? missingBytes(plan, chosenFiles(), doneMap()) : 0
}

/**
 * Commit the choices and start fetching.
 *
 * The queue is rebuilt from the choices. What already arrived stays done, so
 * going back a step and forward again never refetches a file; what was paused
 * or failed goes again, because this press is a start.
 */
export function startFirstLaunchDownloads() {
  const plan = firstLaunch.plan
  if (!plan) return
  markFirstLaunchOffered()
  for (const language of LANGUAGES) setDetection(language.id, firstLaunch.detection[language.id] ?? null)
  const ids = chosenFiles()
  /** @type {Record<string, FileStatus>} */
  const status = {}
  for (const id of ids) {
    const before = firstLaunch.status[id]
    if (plan.files[id]?.installed || before === 'done') status[id] = 'done'
    // A file the user paused stays paused until they resume it; a failed one
    // goes again, because this press is a start.
    else if (before === 'active' || before === 'paused') status[id] = before
    else status[id] = 'waiting'
  }
  firstLaunch.errors = {}
  // A file dropped from the choices while it was downloading is paused, not
  // abandoned: its `.part` stays for the next time it is wanted.
  const dropped = firstLaunch.current && !ids.includes(firstLaunch.current) ? firstLaunch.current : null
  if (dropped) status[dropped] = 'active'
  firstLaunch.queue = ids
  firstLaunch.status = status
  if (dropped) pauseFile(dropped)
  run()
}

/** @param {string} id */
export async function pauseFile(id) {
  const groupId = GROUP_FOR_MODEL[id]
  if (groupId) {
    const members = MODEL_GROUPS[groupId].filter((member) => firstLaunch.queue.includes(member))
    const active = members.filter((member) => firstLaunch.status[member] === 'active')
    const waiting = members.filter((member) => firstLaunch.status[member] === 'waiting')
    if (!active.length && !waiting.length) return
    for (const member of [...active, ...waiting]) {
      if (firstLaunch.status[member] === 'active') pauseRequested.add(member)
      setStatus(member, 'paused')
    }
    if (active.length) await getBackend().cancelDownload({ id: active[0] })
    return
  }
  const status = firstLaunch.status[id]
  if (status === 'waiting') {
    setStatus(id, 'paused')
    return
  }
  if (status !== 'active') return
  if (id === 'samTs') return
  pauseRequested.add(id)
  setStatus(id, 'paused')
  await getBackend().cancelDownload({ id })
}

/** @param {string} id */
export function resumeFile(id) {
  const groupId = GROUP_FOR_MODEL[id]
  if (groupId) {
    let resumed = false
    for (const member of MODEL_GROUPS[groupId]) {
      const status = firstLaunch.status[member]
      if (status === 'paused' || status === 'failed') {
        setStatus(member, 'waiting')
        resumed = true
      }
      const { [member]: _gone, ...errors } = firstLaunch.errors
      firstLaunch.errors = errors
    }
    if (resumed) {
      for (const member of MODEL_GROUPS[groupId]) pauseRequested.delete(member)
      run()
    }
    return
  }
  const status = firstLaunch.status[id]
  if (status !== 'paused' && status !== 'failed') return
  pauseRequested.delete(id)
  const { [id]: _gone, ...errors } = firstLaunch.errors
  firstLaunch.errors = errors
  setStatus(id, 'waiting')
  run()
}

export async function pauseAll() {
  for (const id of firstLaunch.queue) if (firstLaunch.status[id] === 'waiting') setStatus(id, 'paused')
  if (firstLaunch.current) await pauseFile(firstLaunch.current)
}

export function resumeAll() {
  for (const id of firstLaunch.queue) {
    const status = firstLaunch.status[id]
    if (status === 'paused' || status === 'failed') setStatus(id, 'waiting')
  }
  firstLaunch.errors = {}
  pauseRequested.clear()
  run()
}

/**
 * The run: one file at a time, in queue order, until nothing is waiting.
 *
 * Three orderings are load-bearing:
 * 1. **The waiter is registered before the download is asked for.** A `done`
 *    event can beat the `invoke` reply - a cached file verifies in
 *    microseconds.
 * 2. **`alreadyRunning` waits.** Another window is fetching this very file,
 *    and that transfer ends with the same single `done` event.
 * 3. **A pause pressed while the start is in flight is honoured after it.**
 *    `cancelDownload` for an id the backend has not begun is lost.
 *
 * A failure marks its file and moves on: each row says what went wrong and
 * offers its own retry.
 */
async function run() {
  if (firstLaunch.running) return
  firstLaunch.running = true
  listen()
  const backend = getBackend()
  try {
    for (let id = nextWaiting(); id; id = nextWaiting()) {
      firstLaunch.current = id
      const groupId = GROUP_FOR_MODEL[id]
      const groupMembers = groupId
        ? MODEL_GROUPS[groupId].filter((member) => firstLaunch.queue.includes(member) && firstLaunch.status[member] !== 'done')
        : null
      if (groupMembers) {
        for (const member of groupMembers) setStatus(member, 'active')
      } else setStatus(id, 'active')
      try {
        if (groupId && groupMembers) await fetchGroup(backend, groupId, groupMembers)
        else if (id === 'samTs') {
          try {
            await backend.installSamTs()
            setStatus(id, 'done')
          } catch (error) {
            fail(id, String(error))
          }
        } else await fetchOne(backend, id)
      } finally {
        for (const member of groupMembers ?? [id]) pauseRequested.delete(member)
      }
      // The channel was closed under the run (`resetFirstLaunch`): no event
      // can reach the next file's waiter, so there is nothing to wait for.
      if (!unsubscribe) break
    }
  } finally {
    firstLaunch.current = null
    firstLaunch.running = false
    // A download changes which engines the editor may offer.
    await loadCapabilities()
    // Re-read after the await: a resume in that gap started a new run, and
    // the channel is its channel now.
    if (!firstLaunch.open && !firstLaunch.running) stopListening()
  }
}

/**
 * One file, start to `done` event.
 *
 * @param {import('../api/backend.js').Backend} backend
 * @param {string} id
 */
async function fetchOne(backend, id) {
  const arrival = waitForDone(id)
  /** @type {import('../api/backend.js').DownloadStart} */
  let outcome
  try {
    outcome = id === RUNTIME_ID ? await backend.downloadRuntime() : await backend.downloadModel({ id })
  } catch (error) {
    discard(id)
    fail(id, String(error))
    return
  }
  if (outcome === 'alreadyInstalled') {
    discard(id)
    setStatus(id, 'done')
    return
  }
  if (pauseRequested.has(id)) await backend.cancelDownload({ id })
  const error = await arrival
  if (error === 'cancelled') {
    if (firstLaunch.status[id] === 'active') setStatus(id, 'paused')
    return
  }
  if (error) fail(id, error)
  else setStatus(id, 'done')
}

/**
 * Install a logical capability as one native transaction. Member events carry
 * per-file progress, while only the group event answers this waiter; a group
 * failure or cancellation therefore cannot leave a second queued member hung.
 *
 * @param {import('../api/backend.js').Backend} backend
 * @param {string} groupId
 * @param {string[]} members
 */
async function fetchGroup(backend, groupId, members) {
  const arrival = waitForDone(groupId)
  /** @type {import('../api/backend.js').DownloadStart} */
  let outcome
  try {
    outcome = await backend.downloadModelGroup({ id: groupId })
  } catch (error) {
    discard(groupId)
    for (const member of members) fail(member, String(error))
    return
  }
  if (outcome === 'alreadyInstalled') {
    discard(groupId)
    for (const member of members) setStatus(member, 'done')
    return
  }
  const activeMember = members[0]
  if (members.some((member) => pauseRequested.has(member))) await backend.cancelDownload({ id: activeMember })
  const error = await arrival
  if (error === 'cancelled' && members.some((member) => pauseRequested.has(member))) {
    for (const member of members) if (firstLaunch.status[member] === 'active') setStatus(member, 'paused')
    return
  }
  if (error) {
    for (const member of members) fail(member, error)
    return
  }
  for (const member of members) setStatus(member, 'done')
}

/** @returns {string|undefined} */
function nextWaiting() {
  return firstLaunch.queue.find((id) => firstLaunch.status[id] === 'waiting')
}

/** @param {string} id @param {FileStatus} status */
function setStatus(id, status) {
  firstLaunch.status = { ...firstLaunch.status, [id]: status }
}

/** @param {string} id @param {string} message */
function fail(id, message) {
  setStatus(id, 'failed')
  firstLaunch.errors = { ...firstLaunch.errors, [id]: message }
}

/** The ids that are on disk now, from the plan or from this run. @returns {Record<string, boolean>} */
export function finishedFiles() {
  return doneMap()
}

/** @returns {Record<string, boolean>} */
function doneMap() {
  return Object.fromEntries(
    Object.entries(firstLaunch.status).filter(([, status]) => status === 'done').map(([id]) => [id, true]),
  )
}

/* ------------------------------------------------------------------ */
/* The settings the steps change                                       */
/* ------------------------------------------------------------------ */

/**
 * Change one setting the way Settings does: the session's own setter, then
 * the backend half of the whole session, then the capabilities that read it.
 *
 * Unlike Settings, a refused write puts the session back. The step shows the
 * value it holds as the choice that was made, and a choice the backend did
 * not keep would be shown as kept.
 *
 * @template T
 * @param {(value: T) => void} apply - a session setter
 * @param {T} next
 * @param {T} previous - what `apply` restores when the write is refused
 * @returns {Promise<boolean>} whether the backend kept it
 */
export async function saveFirstLaunchSetting(apply, next, previous) {
  apply(next)
  try {
    await writeSettingsSerialized(getBackend(), () => backendSettingsPatch())
  } catch {
    apply(previous)
    return false
  }
  await loadCapabilities()
  return true
}

/**
 * Save the typed Hugging Face key. It goes to the backend alone - the
 * keychain, or the settings file where there is none - and never into the
 * session, which is written to local storage.
 *
 * @returns {Promise<boolean>} whether it was kept
 */
export async function saveFirstLaunchToken() {
  const value = firstLaunch.tokenDraft.trim()
  if (!value) return false
  firstLaunch.tokenFailed = false
  try {
    await writeSettingsSerialized(getBackend(), { hfToken: value })
  } catch {
    firstLaunch.tokenFailed = true
    return false
  }
  firstLaunch.tokenDraft = ''
  if (firstLaunch.plan) firstLaunch.plan.hasToken = true
  return true
}

/**
 * Ask the runtime which processors it can run on, when it can be asked
 * (`runtimeReady`). Until then the dependencies step says when it will know.
 */
export async function loadFirstLaunchAccelerators() {
  if (!runtimeReady(firstLaunch.plan, doneMap(), firstLaunch.current)) return
  try {
    firstLaunch.accelerators = await getBackend().listAccelerators()
    firstLaunch.acceleratorsFailed = false
  } catch {
    firstLaunch.accelerators = null
    firstLaunch.acceleratorsFailed = true
  }
}

/**
 * What the AI redraw helper can run, and the first choice when none was made.
 *
 * The first choice is Settings' (`defaultFluxModel`), and it is written
 * through rather than held in the session alone: the backend falls back to
 * its own default when the setting is empty, and that default need not be a
 * model this helper has.
 */
export async function loadFirstLaunchSidecarModels() {
  if (!capabilities.sidecar) {
    firstLaunch.sidecarModels = []
    return
  }
  try {
    const models = await getBackend().listSidecarModels()
    firstLaunch.sidecarModels = Array.isArray(models) ? models : []
  } catch {
    firstLaunch.sidecarModels = []
    return
  }
  const chosen = defaultFluxModel(firstLaunch.sidecarModels)
  if (!session.fluxModel && chosen) await saveFirstLaunchSetting(setFluxModel, chosen, '')
}

/** Show the provisioner in place of the cloud step's two lines. */
export function openFirstLaunchProvisioner() {
  firstLaunch.provisioning = true
}

/**
 * Back to the two lines, which say what the setup did if it finished. The
 * provisioner goes with them, so nothing is left to be busy.
 */
export function closeFirstLaunchProvisioner() {
  firstLaunch.provisioning = false
  firstLaunch.provisionerBusy = false
}

/**
 * The provisioner's `onbusychange`: true while it creates or removes things
 * in the user's account, false once that has ended however it ended.
 *
 * @param {boolean} busy
 */
export function setFirstLaunchProvisionerBusy(busy) {
  firstLaunch.provisionerBusy = busy === true
}

/**
 * The provisioner's `onconfigured`.
 *
 * The provisioner saves the endpoint and selects it itself (IC-5), so what is
 * left here is the cloud permission, which "Set up now" promises to turn on.
 * It never throws: the setup did succeed, and a provisioner told otherwise
 * would offer to run it again. A refused write is said on the step instead.
 *
 * Only for an endpoint that answered its first check. One that did not is
 * kept and said back, so the step does not offer the setup again, but the
 * permission stays as it was. An answer that names no saved profile has left
 * nothing to run on, so the permission stays as it was then too.
 *
 * @param {{provider?: string, profileId?: string, name?: string, healthy?: boolean}|null|undefined} info
 */
export async function configureFirstLaunchCloud(info) {
  if (typeof info?.profileId !== 'string' || !info.profileId.trim()) return
  const provider = info?.provider === 'beam' ? 'beam' : 'modal'
  const name = typeof info?.name === 'string' && info.name.trim() ? info.name.trim() : null
  const healthy = info?.healthy === true
  firstLaunch.cloud = { provider, name, healthy }
  firstLaunch.cloudSaveFailed = false
  if (!healthy) return
  const previous = session.cloudAllowed
  firstLaunch.cloudSaveFailed = !(await saveFirstLaunchSetting(setCloudAllowed, true, previous))
}

/* ------------------------------------------------------------------ */
/* The event channel                                                   */
/* ------------------------------------------------------------------ */

function listen() {
  if (unsubscribe) return
  unsubscribe = getBackend().subscribe((event) => {
    if (event.type !== 'model-progress') return
    if (!event.done) {
      firstLaunch.progress = {
        ...firstLaunch.progress,
        [event.id]: { downloaded: event.downloaded, total: event.total },
      }
      return
    }
    // The native group summary deliberately reuses its group id (`scriptGate`
    // is also one member's file id), but total=null distinguishes that summary
    // from the successful per-file event. Never let the member event wake the
    // logical-group waiter early.
    const groupSummary = GROUP_FOR_MODEL[event.id] === event.id && event.total === null
    // Progress is kept on a pause, so the bar holds where it stopped. A
    // success is marked here rather than after the run's await, so the bar
    // never drops to empty in between - and so a file Settings finished for
    // us is not fetched again when the queue reaches it.
    if (!event.error && !groupSummary) {
      // Marked whether or not it is queued yet: a file Settings finished
      // while the setup was on an earlier step must not be fetched again.
      setStatus(event.id, 'done')
      const { [event.id]: _gone, ...rest } = firstLaunch.progress
      firstLaunch.progress = rest
    }
    if (!GROUP_FOR_MODEL[event.id] || groupSummary) {
      waiting.get(event.id)?.(event.error ?? null)
      waiting.delete(event.id)
    }
  })
}

function stopListening() {
  // A run parked on a `done` event that will now never come would hold its
  // promise for the life of the page.
  for (const resolve of waiting.values()) resolve('cancelled')
  waiting.clear()
  unsubscribe?.()
  unsubscribe = null
}

/** @param {string} id */
function waitForDone(id) {
  return /** @type {Promise<string|null>} */ (
    new Promise((resolve) => {
      waiting.set(id, resolve)
    })
  )
}

/** A waiter for a download that will never report. @param {string} id */
function discard(id) {
  waiting.get(id)?.(null)
  waiting.delete(id)
}

/** Put the module back as it was. **Tests only.** */
export function resetFirstLaunch() {
  stopListening()
  pauseRequested.clear()
  firstLaunch.open = false
  firstLaunch.step = 'welcome'
  firstLaunch.plan = null
  firstLaunch.detection = {}
  firstLaunch.cleaners = {}
  firstLaunch.queue = []
  firstLaunch.status = {}
  firstLaunch.progress = {}
  firstLaunch.errors = {}
  firstLaunch.running = false
  firstLaunch.current = null
  firstLaunch.tokenDraft = ''
  firstLaunch.tokenFailed = false
  firstLaunch.provisioning = false
  firstLaunch.provisionerBusy = false
  firstLaunch.cloud = null
  firstLaunch.cloudSaveFailed = false
  firstLaunch.detectSaveFailed = false
  firstLaunch.accelerators = null
  firstLaunch.acceleratorsFailed = false
  firstLaunch.sidecarModels = []
  setDialogOutsideStack(false)
}
