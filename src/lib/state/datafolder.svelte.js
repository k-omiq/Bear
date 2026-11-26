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
import { notify } from './app.svelte.js'
import { session, setStartAnswer } from './session.svelte.js'
import { fillsEmptySubfolder, readStartAnswers, shownPath, startAction, startRow } from './startflow.js'

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

/* ------------------------------------------------------------------ */
/* The start flows (docs/shared-data-folder.md section 5)              */
/* ------------------------------------------------------------------ */

/** How often the Working state asks how far the copy has come. */
export const PROGRESS_POLL_MS = 400

/**
 * The centered dialog of rows D and F (section 5.4). Not on the modal stack:
 * like the first-launch screen it belongs to the start, and its Working
 * state must stay up over the busy cover while the library moves.
 *
 * `phase`: `ask` (the question, or Blocked while Pear runs), `working`,
 * `done`, `failed`.
 */
export const startDialog = $state({
  open: false,
  /** @type {'setup'|'join'} */
  variant: 'setup',
  /** @type {'ask'|'working'|'done'|'failed'} */
  phase: 'ask',
  /** The data folder the library goes into. */
  target: '',
  /** @type {import('../api/backend.js').DataFolderPlan|null} */
  plan: null,
  /** The plan is being read. */
  planning: false,
  /** The reason a setup failed, in plain words. @type {string|null} */
  error: null,
  /** The failed setup left everything as it was. */
  nothingChanged: false,
  /** The copy in flight. */
  progress: { copied: 0, total: 0 },
})

/** What the start decided, once per launch. */
export const startFlow = $state({
  /** The start flow has run this launch. */
  ran: false,
  /** Row C: the first-launch dialog carries the Join step. */
  joinStep: false,
})

/** The stored answers, read against the mode now. */
export function startAnswers() {
  return readStartAnswers(session.dataFolderPromptAnswered, dataFolder.status?.mode)
}

/**
 * Run the start table once, after the first status read: a quiet setup
 * (row E), the Join step (row C), or the dialog (rows D and F), each only
 * when its question has no stored answer.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 */
export async function runStartFlow(backend = getBackend()) {
  if (startFlow.ran) return
  startFlow.ran = true
  // An empty `Cleaner/` this app already set up or joined is filled, not
  // asked about (contract 5.1).
  if (fillsEmptySubfolder(dataFolder.status, session.dataFolderPromptAnswered) && typeof backend.dataFolderFillEmpty === 'function') {
    try {
      dataFolder.status = await backend.dataFolderFillEmpty()
      dataFolder.generation += 1
    } catch (error) {
      console.error('the empty Cleaner folder could not be filled', error)
    }
    if (dataFolder.status?.mode === 'shared') return
  }
  const answers = startAnswers()
  const row = startRow(dataFolder.status, { answers, firstLaunchDone: session.firstLaunchOffered })
  const action = startAction(row, answers)
  if (!action) return
  if (action.kind === 'join') {
    startFlow.joinStep = true
    return
  }
  if (action.kind === 'dialog') {
    await openStartDialog(action.variant, backend)
    return
  }
  if (typeof backend.dataFolderQuietSetup !== 'function') return
  try {
    const outcome = await backend.dataFolderQuietSetup()
    dataFolder.status = outcome.status
    if (!outcome.done) return
    dataFolder.generation += 1
    setStartAnswer('setup', 'accepted', 'shared')
    notify({
      key: 'notice.dataFolder.quietSetup',
      params: { path: shownPath(outcome.status.root, outcome.status.home) },
    })
  } catch (error) {
    console.error('the quiet setup could not run', error)
  }
}

/**
 * Open the dialog. Set up proposes `~/Manga Library`; Join names the folder
 * Pear already uses and offers no other.
 *
 * @param {'setup'|'join'} variant
 * @param {import('../api/backend.js').Backend} [backend]
 */
export async function openStartDialog(variant, backend = getBackend()) {
  startDialog.open = true
  startDialog.variant = variant
  startDialog.phase = 'ask'
  startDialog.error = null
  startDialog.nothingChanged = false
  startDialog.progress = { copied: 0, total: 0 }
  startDialog.target = (variant === 'join' ? dataFolder.status?.root : dataFolder.status?.defaultRoot) ?? ''
  dataFolder.error = null
  dataFolder.confirm = null
  await planStartDialog(backend)
}

