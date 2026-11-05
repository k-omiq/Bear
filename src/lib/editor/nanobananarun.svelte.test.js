/**
 * A Nano Banana batch against the browser mock with every delay zero: the
 * question is the real one on the modal stack, and each region goes through
 * the region menu's own `cleanDetected`, so what is checked is what the
 * backend is sent.
 *
 * One Auto clean per region with the Nano Banana engine, and nothing through
 * Clean anyway; only the regions counted when the person was asked; each one
 * read again first, so one cleaned meanwhile is skipped and one on a page the
 * editor is not holding is still found; Stop sends no other and tells the
 * native side; three failures in a row stop it; a page read that fails says
 * so and frees the run slot.
 *
 * Escape and the backdrop on the window itself are driven in
 * `dialogs/NanoBananaConsentDialog.dom.test.js`.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { app, clearNotices, closeModal } from '../state/app.svelte.js'
import { claimRunStart, editor, releaseRunStart } from '../state/editor.svelte.js'
import { nanoBananaRun, resetJobs, stopNanoBananaRun } from '../state/jobs.svelte.js'
import { evictRegions } from '../state/pagewindow.svelte.js'
import { startNanoBananaRun } from './nanobananarun.svelte.js'
import { confirmNanoBanana } from './nanobananaask.svelte.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const CHAPTER = 'tsuki-to-hane-ch107'
const INDICES = [0, 1, 2, 3, 4, 5]

/** @type {any} */
let backend
/** The mock's own Auto clean, for a test that wraps one call. @type {(call: any) => Promise<any>} */
let applyTool

/** Detect a page of the mock and wait for the run to end. @param {number} pageIndex */
async function detectPage(pageIndex) {
  let done = () => {}
  const finished = new Promise((resolve) => { done = /** @type {any} */ (resolve) })
  const stop = backend.subscribe((/** @type {any} */ event) => { if (event.type === 'run-finished') done() })
  await backend.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex, mode: 'detect' })
  await finished
  stop()
}

/** The pages of the mock that have text to detect, by index. */
async function textPages() {
  const pages = await backend.loadPages({ chapterId: CHAPTER, indices: INDICES })
  return pages.filter((/** @type {any} */ page) => page.regions.some((/** @type {any} */ region) => region.outcome === 'pending'))
    .map((/** @type {any} */ page) => page.index)
}

/** Open the chapter in the editor as the backend has it now. */
async function openChapter() {
  editor.chapter = /** @type {any} */ ({ id: CHAPTER, review: [], pages: await backend.loadPages({ chapterId: CHAPTER, indices: INDICES }) })
}

/** The detections the backend holds, by page index. @returns {Promise<Array<{pageIndex: number, id: string}>>} */
async function detections() {
  const pages = await backend.loadPages({ chapterId: CHAPTER, indices: INDICES })
  return pages.flatMap((/** @type {any} */ page) => page.regions
    .filter((/** @type {any} */ region) => region.outcome === 'detected')
    .map((/** @type {any} */ region) => ({ pageIndex: page.index, id: region.id })))
}

/** The regions the batch sent to Nano Banana, by id, in order. */
function sent() {
  return backend.applyTool.mock.calls
    .filter(([/** @type {any} */ call]) => call.tool === 'autoClean' && call.params?.engine === 'nanobanana')
    .map(([/** @type {any} */ call]) => call.regionId)
}

/** Answer the question as soon as it is asked. @param {string} id */
async function answer(id) {
  await vi.waitFor(() => expect(app.modals.length).toBe(1))
  const spec = app.modals[0]
  closeModal({ id, prompt: '' })
  return spec
}

const warnings = () => app.notices.filter((notice) => notice.tone === 'warn').map((notice) => notice.key)

beforeEach(async () => {
  backend = createMockBackend({ timing: ZERO })
  setBackend(backend)
  // Nano Banana is a cloud engine with the person's own key: the consent is
  // granted only with cloud engines on, and a render goes only with a key.
  await backend.writeSettings({ cloudEngines: 'allowed' })
  await backend.storeNanoBananaKey({ key: 'AIzaSyMockMockMockMockMockMockMockMock' })
  const [first, second] = await textPages()
  await detectPage(first)
  await detectPage(second)
  await openChapter()
  editor.pageIndex = first
  applyTool = backend.applyTool.bind(backend)
  vi.spyOn(backend, 'applyTool')
  vi.spyOn(backend, 'cleanAnyway')
})

