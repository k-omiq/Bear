/**
 * What the app does about the shared data folder at start
 * (`docs/shared-data-folder.md` section 5.1), as pure functions, so every row
 * of the table is pinned without mounting anything.
 *
 * | # | State at start                                         | What the app does            |
 * |---|--------------------------------------------------------|------------------------------|
 * | B | A move is unfinished (its journal is there)            | Unavailable; recovery runs   |
 * | A | Pointer good, `Cleaner/` holds its library             | Shared, nothing to ask       |
 * | C | Pointer good, no `Cleaner/` yet, first launch          | First-run step "Join"        |
 * | D | Pointer good, no `Cleaner/` yet, used before (Pending) | The dialog, Join variant     |
 * | E | No pointer, Pear never ran here                        | Quiet setup                  |
 * | F | No pointer, Pear ran here                              | The dialog, Set up variant   |
 *
 * The backend settles which mode the folder is in and whether Pear has run
 * (`data_folder.rs`); what only this side knows is the stored answers
 * (`session.dataFolderPromptAnswered`) and whether the first-launch dialog
 * is done (`session.firstLaunchOffered`).
 */

/** @typedef {'accepted'|'declined'} AnswerHow */
/** @typedef {{setup?: AnswerHow, join?: AnswerHow}} StartAnswers */
/** @typedef {'A'|'B'|'C'|'D'|'E'|'F'|null} StartRow */

/** The rate the time estimate uses (contract 5.4): 20 MB/s. */
export const COPY_BYTES_PER_SECOND = 20_000_000

/**
 * The stored answers, whatever shape they were stored in (contract 5.3).
 * Version 1 kept one boolean: `true` was "Not now" or a Move that worked,
 * told apart by whether the library is in the shared folder now; `false`
 * was no answer.
 *
 * @param {unknown} raw
 * @param {string|null|undefined} mode - the data folder's mode now
 * @returns {StartAnswers}
 */
export function readStartAnswers(raw, mode) {
  if (raw === true) return { setup: mode === 'shared' ? 'accepted' : 'declined' }
  if (!raw || typeof raw !== 'object') return {}
  /** @type {StartAnswers} */
  const out = {}
  for (const question of /** @type {const} */ (['setup', 'join'])) {
    const how = /** @type {Record<string, unknown>} */ (raw)[question]
    if (how === 'accepted' || how === 'declined') out[question] = how
  }
  return out
}

/**
 * The one exception to "no subfolder yet" (contract 5.1): Pending with an
 * empty `Cleaner/`, and this app's stored answer says it already set up or
 * joined this folder. The app then fills the subfolder without asking.
 *
 * A version 1 `true` counts as accepted here: version 1 read an existing
 * `Cleaner/`, even an empty one, as Shared, and made it only on a setup.
 *
 * @param {import('../api/backend.js').DataFolderStatus|null|undefined} status
 * @param {unknown} raw - the stored answers, as kept
 */
export function fillsEmptySubfolder(status, raw) {
  if (status?.mode !== 'pending' || status.cleanerEmpty !== true) return false
  if (raw === true) return true
  const answers = readStartAnswers(raw, status.mode)
  return answers.setup === 'accepted' || answers.join === 'accepted'
}

/** @param {StartAnswers} answers */
export function hasAnyAnswer(answers) {
  return Boolean(answers.setup || answers.join)
}

/**
 * "First launch" (contract 5.1): this app has no title of its own yet, has
 * never stored a start answer, and its first-launch dialog is not done.
 *
 * @param {{titles?: number}|null|undefined} status
 * @param {StartAnswers} answers
 * @param {boolean} firstLaunchDone
 */
export function isFirstLaunch(status, answers, firstLaunchDone) {
  return !firstLaunchDone && !hasAnyAnswer(answers) && (status?.titles ?? 0) === 0
}

/**
 * The row of the start table this status lands in. `null` for an
 * Unavailable folder that is not an unfinished move (it stays as in
 * version 1) and for a status not read yet.
 *
 * @param {import('../api/backend.js').DataFolderStatus|null|undefined} status
 * @param {{answers: StartAnswers, firstLaunchDone: boolean}} facts
 * @returns {StartRow}
 */
export function startRow(status, { answers, firstLaunchDone }) {
  if (!status) return null
  if (status.journal) return 'B'
  if (status.mode === 'shared') return 'A'
  if (status.mode === 'pending') return isFirstLaunch(status, answers, firstLaunchDone) ? 'C' : 'D'
  if (status.mode === 'legacy') return status.sisterPresent ? 'F' : 'E'
  return null
}

/**
 * What the start does for a row, given what was answered before: `quiet`
 * runs the quiet setup, `join` adds the first-run step, `dialog` opens the
 * centered dialog in that variant, `null` does nothing. A declined question
 * is never asked again; Join is its own question, so a declined Set up does
 * not stop it. A quiet setup is not run over a stored Set up answer either:
 * a "Not now" is kept even where nothing would be asked.
 *
 * @param {StartRow} row
 * @param {StartAnswers} answers
 * @returns {{kind: 'quiet'}|{kind: 'join'}|{kind: 'dialog', variant: 'setup'|'join'}|null}
 */
export function startAction(row, answers) {
  if (row === 'E') return answers.setup ? null : { kind: 'quiet' }
  if (row === 'F') return answers.setup ? null : { kind: 'dialog', variant: 'setup' }
  if (row === 'C') return answers.join ? null : { kind: 'join' }
  if (row === 'D') return answers.join ? null : { kind: 'dialog', variant: 'join' }
  return null
}

/**
 * A path as the user is shown it: the Windows `\\?\` (or `\\?\UNC\`) prefix
 * a canonical path carries is dropped (contract section 11), and on macOS
 * and Linux the home folder reads as `~`.
 *
 * @param {string|null|undefined} path
 * @param {string|null|undefined} [home]
 */
export function shownPath(path, home) {
  if (typeof path !== 'string') return ''
  let out = path.startsWith('\\\\?\\UNC\\') ? `\\\\${path.slice(8)}` : path.startsWith('\\\\?\\') ? path.slice(4) : path
  const base = typeof home === 'string' ? home.replace(/\/+$/, '') : ''
  if (base.startsWith('/') && (out === base || out.startsWith(`${base}/`))) out = `~${out.slice(base.length)}`
  return out
}

/**
 * The minutes a copy of `bytes` takes at 20 MB/s, rounded up to whole
 * minutes, and never less than one.
 *
 * @param {number} bytes
 */
export function copyMinutes(bytes) {
  const seconds = Math.max(0, Number(bytes) || 0) / COPY_BYTES_PER_SECOND
  return Math.max(1, Math.ceil(seconds / 60))
}
