/**
 * The time-per-page line every preset row carries, in words.
 *
 * Cloud: the preset's measured seconds on the reference page, or scaled to
 * the chapter's own pages when the caller has them, always said to be on a
 * cloud GPU. Local: the figure measured on this computer, or `null` when
 * there is none, which the row draws as "Not measured yet".
 */

import { t } from '../../i18n/index.js'
import { DENOISE_REFERENCE, chapterEstimate, durationText, localSeconds } from '../../model/denoise.js'

/** @param {number} seconds */
export function durationWords(seconds) {
  const text = durationText(seconds)
  return t(text.key, text.params)
}

/**
 * @param {import('../../model/denoise.js').DenoisePreset} preset
 * @param {string} target - `local` or `cloud`
 * @param {{pages?: Array<{width?: number, height?: number}>|null, localPerPage?: unknown}} [context]
 * @returns {{value: string, where: string}|null}
 */
export function presetTime(preset, target, { pages = null, localPerPage = null } = {}) {
  if (target === 'cloud') {
    const perPage = pages?.length
      ? chapterEstimate({ target, preset, pages }).perPage ?? preset.cloudSecondsPerPage
      : preset.cloudSecondsPerPage
    return {
      value: t('denoise.time.cloud', { duration: durationWords(perPage) }),
      where: t('denoise.time.cloudWhere', { gpu: DENOISE_REFERENCE.gpu }),
    }
  }
  const seconds = localSeconds(localPerPage)
  if (target !== 'local' || seconds === null) return null
  return {
    value: t('denoise.time.local', { duration: durationWords(seconds) }),
    where: t('denoise.time.localWhere'),
  }
}
