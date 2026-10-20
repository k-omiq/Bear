/**
 * The region menu's Text type on a detected region, against the browser mock
 * with every delay zero, and against a stub backend for its refusals.
 *
 * Picking the other kind flips the stored detection's balloon answer and the
 * colour its mask is drawn in; picking the kind it has sends nothing; a
 * region that is not a detection is never sent; a refusal is said, and the
 * region is left as it was.
 *
 * And its Approve mask on a held region: the region becomes a detection with
 * Text cleanup's padding, nothing is cleaned, and the selection follows it.
 *
 * And its Reject result on a cleaned detection: the detection comes back with
 * its mask, the backend records the step, and a refusal is said.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { createHistory, settled } from '../model/history.js'
import { maskColorFor, regionMenuSections } from '../model/masks.js'
import { app, clearNotices } from '../state/app.svelte.js'
import {
  closeEditorChapter,
  editor,
  openEditorChapter,
  redo,
  redoLabelKey,
  reloadPage,
  select,
  undo,
  undoAvailable,
  undoLabelKey,
} from '../state/editor.svelte.js'
import { session } from '../state/session.svelte.js'
import { runRegionMenuItem, setDetectedMaskPadding } from './maskactions.svelte.js'
import { applyActiveToolToRegion } from './toolapply.svelte.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const CHAPTER = 'tsuki-to-hane-ch107'
const INDICES = [0, 1, 2, 3, 4, 5]

afterEach(() => {
  clearNotices()
  editor.chapter = null
  setBackend(null)
  vi.restoreAllMocks()
})

/**
 * The region as the open chapter holds it now.
 *
 * @param {string} regionId
 * @returns {any}
 */
function current(regionId) {
  for (const page of editor.chapter?.pages ?? []) {
    const found = page.regions.find((/** @type {any} */ candidate) => candidate.id === regionId)
    if (found) return found
  }
  return null
}

/** A mock whose first page with text has been through Detect, open in the editor. */
async function detected() {
  const backend = createMockBackend({ timing: ZERO })
  setBackend(backend)
  const pages = await backend.loadPages({ chapterId: CHAPTER, indices: INDICES })
  const page = /** @type {any} */ (pages.find((candidate) => candidate.regions.some((region) => region.outcome === 'pending')))
  let done = () => {}
  const finished = new Promise((resolve) => { done = /** @type {any} */ (resolve) })
  const stop = backend.subscribe((event) => { if (event.type === 'run-finished') done() })
  await backend.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: page.index, mode: 'detect' })
  await finished
  stop()
  editor.chapter = /** @type {any} */ ({ id: CHAPTER, review: [], pages: await backend.loadPages({ chapterId: CHAPTER, indices: INDICES }) })
  const regions = editor.chapter.pages.flatMap((/** @type {any} */ candidate) => candidate.regions)
  return { backend, regions }
}

