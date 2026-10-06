/**
 * Background jobs: the runs and denoises the backend is working through, and
 * the ones that finished lately.
 *
 * Why a store and not the screen that started one. A Detect, a Clean, a cloud
 * Clean, a local Denoise and a cloud Denoise all belong to the backend once
 * started, and the user is free to close the dialog or leave the editor and
 * start another on a different chapter. Whatever shows their progress has to
 * outlive every screen, so it lives here, with **one root subscription**
 * started from `App.svelte` (`startJobs`): the run stream (`page-done`,
 * `run-finished`) and `denoise://progress`.
 *
 * How a job gets here, in the order it can happen:
 *
 * 1. **Its starter registers it** the moment it has a run id (`registerJob`,
 *    `trackDenoise`), so the indicator shows it before the first event. A
 *    `run-finished` that overtakes that registration is held (`earlyEnds`) and
 *    applied when the job arrives.
 * 2. **`listJobs`** answers what is running now: at start, after a window
 *    reload, and whenever an event names a run this store has never heard of
 *    (a resumed job, a run started before the reload).
 *
 * How one ends: a run by its `run-finished` reason, a denoise by its starter's
 * promise, since the report of pages written and failed is the command's
 * answer rather than an event. A denoise that ends while no dialog is watching
 * it says so in a notice, as the dialog did before it could be closed.
 *
 * Finished jobs stay until dismissed, the newest `MAX_FINISHED` of them.
 *
 * The quit guard's question is asked from here too (`askQuit`): it is about
 * these jobs, and it can arrive on any screen, over any dialog.
 *
 * Nothing here forwards `notice` events. Home and the editor each forward
 * those while mounted, and a third subscriber doing it too would say
 * everything twice.
 */

import { trackCloudRun, finishCloudRun } from './cloudgpu.svelte.js'
import { getBackend } from '../api/backend.js'
import { cloud } from './cloud.svelte.js'
import { configurationKey, configurationFailed } from './cloudconfig.svelte.js'
import { notify, openChapter } from './app.svelte.js'

/** Finished jobs kept for the list, newest first. */
export const MAX_FINISHED = 10

/** Kinds that are Detect/Clean runs: stopped with `cancelRun`, ended by `run-finished`. */
export const RUN_KINDS = Object.freeze(['detect', 'clean', 'cloudClean'])
/** Kinds that are denoises: stopped with `cancelDenoiseLocal`, ended by their command's answer. */
export const DENOISE_KINDS = Object.freeze(['denoise', 'cloudDenoise'])
const KINDS = Object.freeze([...RUN_KINDS, ...DENOISE_KINDS])

/** How long `listJobs` waits after an unknown run id, so a burst of events asks once. */
const HYDRATE_DEBOUNCE_MS = 250
/** Ends of runs nobody has registered yet, kept for the moment between a handle and its registration. */
const MAX_EARLY_ENDS = 16

/**
 * @typedef {'detect'|'clean'|'cloudClean'|'denoise'|'cloudDenoise'} JobKind
 * @typedef {'running'|'completed'|'cancelled'|'failed'} JobStatus
 *
 * The contract names a `title`; it is carried as its parts, because a title
 * is words and the words are the catalogue's (`jobs.item.title`).
 *
 * @typedef {Object} Job
 * @property {string} runId
 * @property {JobKind} kind
 * @property {string} chapterId
 * @property {string|null} projectId
 * @property {string|null} projectName
 * @property {string|null} chapterName
 * @property {number|string|null} chapterNumber
 * @property {number} done - pages finished
 * @property {number} total - pages in the job, 0 while unknown
 * @property {number} page - how far through the page in progress, 0 to 1 (denoise only)
 * @property {JobStatus} status
 * @property {boolean} stopping - Stop was pressed and the job has not ended yet
 * @property {number} startedAt
 * @property {number|null} finishedAt
 * @property {import('../api/backend.js').DenoiseReport|null} report - a denoise's answer
 * @property {string|null} errorCode - why a denoise failed
 */

export const jobs = $state({
  /** Running and recently finished, newest first. @type {Job[]} */
  list: [],
  /** The quit guard's question while it is up. @type {{count: number, canHide: boolean}|null} */
  quit: null,
})

