/**
 * Clean the stored detections of a page or a chapter with Nano Banana.
 *
 * The Run button starts this when Clean on is Nano Banana
 * (`cloudrun.js#startCleanRun`). Nothing starts before the person has seen
 * the count and the cost (`nanobananaask.svelte.js`), because every region is
 * a paid request to Google, and nothing goes past what they saw: the window's
 * grant covers exactly the regions counted here, and the batch is handed
 * exactly those ids.
 *
 * **The batch is a native job** (`src-tauri/src/inference/nano_banana_job.rs`).
 * It holds the run slot like a cloud clean, so the editor follows it as a run
 * (`adoptRun`, kind `nanoBanana`): the Run button is its Stop, the jobs list
 * shows it and can stop it, it goes on when the editor leaves the chapter,
 * and the quit guard counts it. Native reads each region again just before
 * it goes and skips one that is no longer a detection. A refused crop is
 * skipped with its reason and the batch goes on; a bad key or missing
 * billing stops it at once; a rate limit is waited for. Its cleans are one
 * undo step. How it ended arrives as `nano-banana-report`, which the jobs
 * store keeps on the job for the summary (`NanoBananaSummaryDialog.svelte`)
 * and for `retryNanoBanana`.
 */
import { notify, pushModal } from '../state/app.svelte.js'
import { adoptRun, claimRunStart, editor, pages, releaseRunStart } from '../state/editor.svelte.js'
import { jobById } from '../state/jobs.svelte.js'
import { getBackend } from '../api/backend.js'
import { hasKey } from '../i18n/index.js'
import { isDetected } from '../model/masks.js'
import { confirmNanoBanana } from './nanobananaask.svelte.js'

/**
 * The stored detections among `indices`' pages, read from the backend: the
 * editor holds only the pages near the one on screen. With `only`, just those
 * ids.
 *
 * @param {number[]} indices
 * @param {import('../api/backend.js').Backend} backend
 * @param {Set<string>} [only]
 * @returns {Promise<string[]>}
 */
async function detectedOn(indices, backend, only) {
  if (!indices.length || !editor.chapter) return []
  const loaded = await backend.loadPages({ chapterId: editor.chapter.id, indices })
  return (loaded ?? []).flatMap((page) => (page.regions ?? [])
    .filter((region) => isDetected(region) && (!only || only.has(region.id)))
    .map((region) => region.id))
}

/**
 * The regions a batch of this scope would clean.
 *
 * @param {'page'|'chapter'} scope
 * @param {import('../api/backend.js').Backend} backend
 * @returns {Promise<string[]>}
 */
function targetsOf(scope, backend) {
  const indices = pages()
    .filter((page) => page.status === 'detected' && (scope !== 'page' || page.index === editor.pageIndex))
    .map((page) => page.index)
  return detectedOn(indices, backend)
}

/**
 * The window, then the job. Answers the run id, or null when nothing started.
 *
 * @param {NonNullable<typeof editor.chapter>} chapter
 * @param {string[]} regionIds
 * @param {'page'|'chapter'} scope
 * @param {import('../api/backend.js').Backend} backend
 * @returns {Promise<string|null>}
 */
async function startBatch(chapter, regionIds, scope, backend) {
  if (regionIds.length === 0) {
    notify({ key: 'notice.run.nothingDetected', tone: 'warn' })
    return null
  }
  const grant = await confirmNanoBanana(regionIds.length, chapter.id, backend)
  if (!grant) return null
  // The grant names this chapter's regions, and the run is followed as the
  // open chapter's: a chapter changed meanwhile starts nothing.
  if (editor.chapter !== chapter || editor.run.active) {
    notify({ key: 'nanoBanana.job.notStarted', tone: 'warn' })
    return null
  }
  /** @type {import('../api/backend.js').RunHandle} */
  let handle
  try {
    handle = await backend.startNanoBananaRun({ chapterId: chapter.id, regionIds, nanoBananaGrant: grant })
  } catch (error) {
    const reason = (error instanceof Error ? error.message : String(error ?? '')).replace(/^Error:\s*/, '')
    console.error('a Nano Banana batch could not start', error)
    notify({ key: hasKey(reason) ? reason : 'nanoBanana.job.notStarted', tone: 'warn' })
    return null
  }
  return adoptRun(handle, scope, 'clean', 'nanoBanana')
}

/**
 * Ask, then start the batch. Never rejects: the Run button fires it and
 * forgets it.
 *
 * @param {'page'|'chapter'} scope
 * @param {import('../api/backend.js').Backend} [backend]
 * @param {object|null} [pendingToken] - the caller's run-start claim, when it took one
 * @returns {Promise<string|null>} the run id, or null when nothing started
 */
export async function startNanoBananaRun(scope, backend = getBackend(), pendingToken = null) {
  const token = claimRunStart(pendingToken)
  if (!token) return null
  try {
    const chapter = editor.chapter
    if (!chapter) return null
    /** @type {string[]} */
    let targets
    try {
      targets = await targetsOf(scope, backend)
    } catch (error) {
      console.error('the regions for a Nano Banana batch could not be read', error)
      notify({ key: 'nanoBanana.run.readFailed', tone: 'warn' })
      return null
    }
    if (editor.chapter !== chapter) return null
    return await startBatch(chapter, targets, scope, backend)
  } finally {
    releaseRunStart(token)
  }
}

/**
 * The regions a finished batch did not clean and may be sent again: the ones
 * refused, and the ones it did not reach.
 *
 * @param {import('../api/backend.js').NanoBananaReport|null|undefined} report
 * @returns {Array<{regionId: string, pageIndex: number}>}
 */
export function retryable(report) {
  if (!report) return []
  return [...(report.failed ?? []), ...(report.left ?? [])]
}

/**
 * Send a finished batch's regions that were not cleaned again, as a new
 * batch behind a new window. Only on the batch's chapter, open in the editor,
 * and only the ones still detections now. Never rejects.
 *
 * @param {string} runId
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<string|null>} the new run's id
 */
export async function retryNanoBanana(runId, backend = getBackend()) {
  const job = jobById(runId)
  const chapter = editor.chapter
  if (!job || job.status === 'running' || !chapter || chapter.id !== job.chapterId) return null
  const wanted = retryable(job.nanoBanana)
  if (!wanted.length) return null
  const token = claimRunStart()
  if (!token) return null
  try {
    /** @type {string[]} */
    let still
    try {
      const indices = [...new Set(wanted.map((entry) => entry.pageIndex))].sort((a, b) => a - b)
      still = await detectedOn(indices, backend, new Set(wanted.map((entry) => entry.regionId)))
    } catch (error) {
      console.error('the regions for a Nano Banana retry could not be read', error)
      notify({ key: 'nanoBanana.run.readFailed', tone: 'warn' })
      return null
    }
    if (editor.chapter !== chapter) return null
    return await startBatch(chapter, still, 'chapter', backend)
  } finally {
    releaseRunStart(token)
  }
}

/**
 * Show how a finished batch ended (`NanoBananaSummaryDialog.svelte`). Nothing
 * when the job is gone or has no report yet.
 *
 * @param {string} runId
 * @returns {boolean} whether the summary opened
 */
export function openNanoBananaSummary(runId) {
  const job = jobById(runId)
  if (!job?.nanoBanana) return false
  pushModal({ kind: 'nanoBananaSummary', titleKey: 'nanoBanana.job.summaryTitle', props: { runId } })
  return true
}
