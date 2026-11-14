/**
 * The folder chooser.
 *
 * A project and a chapter each need to be told where their scans are, and until
 * this existed the only way to say so was to type an absolute path from memory
 * into a text field. That is not an interface anybody can use for a folder
 * eleven levels down a drive, and it is the half of import that was missing:
 * `createProject` and `createChapter` were real commands with no way to reach
 * the one argument that matters.
 *
 * It is deliberately **not** on the seam. The seam
 * is the contract between the interface and a backend that owns a library, and
 * a native file chooser is neither: it produces no library state, it belongs to
 * the window rather than to the data, and a backend that ran headlessly would
 * have nothing to answer with. So it lives beside the adapters instead, and the
 * two dialogs that need it import it directly.
 *
 * Outside a Tauri window - the browser dev server, and vitest - there is no
 * chooser, and this returns `null` rather than throwing. `null` is also what a
 * user who dismissed the chooser produces, and the two are handled the same way
 * on purpose: in both cases nothing was chosen, and the field the caller is
 * filling keeps whatever was already typed in it. The typed field is not a
 * fallback that will be removed once the chooser works; it stays, because a
 * path pasted from a terminal is faster than eleven clicks and because it is
 * the only way this screen works at all when there is no chooser to open.
 */

import { isTauri } from './tauri.js'

/**
 * Ask the user for a directory.
 *
 * Goes straight at the plugin's command rather than through
 * `@tauri-apps/plugin-dialog`, for the reason `tauri.js` gives for taking
 * `invoke` off the window: nothing in `src/lib` may import `@tauri-apps/*`, or
 * the module stops loading in a plain browser and in vitest, where the import
 * throws before any of this is reached. The command name and its argument shape
 * are the plugin's own (`plugin:dialog|open`, `{ options }`), and the window is
 * granted `dialog:allow-open` and nothing else in
 * `src-tauri/capabilities/default.json`.
 *
 * @param {Object} [options]
 * @param {string} [options.title] - already translated; the chooser's own title bar
 * @param {string} [options.defaultPath] - where to open, when there is somewhere sensible
 * @returns {Promise<string|null>} the chosen directory, or null if there was no choice
 */
export async function chooseFolder({ title, defaultPath } = {}) {
  if (!isTauri()) return null
  const invoke = globalThis.__TAURI__?.core?.invoke ?? globalThis.__TAURI_INTERNALS__?.invoke
  if (typeof invoke !== 'function') return null
  const chosen = await invoke('plugin:dialog|open', {
    options: {
      title,
      // Only where there is one. An empty string would be a path the chooser
      // tries to open and fails to.
      defaultPath: defaultPath || undefined,
      directory: true,
      multiple: false,
      // The chapter's own files are read by the Rust side through this crate's
      // commands, not by the webview, so the plugin has no scope to widen.
      recursive: false,
    },
  })
  return typeof chosen === 'string' && chosen !== '' ? chosen : null
}

/** Choose one source page for read-only local model analysis. */
export async function chooseImage({ title } = {}) {
  if (!isTauri()) return null
  const invoke = globalThis.__TAURI__?.core?.invoke ?? globalThis.__TAURI_INTERNALS__?.invoke
  if (typeof invoke !== 'function') return null
  const chosen = await invoke('plugin:dialog|open', {
    options: { title, directory: false, multiple: false,
      filters: [{ name: 'Comic pages', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp'] }] },
  })
  return typeof chosen === 'string' && chosen !== '' ? chosen : null
}

/** Choose a locally obtained ONNX graph for a verified, explicit import. */
export async function chooseOnnx({ title } = {}) {
  if (!isTauri()) return null
  const invoke = globalThis.__TAURI__?.core?.invoke ?? globalThis.__TAURI_INTERNALS__?.invoke
  if (typeof invoke !== 'function') return null
  const chosen = await invoke('plugin:dialog|open', {
    options: { title, directory: false, multiple: false,
      filters: [{ name: 'ONNX graph', extensions: ['onnx'] }] },
  })
  return typeof chosen === 'string' && chosen !== '' ? chosen : null
}

/** The page files the Pages screen offers to add, as the chapter import reads them. */
export const PAGE_FILE_EXTENSIONS = Object.freeze(['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp', 'tif', 'tiff'])

/**
 * Choose page files to add to a chapter, several at once. `[]` when nothing
 * was chosen or there is no chooser (the browser dev server, vitest).
 *
 * @param {{title?: string, defaultPath?: string}} [options]
 * @returns {Promise<string[]>}
 */
export async function chooseImages({ title, defaultPath } = {}) {
  if (!isTauri()) return []
  const invoke = globalThis.__TAURI__?.core?.invoke ?? globalThis.__TAURI_INTERNALS__?.invoke
  if (typeof invoke !== 'function') return []
  const chosen = await invoke('plugin:dialog|open', {
    options: {
      title,
      defaultPath: defaultPath || undefined,
      directory: false,
      multiple: true,
      filters: [{ name: 'Comic pages', extensions: [...PAGE_FILE_EXTENSIONS] }],
    },
  })
  const list = Array.isArray(chosen) ? chosen : typeof chosen === 'string' ? [chosen] : []
  return list.filter((path) => typeof path === 'string' && path !== '')
}

/**
 * Files dropped on the window, by path. Inside a Tauri window the webview does
 * not see a file drop as a DOM event - Tauri takes it and says so as
 * `tauri://drag-drop` with the paths - so this listens there. Outside one it
 * does nothing, and the caller reads the DOM `drop` instead (the mock takes
 * file names).
 *
 * @param {{onenter?: () => void, onleave?: () => void, ondrop: (paths: string[]) => void}} handlers
 * @returns {() => void} stop listening
 */
export function listenFileDrops({ onenter, onleave, ondrop }) {
  const listen = globalThis.__TAURI__?.event?.listen
  if (!isTauri() || typeof listen !== 'function') return () => {}
  /** @type {Array<Promise<() => void>>} */
  const pending = [
    listen('tauri://drag-enter', () => onenter?.()),
    listen('tauri://drag-leave', () => onleave?.()),
    listen('tauri://drag-drop', (event) => {
      onleave?.()
      const paths = event?.payload?.paths
      if (Array.isArray(paths) && paths.length) ondrop(paths.filter((path) => typeof path === 'string'))
    }),
  ].map((promise) => Promise.resolve(promise))
  return () => {
    for (const promise of pending) promise.then((unlisten) => unlisten?.()).catch(() => {})
  }
}