/* Not `$state`: bookkeeping nothing renders. */
/** @type {Map<string, import('../api/backend.js').RunFinishedEvent>} */
const earlyEnds = new Map()
/** Run ids a dialog is showing, with how many are: its ending is not a notice then. @type {Map<string, number>} */
const watchers = new Map()
/** @type {Set<(job: Job) => void>} */
const settleListeners = new Set()
/** @type {import('../api/backend.js').Backend|null} */
let started = null
/** @type {Array<() => void>} */
let detach = []
/** @type {ReturnType<typeof setTimeout>|null} */
let hydrateTimer = null
/** @type {Promise<void>|null} */
let naming = null

/* ------------------------------------------------------------------ */
/* Reads                                                               */
/* ------------------------------------------------------------------ */

/** @param {string|null|undefined} runId @returns {Job|null} */
export function jobById(runId) {
  if (!runId) return null
  return jobs.list.find((job) => job.runId === runId) ?? null
}

/** @returns {Job[]} */
export function runningJobs() {
  return jobs.list.filter((job) => job.status === 'running')
}

/** @returns {number} */
export function runningCount() {
  return jobs.list.reduce((count, job) => count + (job.status === 'running' ? 1 : 0), 0)
}

/** @returns {boolean} whether there is anything to show */
export function hasJobs() {
  return jobs.list.length > 0
}

/**
 * The job running on a chapter, of one of `kinds`.
 *
 * @param {string|null|undefined} chapterId
 * @param {readonly string[]} [kinds]
 * @returns {Job|null}
 */
export function runningJobFor(chapterId, kinds = KINDS) {
  if (!chapterId) return null
  return jobs.list.find((job) => job.status === 'running' && job.chapterId === chapterId && kinds.includes(job.kind)) ?? null
}

/**
 * How far a job is, 0 to 1. The page in progress counts for a denoise, whose
 * progress says how far through it the run is.
 *
 * @param {Job} job
 */
export function jobFraction(job) {
  if (!job.total) return job.status === 'completed' ? 1 : 0
  const partial = job.status === 'running' ? Math.min(1, Math.max(0, Number(job.page) || 0)) : 0
  return Math.min(1, (Math.min(job.done, job.total) + partial) / job.total)
}

/* ------------------------------------------------------------------ */
/* Registration                                                        */
/* ------------------------------------------------------------------ */

/**
 * Add a job its starter has just been given, or fill in what an earlier
 * sighting of it did not know. Answers the store's copy.
 *
 * @param {{runId: string, kind: JobKind, chapterId: string, projectId?: string|null, projectName?: string|null,
 *   chapterName?: string|null, chapterNumber?: number|string|null, done?: number, total?: number, target?: {provider: 'modal'|'beam', profileId: string}|null}} spec
 * @returns {Job|null}
 */
export function registerJob(spec) {
  if (!spec?.runId || !KINDS.includes(spec.kind) || !spec.chapterId) return null
  // A job is only followed while the store listens. `App.svelte` starts it at
  // boot; this covers a starter that runs before that, or a test that mounts
  // one screen alone.
  const backend = getBackend()
  if (started !== backend) startJobs(backend)
  const known = jobById(spec.runId)
  if (known) {
    for (const field of /** @type {const} */ (['projectId', 'projectName', 'chapterName', 'chapterNumber'])) {
      if (known[field] == null && spec[field] != null) known[field] = /** @type {any} */ (spec[field])
    }
    if (!known.total && Number(spec.total) > 0) known.total = Number(spec.total)
    return known
  }
  jobs.list.unshift({
    runId: spec.runId,
    kind: spec.kind,
    chapterId: spec.chapterId,
    projectId: spec.projectId ?? null,
    projectName: spec.projectName ?? null,
    chapterName: spec.chapterName ?? null,
    chapterNumber: spec.chapterNumber ?? null,
    done: Math.max(0, Number(spec.done) || 0),
    total: Math.max(0, Number(spec.total) || 0),
    page: 0,
    status: 'running',
    stopping: false,
    startedAt: Date.now(),
    finishedAt: null,
    report: null,
    errorCode: null,
  })
  const job = /** @type {Job} */ (jobById(spec.runId))
  const early = earlyEnds.get(spec.runId)
  if (early) {
    earlyEnds.delete(spec.runId)
    settleRun(job, early)
  }
  if (job.projectName == null || job.chapterName == null) void nameJobs()
  return job
}

/**
 * Register a denoise and follow it to its end. `start` sends the command;
 * its answer is the report. Answers the job once it has ended, and never
 * rejects: a failure is the job's `failed` status and its `errorCode`.
 *
 * @param {Parameters<typeof registerJob>[0] & {kind: 'denoise'|'cloudDenoise'}} spec
 * @param {() => Promise<import('../api/backend.js').DenoiseReport>} start
 * @returns {Promise<Job|null>}
 */
