/**
 * Page denoise: the six presets, where they can run, and how long a page takes.
 *
 * The presets and their cloud timings are `denoise-presets.json`, which a
 * Python test also reads, so ids and recipes are never restated here. What
 * this module adds is the interface's view of that table: the presets a
 * target offers, the one a stored choice falls back to, and the time per page
 * each place would take.
 *
 * **Times are measured, never guessed.** Cloud seconds are the table's own,
 * taken on the GPU it names for a page of the size it names, and scaled by
 * area when a chapter's page sizes are known. Local seconds come only from
 * `benchmark_denoise_local` on this computer (`denoiseLocalSecondsPerPage`);
 * until that has run, a local time is `null` and every surface says it has
 * not been measured.
 *
 * Pure, so every rule here is tested without a DOM.
 */

import TABLE from './denoise-presets.json'

/** Where denoise runs, as `denoiseTarget` stores it. */
export const DENOISE_TARGETS = Object.freeze(['local', 'cloud', 'off'])

/** @typedef {'local'|'cloud'|'off'} DenoiseTarget */

/**
 * @typedef {Object} DenoisePreset
 * @property {string} id
 * @property {Array<'local'|'cloud'>} targets
 * @property {{schema: number, steps: Array<Object>}} recipe
 * @property {number} cloudSecondsPerPage
 * @property {number} cloudSecondsPerMegapixel
 * @property {{name: string, author: string, license: string}} credit
 */

/** @type {readonly DenoisePreset[]} */
export const DENOISE_PRESETS = Object.freeze(TABLE.presets.map((preset) => Object.freeze(preset)))

/** What the cloud timings were measured on: the GPU and the reference page. */
export const DENOISE_REFERENCE = Object.freeze({
  gpu: TABLE.measuredOn.gpu,
  width: TABLE.measuredOn.pageWidth,
  height: TABLE.measuredOn.pageHeight,
})

/**
 * Each preset's name and one-line note, as whole keys so the catalogue test
 * sees every one of them.
 */
export const PRESET_TEXT = Object.freeze({
  'mangajanai-2x': { nameKey: 'denoise.preset.mangajanai2x.name', noteKey: 'denoise.preset.mangajanai2x.note' },
  'mangajanai-4x': { nameKey: 'denoise.preset.mangajanai4x.name', noteKey: 'denoise.preset.mangajanai4x.note' },
  'waifu2x-scan-4x-n2': { nameKey: 'denoise.preset.waifu2xScan.name', noteKey: 'denoise.preset.waifu2xScan.note' },
  'realcugan-2x-conservative': { nameKey: 'denoise.preset.realcugan2x.name', noteKey: 'denoise.preset.realcugan2x.note' },
  'realcugan-3x-conservative': { nameKey: 'denoise.preset.realcugan3x.name', noteKey: 'denoise.preset.realcugan3x.note' },
  'realcugan-3x-denoise3': { nameKey: 'denoise.preset.realcugan3xStrong.name', noteKey: 'denoise.preset.realcugan3xStrong.note' },
})

/** The catalogue feature the local denoise packages are required by (`weights.rs`). */
export const PAGE_DENOISE_FEATURE = 'pageDenoise'

/** @param {unknown} value @returns {DenoiseTarget} */
export function normalizeDenoiseTarget(value) {
  return value === 'local' || value === 'cloud' ? value : 'off'
}

/**
 * The presets a target offers, in table order. `known` is the backend's own
 * list (`denoise_presets`), when it answered: a preset it does not know is
 * left out rather than offered and refused. An empty or missing list means the
 * backend did not say, and the table stands.
 *
 * @param {DenoiseTarget|string} target
 * @param {Array<{id: string}>|null} [known]
 * @returns {DenoisePreset[]}
 */
export function presetsFor(target, known = null) {
  if (target !== 'local' && target !== 'cloud') return []
  const ids = Array.isArray(known) && known.length ? new Set(known.map((entry) => entry?.id)) : null
  return DENOISE_PRESETS.filter((preset) => preset.targets.includes(target) && (!ids || ids.has(preset.id)))
}

/** @param {string} id @returns {DenoisePreset|null} */
export function presetById(id) {
  return DENOISE_PRESETS.find((preset) => preset.id === id) ?? null
}

/**
 * The preset a stored choice resolves to for a target: the stored one when
 * the target offers it, else the target's first. `null` for `off`.
 *
 * @param {DenoiseTarget|string} target
 * @param {unknown} stored
 * @param {Array<{id: string}>|null} [known]
 * @returns {string|null}
 */
export function validPreset(target, stored, known = null) {
  const offered = presetsFor(target, known)
  if (!offered.length) return null
  return offered.find((preset) => preset.id === stored)?.id ?? offered[0].id
}

