/**
 * Home's own data layer: the project library.
 *
 * It lives here rather than in `src/lib/state` deliberately - nothing outside
 * Home needs the project list, and putting it in `app.svelte.js` would have
 * made every consumer of the route depend on a load. It is a module rather
 * than component state because Home's dialogs are mounted by the global modal
 * host (`src/lib/shell/ModalHost.svelte`), outside `HomeScreen`'s tree: the
 * New chapter dialog has to see the same projects the grid does.
 *
 * Every mutation re-reads the list from the backend instead of patching the
 * local copy. `listProjects()` returns structured clones, so a local patch is
 * a second implementation of what the backend already did, and the two can
 * disagree. A re-read cannot.
 */

import { getBackend } from '../api/backend.js'
import {
  app,
  goLibrary,
  notify,
  openChapter as routeToChapter,
  openProject,
} from '../state/app.svelte.js'
import { requestResume } from '../state/editor.svelte.js'

/**
 * @typedef {import('../api/backend.js').ApiProject} ApiProject
 */

export const library = $state({
  /** @type {ApiProject[]} */
  projects: [],
  /** @type {'idle'|'loading'|'ready'|'failed'} */
  status: 'idle',
  /** Set while a mutation is in flight, so a dialog's primary action can
   *  disable itself instead of running twice. */
  busy: false,
})

/* Not `$state`: guards, not something the interface renders - and, crucially,
   not reactive reads, so `loadLibrary` can be called straight from an
   `$effect` without the effect tracking what the call then writes. */
let inFlight = false
let everLoaded = false

/* ------------------------------------------------------------------ */
/* Loading                                                             */
/* ------------------------------------------------------------------ */

/**
 * Fetch the library. Safe to call from an `$effect` - it reads no reactive
 * state synchronously, so it cannot re-trigger its own effect.
 *
 * @returns {Promise<void>}
 */
export async function loadLibrary() {
  if (inFlight) return
  inFlight = true
  // Only the very first load is allowed to blank the screen; a refresh after a
  // mutation keeps the current grid on screen until the new one arrives.
  if (!everLoaded) library.status = 'loading'
  try {
    library.projects = await getBackend().listProjects()
    everLoaded = true
    library.status = 'ready'
  } catch {
    library.status = 'failed'
  } finally {
    inFlight = false
  }
}

/**
 * Forward backend notices while Home is the mounted screen. The editor owns
 * the channel while *it* is mounted (`state/editor.svelte.js`); the two screens
 * never coexist, so exactly one subscriber is live at a time and no notice is
 * ever delivered twice.
 *
 * Home ignores run events: it has nothing that renders per-region progress.
 *
 * @returns {() => void} teardown
 */
export function subscribeHomeNotices() {
  return getBackend().subscribe((event) => {
    if (event.type !== 'notice') return
    notify({ key: event.key, params: event.params, tone: event.tone })
  })
}

/* ------------------------------------------------------------------ */
/* Reads                                                               */
/* ------------------------------------------------------------------ */

/**
 * @param {string|null} id
 * @returns {ApiProject|null}
 */
export function projectById(id) {
  if (!id) return null
  return library.projects.find((project) => project.id === id) ?? null
}

/**
 * The chapter a project's interrupted job belongs to, or null.
 *
 * @param {ApiProject|null} project
 * @returns {{chapter: import('../api/backend.js').ApiChapter, pageIndex: number}|null}
 */
export function interruptedChapter(project) {
  const job = project?.interruptedJob
  if (!job) return null
  const chapter = project.chapters.find((c) => c.id === job.chapterId)
  return chapter ? { chapter, pageIndex: job.pageIndex } : null
}

/* ------------------------------------------------------------------ */
/* Mutations                                                           */
/* ------------------------------------------------------------------ */

/**
 * Run one mutation, then re-read the list.
 *
 * A failed mutation is reported as a notice and nothing else. `status` belongs
 * to `loadLibrary()` alone: setting `failed` here would replace a grid full of
 * perfectly valid projects with the load-failed empty state, telling the user
 * the library could not be read when what actually failed was a rename.
 *
 * @template T
 * @param {() => Promise<T>} work
 * @returns {Promise<T|null>} null if the mutation failed or one was in flight
 */
async function mutate(work) {
  if (library.busy) return null
  library.busy = true
  try {
    const result = await work()
    await loadLibrary()
    return result
  } catch {
    notify({ key: 'notice.library.changeFailed', params: {}, tone: 'warn' })
    return null
  } finally {
    library.busy = false
  }
}

/**
 * Create a project and descend into it. The backend announces the mode being
 * fixed (`notice.project.created`); Home does not repeat it.
 *
 * @param {{name: string, mode: 'single'|'longstrip', sourcePath: string}} spec
 * @returns {Promise<ApiProject|null>}
 */
export async function createProject(spec) {
  const project = await mutate(() => getBackend().createProject(spec))
  if (project) openProject(project.id)
  return project
}

