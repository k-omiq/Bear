/** Plain-language Qwen guidance, and reviews shared by every cloud entry point. */
import { app, pushModal, notify } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'

export const QWEN_RECIPE = 'mc-qwen-image-edit-2511-v4'
export const isQwen = (modelId) => typeof modelId === 'string' && modelId.startsWith('Disty0/Qwen-Image-Edit')

/** @param {any} model @returns {boolean} */
export function qwenSupported(model) {
  if (!isQwen(model?.pinnedModelId)) return true
  if (model.pinnedRecipeId === QWEN_RECIPE) return true
  notify({ key: 'qwen.prompt.updateRequired', tone: 'warn' })
  return false
}

/** @param {{regionId?: string, batch?: boolean, initial?: any}} [options] */
export function askQwenEdit(options = {}) {
  const region = editor.chapter?.pages?.flatMap((page) => page.regions ?? []).find((region) => region.id === options.regionId)
  const initial = options.initial ?? region?.mask?.provenance?.params_snapshot?.qwen_edit ?? { target: options.batch ? 'auto' : region?.insideBubble === true || region?.kind === 'bubble' ? 'dialogue' : region?.kind === 'sfx' ? 'sound_effect' : 'other', description: '' }
  return new Promise((resolve) => pushModal({ kind: 'qwenPrompt', titleKey: 'qwen.prompt.title', blocking: true,
    props: { initial, batch: options.batch === true }, onresolve: (answer) => {
      if (!answer || typeof answer !== 'object' || !['auto', 'dialogue', 'sound_effect', 'other'].includes(answer.target)
        || typeof answer.description !== 'string' || Array.from(answer.description).length > 500) return resolve(null)
      resolve({ target: answer.target, description: answer.description.trim() })
    } }))
}

const retries = new Map()

/** Remove reviews whose native attempt has ended without sending another decision. */
export function dismissQwenReview(attemptId) {
  app.modals = app.modals.filter((modal) => modal.kind !== 'qwenReview' || modal.props.preview?.attemptId !== attemptId)
}

export function takeQwenRetry(attemptId) {
  const edit = retries.get(attemptId)
  retries.delete(attemptId)
  return edit ?? null
}

/** @param {any} preview @param {import('../api/backend.js').Backend} backend */
export function reviewQwen(preview, backend) {
  if (!/^att-[a-f0-9]{24}$/.test(preview?.attemptId ?? '')) return
  if (preview.closed === true) return dismissQwenReview(preview.attemptId)
  if (!/^data:image\/png;base64,/.test(preview?.before ?? '') || !/^data:image\/png;base64,/.test(preview?.after ?? '')) return
  pushModal({ kind: 'qwenReview', titleKey: 'qwen.review.title', blocking: true, props: { preview }, onresolve: async (answer) => {
    const choice = answer?.choice === 'use' ? 'use' : answer?.choice === 'retry' && preview.canRetry ? 'retry' : 'discard'
    if (choice === 'retry') retries.set(preview.attemptId, answer.edit)
    try { await backend.resolveQwenReview({ attemptId: preview.attemptId, choice }) }
    catch { retries.delete(preview.attemptId); notify({ key: 'qwen.review.expired', tone: 'warn' }) }
  } })
}
