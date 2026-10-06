/**
 * The verbs Home offers on a project, in one place: the card's context menu,
 * the chapter view's header menu and the keyboard shortcut all raise the same
 * dialogs and call the same functions.
 *
 * Dialogs go on the global modal stack rather than being mounted inside the
 * screen, because `N` (new project) is a global shortcut that pushes
 * `{kind: 'newProject'}` from `src/lib/shortcuts.js` - the stack is already the
 * way in, so Home's own buttons use it too and there is exactly one path.
 */

import { notify, pushModal, openProject } from '../state/app.svelte.js'
import { getBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { copySourcePath, removeChapter, removeProject, replaceWithDenoised } from './library.svelte.js'

/** Raise the New project dialog. */
export function openNewProject() {
  pushModal({ kind: 'newProject' })
}

/**
 * Raise the New chapter dialog, optionally with a project preselected.
 * @param {string} [projectId]
 */
export function openNewChapter(projectId) {
  pushModal({ kind: 'newChapter', props: { projectId: projectId ?? null } })
}

/** @param {import('../api/backend.js').ApiProject} project */
export function openRenameProject(project) {
  pushModal({ kind: 'renameProject', props: { projectId: project.id } })
}

/**
 * Removing a project drops it from the library; the pages on disk are not
 * touched, and the copy has to say so or the user cannot tell what they are
 * agreeing to.
 *
 * @param {import('../api/backend.js').ApiProject} project
 */
export function confirmRemoveProject(project) {
  pushModal({
    kind: 'removeProject',
    props: {
      bodyKey: 'home.remove.body',
      bodyParams: { name: project.name, path: project.sourcePath },
    },
    actions: [
      { id: 'cancel', labelKey: 'shell.action.cancel' },
      { id: 'remove', labelKey: 'home.action.remove', variant: 'primary' },
    ],
    onresolve: (result) => {
      if (result === 'remove') removeProject(project.id)
    },
  })
}

/**
 * Delete a chapter, after asking what "delete" is to mean.
 *
 * Two destructive answers rather than one, because the two are not the same
 * act: dropping the chapter from the library leaves the scans where they are
 * and can be undone by adding the chapter again, while taking the scans as well
 * destroys files this app did not make and cannot put back. The dialog names
 * the folder, so the second button is never pressed without its path in view.
 *
 * The safe answer is the primary one. The backend refuses to remove the
 * project's own folder whichever button is pressed - see the seam's
 * `deleteChapter` - so this cannot empty the project even by mistake.
 *
 * @param {import('../api/backend.js').ApiProject} project
 * @param {import('../api/backend.js').ApiChapter} chapter
 */
export function confirmDeleteChapter(project, chapter) {
  const ownFolder = !chapter.sourcePath || chapter.sourcePath === project.sourcePath
  pushModal({
    kind: 'deleteChapter',
    props: {
      bodyKey: ownFolder ? 'home.deleteChapter.bodyOwnFolder' : 'home.deleteChapter.body',
      bodyParams: { number: chapter.number, name: chapter.name, path: chapter.sourcePath },
    },
    actions: [
      { id: 'cancel', labelKey: 'shell.action.cancel' },
      ...(ownFolder ? [] : [{ id: 'scans', labelKey: 'home.deleteChapter.withScans' }]),
      { id: 'chapter', labelKey: 'home.deleteChapter.keepScans', variant: 'primary' },
    ],
    onresolve: (result) => {
      if (result !== 'chapter' && result !== 'scans') return
      removeChapter({
        projectId: project.id,
        chapterId: chapter.id,
        sourceFiles: result === 'scans',
      })
    },
  })
}

/**
 * Denoise a chapter's pages into a folder of new files. The dialog asks where
 * to run and which preset, and for consent before anything goes to a cloud
 * GPU; when denoise is off it offers a short setup first. With `cleaned` it is
 * the same run under its own title, into a `denoised-cleaned` folder, for a
 * chapter denoised after it was cleaned.
 *
 * @param {import('../api/backend.js').ApiProject} project
 * @param {import('../api/backend.js').ApiChapter} chapter
 * @param {{cleaned?: boolean}} [options]
 */
export function openDenoise(project, chapter, { cleaned = false } = {}) {
  const titleKey = cleaned ? 'denoise.titleCleaned' : 'denoise.title'
  pushModal({ kind: 'denoise', titleKey, props: { projectId: project.id, chapter, cleaned } })
}

/**
 * Take the denoised files as the chapter's pages, after saying how many pages
 * change and how many stay, and why: a page with no denoised file (never
 * denoised, or it failed in the run) stays raw, and a page cleaned or changed
 * since stays as it is. Offered while the backend says at least one page can
 * be taken (`chapter.denoiseReplacement`).
 *
 * @param {import('../api/backend.js').ApiProject} project
 * @param {import('../api/backend.js').ApiChapter} chapter
 */
export function confirmReplaceDenoised(project, chapter) {
  const offer = chapter.denoiseReplacement
  if (!offer) return
  pushModal({
    kind: 'replaceDenoised',
    titleKey: 'modal.title.replaceDenoised',
    props: {
      bodyKey: replaceBodyKey(offer),
      bodyParams: { count: offer.pages, kept: offer.kept, missing: offer.missing ?? 0, number: chapter.number },
    },
    actions: [
      { id: 'cancel', labelKey: 'shell.action.cancel' },
      { id: 'replace', labelKey: 'home.replaceDenoised.confirm', variant: 'primary' },
    ],
    onresolve: (result) => {
      if (result === 'replace') replaceWithDenoised({ chapter })
    },
  })
}

/**
 * The confirmation's copy for an offer: one whole key per combination of
 * pages that stay, so each reads as a sentence and the catalogue test sees
 * every key.
 *
 * @param {{kept: number, missing?: number}} offer
 */
function replaceBodyKey(offer) {
  if (offer.missing && offer.kept) return 'home.replaceDenoised.bodyMissingKept'
  if (offer.missing) return 'home.replaceDenoised.bodyMissing'
  if (offer.kept) return 'home.replaceDenoised.bodyKept'
  return 'home.replaceDenoised.body'
}

/**
 * Compare one denoise run of a chapter with the raw pages it was made from,
 * a page at a time. `runs` is the chapter's history (`denoiseHistory`),
 * newest first, which the dialog's run picker lists; `run` is the run it
 * opens on, a `created`.
 *
 * @param {import('../api/backend.js').ApiProject} project
 * @param {import('../api/backend.js').ApiChapter} chapter
 * @param {import('../api/backend.js').DenoiseRun[]} runs
 * @param {number} run
 */
export function openDenoiseCompare(project, chapter, runs, run) {
  pushModal({
    kind: 'denoiseCompare',
    titleKey: 'denoise.compare.title',
    props: { projectId: project.id, chapter, runs, run },
  })
}

/**
 * The chapter's denoise history in the compare view, on its newest run. The
 * history is read when asked for rather than carried by every chapter of the
 * listing, which only knows that there is one (`chapter.denoiseHistory`).
 *
 * @param {import('../api/backend.js').ApiProject} project
 * @param {import('../api/backend.js').ApiChapter} chapter
 */
export async function openDenoiseHistory(project, chapter) {
  let runs = []
  try {
    runs = await getBackend().denoiseHistory({ chapterId: chapter.id })
  } catch {
    runs = []
  }
  if (!runs.length) {
    notify({ key: 'home.denoised.failed', params: { number: chapter.number }, tone: 'warn' })
    return
  }
  openDenoiseCompare(project, chapter, runs, runs[0].created)
}

/**
 * Whether any page of the chapter has cleaning on it, from the page headers
 * the listing already carries: a cleaned page, or one with a cleaned region.
 * Denoise cleaned chapter is only a different run from Denoise when it does.
 *
 * @param {import('../api/backend.js').ApiChapter} [chapter]
 */
export function hasCleaning(chapter) {
  const pages = Array.isArray(chapter?.pages) ? chapter.pages : []
  return pages.some((page) => page?.status === 'cleaned' || (page?.doneCount ?? 0) > 0)
}

/**
 * The context menu for one chapter: Denoise; Denoise cleaned chapter once a
 * page has cleaning on it; Replace pages with denoised once a page can take
 * its denoised file; Denoise history once the chapter was denoised; then the
 * destructive entry alone behind a separator. The row itself is how a chapter
 * is opened. The same items back the row's ... button and its right-click
 * menu.
 *
 * @param {import('../api/backend.js').ApiChapter} [chapter]
 * @returns {Array<Object>}
 */
export function chapterMenuItems(chapter) {
  return [
    { id: 'denoise', label: t('home.action.denoise'), icon: 'wand' },
    ...(hasCleaning(chapter) ? [{ id: 'denoiseCleaned', label: t('home.action.denoiseCleaned'), icon: 'wand' }] : []),
    ...(chapter?.denoiseReplacement ? [{ id: 'replaceDenoised', label: t('home.action.replaceDenoised'), icon: 'refresh' }] : []),
    ...(chapter?.denoiseHistory ? [{ id: 'denoiseHistory', label: t('home.action.denoiseHistory'), icon: 'eye' }] : []),
    { id: 'sep-1', separator: true },
    { id: 'delete', label: t('home.action.deleteChapter'), icon: 'trash' },
  ]
}

/**
 * @param {string} id - the item selected from `chapterMenuItems`
 * @param {import('../api/backend.js').ApiProject} project
 * @param {import('../api/backend.js').ApiChapter} chapter
 */
export function runChapterAction(id, project, chapter) {
  if (id === 'denoise') openDenoise(project, chapter)
  else if (id === 'denoiseCleaned') openDenoise(project, chapter, { cleaned: true })
  else if (id === 'replaceDenoised') confirmReplaceDenoised(project, chapter)
  else if (id === 'denoiseHistory') openDenoiseHistory(project, chapter)
  else if (id === 'delete') confirmDeleteChapter(project, chapter)
}

/**
 * The context menu for one project. Ordered by how often it is reached, with
 * the destructive entry alone at the bottom behind a separator.
 *
 * @param {import('../api/backend.js').ApiProject} project
 * @returns {Array<Object>}
 */
export function projectMenuItems(project) {
  return [
    { id: 'open', label: t('home.action.open'), icon: 'folder' },
    { id: 'newChapter', label: t('home.action.newChapter'), icon: 'plus' },
    { id: 'sep-1', separator: true },
    { id: 'rename', label: t('home.action.rename') },
    { id: 'copyPath', label: t('home.action.copySourcePath') },
    { id: 'sep-2', separator: true },
    { id: 'remove', label: t('home.action.remove'), icon: 'trash' },
  ]
}

/**
 * @param {string} id - the item selected from `projectMenuItems`
 * @param {import('../api/backend.js').ApiProject} project
 */
export function runProjectAction(id, project) {
  switch (id) {
    case 'open':
      openProject(project.id)
      break
    case 'newChapter':
      openNewChapter(project.id)
      break
    case 'rename':
      openRenameProject(project)
      break
    case 'copyPath':
      copySourcePath(project)
      break
    case 'remove':
      confirmRemoveProject(project)
      break
    default:
  }
}
