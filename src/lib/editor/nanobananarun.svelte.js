/**
 * Clean the stored detections of a page or a chapter with Nano Banana.
 *
 * One region at a time through the same `cleanDetected` the region menu's
 * Clean with > Nano Banana uses, so a batch is that click repeated: its undo
 * step and its refusal notice come with it. Nothing starts before the person
 * has seen the count and the cost, because every region is a paid request to
 * Google, and nothing goes past what they saw: the batch cleans only the
 * regions it counted, and reads each one again just before it is sent, so a
 * region deleted, cleaned or changed meanwhile is skipped.
 *
 * **One undo step per region, not one for the batch.** The native side
 * records each clean as its own step when the render commits, and Auto clean
 * on one region carries no group to join them by (`history.rs#record_pages`
 * groups only a run's detection steps). Grouping needs that on the native
 * side; nothing here can merge steps the journal has already written.
 */
import { notify } from '../state/app.svelte.js'
import { claimRunStart, editor, pages, reloadPage, releaseRunStart } from '../state/editor.svelte.js'
import { nanoBananaRun } from '../state/jobs.svelte.js'
import { getBackend } from '../api/backend.js'
import { isDetected } from '../model/masks.js'
import { cleanDetected } from './maskactions.svelte.js'
import { confirmNanoBanana } from './nanobananaask.svelte.js'

/** Failures in a row before a batch stops: a bad key must not fail a whole chapter. */
const STOP_AFTER_FAILURES = 3

/**
 * The regions a batch of this scope would clean, read from the backend: the
 * editor holds only the pages near the one on screen.
 *
 * @param {'page'|'chapter'} scope
 * @param {import('../api/backend.js').Backend} backend
 * @returns {Promise<Array<{pageIndex: number, id: string}>>}
 */
async function targetsOf(scope, backend) {
  const indices = pages()
    .filter((page) => page.status === 'detected' && (scope !== 'page' || page.index === editor.pageIndex))
    .map((page) => page.index)
  if (!indices.length) return []
  const loaded = await backend.loadPages({ chapterId: editor.chapter.id, indices })
  return (loaded ?? []).flatMap((page) => (page.regions ?? [])
    .filter(isDetected)
    .map((region) => ({ pageIndex: page.index, id: region.id })))
}

/**
 * The target as the page holds it now, read into the editor so the clean
 * finds it there, or null when it is no longer a detection.
 *
 * @param {{pageIndex: number, id: string}} target
 * @returns {Promise<import('../api/backend.js').ApiRegion|null>}
 */
async function stillDetected(target) {
  await reloadPage(target.pageIndex)
  const region = pages()[target.pageIndex]?.regions?.find((candidate) => candidate.id === target.id)
  return region && isDetected(region) ? region : null
}

/**
 * Ask, then clean. Answers how many regions were cleaned. Never rejects:
 * ToolBar's button fires it and forgets it.
 *
 * @param {'page'|'chapter'} scope
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<number>}
 */
export async function startNanoBananaRun(scope, backend = getBackend()) {
  const token = claimRunStart()
  if (!token) return 0
  try {
    const chapter = editor.chapter
    /** @type {Array<{pageIndex: number, id: string}>} */
    let targets
    try {
      targets = await targetsOf(scope, backend)
    } catch (error) {
      console.error('the regions for a Nano Banana batch could not be read', error)
      notify({ key: 'nanoBanana.run.readFailed', tone: 'warn' })
      return 0
    }
    if (targets.length === 0) {
      notify({ key: 'notice.run.nothingDetected', tone: 'warn' })
      return 0
    }
    if (!(await confirmNanoBanana(targets.length, backend)) || editor.chapter !== chapter) return 0
    Object.assign(nanoBananaRun, { active: true, stopping: false, done: 0, total: targets.length })
    let cleaned = 0
    let failures = 0
    for (const target of targets) {
      if (nanoBananaRun.stopping || editor.chapter !== chapter) break
      /** @type {import('../api/backend.js').ApiRegion|null} */
      let region = null
      let readFailed = false
      try {
        region = await stillDetected(target)
      } catch (error) {
        console.error('a page could not be read before its Nano Banana request', error)
        readFailed = true
      }
      if (nanoBananaRun.stopping || editor.chapter !== chapter) break
      if (region && (await cleanDetected(region, 'nanobanana'))) {
        cleaned += 1
        failures = 0
      } else if ((region || readFailed) && !nanoBananaRun.stopping && ++failures >= STOP_AFTER_FAILURES) {
        notify({ key: 'nanoBanana.run.stoppedFailing', tone: 'warn' })
        break
      }
      nanoBananaRun.done += 1
    }
    notify({ key: 'nanoBanana.run.finished', params: { cleaned, total: targets.length }, tone: 'info' })
    return cleaned
  } finally {
    Object.assign(nanoBananaRun, { active: false, stopping: false })
    releaseRunStart(token)
  }
}