export async function trackDenoise(spec, start) {
  const job = registerJob(spec)
  if (!job) return null
  if (spec.kind === 'cloudDenoise') trackCloudRun(spec.runId, 'analysis', getBackend(), spec.target)
  const backend = getBackend()
  const configKey = spec.kind === 'cloudDenoise' ? configurationKey(cloud.readiness, spec.target) : null
  try {
    const answer = await start()
    const report = {
      written: Array.isArray(answer?.written) ? answer.written : [],
      failed: Array.isArray(answer?.failed) ? answer.failed : [],
      cancelled: answer?.cancelled === true,
    }
    if (configKey) for (const failure of report.failed) await configurationFailed(backend, configKey, 'denoise', failure.code)
    const held = jobById(spec.runId)
    if (!held) return null
    held.report = report
    if (!report.cancelled && held.total) held.done = held.total
    finish(held, report.cancelled ? 'cancelled' : 'completed')
    if (!isWatched(held.runId)) {
      notify({
        key: 'denoise.notice.finished',
        params: { count: report.written.length, failed: report.failed.length, number: held.chapterNumber ?? '' },
        tone: report.failed.length ? 'warn' : 'info',
      })
    }
    return held
  } catch (error) {
    if (configKey) await configurationFailed(backend, configKey, 'denoise', error)
    const held = jobById(spec.runId)
    if (!held) return null
    held.errorCode = codeOf(error)
    finish(held, 'failed')
    if (!isWatched(held.runId)) {
      notify({ key: 'denoise.notice.stopped', params: { number: held.chapterNumber ?? '' }, tone: 'warn' })
    }
    return held
  } finally {
    if (spec.kind === 'cloudDenoise') finishCloudRun(spec.runId, getBackend())
  }
}

/**
 * A dialog showing a denoise says the ending itself; while it is mounted the
 * store says nothing. Answers the unwatch.
 *
 * @param {string} runId
 * @returns {() => void}
 */
export function watchJob(runId) {
  watchers.set(runId, (watchers.get(runId) ?? 0) + 1)
  let live = true
  return () => {
    if (!live) return
    live = false
    const left = (watchers.get(runId) ?? 1) - 1
    if (left > 0) watchers.set(runId, left)
    else watchers.delete(runId)
  }
}

/** @param {string} runId */
function isWatched(runId) {
  return (watchers.get(runId) ?? 0) > 0
}

/**
 * Called with each job as it ends. Home reads the library again then: a run
 * or a denoise changed what its chapter row says. Answers the unsubscribe.
 *
 * @param {(job: Job) => void} listener
 * @returns {() => void}
 */
export function onJobSettled(listener) {
  settleListeners.add(listener)
  return () => settleListeners.delete(listener)
}

/** The code a rejection carries, without the `Error: ` a thrown one adds. @param {unknown} error */
function codeOf(error) {
  const text = error instanceof Error ? error.message : String(error ?? '')
  return text.replace(/^Error:\s*/, '').trim() || 'unknown'
}

/**
 * @param {Job} job
 * @param {Exclude<JobStatus, 'running'>} status
 */
function finish(job, status) {
  if (job.status !== 'running') return
  job.status = status
  job.stopping = false
  job.page = 0
  job.finishedAt = Date.now()
  trim()
  for (const listener of [...settleListeners]) {
    try {
      listener(job)
    } catch {
      // A listener's failure is its own; the next one still hears the end.
    }
  }
}

/** Keep every running job and the newest `MAX_FINISHED` finished ones. */
function trim() {
  let finished = 0
  const kept = jobs.list.filter((job) => job.status === 'running' || (finished += 1) <= MAX_FINISHED)
  if (kept.length !== jobs.list.length) jobs.list = kept
}

/* ------------------------------------------------------------------ */
/* Events                                                              */
/* ------------------------------------------------------------------ */

/**
 * @param {Job} job
 * @param {import('../api/backend.js').RunFinishedEvent} event
 */
function settleRun(job, event) {
  if (job.status !== 'running') return
  if (Number(event.pagesQueued) > 0 && !job.total) job.total = Number(event.pagesQueued)
  if (typeof event.pagesCleaned === 'number') job.done = event.pagesCleaned
  const reason = /** @type {string} */ (event.reason)
  finish(job, reason === 'completed' ? 'completed' : reason === 'failed' ? 'failed' : 'cancelled')
}

