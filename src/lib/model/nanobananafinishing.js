/**
 * How a Nano Banana layer was finished, as its record names it.
 *
 * A new Nano Banana answer is finished by the shared Post-processing choice
 * (`cloudFinishing`, `cloudFinishingGrayscale`), always, as a FLUX 3 answer
 * is (`inference/nano_banana_finishing.rs`). Its own settings of before
 * (`nanoBananaFinishing`, `nanoBananaFinishingGrayscale`, `nanoBananaStrays`)
 * are taken out of the settings file when it is read, and nothing reads them.
 *
 * A Nano Banana layer records what it was finished by
 * (`params_snapshot.postprocess`), and Try again on it finishes the new answer
 * the same way. Its own finishing, stray marks included, is changed from its
 * previews in the Post-processing window.
 */
import { FINISHING_ALIASES } from './model-names.js'

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