describe('Text type on a detected region', () => {
  it('moves speech bubble text outside, redraws its mask in the outside colour, and back', async () => {
    const { backend, regions } = await detected()
    const bubble = regions.find((/** @type {any} */ region) => region.outcome === 'detected' && region.insideBubble === true && region.pick === 'fill')
    expect(bubble, 'a detection inside a bubble, starting on Fill').toBeTruthy()
    expect(maskColorFor(bubble, session)).toBe(session.maskColor)
    const call = vi.spyOn(backend, 'setDetectionType')

    expect(await runRegionMenuItem('type:outside', bubble)).toBe(true)
    expect(call).toHaveBeenCalledWith({ regionId: bubble.id, inside: false })
    const outside = current(bubble.id)
    expect(outside).toMatchObject({ outcome: 'detected', insideBubble: false, pick: 'lama' })
    expect(maskColorFor(outside, session)).toBe(session.outsideMaskColor)

    expect(await runRegionMenuItem('type:inside', outside)).toBe(true)
    const inside = current(bubble.id)
    expect(inside).toMatchObject({ outcome: 'detected', insideBubble: true, pick: 'lama' })
    expect(maskColorFor(inside, session)).toBe(session.maskColor)
    expect(app.notices.filter((notice) => notice.tone === 'warn')).toEqual([])
  })

  it('sends nothing for the type the region already has', async () => {
    const { backend, regions } = await detected()
    const bubble = regions.find((/** @type {any} */ region) => region.outcome === 'detected' && region.insideBubble === true)
    const call = vi.spyOn(backend, 'setDetectionType')
    expect(await runRegionMenuItem('type:inside', bubble)).toBe(false)
    expect(call).not.toHaveBeenCalled()
    expect(current(bubble.id).insideBubble).toBe(true)
  })

  it('never sends a region that is not a detection', async () => {
    const setDetectionType = vi.fn()
    setBackend(/** @type {any} */ ({ setDetectionType }))
    const cleaned = { id: 'c1-p001-r1', outcome: 'cleaned', mask: { id: 'c1-p001-r1-m1', provenance: { engine: 'fill' } } }
    const held = { id: 'c1-p001-r2', outcome: 'candidate', candidateInsideBubble: false, mask: null }
    for (const region of [cleaned, held]) {
      expect(await runRegionMenuItem('type:inside', /** @type {any} */ (region))).toBe(false)
      expect(await runRegionMenuItem('type:outside', /** @type {any} */ (region))).toBe(false)
    }
    expect(setDetectionType).not.toHaveBeenCalled()
  })

  it('offers both ways for a detection with no answer, and sends either', async () => {
    const answer = { id: 'c1-p001-d1', outcome: 'detected', insideBubble: false }
    const setDetectionType = vi.fn(async () => answer)
    setBackend(/** @type {any} */ ({ setDetectionType }))
    const unknown = /** @type {any} */ ({ id: 'c1-p001-d1', outcome: 'detected', insideBubble: null })
    expect(await runRegionMenuItem('type:outside', unknown)).toBe(true)
    expect(await runRegionMenuItem('type:inside', unknown)).toBe(true)
    expect(setDetectionType.mock.calls.map(([spec]) => spec)).toEqual([
      { regionId: 'c1-p001-d1', inside: false },
      { regionId: 'c1-p001-d1', inside: true },
    ])
  })

  it('says a refusal and leaves the region as it was', async () => {
    const setDetectionType = vi.fn(async () => { throw new Error('not_a_detection: c1-p001-d1 is not a stored detection') })
    setBackend(/** @type {any} */ ({ setDetectionType }))
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const region = /** @type {any} */ ({ id: 'c1-p001-d1', outcome: 'detected', insideBubble: true })
    expect(await runRegionMenuItem('type:outside', region)).toBe(false)
    expect(app.notices.at(-1)).toMatchObject({ key: 'notice.mask.typeFailed', tone: 'warn' })
    expect(region.insideBubble).toBe(true)
  })

  it('says a chapter a run holds in its own words', async () => {
    const setDetectionType = vi.fn(async () => { throw new Error('job_busy: another Manga Cleaner process is using ch.mtclean') })
    setBackend(/** @type {any} */ ({ setDetectionType }))
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const region = /** @type {any} */ ({ id: 'c1-p001-d1', outcome: 'detected', insideBubble: false })
    expect(await runRegionMenuItem('type:inside', region)).toBe(false)
    expect(app.notices.at(-1)?.key).toBe('notice.job.busy')
  })
})


