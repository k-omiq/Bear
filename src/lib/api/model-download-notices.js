import { notify } from '../state/app.svelte.js'
import { RUNTIME_ID, RUNTIME_LABEL_KEY } from '../dialogs/firstlaunch.js'
import { MODELS, model, modelOfFile } from '../model/pipelines.js'
import { settingsLinkForModel } from '../dialogs/settingslinks.js'
import { getBackend } from './backend.js'

/**
 * The Settings section on screen while Settings is open, and null otherwise.
 * Written by the dialog as its tab changes, read once when a failure arrives,
 * so it needs no reactivity.
 *
 * @type {string|null}
 */
let settingsSection = null

/** @param {string|null} id - the Settings tab id now shown, or null when Settings closes */
export function showSettingsSection(id) {
  settingsSection = id
}

/**
 * The Settings section that lists a download's row, and with it the row's
 * inline error. The runtime is Performance's. Every catalogue file and file
 * group, and a file the table does not know yet, is listed in Models
 * (`dialogs/settingslinks.js#settingsLinkForModel` names the row).
 *
 * @param {string|null|undefined} id
 * @returns {string|null} a Settings section id
 */
export function settingsSectionOf(id) {
  if (typeof id !== 'string' || !id) return null
  return settingsLinkForModel(id).section
}

/**
 * Whether a screen already shows a failure inline, so a notice would say it
 * twice: the setup screen's download step for a file it is fetching, or
 * Settings on top **and on the section with that download's row**. Settings
 * over General, Cloud or Shortcuts shows nothing of it.
 *
 * @param {{id?: string}|null|undefined} event
 * @param {{firstLaunch?: {open: boolean, step: string, queue: string[]}|null, topModal?: string|null}} screen
 * @returns {boolean}
 */
export function downloadFailureShown(event, { firstLaunch = null, topModal = null } = {}) {
  const id = event?.id
  if (typeof id !== 'string' || !id) return false
  const members = MODELS.find((entry) => entry.group === id)?.files ?? []
  const setup = Boolean(firstLaunch?.open && firstLaunch.step === 'downloads' &&
    (firstLaunch.queue.includes(id) || members.some((member) => firstLaunch.queue.includes(member))))
  const settings = topModal === 'settings' && settingsSection !== null && settingsSection === settingsSectionOf(id)
  return setup || settings
}

/**
 * The name Settings gives a download, as an i18n key, from the id its
 * `model-progress` events carry.
 *
 * No table of its own. A download is either one catalogue file or a file group
 * (`scriptGate`, `mangaOcr`), and either way Settings lists it under the
 * logical model `model/pipelines.js#MODELS` files it under, by that model's
 * `nameKey`; the runtime has the label Settings and the first-launch plan
 * share. A file this build's table does not know yet falls back to the
 * catalogue's own `kindKey` - the key the native side chose for it.
 *
 * @param {string|null|undefined} id
 * @param {{listModels: () => Promise<import('./backend.js').ModelsView>}|null} [backend]
 * @returns {Promise<string|null>} null when nothing names it
 */
export async function downloadNameKey(id, backend = getBackend()) {
  if (typeof id !== 'string' || !id) return null
  if (id === RUNTIME_ID) return RUNTIME_LABEL_KEY
  // A file first: `scriptGate` is both a group and one of its files, and both
  // readings land on the same model.
  const logical = modelOfFile(id) ?? model(id)
  if (logical?.nameKey) return logical.nameKey
  try {
    const view = await backend?.listModels()
    const row = view?.models?.find((candidate) => candidate.id === id)
    return typeof row?.kindKey === 'string' && row.kindKey ? row.kindKey : null
  } catch {
    return null
  }
}

/**
 * Send an event-channel model failure to the app notice stack unless a live
 * download screen is already showing its inline error.
 *
 * The notice names the download the way Settings does rather than by its id
 * (`mangaOcr` reads as the Japanese OCR rescue). The decision to raise it is
 * made at once, while `covered` is still true to the screen the event arrived
 * over; only a file the table does not know costs a catalogue read first.
 *
 * @param {{type?: string, done?: boolean, error?: string|null, id?: string}|null|undefined} event
 * @param {{covered?: boolean, backend?: {listModels: () => Promise<import('./backend.js').ModelsView>}|null}} [options]
 * @returns {Promise<boolean>} whether a notice was queued. Never rejects.
 */
export async function reportModelDownloadFailure(event, { covered = false, backend } = {}) {
  if (event?.type !== 'model-progress' || !event.done || !event.error || event.error === 'cancelled' || covered) return false
  // The copy puts a full stop after the reason; one the backend already ended
  // on would print twice.
  const error = String(event.error).trim().replace(/[.\s]+$/, '')
  const nameKey = await downloadNameKey(event.id, backend === undefined ? getBackend() : backend)
  // No `icon`: a warn notice draws `warning-triangle` by itself. The name this
  // used to pass, `alert-triangle`, is not in the icon set, and `Icon` throws
  // on an unknown name, which took the whole notice stack down with it.
  notify({
    key: nameKey ? 'notice.download.failed' : 'notice.download.failedUnnamed',
    params: nameKey ? { nameKey, error } : { error },
    tone: 'warn',
    duration: 9000,
  })
  return true
}