afterEach(() => {
  clearNotices()
  app.modals.length = 0
  editor.chapter = null
  resetJobs()
  setBackend(null)
  vi.restoreAllMocks()
})

describe('a Nano Banana batch', () => {
  it('cleans each detection of the chapter once, with Nano Banana, and never through Clean anyway', async () => {
    const targets = await detections()
    expect(new Set(targets.map((target) => target.pageIndex)).size).toBe(2)
    const started = startNanoBananaRun('chapter')
    const spec = await answer('confirm')
    expect(spec.props.count).toBe(targets.length)
    expect(await started).toBe(targets.length)
    expect(sent()).toEqual(targets.map((target) => target.id))
    expect(backend.cleanAnyway).not.toHaveBeenCalled()
    expect(warnings()).toEqual([])
    expect(app.notices.map((notice) => notice.key)).toContain('nanoBanana.run.finished')
    expect(await detections()).toEqual([])
    expect(nanoBananaRun.active).toBe(false)
  })

  it('holds a page batch to the page on screen', async () => {
    const onScreen = (await detections()).filter((target) => target.pageIndex === editor.pageIndex)
    const started = startNanoBananaRun('page')
    await answer('confirm')
    await started
    expect(sent()).toEqual(onScreen.map((target) => target.id))
  })

  it('finds a region on a page the editor is not holding', async () => {
    const far = (await detections()).find((target) => target.pageIndex !== editor.pageIndex)
    evictRegions(editor.chapter.pages[/** @type {any} */ (far).pageIndex])
    const started = startNanoBananaRun('chapter')
    await answer('confirm')
    await started
    expect(sent()).toContain(far?.id)
  })

  it('skips a region that stopped being a detection while the batch ran', async () => {
    const targets = await detections()
    backend.applyTool.mockImplementationOnce(async (/** @type {any} */ call) => {
      // The second region is cleaned by hand while the first is in flight.
      await applyTool({ ...call, regionId: targets[1].id, pageIndex: targets[1].pageIndex, params: {} })
      return applyTool(call)
    })
    const started = startNanoBananaRun('chapter')
    await answer('confirm')
    expect(await started).toBe(targets.length - 1)
    expect(sent()).toEqual(targets.filter((_, index) => index !== 1).map((target) => target.id))
  })

  it('cleans no more than the person was asked about', async () => {
    const counted = await detections()
    const started = startNanoBananaRun('chapter')
    await vi.waitFor(() => expect(app.modals.length).toBe(1))
    // More text is detected while the question is up.
    const [, , third] = await textPages()
    await detectPage(third)
    expect((await detections()).length).toBeGreaterThan(counted.length)
    closeModal({ id: 'confirm', prompt: '' })
    expect(await started).toBe(counted.length)
    expect(sent()).toEqual(counted.map((target) => target.id))
  })

  it('sends nothing on Cancel, and frees the run slot', async () => {
    const started = startNanoBananaRun('chapter')
    await answer('cancel')
    expect(await started).toBe(0)
    expect(sent()).toEqual([])
    const token = claimRunStart()
    expect(token).toBeTruthy()
    releaseRunStart(token)
  })

  it('does not start beside a run', async () => {
    const token = claimRunStart()
    expect(await startNanoBananaRun('chapter')).toBe(0)
    expect(app.modals).toEqual([])
    expect(warnings()).toContain('notice.run.busy')
    releaseRunStart(token)
  })

  it('asks nothing when nothing is detected', async () => {
    for (const page of editor.chapter.pages) page.status = 'pending'
    expect(await startNanoBananaRun('chapter')).toBe(0)
    expect(app.modals).toEqual([])
    expect(warnings()).toContain('notice.run.nothingDetected')
  })

  it('says so when the regions cannot be read, and frees the run slot', async () => {
    vi.spyOn(backend, 'loadPages').mockRejectedValue(new Error('disk'))
    expect(await startNanoBananaRun('chapter')).toBe(0)
    expect(app.modals).toEqual([])
    expect(warnings()).toContain('nanoBanana.run.readFailed')
    const token = claimRunStart()
    expect(token).toBeTruthy()
    releaseRunStart(token)
  })

  it('stops after three failures in a row, so a bad key cannot fail a chapter', async () => {
    expect((await detections()).length).toBeGreaterThan(3)
    backend.applyTool.mockRejectedValue(new Error('decline.reason.nanoBananaKeyRejected'))
    const started = startNanoBananaRun('chapter')
    await answer('confirm')
    expect(await started).toBe(0)
    expect(sent()).toHaveLength(3)
    expect(warnings()).toContain('nanoBanana.run.stoppedFailing')
    expect(nanoBananaRun.active).toBe(false)
  })

  it('on Stop finishes the region in flight, starts no other, and tells the native side', async () => {
    backend.nanoBananaStop = vi.fn(async () => {})
    /** @type {any} */
    let during = null
    backend.applyTool.mockImplementationOnce(async (/** @type {any} */ call) => {
      stopNanoBananaRun()
      during = { ...nanoBananaRun }
      return applyTool(call)
    })
    const started = startNanoBananaRun('chapter')
    await answer('confirm')
    expect(await started).toBe(1)
    expect(sent()).toHaveLength(1)
    expect(during).toMatchObject({ active: true, stopping: true })
    await vi.waitFor(() => expect(backend.nanoBananaStop).toHaveBeenCalledTimes(1))
    expect(nanoBananaRun).toMatchObject({ active: false, stopping: false })
    expect(warnings()).not.toContain('nanoBanana.run.stoppedFailing')
  })

  it('does not count a request Stop cut short as a failure', async () => {
    backend.applyTool
      .mockImplementationOnce(applyTool)
      .mockImplementationOnce(async () => {
        stopNanoBananaRun()
        throw new Error('decline.reason.nanoBananaStopped')
      })
    const started = startNanoBananaRun('chapter')
    await answer('confirm')
    expect(await started).toBe(1)
    expect(sent()).toHaveLength(2)
    expect(warnings()).not.toContain('nanoBanana.run.stoppedFailing')
  })
})