/** @param {import('../api/backend.js').BackendEvent} event */
function onRunEvent(event) {
  const runId = /** @type {any} */ (event)?.runId
  if (typeof runId !== 'string' || !runId) return
  const job = jobById(runId)
  switch (event.type) {
    case 'page-started':
      if (!job) scheduleHydrate()
      break
    case 'page-done':
      if (!job) scheduleHydrate()
      else if (job.status === 'running') job.done = job.total ? Math.min(job.total, job.done + 1) : job.done + 1
      break
    case 'run-finished':
      if (job) settleRun(job, event)
      else {
        earlyEnds.set(runId, event)
        if (earlyEnds.size > MAX_EARLY_ENDS) earlyEnds.delete(/** @type {string} */ (earlyEnds.keys().next().value))
      }
      break
  }
}

/** @param {import('../api/backend.js').DenoiseProgress} progress */
function onDenoiseProgress(progress) {
  const job = jobById(progress?.runId)
  if (!job) {
    if (progress?.runId) scheduleHydrate()
    return
  }
  if (job.status !== 'running') return
  if (Number.isFinite(progress.total) && progress.total > 0) job.total = progress.total
  if (Number.isFinite(progress.done)) job.done = Math.max(0, progress.done)
  job.page = Number.isFinite(progress.page) ? progress.page : 0
}

/**
 * A finished denoise's pages reached its chapter's manifest late: a run was
 * walking the chapter, and the denoise answered without waiting for it. Its
 * chapter row changed only now, so the settle listeners hear the job again.
 *
 * @param {{runId?: string}} recorded
 */
function onDenoiseRecorded(recorded) {
  const job = jobById(recorded?.runId ?? '')
  if (!job || job.status === 'running') return
  for (const listener of [...settleListeners]) {
    try {
      listener(job)
    } catch {
      // As in `finish`: one listener's failure is its own.
    }
  }
}

function scheduleHydrate() {
  if (hydrateTimer || !started) return
  hydrateTimer = setTimeout(() => {
    hydrateTimer = null
    void refreshJobs()
  }, HYDRATE_DEBOUNCE_MS)
}

/**
 * Ask the backend what is running and take in what this store did not know.
 * A job it lists is running; what it no longer lists is left to its own
 * ending, which is already on its way.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<import('../api/backend.js').RunningJob[]>} the listing, empty when it could not be read
 */
export async function refreshJobs(backend = getBackend()) {
  if (typeof backend?.listJobs !== 'function') return []
  let listed
  try {
    listed = await backend.listJobs()
  } catch {
    return []
  }
  if (!Array.isArray(listed)) return []
  for (const entry of listed) {
    if (!entry || !KINDS.includes(entry.kind)) continue
    const known = jobById(entry.runId)
    if (known) {
      if (known.status === 'running') {
        if (Number(entry.total) > 0) known.total = Number(entry.total)
        if (Number(entry.done) > known.done) known.done = Number(entry.done)
      }
    } else {
      registerJob(entry)
    }
  }
  return listed
}

/**
 * Fill in the names a job was registered without (a hydrated one knows only
 * its chapter id). One library read covers every job that needs it.
 */
function nameJobs() {
  if (naming) return naming
  naming = (async () => {
    try {
      const backend = getBackend()
      if (typeof backend?.listProjects !== 'function') return
      const projects = await backend.listProjects()
      for (const job of jobs.list) {
        if (job.projectName != null && job.chapterName != null && job.projectId != null) continue
        for (const project of projects ?? []) {
          const chapter = project?.chapters?.find((/** @type {any} */ entry) => entry.id === job.chapterId)
          if (!chapter) continue
          job.projectId ??= project.id
          job.projectName ??= project.name
          job.chapterName ??= chapter.name
          job.chapterNumber ??= chapter.number
          break
        }
      }
    } catch {
      // Unnamed jobs still show, as their kind and progress.
    } finally {
      naming = null
    }
  })()
  return naming
}

/* ------------------------------------------------------------------ */
/* Actions                                                             */
/* ------------------------------------------------------------------ */

/**
 * Stop a running job: a run with `cancelRun`, a denoise with
 * `cancelDenoiseLocal`, which stops a cloud one too. The job ends when its
 * ending arrives, not here.
 *
 * @param {string} runId
 */