describe('padding on one detected mask', () => {
  it('changes only the chosen region, reloads its mask, and restores zero padding', async () => {
    const { backend, regions } = await detected()
    const target = regions.find((region) => region.outcome === 'detected')
    const original = structuredClone(target)
    const siblings = regions.filter((region) => region.id !== target.id).map((region) => structuredClone(region))
    const page = editor.chapter.pages.find((page) => page.regions.some((region) => region.id === target.id))
    const call = vi.spyOn(backend, 'setDetectionPadding')
    expect(await setDetectedMaskPadding(target, 6)).toBe(true)
    expect(call).toHaveBeenCalledWith({ chapterId: CHAPTER, pageIndex: page.index, regionId: target.id, paddingPx: 6 })
    expect(current(target.id).paddingPx).toBe(6)
    expect(current(target.id).mask.provenance.mask_sha256).not.toBe(original.mask.provenance.mask_sha256)
    expect(siblings.map((region) => current(region.id))).toEqual(siblings)
    expect(await setDetectedMaskPadding(current(target.id), 0)).toBe(true)
    expect(current(target.id).bbox).toEqual(original.bbox)
    expect(current(target.id).paddingPx ?? 0).toBe(0)
  })

  it('rejects non-detections and invalid values before calling the backend', async () => {
    const { backend, regions } = await detected()
    const target = regions.find((region) => region.outcome === 'detected')
    const call = vi.spyOn(backend, 'setDetectionPadding')
    for (const padding of [-1, 33, 1.5, NaN]) expect(await setDetectedMaskPadding(target, padding)).toBe(false)
    expect(await setDetectedMaskPadding({ ...target, outcome: 'cleaned', detected: false }, 4)).toBe(false)
    expect(call).not.toHaveBeenCalled()
  })

  it('reports a refused write and leaves the saved region alone', async () => {
    const { backend, regions } = await detected()
    const target = regions.find((region) => region.outcome === 'detected')
    const original = structuredClone(target)
    vi.spyOn(backend, 'setDetectionPadding').mockRejectedValue(new Error('mask_padding_target_invalid'))
    vi.spyOn(console, 'error').mockImplementation(() => {})
    expect(await setDetectedMaskPadding(target, 4)).toBe(false)
    expect(current(target.id)).toEqual(original)
    expect(app.notices.at(-1)).toMatchObject({ key: 'notice.mask.paddingFailed', tone: 'warn' })
  })
})


it('reads the page again and says so when the padding ran the mask into another', async () => {
  const { backend, regions } = await detected()
  const [kept, target] = regions.filter((region) => region.outcome === 'detected')
  const page = editor.chapter.pages.find((page) => page.regions.some((region) => region.id === target.id))
  vi.spyOn(backend, 'setDetectionPadding').mockResolvedValue({ changed: [kept.id], removed: [target.id], pages: [page.index] })
  const read = vi.spyOn(backend, 'loadPages')
  expect(await setDetectedMaskPadding(target, 8)).toBe(true)
  expect(read).toHaveBeenCalledWith({ chapterId: CHAPTER, indices: [page.index] })
  expect(app.notices.at(-1)).toMatchObject({ key: 'notice.mask.paddingMerged', params: { count: 1 } })
})

it('keeps a saved padding when the page refresh fails, and reports the refresh separately', async () => {
  const { backend, regions } = await detected()
  const target = regions.find((region) => region.outcome === 'detected')
  vi.spyOn(backend, 'loadPages').mockRejectedValue(new Error('read failed'))
  vi.spyOn(console, 'error').mockImplementation(() => {})
  expect(await setDetectedMaskPadding(target, 5)).toBe(true)
  expect(current(target.id).paddingPx).toBe(5)
  expect(app.notices.at(-1)).toMatchObject({ key: 'notice.mask.paddingRefreshFailed', tone: 'warn' })
})