/**
 * Change where Set up puts the folder ("Change…"), and read the plan again.
 *
 * @param {string} target
 * @param {import('../api/backend.js').Backend} [backend]
 */
export async function setStartTarget(target, backend = getBackend()) {
  if (startDialog.variant !== 'setup' || startDialog.phase === 'working') return
  startDialog.target = target
  await planStartDialog(backend)
}

/** @param {import('../api/backend.js').Backend} backend */
async function planStartDialog(backend) {
  if (typeof backend.dataFolderPlan !== 'function') return
  startDialog.planning = true
  const asked = startDialog.target
  try {
    const plan = await backend.dataFolderPlan({ target: startDialog.variant === 'join' ? null : asked })
    if (startDialog.target === asked) startDialog.plan = plan
  } catch (error) {
    startDialog.plan = null
    startDialog.error = message(error)
  } finally {
    startDialog.planning = false
  }
}

/**
 * Whether the dialog may not go ahead now: Pear is open in the folder a
 * Join would move into ("Close Pear first."). A drive that cannot tell is
 * not blocked: the move asks the user to confirm instead.
 */
export function startBlocked() {
  return startDialog.variant === 'join' && dataFolder.status?.typesetterRunning === 'yes'
}

/** "Check again": read the status, which says whether Pear still runs. */
export async function checkStartAgain(backend = getBackend()) {
  await loadDataFolder(backend)
}

/**
 * "Use one folder": run the setup or the Join, the copy's progress polled
 * meanwhile. Done, it stores the answer as accepted; a drive that cannot
 * tell whether Pear runs puts the question back with the confirm.
 *
 * @param {{confirmedClosed?: boolean, backend?: import('../api/backend.js').Backend}} [options]
 */
export async function acceptStart({ confirmedClosed = false, backend = getBackend() } = {}) {
  if (startDialog.phase === 'working' || startBlocked()) return false
  const before = dataFolder.status?.mode ?? null
  startDialog.phase = 'working'
  startDialog.error = null
  startDialog.progress = { copied: 0, total: 0 }
  const timer = globalThis.setInterval(() => void pollProgress(backend), PROGRESS_POLL_MS)
  let done = false
  try {
    done = await changeDataFolder('setup', startDialog.target, { confirmedClosed, backend })
  } finally {
    globalThis.clearInterval(timer)
  }
  if (done) {
    setStartAnswer(startDialog.variant, 'accepted', 'shared')
    startDialog.phase = 'done'
    return true
  }
  if (dataFolder.confirm) {
    startDialog.phase = 'ask'
    return false
  }
  startDialog.phase = 'failed'
  startDialog.error = dataFolder.error
  startDialog.nothingChanged = (dataFolder.status?.mode ?? null) === before
  return false
}

/** @param {import('../api/backend.js').Backend} backend */
async function pollProgress(backend) {
  if (startDialog.phase !== 'working' || typeof backend.dataFolderProgress !== 'function') return
  try {
    startDialog.progress = await backend.dataFolderProgress()
  } catch {
    // The next tick asks again.
  }
}

/** "Not now": stored as declined, never asked again. */
export function declineStart() {
  if (startDialog.phase === 'working') return
  setStartAnswer(startDialog.variant, 'declined', dataFolder.status?.mode ?? null)
  closeStartDialog()
}

export function closeStartDialog() {
  if (startDialog.phase === 'working') return
  startDialog.open = false
  dataFolder.confirm = null
}

/**
 * The first-run Join step's "Use the same folder" (row C). Nothing to move on
 * a new install: the backend makes `Cleaner/` and its index.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 */
export async function joinFromFirstRun(backend = getBackend()) {
  const root = dataFolder.status?.root
  if (!root) return false
  const done = await changeDataFolder('setup', root, { backend })
  if (done) setStartAnswer('join', 'accepted', 'shared')
  return done
}

/** The first-run Join step's "Keep separate". */
export function keepSeparate() {
  setStartAnswer('join', 'declined', dataFolder.status?.mode ?? null)
}

/** For tests. */
export function resetDataFolder() {
  startFlow.ran = false
  startFlow.joinStep = false
  startDialog.open = false
  startDialog.phase = 'ask'
  startDialog.plan = null
  startDialog.error = null
  startDialog.target = ''
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