export async function stopJob(runId) {
  const job = jobById(runId)
  if (!job || job.status !== 'running' || job.stopping) return
  job.stopping = true
  const backend = getBackend()
  try {
    if (RUN_KINDS.includes(job.kind)) await backend.cancelRun({ runId })
    else await backend.cancelDenoiseLocal({ runId })
  } catch {
    const held = jobById(runId)
    if (held) held.stopping = false
  }
}

/** Drop a finished job from the list. @param {string} runId */
export function dismissJob(runId) {
  const index = jobs.list.findIndex((job) => job.runId === runId && job.status !== 'running')
  if (index >= 0) jobs.list.splice(index, 1)
}

/** Drop every finished job. */
export function clearFinishedJobs() {
  const kept = jobs.list.filter((job) => job.status === 'running')
  if (kept.length !== jobs.list.length) jobs.list = kept
}

/**
 * Open the job's chapter in the editor. A job hydrated from `listJobs` knows
 * its chapter and not its project, which the route needs, so that is looked up
 * first. Answers whether the route was changed.
 *
 * @param {string} runId
 * @returns {Promise<boolean>}
 */
export async function openJobChapter(runId) {
  let job = jobById(runId)
  if (!job) return false
  if (!job.projectId) {
    await nameJobs()
    job = jobById(runId)
  }
  if (!job?.projectId) return false
  openChapter(job.projectId, job.chapterId)
  return true
}

/* ------------------------------------------------------------------ */
/* The quit guard                                                      */
/* ------------------------------------------------------------------ */

/**
 * The native side held a quit because jobs are running
 * (`app://quit-requested`). Asked once: a second request while the question
 * is up is the same question.
 *
 * Not on the modal stack. Only the stack's top dialog is mounted
 * (`shell/ModalHost.svelte`), so pushing this would unmount whatever the user
 * had open - a cloud review would stop its tiles and the provisioner would
 * forget the keys typed into it - and a Cancel here would bring it back
 * empty. `App.svelte` draws `dialogs/QuitJobsDialog.svelte` over the stack
 * instead, as it does the first-launch offer.
 *
 * @param {import('../api/backend.js').QuitRequest} request
 */
export function askQuit(request) {
  if (jobs.quit) return
  jobs.quit = {
    count: Math.max(runningCount(), Number(request?.jobs) || 0),
    canHide: request?.canHide === true,
  }
}

/**
 * The answer: `quit` stops every job and quits, `hide` hides the window (only
 * offered with a tray), anything else leaves the app as it was.
 *
 * @param {'quit'|'hide'|'cancel'|null} result
 */
export function answerQuit(result) {
  const asked = jobs.quit
  jobs.quit = null
  if (!asked) return
  const backend = getBackend()
  if (result === 'quit') void Promise.resolve().then(() => backend.confirmQuit()).catch(() => {})
  else if (result === 'hide' && asked.canHide) void Promise.resolve().then(() => backend.hideToTray()).catch(() => {})
}

/* ------------------------------------------------------------------ */
/* Lifecycle                                                           */
/* ------------------------------------------------------------------ */

/**
 * Start the root subscription, once per backend, and read what is running.
 * Answers the teardown; `App.svelte` runs it as an effect.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {() => void}
 */
export function startJobs(backend = getBackend()) {
  if (started === backend) return stopJobs
  stopJobs()
  started = backend
  const off = backend.subscribe?.(onRunEvent)
  if (typeof off === 'function') detach.push(off)
  for (const [method, handler] of /** @type {const} */ ([['onDenoiseProgress', onDenoiseProgress], ['onDenoiseRecorded', onDenoiseRecorded], ['onQuitRequested', askQuit]])) {
    const listen = /** @type {any} */ (backend)[method]
    if (typeof listen !== 'function') continue
    let live = true
    /** @type {(() => void)|null} */
    let unlisten = null
    detach.push(() => {
      live = false
      unlisten?.()
    })
    Promise.resolve(listen.call(backend, handler))
      .then((stop) => {
        if (typeof stop !== 'function') return
        if (live) unlisten = stop
        else stop()
      })
      .catch(() => {})
  }
  void refreshJobs(backend)
  return stopJobs
}

/** Detach from the backend. The list stays. */
export function stopJobs() {
  for (const off of detach.splice(0)) off()
  started = null
  if (hydrateTimer) clearTimeout(hydrateTimer)
  hydrateTimer = null
}

/** Forget everything. For tests. */
export function resetJobs() {
  stopJobs()
  jobs.list = []
  jobs.quit = null
  earlyEnds.clear()
  watchers.clear()
  settleListeners.clear()
  naming = null
}
