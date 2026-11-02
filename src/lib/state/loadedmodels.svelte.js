/**
 * What the backend has in memory right now, polled.
 *
 * **A poll and not the event channel**, deliberately. The seam's event stream is a
 * *run's* narration - page started, region done, run finished - and it is
 * ordered per run. What is loaded is not a fact about a run: a region edit
 * loads a model without starting one, a session is given back between two
 * regions, and rung 3a's child comes and goes inside a single click. Putting
 * that on the run stream would either invent events for things that are not
 * runs or leave the tab stale for the cases that are not.
 *
 * The cost is bounded on purpose: the command behind this walks a handful of
 * rows under one mutex and touches no session (`src-tauri/src/models.rs`), and
 * the poll only runs while the editor is on screen.
 *
 * **The list is one interval behind the truth, in both directions**, and the
 * interface is written for that rather than against it. A model can finish and
 * vanish between two polls, and a model asked to unload keeps its row until the
 * run reaches a region boundary - which is why `unloading` is a field the row
 * carries rather than something this module fakes by removing the row early.
 * Removing it early would claim memory back that is still held.
 */

import { getBackend } from '../api/backend.js'

/**
 * How often to ask. Two seconds is under the time it takes to wonder whether
 * something is stuck and well over the cost of the call; anything faster would
 * be polling a mutex for a list that changes once a page.
 */
export const POLL_MS = 2000

export const loadedModels = $state({
  /** @type {import('../api/backend.js').LoadedModel[]} */
  models: [],
  /** All of it as the operating system counts it, or `null` when it cannot say. @type {number|null} */
  totalBytes: null,
})

/**
 * Ask once. A backend that cannot answer leaves the list **empty** rather than
 * leaving the last answer up: a stale row invites a click on a close button for
 * a model that is not there, and an empty tab is hidden, which is the honest
 * shape for "cannot say".
 *
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<void>}
 */
export async function refreshLoadedModels(backend = getBackend()) {
  try {
    const [models, totalBytes] = await Promise.all([backend.listLoadedModels(), backend.memoryInUse()])
    loadedModels.models = Array.isArray(models) ? models : []
    loadedModels.totalBytes = typeof totalBytes === 'number' ? totalBytes : null
  } catch {
    loadedModels.models = []
    loadedModels.totalBytes = null
  }
}

/**
 * Ask for one back, and refresh so the row shows the request.
 *
 * The refresh is what makes the button honest: the backend answers `true` for
 * "recorded", the row comes back with `unloading: true`, and it disappears on
 * the poll after the owning thread actually dropped the session.
 *
 * @param {number} id
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<void>}
 */
export async function unloadModel(id, backend = getBackend()) {
  try {
    await backend.unloadModel({ id })
  } catch {
    // A failed request is a row that stays as it was. There is nothing to tell
    // the user that the next poll will not tell them better.
  }
  await refreshLoadedModels(backend)
}

/**
 * Poll for as long as the caller is mounted. Shaped as an `$effect` body: it
 * returns the teardown.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {() => void}
 */
export function pollLoadedModels(backend = getBackend()) {
  let live = true
  const tick = () => {
    if (live) refreshLoadedModels(backend)
  }
  tick()
  const timer = setInterval(tick, POLL_MS)
  return () => {
    live = false
    clearInterval(timer)
    loadedModels.models = []
  }
}
