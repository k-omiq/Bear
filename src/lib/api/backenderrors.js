/**
 * Every rejected backend command, told to whoever listens.
 *
 * Some refusals are about the library as a whole rather than the call that
 * met them: the data folder went away (`data_folder_unavailable:`) or is being
 * moved (`data_folder_moving:`). Any command can meet one, from Home, the
 * editor or a dialog, so the adapter reports every rejection here and the data
 * folder store (`state/datafolder.svelte.js`) decides what it means. The
 * caller still gets its rejection, unchanged.
 */

/** @type {Set<(error: unknown) => void>} */
const listeners = new Set()

/**
 * @param {(error: unknown) => void} listener
 * @returns {() => void} stop listening
 */
export function onBackendError(listener) {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

/** @param {unknown} error */
export function reportBackendError(error) {
  for (const listener of listeners) {
    try {
      listener(error)
    } catch (thrown) {
      console.error('a backend error listener failed', thrown)
    }
  }
}