describe('Approve mask on a held region', () => {
  const HELD = 'wandering-moon-ch12'

  /** The fixture chapter with a region the gate held outside a bubble, open in the editor. */
  async function held() {
    const backend = createMockBackend({ timing: ZERO })
    setBackend(backend)
    const pages = await backend.loadPages({ chapterId: HELD, indices: [0, 1, 2, 3, 4, 5, 6] })
    editor.chapter = /** @type {any} */ ({ id: HELD, review: [], pages })
    const region = editor.chapter.pages.flatMap((/** @type {any} */ page) => page.regions)
      .find((/** @type {any} */ candidate) => candidate.gateSkipCause === 'outside-bubble')
    expect(region, 'a region the gate held outside a bubble').toBeTruthy()
    return { backend, region }
  }

  afterEach(() => {
    editor.toolParams.autoClean.maskPadding = 0
    select(null)
  })

  it('stores the held region as a detection with Text cleanup\'s padding, and cleans nothing', async () => {
    const { backend, region } = await held()
    editor.toolParams.autoClean.maskPadding = 6
    select(region.id)
    const approve = vi.spyOn(backend, 'approveHeldMask')
    const clean = vi.spyOn(backend, 'cleanAnyway')

    expect(await runRegionMenuItem('approveMask', region)).toBe(true)
    expect(approve).toHaveBeenCalledWith({ regionId: region.id, paddingPx: 6 })
    expect(clean).not.toHaveBeenCalled()
    const stored = current(region.id)
    expect(stored).toMatchObject({ outcome: 'detected', insideBubble: false, paddingPx: 6 })
    expect(stored.mask).toBeTruthy()
    expect(maskColorFor(stored, session)).toBe(session.outsideMaskColor)
    expect(editor.selectionId).toBe(region.id)
    expect(app.notices.filter((notice) => notice.tone === 'warn')).toEqual([])
  })

  it('lets go of a selection whose mask is not on the page it read again', async () => {
    const { backend, region } = await held()
    select(region.id)
    // The native answer names a new id; one a padding merged away is on no page.
    vi.spyOn(backend, 'approveHeldMask').mockResolvedValue({ regionId: 'gone-p001-d9', pageStatus: 'detected' })
    expect(await runRegionMenuItem('approveMask', region)).toBe(true)
    expect(editor.selectionId).toBeNull()
  })

  it('never sends a region nothing held', async () => {
    const approveHeldMask = vi.fn()
    setBackend(/** @type {any} */ ({ approveHeldMask }))
    const detection = { id: 'c1-p001-d1', outcome: 'detected', insideBubble: true }
    const cleaned = { id: 'c1-p001-r1', outcome: 'cleaned', mask: { id: 'c1-p001-r1-m1', provenance: { engine: 'fill' } } }
    const declined = { id: 'c1-p001-u1-2-3-4', outcome: 'declined', mask: null }
    for (const region of [detection, cleaned, declined]) {
      expect(await runRegionMenuItem('approveMask', /** @type {any} */ (region))).toBe(false)
    }
    expect(approveHeldMask).not.toHaveBeenCalled()
  })

  it('says a refusal, and a chapter a run holds in its own words', async () => {
    const { backend, region } = await held()
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const approve = vi.spyOn(backend, 'approveHeldMask').mockRejectedValueOnce(new Error('disk full'))
    expect(await runRegionMenuItem('approveMask', region)).toBe(false)
    expect(app.notices.at(-1)).toMatchObject({ key: 'notice.mask.approveFailed', tone: 'warn' })
    approve.mockRejectedValueOnce(new Error('job_busy: another Manga Cleaner process is using ch.mtclean'))
    expect(await runRegionMenuItem('approveMask', region)).toBe(false)
    expect(app.notices.at(-1)?.key).toBe('notice.job.busy')
    expect(current(region.id)).toMatchObject({ outcome: 'gate-skipped', mask: null })
  })
})

/**
 * Reject result on a cleaned detection, against the browser mock: the region
 * is detected again with the mask it had, the backend records the step and
 * this side records nothing, and Undo puts the same result back.
 */
