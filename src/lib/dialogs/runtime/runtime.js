/**
 * What the Runtime screen's parts share: the builds' names, how a timing is
 * written, and a model row's name. Settings and setup draw the same parts, so
 * they read these from one place.
 */

import { t } from '../../i18n/index.js'
import { CLEANERS, model as pipelineModel } from '../../model/pipelines.js'

/**
 * Each runtime build's name, chosen between rather than built from its id.
 * `stock` is the platform's plain build, and its name is the platform's.
 */
export const FLAVOUR_NAMES = Object.freeze({ directml: 'DirectML', cuda12: 'CUDA 12', cuda13: 'CUDA 13' })

/** @param {string|null|undefined} id */
export function flavourName(id) {
  return FLAVOUR_NAMES[/** @type {keyof typeof FLAVOUR_NAMES} */ (id ?? '')] ?? t('settings.runtime.build.standard')
}

/** @param {string|null|undefined} id */
export function cudaMajorOf(id) {
  const major = typeof id === 'string' && id.startsWith('cuda') ? Number(id.slice(4)) : NaN
  return Number.isFinite(major) ? major : null
}

/**
 * A duration as the screen writes it: whole milliseconds under a second,
 * one decimal of a second above.
 *
 * @param {number} ms
 */
export function duration(ms) {
  if (ms < 1000) return t('settings.runtime.measure.ms', { value: Math.max(1, Math.round(ms)) })
  return t('settings.runtime.measure.seconds', { value: (Math.round(ms / 100) / 10).toLocaleString() })
}

/** @param {string} id - a provider id */
export function backendName(id) {
  return t(`accel.${id}`)
}

/**
 * A `listAccelerators` model row's name: the backend's own, else the
 * pipeline's product name, else the model kind.
 *
 * @param {{id: string, modelName?: string|null, modelKey?: string}} row
 */
export function modelName(row) {
  return row.modelName ?? pipelineModel(row.id)?.product ?? (row.id === 'inpainter' ? CLEANERS[0].name : t(row.modelKey ?? ''))
}

/**
 * An error a measurement or the backend answered with: a catalogue key where
 * it is one, the runtime's own words where it is not.
 *
 * @param {unknown} error
 * @param {(key: string) => boolean} known
 */
export function errorText(error, known) {
  const text = error instanceof Error ? error.message : String(error ?? '')
  return known(text) ? t(text) : text
}
