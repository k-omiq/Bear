/**
 * Plain-language guidance (Qwen, FLUX.2 Klein and FLUX 3), shared by every
 * cloud entry point and every clean with FLUX on this computer.
 */
import { pushModal, notify } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { session } from '../state/session.svelte.js'
import { isCloudMask, maskEngine } from '../model/masks.js'

export const QWEN_RECIPE = 'mc-qwen-image-edit-2511-v4'
export const isQwen = (modelId) => typeof modelId === 'string' && modelId.startsWith('Disty0/Qwen-Image-Edit')

/** The recipes whose worker reads guidance; render.rs GUIDED_RECIPES is the same set. */
const FLUX3_RECIPES = ['mc-flux3-klein-edit-v2', 'mc-flux3-klein-bf16-edit-v2']
export const GUIDED_RECIPES = [QWEN_RECIPE, 'mc-flux2-klein-inpaint-v2', 'mc-flux2-klein-9b-inpaint-v2', ...FLUX3_RECIPES]
/** @param {any} model @returns {boolean} */
export const takesGuidance = (model) => GUIDED_RECIPES.includes(model?.pinnedRecipeId)

/** @param {string|undefined} regionId */
const regionOf = (regionId) => editor.chapter?.pages?.flatMap((page) => page.regions ?? []).find((region) => region.id === regionId)

/**
 * Whether a one-region cloud clean asks for guidance first. Qwen and FLUX 3
 * always do. Klein v2 asks only to redo a cloud result: a first clean keeps its
 * one click, and a cloud result that came out wrong is where steering helps.
 *
 * @param {any} model
 * @param {string} regionId
 * @returns {boolean}
 */
export function asksGuidance(model, regionId) {
  if (!takesGuidance(model)) return false
  return isQwen(model.pinnedModelId) || FLUX3_RECIPES.includes(model.pinnedRecipeId) || isCloudMask(regionOf(regionId)?.mask)
}

/**
 * The guidance for a batch clean, asked before its plan is prepared. Qwen and
 * FLUX 3 ask, with Automatic chosen. A model that does not ask gets `undefined`,
 * and native sends Auto if its recipe takes guidance.
 *
 * @param {any} model
 * @returns {Promise<{target: string, description: string}|null|undefined>} `null` stops the clean
 */
export async function askBatchGuidance(model) {
  if (!qwenSupported(model)) return null
  const qwen = isQwen(model?.pinnedModelId)
  if (!qwen && !FLUX3_RECIPES.includes(model?.pinnedRecipeId)) return undefined
  return askQwenEdit({ batch: true, flux: !qwen })
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

/** The FLUX model a render on this computer runs when none is set (`region.rs#FLUX_MODEL`). */
export const DEFAULT_LOCAL_FLUX = 'flux2-klein-4b'

/**
 * The local models that always ask, as the cloud's Qwen and FLUX 3 do. The
 * Klein models, and a model placed by hand, which cleans as Klein
 * (`models.rs#recipe_of`), ask only to redo a FLUX result.
 */
const LOCAL_ALWAYS_ASK = ['flux3-image-klein', 'qwen-image-edit-2511']

/** @returns {string} the FLUX model a render on this computer runs now */
export const localFluxModel = () => session.fluxModel || DEFAULT_LOCAL_FLUX

/** Qwen reads the description as the lettering, the FLUX models as what it covers. */
const isLocalQwen = (/** @type {string} */ modelId) => modelId.startsWith('qwen-image-edit')

/**
 * Whether a one-region clean with FLUX on this computer asks for guidance
 * first, by the cloud's rule for the same family (`asksGuidance`): FLUX 3 and
 * Qwen always; Klein only to redo a region a model cleaned already, with FLUX
 * here or on the cloud GPU, where steering helps, so a first clean keeps its
 * one click.
 *
 * @param {string|null|undefined} regionId - absent for a region not made yet
 * @param {string} [modelId]
 * @returns {boolean}
 */
export function asksLocalGuidance(regionId, modelId = localFluxModel()) {
  if (LOCAL_ALWAYS_ASK.includes(modelId)) return true
  const mask = regionId ? regionOf(regionId)?.mask ?? null : null
  return maskEngine(mask) === 'flux' || isCloudMask(mask)
}

/**
 * The guidance for one region cleaned with FLUX on this computer: `undefined`
 * when this model does not ask (the render keeps its recipe's own words),
 * `null` when the user cancelled (the clean stops), the answer otherwise. The
 * dialog opens on the region's last answer, cloud or local.
 *
 * @param {string|null|undefined} regionId
 * @param {string} [modelId]
 * @returns {Promise<{target: string, description: string}|null|undefined>}
 */
export async function askLocalGuidance(regionId, modelId = localFluxModel()) {
  if (!asksLocalGuidance(regionId, modelId)) return undefined
  return askQwenEdit({ regionId: regionId ?? undefined, flux: !isLocalQwen(modelId) })
}

/**
 * The guidance for a run that cleans with FLUX on this computer, asked before
 * it starts, as `askBatchGuidance` asks before a cloud clean: FLUX 3 and Qwen
 * ask with Automatic chosen, Klein does not (`undefined`). `null` stops it.
 *
 * @param {string} [modelId]
 * @returns {Promise<{target: string, description: string}|null|undefined>}
 */
export async function askLocalBatchGuidance(modelId = localFluxModel()) {
  if (!LOCAL_ALWAYS_ASK.includes(modelId)) return undefined
  return askQwenEdit({ batch: true, flux: !isLocalQwen(modelId) })
}
