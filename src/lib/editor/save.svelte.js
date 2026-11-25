/**
 * Save: the open chapter, every page exported, handed to the typesetter
 * through the shared data folder (`docs/shared-data-folder.md` section 9).
 *
 * The button and Cmd/Ctrl+S both come here. The notice that says what
 * happened is the backend's (`notice.save.*`, or an export refusal under
 * `notice.export.*`); this raises one only when the command itself fails, so
 * a failure is never silent.
 *
 * Save needs Shared mode and no run on the chapter. The button says why it is
 * off; a shortcut pressed then says it as a notice.
 */

import { getBackend } from '../api/backend.js'
import { notify } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { runningJobFor } from '../state/jobs.svelte.js'
import { dataFolderMode, isUnavailableError, loadDataFolder } from '../state/datafolder.svelte.js'

export const saving = $state({ active: false })

/**
 * Why Save cannot run now, as a key, or null when it can.
 *
 * @returns {string|null}
 */
export function saveBlockedKey() {
  if (!editor.chapter || editor.loading) return 'editor.save.noChapter'
  if (dataFolderMode() !== 'shared') return 'editor.save.needsDataFolder'
  if (editor.run.active || runningJobFor(editor.chapter.id)) return 'editor.save.running'
  return null
}

/** @param {import('../api/backend.js').Backend} [backend] */
export async function saveChapter(backend = getBackend()) {
  if (saving.active) return null
  const blocked = saveBlockedKey()
  if (blocked) {
    if (blocked !== 'editor.save.noChapter') notify({ key: blocked, params: {}, tone: 'warn' })
    return null
  }
  const chapterId = /** @type {{id: string}} */ (editor.chapter).id
  saving.active = true
  try {
    return await backend.saveToTypesetter({ chapterId })
  } catch (error) {
    if (isUnavailableError(error)) void loadDataFolder(backend)
    const detail = error instanceof Error ? error.message : String(error ?? '')
    notify({ key: 'notice.save.failed', params: { detail }, tone: 'warn' })
    return null
  } finally {
    saving.active = false
  }
}
