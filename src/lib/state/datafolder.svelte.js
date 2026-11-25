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
 *
 * Two refusals any library call can meet are about the folder, not the call
 * (`api/backenderrors.js` reports them all): `data_folder_unavailable:` reads
 * the status again, which brings up the shell's blocking screen, and
 * `data_folder_moving:` (the other app is moving the folder) holds the window
 * under the busy cover and polls until the library answers again. The
 * backend's text puts other words first (`library: data_folder_...`), so both
 * are matched anywhere in the message.
 *
 * `generation` counts the times the library may have changed place: Home
 * reads it to list its projects again.
 */

import { getBackend } from '../api/backend.js'
import { onBackendError } from '../api/backenderrors.js'

/** @typedef {import('../api/backend.js').DataFolderStatus} DataFolderStatus */
/** @typedef {'setup'|'move'|'useExisting'} ChangeKind */

/** The error prefix the backend uses when it cannot tell whether the typesetter is open. */
export const TYPESETTER_UNKNOWN = 'typesetter_unknown:'
/** The code every library call's error carries while the folder is unavailable. */
export const DATA_FOLDER_UNAVAILABLE = 'data_folder_unavailable:'
/** The code every library call's error carries while a move holds the folder. */
export const DATA_FOLDER_MOVING = 'data_folder_moving:'
/** How often a held window asks whether the move has ended. */
export const MOVING_POLL_MS = 2000

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
  /** Another process is moving the folder: library calls are refused until it ends. */
  moving: false,
  /** Bumped after every change of place, so Home lists its projects again. */
  generation: 0,
})

/** @param {unknown} error */
const message = (error) => (error instanceof Error ? error.message : String(error ?? ''))

/** @param {unknown} error */
export function isUnavailableError(error) {
  return message(error).includes(DATA_FOLDER_UNAVAILABLE)
}

/** @param {unknown} error */
export function isMovingError(error) {
  return message(error).includes(DATA_FOLDER_MOVING)
}

/** Whether the window is held under the busy cover: a change of place in flight here, or a move elsewhere. */
export function dataFolderHeld() {
  return dataFolder.moving || (dataFolder.busy !== null && dataFolder.busy !== 'delete')
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
let reading = /** @type {Promise<DataFolderStatus|null>|null} */ (null)

export function loadDataFolder(backend = getBackend()) {
  // One read at a time: a burst of refused calls asks once.
  reading ??= readStatus(backend).finally(() => {
    reading = null
  })
  return reading
}

/** @param {import('../api/backend.js').Backend} backend */
async function readStatus(backend) {
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
    dataFolder.generation += 1
    return true
  } catch (error) {
    // A move that failed half way may still have switched the pointer.
    await loadDataFolder(backend)
    // Asked only once the change is over, so the answer can send it again.
    dataFolder.busy = null
    const said = message(error)
    if (said.includes(TYPESETTER_UNKNOWN) && !confirmedClosed) dataFolder.confirm = { kind, target }
    else dataFolder.error = said
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

/* ------------------------------------------------------------------ */
/* Refusals about the whole folder                                     */
/* ------------------------------------------------------------------ */

/** @type {ReturnType<typeof setTimeout>|null} */
let poll = null

/**
 * What a refused library call says about the folder, if anything. A change of
 * place in flight here explains its own refusals, and its end reads the
 * status anyway.
 *
 * @param {unknown} error
 * @param {import('../api/backend.js').Backend} [backend]
 */
export function noteLibraryError(error, backend) {
  if (dataFolder.busy !== null && dataFolder.busy !== 'delete') return
  if (isMovingError(error)) startMoving(backend)
  else if (isUnavailableError(error)) void loadDataFolder(backend)
}

/** @param {import('../api/backend.js').Backend} [backend] */
function startMoving(backend) {
  dataFolder.moving = true
  if (poll === null) poll = setTimeout(() => void checkMoving(backend), MOVING_POLL_MS)
}

/**
 * Ask the library whether the move has ended: still refused as moving, wait
 * again; anything else, the move is over and the status is read again.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 */
async function checkMoving(backend = getBackend()) {
  poll = null
  try {
    await backend.listProjects()
  } catch (error) {
    if (isMovingError(error)) {
      poll = setTimeout(() => void checkMoving(backend), MOVING_POLL_MS)
      return
    }
  }
  dataFolder.moving = false
  await loadDataFolder(backend)
  dataFolder.generation += 1
}

onBackendError((error) => noteLibraryError(error))

/** For tests. */
export function resetDataFolder() {
  if (poll !== null) clearTimeout(poll)
  poll = null
  reading = null
  dataFolder.moving = false
  dataFolder.generation = 0
  dataFolder.status = null
  dataFolder.checked = false
  dataFolder.busy = null
  dataFolder.error = null
  dataFolder.confirm = null
  dataFolder.recoveredSeen = false
  dataFolder.pendingLater = false
}