describe('Reject result on a cleaned detection', () => {
  /** A region out of the open chapter as plain data, to compare by value. */
  const plain = (/** @type {any} */ value) => JSON.parse(JSON.stringify(value))

  /** A detection cleaned here, as the open chapter holds it, with the detection it was. */
  async function cleaned() {
    const { backend, regions } = await detected()
    const detection = plain(regions.find((/** @type {any} */ region) => region.outcome === 'detected'))
    editor.history = createHistory()
    expect(await runRegionMenuItem('engine:fill', current(detection.id))).toBe(true)
    const result = plain(current(detection.id))
    expect(result).toMatchObject({ outcome: 'cleaned', source: 'auto' })
    return { backend, detection, result }
  }

  afterEach(() => {
    editor.history = createHistory()
  })

  it('offers the entry on the result, and not on the detection it was', async () => {
    const { detection, result } = await cleaned()
    const ids = (/** @type {any} */ region) => regionMenuSections(region).flatMap((section) => section.items.map((item) => item.id))
    expect(ids(result)).toContain('reject')
    expect(ids(detection)).not.toContain('reject')
  })

  it('puts the detection back with its mask and adopts the step the backend recorded', async () => {
    const { backend, detection, result } = await cleaned()
    const reject = vi.spyOn(backend, 'rejectResult')
    const push = vi.spyOn(backend, 'historyPush')

    expect(await runRegionMenuItem('reject', current(detection.id))).toBe(true)
    expect(reject).toHaveBeenCalledWith({ regionId: detection.id })
    expect(plain(current(detection.id))).toEqual(detection)
    expect(current(detection.id).mask).toEqual(detection.mask)
    // The backend's own two steps, and none of this side's.
    expect(push).not.toHaveBeenCalled()
    expect(editor.history.cursor).toBe(3)
    expect(editor.history.entries.map((entry) => entry.label))
      .toEqual(['masks.command.detectPages', 'masks.command.cleanRegion', 'masks.command.rejectResult'])
    expect(app.notices.filter((notice) => notice.tone === 'warn')).toEqual([])

    // Undo of the reject is the same result, with no new render.
    const render = vi.spyOn(backend, 'applyTool')
    undo()
    await settled(editor.history)
    expect(plain(current(detection.id))).toEqual(result)
    expect(render).not.toHaveBeenCalled()
    // And Undo of the clean is the detection again.
    undo()
    await settled(editor.history)
    expect(plain(current(detection.id))).toEqual(detection)
    redo()
    await settled(editor.history)
    expect(plain(current(detection.id))).toEqual(result)
    expect(push).not.toHaveBeenCalled()
  })

  it('restores the mask of a result an older build cleaned, and Undo takes it back', async () => {
    // A fixture chapter cleaned from the start, as an older build left one.
    const OLD = 'wandering-moon-ch12'
    const backend = createMockBackend({ timing: ZERO })
    setBackend(backend)
    editor.chapter = /** @type {any} */ ({ id: OLD, review: [], pages: await backend.loadPages({ chapterId: OLD, indices: [0] }) })
    editor.history = createHistory()
    const region = editor.chapter.pages[0].regions.find((/** @type {any} */ candidate) => candidate.outcome === 'cleaned' && candidate.mask)
    const before = plain(region)

    expect(await runRegionMenuItem('reject', region)).toBe(true)
    expect(current(region.id)).toMatchObject({ outcome: 'detected', mask: before.mask })
    expect(editor.history.entries.map((entry) => entry.label)).toEqual(['masks.command.rejectResult'])
    expect(app.notices).toEqual([])
  })

  it('says a layer no cleaner made has no mask to restore, and leaves it as it was', async () => {
    const region = { id: 'p0-d1', pageId: 'p0', source: 'auto', outcome: 'cleaned', mask: { id: 'p0-d1-m1', provenance: { engine: 'fill' } } }
    editor.chapter = /** @type {any} */ ({ id: 'c', review: [], pages: [{ id: 'p0', index: 0, status: 'cleaned', resident: true, regions: [region] }] })
    vi.spyOn(console, 'error').mockImplementation(() => {})
    setBackend(/** @type {any} */ ({ rejectResult: vi.fn().mockRejectedValue(
      new Error('clean_not_restorable: no detection was kept for region p0-d1; its mask cannot be put back')) }))
    expect(await runRegionMenuItem('reject', current('p0-d1'))).toBe(false)
    expect(app.notices.map((notice) => notice.key)).toEqual(['notice.mask.rejectNoMask'])
    expect(app.notices[0].tone).toBe('warn')
    expect(plain(current('p0-d1'))).toEqual(plain(region))
  })

  it('says any other refusal in one sentence, and a page a run holds in its own words', async () => {
    const region = { id: 'p0-d1', pageId: 'p0', source: 'auto', outcome: 'cleaned', mask: { id: 'p0-d1-m1', provenance: { engine: 'fill' } } }
    editor.chapter = /** @type {any} */ ({ id: 'c', review: [], pages: [{ id: 'p0', index: 0, status: 'cleaned', resident: true, regions: [region] }] })
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const rejectResult = vi.fn()
    setBackend(/** @type {any} */ ({ rejectResult }))

    rejectResult.mockRejectedValueOnce(new Error('clean_mask_unreadable: the saved mask of region p0-d1 cannot be read (d.mask: gone)'))
    expect(await runRegionMenuItem('reject', current('p0-d1'))).toBe(false)
    expect(app.notices.at(-1)).toMatchObject({ key: 'notice.mask.rejectFailed', tone: 'warn' })

    rejectResult.mockRejectedValueOnce(new Error('page_busy'))
    expect(await runRegionMenuItem('reject', current('p0-d1'))).toBe(false)
    expect(app.notices.at(-1)).toMatchObject({ key: 'notice.run.pageBusy', tone: 'warn' })

    // No chapter names the region: nothing changed, and nothing to say.
    rejectResult.mockResolvedValueOnce(null)
    const said = app.notices.length
    expect(await runRegionMenuItem('reject', current('p0-d1'))).toBe(false)
    expect(app.notices).toHaveLength(said)
    expect(current('p0-d1')).toMatchObject({ outcome: 'cleaned' })
  })

  it('never sends a hand mask, a detection or a region with no mask', async () => {
    const rejectResult = vi.fn()
    setBackend(/** @type {any} */ ({ rejectResult }))
    const mask = { id: 'p0-h1-m1', provenance: { engine: 'fill' } }
    for (const region of [
      { id: 'p0-h1', source: 'hand', outcome: 'cleaned', mask },
      { id: 'p0-d1', source: 'auto', outcome: 'detected', mask },
      { id: 'p0-d2', source: 'auto', outcome: 'pending', mask: null },
    ]) {
      expect(await runRegionMenuItem('reject', /** @type {any} */ (region))).toBe(false)
    }
    expect(rejectResult).not.toHaveBeenCalled()
  })
})

