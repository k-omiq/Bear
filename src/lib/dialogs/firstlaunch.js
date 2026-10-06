/**
 * What onboarding downloads, decided from the catalogue and the user's choices.
 *
 * Pure, so the plan and the queue can be pinned without mounting the screen.
 * Which engine needs which file is `model/pipelines.js`'s table; this module
 * only joins that table to what `listModels` says is on disk.
 */

import { filesFor } from '../model/pipelines.js'
import { denoiseRows } from '../model/denoise.js'

/** The id the ONNX Runtime's own download reports under (`src-tauri/src/weights.rs`). */
export const RUNTIME_ID = 'runtime'

/**
 * The model Settings falls back to when the sidecar lists models and none has
 * been chosen. Settings > Models makes the same choice with the same id.
 */
export const DEFAULT_FLUX_MODEL = 'flux2-klein-4b'

/**
 * The runtime has no `models.kind.*` name of its own - it is an archive rather
 * than a weight - so it borrows the one Settings gives it.
 */
export const RUNTIME_LABEL_KEY = 'settings.models.runtime.label'

/**
 * The setup's steps, in order, one decision each. `cloud` sits after
 * `cleaning` because what it sets up is a place to run AI redraw, and
 * `denoise` after `cloud` because whether Cloud GPU is offered depends on it.
 */
export const FIRST_LAUNCH_STEPS = Object.freeze([
  'welcome',
  'theme',
  'token',
  'background',
  'detection',
  'cleaning',
  'denoise',
  'cloud',
  'community',
  'dependencies',
  'downloads',
])

/**
 * @typedef {Object} PlanRow
 * @property {string} id - a catalogue row's id, or `runtime`
 * @property {string} labelKey - the i18n key that names it
 * @property {number} bytes - what its download costs; 0 when the view cannot say
 * @property {boolean} installed
 */

/**
 * @typedef {Object} FirstLaunchPlan
 * @property {Record<string, PlanRow>} files - every catalogue row, by id, plus the runtime when this platform publishes one
 * @property {boolean} runtimeUnavailable - this platform publishes no runtime, so local models cannot run until one is placed by hand
 * @property {{version: string|null, flavour: string|null, needs: string[]}} runtime - the build that would be fetched, and what it needs installed by hand first (a CUDA toolkit, say)
 * @property {string|null} platform - `macos-arm64` and the like, from the backend; null when it did not say
 * @property {boolean} hasToken - a Hugging Face token is already stored
 * @property {string[]} denoise - the catalogue ids local page denoise downloads (required by `pageDenoise`)
 */

/**
 * @param {import('../api/backend.js').ModelsView|null|undefined} view
 * @returns {FirstLaunchPlan}
 */
export function firstLaunchPlan(view) {
  const models = Array.isArray(view?.models) ? view.models : []
  const runtime = view?.runtime
  /** @type {Record<string, PlanRow>} */
  const files = {}
  for (const row of models) {
    if (typeof row?.id === 'string') files[row.id] = rowOf(row.id, row.kindKey, row.bytes, row.installed)
  }
  // An unavailable runtime - an Intel Mac, where nothing is published - is
  // left out rather than listed as a row no download can satisfy.
  // One placed by hand on such a platform is still installed, and says so.
  if (runtime && (runtime.available !== false || runtime.installed === true)) {
    files[RUNTIME_ID] = rowOf(RUNTIME_ID, RUNTIME_LABEL_KEY, runtime.bytes, runtime.installed)
  }
  return {
    files,
    runtimeUnavailable: runtime ? runtime.available === false && runtime.installed !== true : false,
    runtime: {
      version: runtime?.version ?? null,
      flavour: runtime?.flavour ?? null,
      needs: runtimeNeeds(runtime),
    },
    platform: typeof runtime?.platform === 'string' ? runtime.platform : null,
    hasToken: view?.hasToken === true,
    denoise: denoiseRows(models).map((row) => row.id).filter((id) => typeof id === 'string'),
  }
}

/**
 * The ids the choices need, in download order: **the runtime first**, because
 * it is what every weight needs to be used, then the files in table order.
 * No weight chosen, no runtime: it runs nothing on its own.
 * Installed files are included; the queue marks them done rather than fetching.
 *
 * @param {FirstLaunchPlan} plan
 * @param {Record<string, string|null>} detection
 * @param {Record<string, boolean>} cleaners
 * @param {{textPolicy?: string, ocrRescue?: boolean, detectorModels?: string[]|null, analysisTargets?: Record<string, string>|null}} [workflow] - see `filesFor`
 * @returns {string[]}
 */
