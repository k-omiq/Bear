/**
 * The region menu's Text type on a detected region, against the browser mock
 * with every delay zero, and against a stub backend for its refusals.
 *
 * Picking the other kind flips the stored detection's balloon answer and the
 * colour its mask is drawn in; picking the kind it has sends nothing; a
 * region that is not a detection is never sent; a refusal is said, and the
 * region is left as it was.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { maskColorFor } from '../model/masks.js'
import { app, clearNotices } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { session } from '../state/session.svelte.js'
import { runRegionMenuItem, setDetectedMaskPadding } from './maskactions.svelte.js'

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