/**
 * A hand tool clicked onto a detected region cleans it, and the backend
 * records that step as it records every clean of a detection. This side
 * records nothing for it, so the edit is one undo step and not two.
 */
describe('a tool applied to a detected region', () => {
  afterEach(() => {
    editor.history = createHistory()
    editor.tool = 'autoClean'
  })

  it('leaves the undo step to the backend, and still records an edit of a cleaned region', async () => {
    const { backend, regions } = await detected()
    const detection = regions.find((/** @type {any} */ region) => region.outcome === 'detected')
    editor.history = createHistory()
    editor.tool = 'contentAwareFill'
    const push = vi.spyOn(backend, 'historyPush')

    expect(await applyActiveToolToRegion(detection.id)).toBe(true)
    expect(current(detection.id)).toMatchObject({ outcome: 'cleaned' })
    expect(push).not.toHaveBeenCalled()
    const { entry } = await backend.historyMove({ chapterId: CHAPTER, direction: 'undo' })
    expect(entry).toMatchObject({ op: 'clean-state', label: 'masks.command.cleanRegion', regionId: detection.id })
    expect(entry.before.region).toMatchObject({ outcome: 'detected', mask: { id: detection.mask.id } })
    await backend.historyMove({ chapterId: CHAPTER, direction: 'redo' })

    // The same tool on the result is this side's edit, recorded as before.
    expect(await applyActiveToolToRegion(detection.id)).toBe(true)
    await settled(editor.history)
    expect(push).toHaveBeenCalledTimes(1)
    expect(push.mock.calls[0][0].entry).toMatchObject({ op: 'region-state', label: 'canvas.command.applyTool' })
  })
})

/**
 * The whole round as the browser mock serves it to an open editor: Detect,
 * Clean one region, Undo, Redo, Reject result, Undo. The editor hears of the
 * backend's steps on the event channel and replays them through the seam.
 */
