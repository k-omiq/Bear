/**
 * The shared data folder, as the interface sees it (`docs/shared-data-folder.md`).
 *
 * One store because four places read it: the shell (Unavailable is a blocking
 * screen), Home (the Pending card), Settings (the section that sets it up and
 * moves it) and the editor (Save needs Shared mode). Each change answers with
 * the new status, so the store is replaced, never patched.
 *
 * A change can take minutes (a move to another drive copies and verifies every
 * file), so `busy` names the change in flight, and Settings will not close
 * under it. A change the drive cannot vouch for, because it cannot tell whether
 * the typesetter is open (`typesetter_unknown:`), is held in `confirm` until the
 * user says the typesetter is closed, then sent again with `confirmedClosed`.
 */

import { getBackend } from '../api/backend.js'

/** @typedef {import('../api/backend.js').DataFolderStatus} DataFolderStatus */
/** @typedef {'setup'|'move'|'useExisting'} ChangeKind */

/** The error prefix the backend uses when it cannot tell whether the typesetter is open. */
export const TYPESETTER_UNKNOWN = 'typesetter_unknown:'
/** The error prefix every library call answers with while the folder is unavailable. */
export const DATA_FOLDER_UNAVAILABLE = 'data_folder_unavailable:'

export const dataFolder = $state({
  /** @type {DataFolderStatus|null} */
  status: null,
  /** The first read has answered, either way. */
  checked: false,
  /** @type {ChangeKind|'delete'|null} */
  busy: null,
  /** What the last change failed with, as the backend said it. @type {string|null} */
  error: null,
  /** A change waiting for "the typesetter is closed". @type {{kind: ChangeKind, target: string}|null} */
  confirm: null,
  /** The start-up recovery line was dismissed this session. */
  recoveredSeen: false,
  /** Home's Pending card was put off this session. */
  pendingLater: false,
})

/** @param {unknown} error */
const message = (error) => (error instanceof Error ? error.message : String(error ?? ''))

/** @param {unknown} error */
export function isUnavailableError(error) {
  return message(error).startsWith(DATA_FOLDER_UNAVAILABLE)
}

/** The mode, or null before the first answer and on a backend that has none. */
export function dataFolderMode() {
  return dataFolder.status?.mode ?? null
}

/**
 * Read the status. A backend that cannot answer leaves the last status in
 * place: the app then behaves as before the feature (Legacy).
 *
 * @param {import('../api/backend.js').Backend} [backend]
 */
export async function loadDataFolder(backend = getBackend()) {
  try {
    if (typeof backend.dataFolderStatus === 'function') dataFolder.status = await backend.dataFolderStatus()
  } catch (error) {
    console.error('the data folder status could not be read', error)
  } finally {
    dataFolder.checked = true
  }
  return dataFolder.status
}

/** @type {Record<ChangeKind, 'dataFolderSetup'|'dataFolderMove'|'dataFolderUseExisting'>} */
const COMMANDS = {
  setup: 'dataFolderSetup',
  move: 'dataFolderMove',
  useExisting: 'dataFolderUseExisting',
}

/**
 * Set up, move, or point at another folder. Answers whether it was done.
 *
 * @param {ChangeKind} kind
 * @param {string} target
 * @param {{confirmedClosed?: boolean, backend?: import('../api/backend.js').Backend}} [options]
 * @returns {Promise<boolean>}
 */
export async function changeDataFolder(kind, target, { confirmedClosed = false, backend = getBackend() } = {}) {
  if (dataFolder.busy) return false
  dataFolder.busy = kind
  dataFolder.error = null
  dataFolder.confirm = null
  try {
    dataFolder.status = await backend[COMMANDS[kind]]({ target, confirmedClosed })
    return true
  } catch (error) {
    const said = message(error)
    if (said.startsWith(TYPESETTER_UNKNOWN) && !confirmedClosed) dataFolder.confirm = { kind, target }
    else dataFolder.error = said
    // A move that failed half way may still have switched the pointer.
    await loadDataFolder(backend)
    return false
  } finally {
    dataFolder.busy = null
  }
}

/** Send the held change again, the typesetter confirmed closed. */
export async function confirmTypesetterClosed(backend = getBackend()) {
  const held = dataFolder.confirm
  if (!held) return false
  return changeDataFolder(held.kind, held.target, { confirmedClosed: true, backend })
}

/** Drop the held change. */
export function cancelDataFolderConfirm() {
  dataFolder.confirm = null
}

/**
 * Delete an old copy a move set aside.
 *
 * @param {string} path
 * @param {import('../api/backend.js').Backend} [backend]
 */
export async function deleteOldCopy(path, backend = getBackend()) {
  if (dataFolder.busy) return false
  dataFolder.busy = 'delete'
  dataFolder.error = null
  try {
    dataFolder.status = await backend.dataFolderDeleteOldCopy({ path })
    return true
  } catch (error) {
    dataFolder.error = message(error)
    return false
  } finally {
    dataFolder.busy = null
  }
}

/** For tests. */
export function resetDataFolder() {
  dataFolder.status = null
  dataFolder.checked = false
  dataFolder.busy = null
  dataFolder.error = null
  dataFolder.confirm = null
  dataFolder.recoveredSeen = false
  dataFolder.pendingLater = false
}
