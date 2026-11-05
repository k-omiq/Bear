/**
 * The window in front of every Nano Banana request: what it will cost, that the
 * crops go to Google, and a box for the person to say what to edit.
 *
 * A batch asks once (`nanobananarun.svelte.js`) and a single region asks for
 * itself (`maskactions.svelte.js#rerunMask`, `#runRegionMenuItem`). What the
 * person writes is kept as a setting (`nanoBananaPrompt`), so the next window
 * opens with it.
 *
 * **A yes is a grant.** Once the instruction is saved, the backend is asked for
 * a grant for this many regions on the model the window showed
 * (`grantNanoBanana`), which binds that model and that instruction. The
 * backend keeps it and sends it with each Nano Banana render, and the native
 * side renders nothing without one, so a caller only ever sees the boolean.
 */
import { notify, pushModal } from '../state/app.svelte.js'
import { getBackend } from '../api/backend.js'
import { hasKey } from '../i18n/index.js'
import { writeSettingsSerialized } from '../state/settingswrite.js'
import { MAX_INSTRUCTION_CHARS, nanoBananaModel, nanoBananaStartSize } from '../model/nanobanana.js'

/**
 * Ask, keep the instruction, and take the grant. Answers whether to send.
 * Nothing is sent when the instruction could not be saved, because the grant
 * would carry the old one, nor when no grant was given (cloud engines turned
 * off, or the model changed while the window was open).
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
  try {
    await backend.grantNanoBanana({ model, count })
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error)
    notify({ key: hasKey(reason) ? reason : 'decline.reason.nanoBananaNoConsent', tone: 'warn' })
    return false
  }
  return true
}