describe('clean, undo, redo and reject in an open chapter', () => {
  afterEach(() => {
    closeEditorChapter()
  })

  it('walks a detection to its result and back, with the history following', async () => {
    const backend = createMockBackend({ timing: ZERO })
    setBackend(backend)
    const project = (await backend.listProjects()).find((candidate) => candidate.chapters.some((chapter) => chapter.id === CHAPTER))
    expect(await openEditorChapter(project.id, CHAPTER)).toBe(true)
    const plain = (/** @type {any} */ value) => JSON.parse(JSON.stringify(value))
    const quiet = async () => {
      await settled(editor.history)
      await new Promise((resolve) => setTimeout(resolve, 0))
      await settled(editor.history)
    }

    let done = () => {}
    const finished = new Promise((resolve) => { done = /** @type {any} */ (resolve) })
    const stop = backend.subscribe((event) => { if (event.type === 'run-finished') done() })
    await backend.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode: 'detect' })
    await finished
    stop()
    await reloadPage(0)
    await quiet()
    const detection = plain(editor.chapter.pages[0].regions.find((/** @type {any} */ region) => region.outcome === 'detected'))
    expect(detection, 'a detection on the first page').toBeTruthy()
    // The Detect run is a step of its own, which the backend recorded.
    expect(undoLabelKey()).toBe('masks.command.detectPages')

    // Clean: the backend's step arrives as `history-changed`.
    expect(await runRegionMenuItem('engine:fill', current(detection.id))).toBe(true)
    await quiet()
    const result = plain(current(detection.id))
    expect(result.outcome).toBe('cleaned')
    expect(undoLabelKey()).toBe('masks.command.cleanRegion')

    undo()
    await quiet()
    expect(plain(current(detection.id))).toEqual(detection)
    expect(undoLabelKey()).toBe('masks.command.detectPages')
    expect(redoLabelKey()).toBe('masks.command.cleanRegion')

    redo()
    await quiet()
    expect(plain(current(detection.id))).toEqual(result)

    expect(await runRegionMenuItem('reject', current(detection.id))).toBe(true)
    await quiet()
    expect(plain(current(detection.id))).toEqual(detection)
    expect(undoLabelKey()).toBe('masks.command.rejectResult')
    expect(editor.history.entries).toHaveLength(3)

    undo()
    await quiet()
    expect(plain(current(detection.id))).toEqual(result)
    expect(undoLabelKey()).toBe('masks.command.cleanRegion')

    // Back past the clean and the Detect run: the page holds no detection.
    // (The mock keeps the region's id where the native side mints one, so
    // the row is still there, as it was before Detect.)
    undo()
    await quiet()
    undo()
    await quiet()
    expect(current(detection.id)?.outcome).not.toBe('detected')
    expect(editor.chapter.pages[0].regions.some((/** @type {any} */ region) => region.outcome === 'detected')).toBe(false)
    expect(undoAvailable()).toBe(false)
    // And forward again: the same detection, mask and all.
    redo()
    await quiet()
    expect(plain(current(detection.id))).toEqual(detection)
    expect(app.notices.filter((notice) => notice.tone === 'warn')).toEqual([])
  })

  it('takes back a deleted detection, a text type and a dismissed warning', async () => {
    const backend = createMockBackend({ timing: ZERO })
    setBackend(backend)
    const project = (await backend.listProjects()).find((candidate) => candidate.chapters.some((chapter) => chapter.id === CHAPTER))
    expect(await openEditorChapter(project.id, CHAPTER)).toBe(true)
    const plain = (/** @type {any} */ value) => JSON.parse(JSON.stringify(value))
    const quiet = async () => {
      await settled(editor.history)
      await new Promise((resolve) => setTimeout(resolve, 0))
      await settled(editor.history)
    }
    let done = () => {}
    const finished = new Promise((resolve) => { done = /** @type {any} */ (resolve) })
    const stop = backend.subscribe((event) => { if (event.type === 'run-finished') done() })
    await backend.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode: 'detect' })
    await finished
    stop()
    await reloadPage(0)
    await quiet()
    const regions = () => plain(editor.chapter.pages[0].regions)
    const detection = regions().find((/** @type {any} */ region) => region.outcome === 'detected')
    expect(detection).toBeTruthy()

    // A text type, then a delete: two steps, each undone exactly.
    expect(await runRegionMenuItem(detection.insideBubble ? 'type:outside' : 'type:inside', current(detection.id))).toBe(true)
    await quiet()
    expect(undoLabelKey()).toBe('masks.command.detectionType')
    expect(await runRegionMenuItem('delete', current(detection.id))).toBe(true)
    await quiet()
    expect(current(detection.id)).toBeFalsy()
    expect(undoLabelKey()).toBe('masks.command.deleteDetection')
    undo()
    await quiet()
    expect(current(detection.id)).toBeTruthy()
    undo()
    await quiet()
    expect(plain(current(detection.id))).toEqual(detection)

    // A warning row, dismissed and taken back.
    const page = (await backend.loadPages({ chapterId: CHAPTER, indices: [1] }))[0]
    const warning = page?.regions.find((/** @type {any} */ region) => !region.mask)
    expect(warning, 'a row with no mask on the second page').toBeTruthy()
    await reloadPage(1)
    expect(await runRegionMenuItem('delete', current(warning.id))).toBe(true)
    await quiet()
    expect(current(warning.id)).toBeFalsy()
    expect(undoLabelKey()).toBe('masks.command.deleteRegion')
    undo()
    await quiet()
    await reloadPage(1)
    expect(plain(current(warning.id))).toEqual(plain(warning))
    expect(app.notices.filter((notice) => notice.tone === 'warn')).toEqual([])
  })
})
