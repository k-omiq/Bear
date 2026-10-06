/**
 * What this machine can do that another one cannot.
 *
 * Two questions with the same shape and the same rule. Rung 3a, the FLUX
 * sidecar, ships with nothing and is installed by
 * hand. The weights are not bundled either - they
 * are downloaded after install, from Settings › Models - so on a fresh machine
 * *every* engine below rung 3a is missing too, and until this store carried
 * them the interface offered all five rungs and a run failed on the first page
 * for want of a file the user had no way to fetch.
 *
 * **Absence is the normal state and it is silent.** The backend answers
 * `reasonKey: null` for a machine that never installed anything, which is what
 * §4a asks for: a user "is told why rather than shown a control that fails",
 * and the answer to *why* for a machine that installed nothing is that there is
 * nothing to tell. A reason arrives only when something *is* installed and this
 * machine still will not carry it, and then it is in the same
 * `decline.reason.*` vocabulary a review row uses. The weights get the same
 * treatment for a different reason: the remedy for a missing weight is one
 * press in Settings, so the tool that needs it names Settings rather than
 * showing a control that would fail.
 *
 * **The local engine map is derived from the catalogue's own `requiredBy`.**
 * `src-tauri/src/weights.rs` says which engines each
 * file is a precondition of; this walks the rows and intersects them. An engine
 * no row names - `fill`, which needs no weights at all - is
 * available by falling out of the rule rather than by being written down a
 * second time, and a seventh weight added to the catalogue tomorrow gates
 * whatever it says it gates with no change here. Auto clean is checked against
 * the detector combination selected in the session, since its models vary.
 */

import { getBackend } from '../api/backend.js'
import { model, workflowNeeds } from '../model/pipelines.js'
import { session } from './session.svelte.js'

/**
 * The engines a mask row, the Shapes row and the AI mask brush can offer, plus
 * the one feature that is not an engine.
 *
 * `flux` is the sidecar's and is answered by `sidecarAvailable`; the other
 * two and `autoClean` come from the catalogue.
 */
const ALWAYS_AVAILABLE = Object.freeze({
  fill: true,
  lama: true,
  flux: false,
})

export const capabilities = $state({
  /** Whether rung 3a can be offered here. */
  sidecar: false,
  /** Why not, where there is something to say. @type {string|null} */
  sidecarReasonKey: null,
  /**
   * Which rungs have the weights they need on this machine.
   *
   * Optimistic before the first `loadCapabilities`, and that is deliberate:
   * this is read by every engine list in the editor, and a store that started
   * empty would blank the pickers for the moment between mount and the first
   * answer. Missing weights are the unusual case; a flicker on every launch is
   * not worth guarding against one.
   *
   * @type {Record<string, boolean>}
   */
  engines: { ...ALWAYS_AVAILABLE },
  /** Last answer for the currently selected automatic workflow. */
  autoClean: true,
  /** Last catalogue and verified review graphs, used against the current detector selection. */
  modelRows: null,
  workflowCaps: null,
  /** Whether an ONNX Runtime was found. Nothing runs without one. */
  runtime: true,
})

/**
 * Turn a catalogue into `{engine: boolean}`.
 *
 * A feature is available when **every** row that names it is installed, so a
 * script gate whose labels file is missing takes Auto clean down with it - the
 * two must match and one without the other is not a gate.
 *
 * Exported for the tests: it is pure, and it is the whole of the gating rule.
 *
 * @param {Array<{requiredBy?: string[], installed?: boolean}>} rows
 * @returns {Record<string, boolean>}
 */
export function featuresFrom(rows) {
  /** @type {Record<string, boolean>} */
  const features = {}
  for (const row of rows ?? []) {
    for (const feature of row.requiredBy ?? []) {
      features[feature] = (features[feature] ?? true) && row.installed === true
    }
  }
  return features
}

/**
 * The logical models (`pipelines.js#MODELS`) the selected detector
 * combination needs on this computer and does not have, in need order. A
 * stage routed to the cloud GPU is not a need here (`workflowNeeds`).
 */
export function selectedWorkflowMissing(rows, workflowCaps, choices) {
  const byId = new Map((rows ?? []).map((row) => [row.id, row]))
  return workflowNeeds(choices).filter((id) => {
    if (id === 'samTs') return workflowCaps?.samInstalled !== true
    if (id === 'rtFull' && workflowCaps?.fullRtInstalled !== undefined) return !workflowCaps.fullRtInstalled
    return (model(id)?.files ?? []).some((file) => {
      const row = byId.get(file)
      return row ? row.installed !== true : (rows ?? []).length > 0
    })
  })
}

/** A run needs exactly the files in its selected detector combination. */
export function selectedWorkflowAvailable(rows, workflowCaps, choices) {
  return selectedWorkflowMissing(rows, workflowCaps, choices).length === 0
}

/**
 * What the current selection is missing on this computer. `null` before the
 * catalogue has been read and the last answer was that something is
 * missing: which one is not known then.
 *
 * `overrides` asks the same question of other choices, such as Detect on set
 * the other way (`{analysisTargets}`), without changing the session.
 *
 * @param {{analysisTargets?: Record<string, string>}} [overrides]
 * @returns {string[]|null}
 */
export function currentWorkflowMissing(overrides = {}) {
  if (!capabilities.modelRows) return capabilities.autoClean ? [] : null
  return selectedWorkflowMissing(capabilities.modelRows, capabilities.workflowCaps, {
    textPolicy: session.textPolicy, detection: session.detection,
    ocrRescue: session.ocrRescue, detectorModels: session.detectorModels,
    analysisTargets: session.analysisTargets,
    ...overrides,
  })
}

export function currentWorkflowAvailable() {
  return currentWorkflowMissing()?.length === 0
}

/**
 * Ask the backend once. A backend that cannot answer leaves the defaults -
 * which offer nothing for the sidecar, the safe direction there, and everything
 * for the weights, the safe direction here: a machine whose catalogue cannot be
 * read is far more likely to be one running an older adapter than one with no
 * models, and hiding every engine on a failed call would be a worse failure
 * than offering one that reports a missing file.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<void>}
 */
export async function loadCapabilities(backend = getBackend()) {
  try {
    const answer = await backend.sidecarAvailable()
    capabilities.sidecar = Boolean(answer?.available)
    capabilities.sidecarReasonKey = answer?.reasonKey ?? null
  } catch {
    capabilities.sidecar = false
    capabilities.sidecarReasonKey = null
  }

  try {
    const view = await backend.listModels()
    const features = featuresFrom(view?.models ?? [])
    const selected = session.detectorModels
    let workflowCaps = null
    if (selected.includes('samTs') || selected.includes('rtFull')) {
      try { workflowCaps = await backend.listWorkflowCapabilities() } catch { /* no verified graph answer */ }
    }
    capabilities.modelRows = view?.models ?? []
    capabilities.workflowCaps = workflowCaps
    capabilities.engines = {
      ...ALWAYS_AVAILABLE,
      lama: features.lama !== false,
      flux: capabilities.sidecar,
    }
    capabilities.autoClean = currentWorkflowAvailable()
    capabilities.runtime = view?.runtime?.installed !== false
  } catch {
    capabilities.engines = { ...ALWAYS_AVAILABLE, flux: capabilities.sidecar }
    capabilities.modelRows = null
    capabilities.workflowCaps = null
    capabilities.autoClean = true
    capabilities.runtime = true
  }
}
