/**
 * The window in front of every Nano Banana request: what it will cost, that the
 * crops go to Google, and a box for the person to say what to edit.
 *
 * A batch asks once (`nanobananarun.svelte.js`) and a single region asks for
 * itself (`maskactions.svelte.js#rerunMask`, `#runRegionMenuItem`). What the
 * person writes is kept as a setting (`nanoBananaPrompt`), so the next window
 * opens with it and the native side reads it when the request is made.
 */
import { notify, pushModal } from '../state/app.svelte.js'
import { getBackend } from '../api/backend.js'
import { writeSettingsSerialized } from '../state/settingswrite.js'
import { MAX_INSTRUCTION_CHARS, nanoBananaModel, nanoBananaStartSize } from '../model/nanobanana.js'

/**
 * Ask, and keep the instruction. Answers whether to send. Nothing is sent when
 * the instruction could not be saved, because the request would carry the old one.
 *
 * @param {number} count - regions the request covers
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<boolean>}
 */
export async function confirmNanoBanana(count, backend = getBackend()) {
  /** @type {any} */
  let settings = null
  try { settings = await backend.readSettings() } catch { /* the defaults stand */ }
  const model = nanoBananaModel(settings?.nanoBananaModel).id
  const answer = /** @type {{ id?: string, prompt?: string } | null} */ (await new Promise((resolve) => {
    pushModal({
      kind: 'nanoBananaConsent',
      titleKey: 'nanoBanana.run.title',
      blocking: true,
      props: {
        count,
        model,
        size: nanoBananaStartSize(model),
        prompt: String(settings?.nanoBananaPrompt ?? ''),
      },
      actions: [
        { id: 'cancel', labelKey: 'nanoBanana.run.cancel' },
        { id: 'confirm', labelKey: 'nanoBanana.run.confirm', variant: 'primary' },
      ],
      onresolve: resolve,
    })
  }))
  if (answer?.id !== 'confirm') return false
  try {
    await writeSettingsSerialized(backend, {
      nanoBananaPrompt: String(answer.prompt ?? '').trim().slice(0, MAX_INSTRUCTION_CHARS),
    })
  } catch {
    notify({ key: 'nanoBanana.run.promptNotSaved', tone: 'warn' })
    return false
  }
  return true
}
