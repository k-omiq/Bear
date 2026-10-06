/**
 * A chapter's denoise history as the interface words it: the compare view's
 * run picker and the line of facts under it (`DenoiseCompareDialog.svelte`).
 *
 * A run is `denoise_history.rs#DenoiseRun`: the rows one denoise wrote,
 * named by `created`. A run recorded before presets and targets were kept
 * has neither, and says so rather than guessing.
 */

import { t } from '../i18n/index.js'
import { PRESET_TEXT } from './denoise.js'

/** @typedef {import('../api/backend.js').DenoiseRun} DenoiseRun */

/**
 * The preset's display name, the id itself for one this build does not know,
 * or "Preset not recorded" for an old run.
 *
 * @param {string|null|undefined} id
 */
export function presetName(id) {
  if (!id) return t('home.denoised.presetUnknown')
  const text = /** @type {Record<string, {nameKey: string}>} */ (PRESET_TEXT)[id]
  return text ? t(text.nameKey) : id
}

/**
 * When a run was recorded, in the user's own date and time format.
 *
 * @param {number} created - seconds since the epoch
 */
export function runWhen(created) {
  return new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(created * 1000)
}

/** The run's first line: when, and which preset. @param {DenoiseRun} run */
export function runLabel(run) {
  return t('home.denoised.run', { when: runWhen(run.created), preset: presetName(run.preset) })
}

/**
 * The run's second line, as separate facts: where it ran, what it was made
 * from, how many pages, how many were taken, how many files are gone. A fact
 * the history cannot state is left out rather than guessed.
 *
 * @param {DenoiseRun} run
 * @returns {string[]}
 */
export function runFacts(run) {
  const facts = []
  if (run.target === 'local') facts.push(t('denoise.target.local'))
  else if (run.target === 'cloud') facts.push(t('denoise.target.cloud'))
  if (run.fromCleaned === true) facts.push(t('home.denoised.fromCleaned'))
  else if (run.fromCleaned === false) facts.push(t('home.denoised.fromRaw'))
  facts.push(t('home.denoised.pages', { count: run.pages.length }))
  const taken = run.pages.filter((page) => page.taken).length
  if (taken) facts.push(t('home.denoised.taken', { count: taken }))
  const missing = run.pages.filter((page) => !page.exists).length
  if (missing) facts.push(t('home.denoised.missing', { count: missing }))
  return facts
}

/**
 * The media type of an image's bytes, from its first bytes. The compare view
 * wraps the bytes in a `Blob`, and a `Blob` with no type is only sniffed as
 * a raster: an SVG (the browser mock's stand-in pages) needs its type named.
 *
 * @param {ArrayBuffer|Uint8Array} data
 */
export function imageType(data) {
  const bytes = data instanceof Uint8Array ? data : new Uint8Array(data)
  const starts = (/** @type {number[]} */ ...magic) => magic.every((value, at) => bytes[at] === value)
  if (starts(0x89, 0x50, 0x4e, 0x47)) return 'image/png'
  if (starts(0xff, 0xd8, 0xff)) return 'image/jpeg'
  if (starts(0x52, 0x49, 0x46, 0x46)) return 'image/webp'
  const head = new TextDecoder().decode(bytes.subarray(0, 64)).trimStart()
  if (head.startsWith('<svg') || head.startsWith('<?xml')) return 'image/svg+xml'
  return ''
}
