/**
 * Send to Pear (`docs/shared-data-folder.md` section 8.4.1): the title list,
 * and the background send it starts.
 *
 * The list comes from the backend (`send_to_pear.rs#titles`), which knows
 * which chapters have a cleaned page and which were sent before. A title is
 * checked by default when it has at least one cleaned chapter; one with none
 * is unchecked and says "Nothing cleaned yet". A title with a sent chapter
 * says "Sent before" and sends only its changed and never-sent chapters,
 * unless "Send all again" is on.
 *
 * The send runs in the backend, one chapter at a time. This store polls its
 * progress while it runs, for "Sending 40 of 63 chapters" and Stop, which
 * outlive the dialog that started it (`shell/SendToPearStatus.svelte`). The
 * closing notice comes from the backend.
 */

import { getBackend } from '../api/backend.js'
import { pushModal } from './app.svelte.js'

/** How often a running send is asked how far it has come. */
export const SEND_POLL_MS = 500

export const sendToPear = $state({
  /** @type {import('../api/backend.js').SendTitle[]|null} */
  titles: null,
  loading: false,
  /** @type {string|null} */
  error: null,
  /** Project id → checked. @type {Record<string, boolean>} */
  checked: {},
  /** "Send all again": every cleaned chapter, sent before or not. */
  again: false,
  /** @type {import('../api/backend.js').SendProgress|null} */
  progress: null,
})

/** @type {ReturnType<typeof setTimeout>|null} */
let poll = null
/** The backend the running send was started on, so Stop reaches the same one. @type {import('../api/backend.js').Backend|null} */
let sendBackend = null

/** @param {unknown} error */
const message = (error) => (error instanceof Error ? error.message : String(error ?? ''))

/** Open the title list over whatever is up. */
export function openSendToPear() {
  pushModal({ kind: 'sendToPear', titleKey: 'sendToPear.title' })
}

/**
 * Read the title list, each title checked by default when it has a cleaned
 * chapter.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 */
export async function loadSendTitles(backend = getBackend()) {
  sendToPear.loading = true
  sendToPear.error = null
  try {
    const titles = await backend.sendToPearTitles()
    sendToPear.titles = titles
    sendToPear.checked = Object.fromEntries(titles.map((title) => [title.projectId, title.cleaned > 0]))
  } catch (error) {
    sendToPear.titles = null
    sendToPear.error = message(error)
  } finally {
    sendToPear.loading = false
  }
}

/**
 * Whether a title can be sent at all, and whether it is checked.
 *
 * @param {import('../api/backend.js').SendTitle} title
 */
export function titleSendable(title) {
  return title.cleaned > 0
}

/** The titles the send would take. */
export function chosenTitles() {
  return (sendToPear.titles ?? []).filter((title) => titleSendable(title) && sendToPear.checked[title.projectId])
}

/**
 * How many chapters the send would write: every cleaned chapter with
 * "Send all again", else the ones never sent or changed since.
 */
export function chosenChapters() {
  return chosenTitles().reduce((sum, title) => sum + (sendToPear.again ? title.cleaned : title.pending), 0)
}

/** @param {string} projectId @param {boolean} on */
export function checkTitle(projectId, on) {
  sendToPear.checked = { ...sendToPear.checked, [projectId]: on }
}

export function sendRunning() {
  return sendToPear.progress?.running === true
}

/**
 * Start the send of the checked titles. Answers whether it started.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 */
export async function startSend(backend = getBackend()) {
  if (sendRunning()) return false
  const projectIds = chosenTitles().map((title) => title.projectId)
  if (projectIds.length === 0) return false
  sendToPear.error = null
  sendBackend = backend
  try {
    sendToPear.progress = await backend.sendToPearStart({ projectIds, again: sendToPear.again })
  } catch (error) {
    sendToPear.error = message(error)
    return false
  }
  schedule(backend)
  return true
}

/** Stop after the chapter being written. @param {import('../api/backend.js').Backend} [backend] */
export async function stopSend(backend = sendBackend ?? getBackend()) {
  try {
    sendToPear.progress = await backend.sendToPearStop()
  } catch (error) {
    sendToPear.error = message(error)
  }
}

/** @param {import('../api/backend.js').Backend} backend */
function schedule(backend) {
  if (poll !== null) return
  poll = setTimeout(() => void follow(backend), SEND_POLL_MS)
}

/** @param {import('../api/backend.js').Backend} backend */
async function follow(backend) {
  poll = null
  try {
    sendToPear.progress = await backend.sendToPearProgress()
  } catch {
    // Asked again on the next tick.
  }
  if (sendRunning()) schedule(backend)
}

/** For tests. */
export function resetSendToPear() {
  if (poll !== null) clearTimeout(poll)
  poll = null
  sendBackend = null
  sendToPear.titles = null
  sendToPear.loading = false
  sendToPear.error = null
  sendToPear.checked = {}
  sendToPear.again = false
  sendToPear.progress = null
}
