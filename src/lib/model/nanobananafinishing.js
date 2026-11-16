/**
 * How a Nano Banana answer is finished before it is written: its own three
 * settings, apart from the finishing LaMa and local FLUX share
 * (`inference/nano_banana_finishing.rs`).
 *
 * - `nanoBananaFinishing`: one of `FINISHINGS`, null for the default (`tone-fill-dots`).
 * - `nanoBananaFinishingGrayscale`: whether a colour answer is made grey first.
 * - `nanoBananaStrays`: whether small marks the model drew on its own inside
 *   the mask are filled from the tone round them first. Off by default.
 *
 * A Nano Banana layer records what it was finished by
 * (`params_snapshot.postprocess`), and Try again on it finishes the new answer
 * the same way.
 */
import { DEFAULT_FINISHING, FINISHINGS, FINISHING_ALIASES } from './model-names.js'

export const NANO_BANANA_FINISHING_KEY = 'nanoBananaFinishing'
export const NANO_BANANA_GRAYSCALE_KEY = 'nanoBananaFinishingGrayscale'
export const NANO_BANANA_STRAYS_KEY = 'nanoBananaStrays'

/** @typedef {{ finishing: string, grayscale: boolean, strays: boolean }} NanoBananaFinishing */

/**
 * A finishing name under the name it has now: an older name
 * (`FINISHING_ALIASES`) as native's serde aliases read it.
 *
 * @param {unknown} name
 */
function currentName(name) {
  return typeof name === 'string' && Object.hasOwn(FINISHING_ALIASES, name) ? /** @type {Record<string, string>} */ (FINISHING_ALIASES)[name] : name
}

/**
 * The choice settings hold, defaults where a key is absent or null. A value
 * native would refuse reads as the default here, as a region edit reads it.
 *
 * @param {Record<string, unknown>|null|undefined} settings
 * @returns {NanoBananaFinishing}
 */
export function nanoBananaFinishing(settings) {
  const finishing = currentName(settings?.[NANO_BANANA_FINISHING_KEY])
  return {
    finishing: typeof finishing === 'string' && FINISHINGS.includes(/** @type {any} */ (finishing)) ? finishing : DEFAULT_FINISHING,
    grayscale: settings?.[NANO_BANANA_GRAYSCALE_KEY] === true,
    strays: settings?.[NANO_BANANA_STRAYS_KEY] === true,
  }
}

/**
 * Why native refuses this settings patch's Nano Banana finishing, or null
 * when it does not (`nano_banana_finishing::from_settings`).
 *
 * @param {Record<string, unknown>|null|undefined} patch
 * @returns {string|null}
 */
export function nanoBananaFinishingRefusal(patch) {
  const finishing = currentName(patch?.[NANO_BANANA_FINISHING_KEY])
  if (finishing != null && !FINISHINGS.includes(/** @type {any} */ (finishing))) {
    return `${NANO_BANANA_FINISHING_KEY} must be one of ${JSON.stringify(FINISHINGS)}`
  }
  for (const key of [NANO_BANANA_GRAYSCALE_KEY, NANO_BANANA_STRAYS_KEY]) {
    const value = patch?.[key]
    if (value != null && typeof value !== 'boolean') return `${key} must be true or false`
  }
  return null
}

/**
 * The finishing a layer's record names, as native reads it for a Try again
 * (`nano_banana_finishing::recorded`): only a Nano Banana layer's.
 *
 * @param {any} provenance
 * @returns {NanoBananaFinishing|null}
 */
export function recordedNanoBananaFinishing(provenance) {
  const postprocess = provenance?.params_snapshot?.postprocess
  if (provenance?.engine !== 'nanobanana' || typeof postprocess?.finishing !== 'string') return null
  return { finishing: /** @type {string} */ (currentName(postprocess.finishing)), grayscale: postprocess.grayscale === true, strays: postprocess.strays === true }
}