describe('the window in front of a Nano Banana request', () => {
  it('opens with the model, the size the model answers at, and the saved instruction', async () => {
    const stub = { grantNanoBanana: vi.fn(async () => ({ grant: 'g', model: 'gemini-3.1-flash-image', count: 1 })), readSettings: vi.fn(async () => ({ nanoBananaModel: 'gemini-nano-banana-2.1', nanoBananaPrompt: 'a' })), writeSettings: vi.fn() }
    const asked = confirmNanoBanana(1, /** @type {any} */ (stub))
    const spec = await answer('cancel')
    expect(spec.props).toEqual({ count: 1, model: 'gemini-nano-banana-2.1', size: '1K', prompt: 'a' })
    expect(await asked).toBe(false)
    expect(stub.writeSettings).not.toHaveBeenCalled()
  })

  it('keeps what the person wrote, trimmed and cut, before anything is sent', async () => {
    const stub = { grantNanoBanana: vi.fn(async () => ({ grant: 'g', model: 'gemini-3.1-flash-image', count: 1 })), readSettings: vi.fn(async () => ({})), writeSettings: vi.fn(async () => ({})) }
    const asked = confirmNanoBanana(1, /** @type {any} */ (stub))
    await vi.waitFor(() => expect(app.modals.length).toBe(1))
    closeModal({ id: 'confirm', prompt: `  ${'x'.repeat(900)}  ` })
    expect(await asked).toBe(true)
    expect(stub.writeSettings).toHaveBeenCalledWith({ nanoBananaPrompt: 'x'.repeat(600) })
  })

  it('sends nothing when the instruction could not be saved', async () => {
    const stub = { grantNanoBanana: vi.fn(async () => ({ grant: 'g', model: 'gemini-3.1-flash-image', count: 1 })), readSettings: vi.fn(async () => ({})), writeSettings: vi.fn(async () => { throw new Error('disk') }) }
    const asked = confirmNanoBanana(1, /** @type {any} */ (stub))
    await answer('confirm')
    expect(await asked).toBe(false)
    expect(warnings()).toContain('nanoBanana.run.promptNotSaved')
  })
})
