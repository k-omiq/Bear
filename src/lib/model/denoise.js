/**
 * Page denoise: the presets, where they can run, what pages they are made
 * for, how large a result they can give, and how long a page takes.
 *
 * The presets and their cloud timings are `denoise-presets.json`, which a
 * Python test also reads, so ids and recipes are never restated here. What
 * this module adds is the interface's view of that table: the presets a
 * target offers, the ones a page type shows, the one a stored choice falls
 * back to, the output sizes a preset offers, and the time per page each place
 * would take.
 *
 * **Times are measured, never guessed.** Cloud seconds are the table's own,
 * taken on the GPU it names for a page of the size it names, and scaled by
 * area when a chapter's page sizes are known. A preset the table has no
 * seconds for has not been timed, and every surface says so. Local seconds
 * come only from `benchmark_denoise_local` on this computer
 * (`denoiseLocalSecondsPerPage`); until that has run, a local time is `null`
 * and every surface says it has not been measured.
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
 * @property {'bw'|'color'|'any'} kind - the pages it is made for: black and white, colour, or both
 * @property {number} maxScale - the largest output scale it can give, 1 to 4; 1 keeps the page size
 * @property {Array<'local'|'cloud'>} targets
 * @property {{schema: number, steps: Array<Object>}} recipe
 * @property {number|null} cloudSecondsPerPage - null until it is measured
 * @property {number|null} cloudSecondsPerMegapixel - null until it is measured
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
  'digimanga-bw-4x': { nameKey: 'denoise.preset.digimangaBw4x.name', noteKey: 'denoise.preset.digimangaBw4x.note' },
  'mangajpeg-hq': { nameKey: 'denoise.preset.mangajpegHq.name', noteKey: 'denoise.preset.mangajpegHq.note' },
  'mangajpeg-lq': { nameKey: 'denoise.preset.mangajpegLq.name', noteKey: 'denoise.preset.mangajpegLq.note' },
  'waifu2x-scan-4x-n2': { nameKey: 'denoise.preset.waifu2xScan.name', noteKey: 'denoise.preset.waifu2xScan.note' },
  'waifu2x-scan-n1': { nameKey: 'denoise.preset.waifu2xScanLight.name', noteKey: 'denoise.preset.waifu2xScanLight.note' },
  'waifu2x-scan-n3': { nameKey: 'denoise.preset.waifu2xScanHeavy.name', noteKey: 'denoise.preset.waifu2xScanHeavy.note' },
  'realcugan-2x-conservative': { nameKey: 'denoise.preset.realcugan2x.name', noteKey: 'denoise.preset.realcugan2x.note' },
  'realcugan-3x-conservative': { nameKey: 'denoise.preset.realcugan3x.name', noteKey: 'denoise.preset.realcugan3x.note' },
  'realcugan-3x-denoise3': { nameKey: 'denoise.preset.realcugan3xStrong.name', noteKey: 'denoise.preset.realcugan3xStrong.note' },
  'realcugan-4x-conservative': { nameKey: 'denoise.preset.realcugan4x.name', noteKey: 'denoise.preset.realcugan4x.note' },
  'waifu2x-art-2x-n1': { nameKey: 'denoise.preset.waifu2xArtLight.name', noteKey: 'denoise.preset.waifu2xArtLight.note' },
  'waifu2x-art-2x-n3': { nameKey: 'denoise.preset.waifu2xArtStrong.name', noteKey: 'denoise.preset.waifu2xArtStrong.note' },
})

/** The page types the preset list is filtered by, in the order they are offered. */
export const PAGE_KINDS = Object.freeze(['bw', 'color'])

/** @typedef {'bw'|'color'} PageKind */

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
 * The presets shown for a page type, in the order given: those made for it
 * and those made for both. A preset that names no type is read as both.
 *
 * @template {{kind?: string}} T
 * @param {readonly T[]} presets
 * @param {PageKind|string} pages
 * @returns {T[]}
 */
export function presetsForPages(presets, pages) {
  return presets.filter((preset) => preset.kind === pages || preset.kind === 'any' || !preset.kind)
}

/**
 * The page type the list is filtered to. The user's pick stands while it
 * shows the chosen preset; otherwise the chosen preset decides: colour for a
 * colour preset, black and white for anything else. So the chosen preset is
 * always on screen, whoever chose it.
 *
 * @param {readonly {id: string, kind?: string}[]} presets
 * @param {string|null|undefined} chosenId
 * @param {PageKind|null} [picked]
 * @returns {PageKind}
 */
export function pageKindFor(presets, chosenId, picked = null) {
  const chosen = presets.find((preset) => preset.id === chosenId) ?? null
  if (picked && (!chosen || presetsForPages([chosen], picked).length)) return picked
  return chosen?.kind === 'color' ? 'color' : 'bw'
}

/**
 * The output scales a preset offers, smallest first: 1 (the page's own size)
 * up to its `maxScale`. A missing or unreadable `maxScale` reads as 1, so a
 * preset is never offered a size it has not said it can give.
 *
 * @param {{maxScale?: unknown}|null|undefined} preset
 * @returns {number[]}
 */
export function outputScalesFor(preset) {
  const max = Number(preset?.maxScale)
  const top = Number.isInteger(max) && max >= 1 ? Math.min(max, 4) : 1
  return Array.from({ length: top }, (_, index) => index + 1)
}

/**
 * The recipe a cloud run sends for a preset at an output scale. At 1, or at a
 * scale the preset does not offer, it is the preset's own recipe untouched:
 * the gateway refuses `output_scale: 1`, and the recipe's bytes are hashed, so
 * the key is only there when it says something.
 *
 * @param {{recipe: Object, maxScale?: unknown}} preset
 * @param {unknown} scale
 * @returns {Object}
 */
export function recipeAt(preset, scale) {
  const wanted = Number(scale)
  if (wanted === 1 || !outputScalesFor(preset).includes(wanted)) return preset.recipe
  return { ...preset.recipe, output_scale: wanted }
}

/**
 * A run's output scale as the history answers it: a whole number from 2 to 4,
 * or 1 for anything else, which is also what an older backend's missing field
 * reads as.
 *
 * @param {unknown} value
 * @returns {number}
 */
export function runScale(value) {
  return Number.isInteger(value) && /** @type {number} */ (value) >= 2 && /** @type {number} */ (value) <= 4
    ? /** @type {number} */ (value)
    : 1
}

/**
 * A measured time in seconds, stored locally or read from the table, or null
 * when there is none worth showing.
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
 *   page's time for each page (`basis: 'reference'`). A preset the table has
 *   no seconds for gives `null` seconds (`basis: 'unmeasured'`).
 * - **local**: the measured seconds per page times the page count
 *   (`basis: 'measured'`), or `null` seconds when nothing was measured.
 *
 * The output scale is not in it: the model's work is the same whatever size
 * the result is saved at.
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
    const perMegapixel = localSeconds(preset.cloudSecondsPerMegapixel)
    const perPage = localSeconds(preset.cloudSecondsPerPage)
    if (areas && perMegapixel !== null) {
      const seconds = areas.reduce((sum, area) => sum + area * perMegapixel, 0)
      return { seconds, perPage: seconds / count, basis: 'pages' }
    }
    if (!areas && perPage !== null) return { seconds: perPage * count, perPage, basis: 'reference' }
    return { seconds: null, perPage: null, basis: 'unmeasured' }
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
