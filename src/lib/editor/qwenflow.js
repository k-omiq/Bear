/** Plain-language cloud guidance (Qwen, FLUX.2 Klein v2 and FLUX 3), shared by every cloud entry point. */
import { pushModal, notify } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { isCloudMask } from '../model/masks.js'

export const QWEN_RECIPE = 'mc-qwen-image-edit-2511-v4'
export const isQwen = (modelId) => typeof modelId === 'string' && modelId.startsWith('Disty0/Qwen-Image-Edit')

/** The recipes whose worker reads guidance; render.rs GUIDED_RECIPES is the same set. */
export const GUIDED_RECIPES = [QWEN_RECIPE, 'mc-flux2-klein-inpaint-v2', 'mc-flux2-klein-9b-inpaint-v2', 'mc-flux3-klein-edit-v1']
/** @param {any} model @returns {boolean} */
export const takesGuidance = (model) => GUIDED_RECIPES.includes(model?.pinnedRecipeId)

/** @param {string|undefined} regionId */
const regionOf = (regionId) => editor.chapter?.pages?.flatMap((page) => page.regions ?? []).find((region) => region.id === regionId)

/**
 * Whether a one-region cloud clean asks for guidance first. Qwen always does.
 * Klein v2 asks only to redo a cloud result: a first clean keeps its one
 * click, and a cloud result that came out wrong is where steering helps.
 *
 * @param {any} model
 * @param {string} regionId
 * @returns {boolean}
 */
export function asksGuidance(model, regionId) {
  if (!takesGuidance(model)) return false
  return isQwen(model.pinnedModelId) || isCloudMask(regionOf(regionId)?.mask)
}

/** @param {any} model @returns {boolean} */
export function qwenSupported(model) {
  if (!isQwen(model?.pinnedModelId)) return true
  if (model.pinnedRecipeId === QWEN_RECIPE) return true
  notify({ key: 'qwen.prompt.updateRequired', tone: 'warn' })
  return false
}

/**
 * Klein reads the description as what the lettering covers, Qwen as the
 * lettering itself; `flux` picks the dialog's wording.
 *
 * @param {{regionId?: string, batch?: boolean, flux?: boolean}} [options]
 */
export function askQwenEdit(options = {}) {
  const region = regionOf(options.regionId)
  const initial = region?.mask?.provenance?.params_snapshot?.qwen_edit ?? { target: options.batch ? 'auto' : region?.insideBubble === true || region?.kind === 'bubble' ? 'dialogue' : region?.kind === 'sfx' ? 'sound_effect' : 'other', description: '' }
  return new Promise((resolve) => pushModal({ kind: 'qwenPrompt', titleKey: 'qwen.prompt.title', blocking: true,
    props: { initial, batch: options.batch === true, flux: options.flux === true }, onresolve: (answer) => {
      if (!answer || typeof answer !== 'object' || !['auto', 'dialogue', 'sound_effect', 'other'].includes(answer.target)
        || typeof answer.description !== 'string' || Array.from(answer.description).length > 500) return resolve(null)
      resolve({ target: answer.target, description: answer.description.trim() })
    } }))
}
