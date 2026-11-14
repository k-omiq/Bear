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
 * a grant for this many regions of this chapter on the model and the profile
 * the window showed (`grantNanoBanana`), which binds them and that
 * instruction. The profile is the active one in Settings > Cloud; its key
 * pays, and the window says so. Each window's grant is its own: the caller gets it back and sends it with
 * the edit or the batch it confirmed (`params.nanoBananaGrant`), and Stop on
 * that edit names it (`nanobananaedits.svelte.js`). The native side renders
 * nothing without one.
 */
import { notify, pushModal } from '../state/app.svelte.js'
import { getBackend } from '../api/backend.js'
import { hasKey } from '../i18n/index.js'
import { writeSettingsSerialized } from '../state/settingswrite.js'
import { MAX_GRANT_REGIONS, MAX_INSTRUCTION_CHARS, nanoBananaModel, nanoBananaStartSize } from '../model/nanobanana.js'

/**
 * Ask, keep the instruction, and take the grant. Answers the grant to send
 * with, or null to send nothing. Nothing is sent when the instruction could
 * not be saved, because the grant would carry the old one, nor when no grant
 * was given (cloud engines turned off, or the model or the active profile
 * changed while the window was open), nor when no profile is saved. More
 * regions than one grant covers are refused before the window opens, with
 * their own message.
 *
 * @param {number} count - regions the request covers
 * @param {string|null|undefined} chapterId - the chapter they are on
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<string|null>}
 */
export async function confirmNanoBanana(count, chapterId, backend = getBackend()) {
  if (!chapterId) return null
  if (count > MAX_GRANT_REGIONS) {
    notify({ key: 'nanoBanana.run.tooMany', params: { max: MAX_GRANT_REGIONS }, tone: 'warn' })
    return null
  }
  /** @type {any} */
  let settings = null
  try { settings = await backend.readSettings() } catch { /* the defaults stand */ }
  // The active profile pays, with its model. A backend that cannot list
  // profiles leaves the choice to the grant, which refuses a mismatch.
  /** @type {import('../api/backend.js').NanoBananaProfiles|null} */
  let profiles = null
  try { profiles = await backend.listNanoBananaProfiles?.() ?? null } catch { /* the grant decides */ }
  const paying = profiles?.profiles?.find((profile) => profile.id === profiles?.active) ?? null
  if (profiles && !paying) {
    notify({ key: 'nanoBanana.target.noKey', tone: 'warn' })
    return null
  }
  const model = nanoBananaModel(paying?.model ?? settings?.nanoBananaModel).id
  const answer = /** @type {{ id?: string, prompt?: string } | null} */ (await new Promise((resolve) => {
    pushModal({
      kind: 'nanoBananaConsent',
      titleKey: 'nanoBanana.run.title',
      blocking: true,
      props: {
        count,
        model,
        profile: paying?.name ?? null,
        size: nanoBananaStartSize(model),
        prompt: String(settings?.nanoBananaPrompt ?? ''),
        // Read by native when the grant is made, so it is what this window
        // will send.
        restrictive: settings?.nanoBananaRestrictive === true,
      },
      actions: [
        { id: 'cancel', labelKey: 'nanoBanana.run.cancel' },
        { id: 'confirm', labelKey: 'nanoBanana.run.confirm', variant: 'primary' },
      ],
      onresolve: resolve,
    })
  }))
  if (answer?.id !== 'confirm') return null
  try {
    await writeSettingsSerialized(backend, {
      nanoBananaPrompt: String(answer.prompt ?? '').trim().slice(0, MAX_INSTRUCTION_CHARS),
    })
  } catch {
    notify({ key: 'nanoBanana.run.promptNotSaved', tone: 'warn' })
    return null
  }
  try {
    const granted = await backend.grantNanoBanana({ model, count, chapterId, ...(paying ? { profileId: paying.id } : {}) })
    if (typeof granted?.grant !== 'string') throw new Error('decline.reason.nanoBananaNoConsent')
    return granted.grant
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error)
    notify({ key: hasKey(reason) ? reason : 'decline.reason.nanoBananaNoConsent', tone: 'warn' })
    return null
  }
}