export function neededFiles(plan, detection, cleaners, workflow = {}) {
  const ids = filesFor(detection, cleaners, workflow).filter((id) => plan.files[id])
  return plan.files[RUNTIME_ID] && ids.length ? [RUNTIME_ID, ...ids] : ids
}

/**
 * What the choices still cost, in bytes.
 *
 * @param {FirstLaunchPlan} plan
 * @param {string[]} ids
 * @param {Record<string, boolean>} [finished] - ids that arrived since the plan was taken
 */
export function missingBytes(plan, ids, finished = {}) {
  return ids.reduce((sum, id) => {
    const row = plan.files[id]
    return row && !row.installed && !finished[id] ? sum + row.bytes : sum
  }, 0)
}

/**
 * Whether the runtime can be asked which processors it offers.
 *
 * Asking maps the runtime's library into this process, and Windows will not
 * replace a file that is mapped - which is exactly what the runtime's own
 * download has to do. So the question waits until the runtime is here and is
 * not the transfer in flight. A view with no runtime row at all is an adapter
 * older than the row, and is answered the way `capabilities` answers it.
 *
 * @param {FirstLaunchPlan|null|undefined} plan
 * @param {Record<string, boolean>} finished
 * @param {string|null} current
 * @returns {boolean}
 */
export function runtimeReady(plan, finished, current) {
  if (!plan || plan.runtimeUnavailable) return false
  if (current === RUNTIME_ID) return false
  const runtime = plan.files[RUNTIME_ID]
  if (!runtime) return true
  return runtime.installed || finished?.[RUNTIME_ID] === true
}

/**
 * The sentence a refused download has, when the backend has one.
 *
 * The runtime's refusals arrive as `key name=value ...` - `notice.runtime.noSpace
 * needed=253000000 free=1200000` - which is the grammar Settings > Models
 * reads as well. Only `notice.runtime.*` is honoured, for the reason Settings
 * gives: a failure free to name any key would be the backend choosing what the
 * interface says. The caller still checks that the key exists.
 *
 * @param {unknown} message
 * @returns {{key: string, params: Record<string, number>}|null}
 */
export function runtimeNotice(message) {
  const text = typeof message === 'string' ? message.trim() : ''
  const [key, ...pairs] = text.split(/\s+/)
  if (!key || !/^notice\.runtime\.[a-zA-Z0-9]+$/.test(key)) return null
  /** @type {Record<string, number>} */
  const params = {}
  for (const pair of pairs) {
    const at = pair.indexOf('=')
    if (at <= 0) continue
    const value = Number(pair.slice(at + 1))
    if (Number.isFinite(value)) params[pair.slice(0, at)] = value
  }
  return { key, params }
}

/**
 * Which AI redraw model a first choice lands on: the recommended one when the
 * sidecar has it, else the first it lists, else none.
 *
 * @param {Array<{id: string}>|null|undefined} models
 * @returns {string|null}
 */
export function defaultFluxModel(models) {
  const list = Array.isArray(models) ? models : []
  if (list.some((model) => model?.id === DEFAULT_FLUX_MODEL)) return DEFAULT_FLUX_MODEL
  return typeof list[0]?.id === 'string' ? list[0].id : null
}

/** @param {FirstLaunchPlan|null} plan @param {string} id @returns {string|null} */
export function labelKeyFor(plan, id) {
  return plan?.files[id]?.labelKey ?? null
}

/**
 * What the chosen runtime build needs the user to have installed, from its
 * own row in `flavours`. Names are data (`CUDA 12`, `cuDNN 9`), not copy.
 *
 * @param {import('../api/backend.js').ModelsView['runtime']|undefined} runtime
 * @returns {string[]}
 */
function runtimeNeeds(runtime) {
  const chosen = runtime?.flavours?.find((flavour) => flavour.id === runtime.flavour)
  return Array.isArray(chosen?.userInstalled) ? chosen.userInstalled.filter((item) => typeof item === 'string') : []
}

/**
 * @param {string} id
 * @param {unknown} labelKey
 * @param {unknown} bytes
 * @param {unknown} installed
 * @returns {PlanRow}
 */
function rowOf(id, labelKey, bytes, installed) {
  return {
    id,
    labelKey: typeof labelKey === 'string' ? labelKey : '',
    // A view that cannot say how large something is contributes nothing to
    // the total rather than an invented figure.
    bytes: typeof bytes === 'number' && Number.isFinite(bytes) && bytes > 0 ? bytes : 0,
    installed: installed === true,
  }
}
