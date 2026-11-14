/**
 * Going back to an earlier released version: the rules the dialog follows.
 *
 * The facts come from the backend (`src-tauri/src/rollback.rs`): which
 * versions were released, the oldest one it is safe to go back to, the chapter
 * format each one opens, and whether this copy can be replaced in place. This
 * module orders them, decides what each row may do, and keeps the one thing
 * the app has to remember across the restart: the version the user left, so
 * the update badge does not offer it straight back.
 *
 * Plain JavaScript, no Svelte, so it is tested in the `node` environment.
 */

import { isTauri } from './api/tauri.js'
import { readRecord, writeRecord, defaultStorage } from './state/persist.js'

/**
 * @typedef {Object} RollbackEntry
 * @property {string} version
 * @property {string|null} [date]
 * @property {string|null} [notes]
 * @property {string} pageUrl
 * @property {number} readsFormat
 * @property {boolean} honoursSkip
 */

/**
 * @typedef {Object} RollbackCatalog
 * @property {string} currentVersion
 * @property {string} bundle
 * @property {boolean} inPlace
 * @property {string} floor
 * @property {number} manifestFormat
 * @property {RollbackEntry[]} versions
 */

/**
 * @typedef {RollbackEntry & {
 *   state: 'installed'|'available'|'belowFloor'|'pageOnly',
 *   loses: boolean,
 * }} RollbackRow
 */

/**
 * `1.0.0-beta.3` as parts, or `null`. A leading `v` is allowed.
 *
 * @param {unknown} raw
 */
export function parseVersion(raw) {
  if (typeof raw !== 'string') return null
  const m = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(raw.trim())
  if (!m) return null
  return { core: [Number(m[1]), Number(m[2]), Number(m[3])], pre: m[4] ? m[4].split('.') : [] }
}

/**
 * Semver order: `-1`, `0` or `1`. A prerelease sorts before its release, and
 * numeric prerelease parts compare as numbers, so `beta.9` is older than
 * `beta.10`. Something that is not a version sorts before every version.
 *
 * @param {string} a
 * @param {string} b
 */
export function compareVersions(a, b) {
  const x = parseVersion(a)
  const y = parseVersion(b)
  if (!x || !y) return x ? 1 : y ? -1 : 0
  for (let i = 0; i < 3; i++) {
    if (x.core[i] !== y.core[i]) return x.core[i] < y.core[i] ? -1 : 1
  }
  if (!x.pre.length || !y.pre.length) return x.pre.length === y.pre.length ? 0 : x.pre.length ? -1 : 1
  for (let i = 0; i < Math.max(x.pre.length, y.pre.length); i++) {
    const p = x.pre[i]
    const q = y.pre[i]
    if (p === undefined) return -1
    if (q === undefined) return 1
    if (p === q) continue
    const pn = /^\d+$/.test(p)
    const qn = /^\d+$/.test(q)
    if (pn && qn) return Number(p) < Number(q) ? -1 : 1
    if (pn !== qn) return pn ? -1 : 1
    return p < q ? -1 : 1
  }
  return 0
}

/** @param {string} v */
export function displayVersion(v) {
  return v ? (v.startsWith('v') ? v : `v${v}`) : ''
}

/**
 * The rows the dialog lists: the installed version and every older release,
 * newest first. A version newer than the installed one is an update, not a
 * rollback, and the update dialog offers it.
 *
 * - `installed`: marked, not chosen.
 * - `belowFloor`: listed so the user sees why it is missing, not chosen.
 * - `pageOnly`: this copy cannot be replaced in place (a `.deb` install, or a
 *   development build), so the row opens that version's download page.
 * - `available`: chosen to go back to.
 *
 * `loses` is true when the version opens an older chapter format than this
 * build writes: chapters saved in the newer format will not open in it.
 *
 * @param {RollbackCatalog} catalog
 * @returns {RollbackRow[]}
 */
export function rollbackRows(catalog) {
  const current = catalog?.currentVersion ?? ''
  const seen = new Set()
  /** @type {RollbackRow[]} */
  const rows = []
  for (const entry of catalog?.versions ?? []) {
    if (!parseVersion(entry?.version) || seen.has(entry.version)) continue
    seen.add(entry.version)
    const order = compareVersions(entry.version, current)
    if (order > 0) continue
    /** @type {RollbackRow['state']} */
    let state = 'available'
    if (order === 0) state = 'installed'
    else if (compareVersions(entry.version, catalog.floor) < 0) state = 'belowFloor'
    else if (!catalog.inPlace) state = 'pageOnly'
    rows.push({ ...entry, state, loses: Number(entry.readsFormat) < Number(catalog.manifestFormat) })
  }
  if (!seen.has(current) && parseVersion(current)) {
    rows.push({
      version: current,
      date: null,
      notes: null,
      pageUrl: '',
      readsFormat: catalog.manifestFormat,
      honoursSkip: true,
      state: 'installed',
      loses: false,
    })
  }
  return rows.sort((a, b) => compareVersions(b.version, a.version))
}