/**
 * A stored local measurement, or null when there is none worth showing.
 *
 * @param {unknown} value
 * @returns {number|null}
 */
export function localSeconds(value) {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : null
}

/**
 * Read `benchmark_denoise_local`'s answer. The command answers seconds per
 * page, bare or as `{secondsPerPage}`.
 *
 * @param {unknown} answer
 * @returns {number|null}
 */
export function benchmarkSeconds(answer) {
  if (typeof answer === 'number') return localSeconds(answer)
  const record = answer && typeof answer === 'object' ? /** @type {Record<string, unknown>} */ (answer) : {}
  return localSeconds(record.secondsPerPage ?? record.seconds_per_page)
}

/**
 * The area of each page that has a size, in megapixels. `null` when any page
 * lacks one: an estimate from some pages would be a figure for a different
 * chapter.
 *
 * @param {Array<{width?: number, height?: number}>|null|undefined} pages
 * @returns {number[]|null}
 */
export function pageMegapixels(pages) {
  const list = Array.isArray(pages) ? pages : []
  if (!list.length) return null
  const areas = list.map((page) => {
    const width = Number(page?.width)
    const height = Number(page?.height)
    return width > 0 && height > 0 ? (width * height) / 1e6 : null
  })
  return areas.every((area) => area !== null) ? /** @type {number[]} */ (areas) : null
}

/**
 * How long a chapter would take, and how the figure was reached.
 *
 * - **cloud**: the preset's seconds per megapixel over the chapter's page
 *   areas when every page has a size (`basis: 'pages'`), else the reference
 *   page's time for each page (`basis: 'reference'`).
 * - **local**: the measured seconds per page times the page count
 *   (`basis: 'measured'`), or `null` seconds when nothing was measured.
 *
 * GPU time only: a cold start and the upload are not in it, and the dialog
 * says so.
 *
 * @param {{target: DenoiseTarget|string, preset: DenoisePreset|null, pages: Array<{width?: number, height?: number}>, localPerPage?: unknown}} spec
 * @returns {{seconds: number|null, perPage: number|null, basis: 'pages'|'reference'|'measured'|'unmeasured'|'none'}}
 */
export function chapterEstimate({ target, preset, pages, localPerPage = null }) {
  const count = Array.isArray(pages) ? pages.length : 0
  if (!preset || !count) return { seconds: null, perPage: null, basis: 'none' }
  if (target === 'cloud') {
    const areas = pageMegapixels(pages)
    if (areas) {
      const seconds = areas.reduce((sum, area) => sum + area * preset.cloudSecondsPerMegapixel, 0)
      return { seconds, perPage: seconds / count, basis: 'pages' }
    }
    return { seconds: preset.cloudSecondsPerPage * count, perPage: preset.cloudSecondsPerPage, basis: 'reference' }
  }
  if (target === 'local') {
    const perPage = localSeconds(localPerPage)
    return perPage === null
      ? { seconds: null, perPage: null, basis: 'unmeasured' }
      : { seconds: perPage * count, perPage, basis: 'measured' }
  }
  return { seconds: null, perPage: null, basis: 'none' }
}

const tenths = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 })

/**
 * A duration as a whole key and its parameters: seconds with one decimal
 * under a minute, whole minutes under an hour, then hours and minutes.
 *
 * @param {number} seconds
 * @returns {{key: string, params: Record<string, string|number>}}
 */
export function durationText(seconds) {
  const value = Math.max(0, Number(seconds) || 0)
  if (value < 60) return { key: 'denoise.duration.seconds', params: { value: tenths.format(value) } }
  const minutes = Math.round(value / 60)
  if (minutes < 60) return { key: 'denoise.duration.minutes', params: { value: minutes } }
  return { key: 'denoise.duration.hours', params: { hours: Math.floor(minutes / 60), minutes: minutes % 60 } }
}

/**
 * The catalogue rows the local presets download: every row required by
 * `pageDenoise`, in catalogue order.
 *
 * @param {Array<{id: string, requiredBy?: string[]}>|null|undefined} models
 */
export function denoiseRows(models) {
  return (Array.isArray(models) ? models : []).filter((row) => row?.requiredBy?.includes(PAGE_DENOISE_FEATURE))
}

/**
 * Where denoised pages go by default: a `denoised` folder (or `name`) inside
 * the chapter's own, written with the chapter path's own separator. Empty
 * when the chapter has no folder to put it in; the dialog then asks for one.
 *
 * @param {string|null|undefined} sourcePath
 * @param {string} [name]
 */
export function defaultOutDir(sourcePath, name = 'denoised') {
  const path = typeof sourcePath === 'string' ? sourcePath.trim() : ''
  if (!path) return ''
  const separator = path.includes('\\') && !path.includes('/') ? '\\' : '/'
  return `${path.replace(/[\\/]+$/, '')}${separator}${name}`
}
