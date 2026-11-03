/**
 * Clean the stored detections of a page or a chapter with Nano Banana.
 *
 * One region at a time through the same `cleanAnyway` a region's own menu uses,
 * so a batch is that click repeated: its undo step, its refusal notice, and the
 * native gates all come with it. Nothing starts before the person has seen the
 * count and the cost, because every region is a paid request to Google.
 */
import { notify } from '../state/app.svelte.js'
import { claimRunStart, editor, pages, releaseRunStart } from '../state/editor.svelte.js'
import { getBackend } from '../api/backend.js'
import { isDetected } from '../model/masks.js'
import { cleanAnyway } from './maskactions.svelte.js'
import { confirmNanoBanana } from './nanobananaask.svelte.js'

/** Failures in a row before a batch stops: a bad key must not fail a whole chapter. */
const STOP_AFTER_FAILURES = 3

export const nanoBananaRun = $state({ active: false, stopping: false, done: 0, total: 0 })

/**
 * The regions a batch of this scope would clean, read from the backend: the
 * editor holds only the pages near the one on screen.
 *
 * @param {'page'|'chapter'} scope
 * @param {import('../api/backend.js').Backend} backend
 */
async function targetsOf(scope, backend) {
  const indices = pages()
    .filter((page) => page.status === 'detected' && (scope !== 'page' || page.index === editor.pageIndex))
    .map((page) => page.index)
  if (!indices.length) return []
  const loaded = await backend.loadPages({ chapterId: editor.chapter.id, indices })
  return (loaded ?? []).flatMap((page) => page.regions ?? []).filter(isDetected)
}

/**
 * Ask, then clean. Answers how many regions were cleaned.
 *
 * @param {'page'|'chapter'} scope
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<number>}
 */
export async function startNanoBananaRun(scope, backend = getBackend()) {
  const count = (await targetsOf(scope, backend)).length
  if (count === 0) {
    notify({ key: 'notice.run.nothingDetected', tone: 'warn' })
    return 0
  }
  const token = claimRunStart()
  if (!token) return 0
  try {
    const chapter = editor.chapter
    if (!(await confirmNanoBanana(count, backend)) || editor.chapter !== chapter) return 0
    // Read again: the page may have changed while the question was up.
    const targets = await targetsOf(scope, backend)
    Object.assign(nanoBananaRun, { active: true, stopping: false, done: 0, total: targets.length })
    let cleaned = 0
    let failures = 0
    for (const region of targets) {
      if (nanoBananaRun.stopping || editor.chapter !== chapter) break
      if (await cleanAnyway(region, 'nanobanana')) {
        cleaned += 1
        failures = 0
      } else if (++failures >= STOP_AFTER_FAILURES) {
        notify({ key: 'nanoBanana.run.stoppedFailing', tone: 'warn' })
        break
      }
      nanoBananaRun.done += 1
    }
    notify({ key: 'nanoBanana.run.finished', params: { cleaned, total: targets.length }, tone: 'info' })
    return cleaned
  } finally {
    nanoBananaRun.active = false
    releaseRunStart(token)
  }
}

/** Finish the region in flight and start no other. */
export function stopNanoBananaRun() {
  nanoBananaRun.stopping = true
}