/**
 * The first line or so of a release note, for the list. The whole note is
 * shown once the version is chosen.
 *
 * @param {string|null|undefined} notes
 * @param {number} [max]
 */
export function shortNotes(notes, max = 140) {
  const text = String(notes ?? '').replace(/\s+/g, ' ').trim()
  if (text.length <= max) return text
  const cut = text.slice(0, max)
  const space = cut.lastIndexOf(' ')
  return `${(space > max / 2 ? cut.slice(0, space) : cut).trimEnd()}…`
}

/* ------------------------------------------------------------------ */
/* The skipped version                                                 */
/* ------------------------------------------------------------------ */

const SKIP_KEY = 'updateSkip.v1'

/**
 * The version the user went back from, or `''`.
 *
 * @param {Storage|null} [store]
 */
export function skippedVersion(store = defaultStorage()) {
  const record = readRecord(SKIP_KEY, {}, store)
  const version = /** @type {any} */ (record).version
  return parseVersion(version) ? version : ''
}

/**
 * Remember the version being left. Answers what was there before, so a
 * failed install can put it back.
 *
 * @param {string} version
 * @param {Storage|null} [store]
 */
export function rememberSkipped(version, store = defaultStorage()) {
  const before = skippedVersion(store)
  writeRecord(SKIP_KEY, parseVersion(version) ? { version } : {}, store)
  return before
}

/** @param {Storage|null} [store] */
export function clearSkipped(store = defaultStorage()) {
  writeRecord(SKIP_KEY, {}, store)
}

/**
 * Whether the update found at start may show its badge. A version at or below
 * the one the user went back from is not offered on its own; a newer release
 * is, since it may fix what made them leave. Once the installed version has
 * reached the skipped one (an update on purpose, or an installer run by hand)
 * the skip is over and is forgotten.
 *
 * "Check for updates" in Settings never asks this: that is updating on
 * purpose.
 *
 * @param {string|null|undefined} updateVersion
 * @param {string} installedVersion
 * @param {Storage|null} [store]
 */
export function shouldOfferUpdate(updateVersion, installedVersion, store = defaultStorage()) {
  if (!updateVersion) return false
  const skipped = skippedVersion(store)
  if (!skipped) return true
  if (parseVersion(installedVersion) && compareVersions(installedVersion, skipped) >= 0) {
    clearSkipped(store)
    return true
  }
  return compareVersions(updateVersion, skipped) > 0
}

/* ------------------------------------------------------------------ */
/* The update handle                                                   */
/* ------------------------------------------------------------------ */

/**
 * The backend's `rollbackPrepare` answer as something `installUpdate` takes.
 * In the app it is the updater plugin's metadata, and the plugin's own
 * `Update` is built from it, so download, signature check and install are the
 * plugin's. The mock answers an object that already behaves like one.
 *
 * @param {any} prepared
 */
export async function asUpdate(prepared) {
  if (!prepared || !isTauri() || typeof prepared.downloadAndInstall === 'function') return prepared
  const { Update } = await import('@tauri-apps/plugin-updater')
  return new Update(prepared)
}

/**
 * The `rollback_*` code a backend refusal starts with, or `''`.
 *
 * @param {unknown} error
 */
export function rollbackErrorCode(error) {
  const text = String(/** @type {any} */ (error)?.message ?? error ?? '')
  const match = /\brollback_([a-z_]+)\b/.exec(text)
  if (match) return match[1]
  if (/signature/i.test(text)) return 'signature'
  return ''
}

/**
 * The message key for a refusal. Every one is literal so the catalogue test
 * sees it. A code without a key shows the backend's own words, as the
 * update dialog does.
 */
export const ROLLBACK_ERROR_KEYS = Object.freeze({
  offline: 'update.rollback.error.offline',
  unreachable: 'update.rollback.error.unreachable',
  platform: 'update.rollback.error.platform',
  in_place_unsupported: 'update.rollback.error.inPlace',
  below_floor: 'update.rollback.error.belowFloor',
  not_older: 'update.rollback.error.notOlder',
  network: 'update.rollback.error.network',
  signature: 'update.rollback.error.signature',
})

/**
 * Whether a refusal leaves the download page as the way forward.
 *
 * @param {string} code
 */
export function offersPage(code) {
  return code === 'unreachable' || code === 'platform' || code === 'in_place_unsupported'
}