/**
 * Add a chapter to a project.
 *
 * `sourcePath` is where the chapter's pages are. Omitting it asks the backend
 * for its default - a subfolder named after the chapter, else the project's own
 * folder - and a backend that cannot give a folder no other chapter of that
 * project already reads answers `null` with a notice saying which one has it.
 * So `null` here is not always "no such
 * project"; it is "no chapter was created", and the reason is on the stack.
 *
 * `number` is the chapter's number, chosen by the user in the dialog and stored
 * verbatim. Omitting it leaves the backend's `max + 1`, which is what the
 * dialog starts the field at.
 *
 * @param {{projectId: string, name: string, number?: number, sourcePath?: string}} spec
 * @returns {Promise<import('../api/backend.js').ApiChapter|null>}
 */
export async function createChapter(spec) {
  const chapter = await mutate(() => getBackend().createChapter(spec))
  if (chapter) openProject(spec.projectId)
  return chapter
}

/**
 * @param {{projectId: string, name: string}} spec
 * @returns {Promise<ApiProject|null>}
 */
export async function renameProject(spec) {
  return mutate(() => getBackend().renameProject(spec))
}

/**
 * Drop a project from the library. The backend's `deleteProject` removes the
 * entry, not the pages on disk.
 *
 * @param {string} projectId
 * @returns {Promise<boolean>}
 */
export async function removeProject(projectId) {
  const wasOpen = app.route.projectId === projectId
  const done = await mutate(() => getBackend().deleteProject({ projectId }))
  // Only leave if the removed project is the one being looked at. Removing one
  // from the grid must not throw the user out of wherever they were.
  if (done && wasOpen) goLibrary()
  return !!done
}

/**
 * Delete a chapter.
 *
 * The row and the library's own files for it always go. `sourceFiles` is the
 * user's answer to the dialog's question and is the only thing that can reach
 * the scans - and even then the backend refuses to remove the project's own
 * folder, so deleting one chapter can never empty the project. The notice says
 * which of those happened.
 *
 * @param {{projectId: string, chapterId: string, sourceFiles?: boolean}} spec
 * @returns {Promise<boolean>} whether a chapter was deleted
 */
export async function removeChapter(spec) {
  const done = await mutate(() => getBackend().deleteChapter(spec))
  // The list the user is looking at is the one that changed.
  if (done) openProject(spec.projectId)
  return !!done
}

/**
 * Take a chapter's denoised files as its pages, then say what changed. The
 * list is read again either way, since the offer may be gone.
 *
 * @param {{chapter: import('../api/backend.js').ApiChapter}} spec
 * @returns {Promise<boolean>} whether pages were replaced
 */
export async function replaceWithDenoised({ chapter }) {
  if (library.busy) return false
  library.busy = true
  const number = chapter.number
  try {
    const report = await getBackend().replaceWithDenoised({ chapterId: chapter.id })
    if (report.failed.length) {
      notify({ key: 'denoise.notice.replaceFailed', params: { number, page: report.failed[0].pageIndex + 1 }, tone: 'warn' })
      return false
    }
    const count = report.replaced.length
    // Pages kept because they changed and pages with no denoised file to take
    // are one figure here: the confirmation already said which was which.
    const kept = report.kept.length + (report.missing?.length ?? 0)
    notify(kept
      ? { key: 'denoise.notice.replacedKept', params: { number, count, kept } }
      : { key: 'denoise.notice.replaced', params: { number, count } })
    return true
  } catch (error) {
    const code = String(error instanceof Error ? error.message : error ?? '')
    notify({
      key: code.includes('job_busy') ? 'notice.job.busy'
        : 'notice.library.changeFailed',
      params: { number },
      tone: 'warn',
    })
    return false
  } finally {
    await loadLibrary()
    library.busy = false
  }
}

/**
 * Continue an interrupted job. Home does not start the run: it records the
 * request and routes to the editor, which starts it once the chapter is open
 * (`state/editor.svelte.js#consumeResume`). Starting it here would put a run's
 * events on the channel before the screen that consumes them exists - which
 * happens to work with the mock's timings and is a race with any other
 * backend.
 *
 * @param {string} projectId
 * @returns {boolean} whether a resumable job was found
 */
export function resumeProject(projectId) {
  const target = interruptedChapter(projectById(projectId))
  if (!target) return false
  requestResume(projectId, target.chapter.id)
  routeToChapter(projectId, target.chapter.id)
  return true
}

/**
 * The source folder. A real backend would reveal it in the file manager;
 * there is no such method on the adapter (see the task report), so the path
 * goes to the clipboard, which is a thing the interface can honestly do.
 *
 * @param {ApiProject} project
 * @returns {Promise<void>}
 */
export async function copySourcePath(project) {
  try {
    await globalThis.navigator?.clipboard?.writeText(project.sourcePath)
    notify({ key: 'notice.project.sourcePathCopied', params: { path: project.sourcePath } })
  } catch {
    notify({ key: 'notice.project.sourcePathCopyFailed', params: {}, tone: 'warn' })
  }
}
