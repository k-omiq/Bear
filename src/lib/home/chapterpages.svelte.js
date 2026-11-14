/**
 * The Pages screen's calls: read one chapter's pages, add, remove, move and
 * put back, and keep Home's list and an open editor in step with each edit.
 *
 * Every edit answers with the new listing or `null`; a `null` has already put
 * its reason on the notice stack (the backend's `notice.pages.*`), so the
 * screen only has to keep what it shows.
 */

import { getBackend } from '../api/backend.js'
import { notify, pushModal } from '../state/app.svelte.js'
import { editor, goToPage, openEditorChapter, pages as editorPages } from '../state/editor.svelte.js'
import { loadLibrary } from './library.svelte.js'
import { positionOfSource } from './pagerules.js'

/**
 * Raise the Pages screen for one chapter.
 *
 * @param {{projectId: string, chapterId: string, number: number|string, name: string}} chapter
 */
export function openChapterPages({ projectId, chapterId, number, name }) {
  pushModal({
    kind: 'chapterPages',
    titleKey: 'home.pagesScreen.title',
    props: { projectId, chapterId, number, name },
  })
}

/**
 * @param {string} chapterId
 * @returns {Promise<import('../api/backend.js').ChapterPages|null>}
 */
export async function readChapterPages(chapterId) {
  try {
    return await getBackend().chapterPages({ chapterId })
  } catch {
    notify({ key: 'notice.library.changeFailed', params: {}, tone: 'warn' })
    return null
  }
}

/**
 * Run one edit, then bring Home's list and an open editor up to date.
 *
 * @param {string} projectId
 * @param {string} chapterId
 * @param {() => Promise<import('../api/backend.js').ChapterPages|null>} work
 */
async function edit(projectId, chapterId, work) {
  // Where the editor is, by the page's own slot rather than its position: a
  // page edit can move the page the reader is on.
  const open = editor.chapter?.id === chapterId
  const at = open ? editorPages()[editor.pageIndex] : null
  let listing = null
  try {
    listing = await work()
  } catch (error) {
    const code = String(error instanceof Error ? error.message : error ?? '')
    notify({
      key: code.includes('job_busy') ? 'notice.job.busy' : code.includes('run_in_progress') ? 'notice.run.busy' : 'notice.library.changeFailed',
      params: {},
      tone: 'warn',
    })
    return null
  }
  if (!listing) return null
  await loadLibrary()
  if (open && editor.chapter?.id === chapterId) {
    const reopened = await openEditorChapter(projectId, chapterId, { reload: true })
    if (reopened) goToPage(positionOfSource(editorPages(), at?.sourceIndex, editor.pageIndex))
  }
  return listing
}

/**
 * @param {{projectId: string, chapterId: string, paths: string[], at?: number}} spec
 */
export function addPages({ projectId, chapterId, paths, at }) {
  return edit(projectId, chapterId, () => getBackend().addPages({ chapterId, paths, at }))
}

/**
 * @param {{projectId: string, chapterId: string, indices: number[]}} spec
 */
export function removePages({ projectId, chapterId, indices }) {
  return edit(projectId, chapterId, () => getBackend().removePages({ chapterId, indices }))
}

/**
 * @param {{projectId: string, chapterId: string, order: number[]}} spec
 */
export function movePages({ projectId, chapterId, order }) {
  return edit(projectId, chapterId, () => getBackend().movePages({ chapterId, order }))
}

/**
 * @param {{projectId: string, chapterId: string, sources: number[]}} spec
 */
export function restorePages({ projectId, chapterId, sources }) {
  return edit(projectId, chapterId, () => getBackend().restorePages({ chapterId, sources }))
}
