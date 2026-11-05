/**
 * A Nano Banana batch against the browser mock with every delay zero: the
 * question is the real one on the modal stack, and the batch is the mock's
 * native job (`mock.js#startNanoBananaRun`, as `nano_banana_job.rs`), so what
 * is checked is what the backend is handed and what it says back.
 *
 * Exactly the regions counted when the person was asked; a page batch holds
 * to the page on screen; a region on a page the editor is not holding is
 * still found; the job is followed as the editor's run and on the jobs list,
 * with its report kept for the summary; a retry sends only what was not
 * cleaned. How the job itself skips, stops and waits is
 * `api/mock.nanobanana.test.js`'s and `nano_banana_job.rs`'s.
 *
 * Escape and the backdrop on the window itself are driven in
 * `dialogs/NanoBananaConsentDialog.dom.test.js`.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { app, clearNotices, closeModal } from '../state/app.svelte.js'
import { claimRunStart, editor, releaseRunStart } from '../state/editor.svelte.js'
import { jobById, resetJobs, startJobs } from '../state/jobs.svelte.js'
import { evictRegions } from '../state/pagewindow.svelte.js'
import { openNanoBananaSummary, retryable, retryNanoBanana, startNanoBananaRun } from './nanobananarun.svelte.js'
import { confirmNanoBanana } from './nanobananaask.svelte.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const CHAPTER = 'tsuki-to-hane-ch107'
const INDICES = [0, 1, 2, 3, 4, 5]

/** @type {any} */
let backend

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

/** The regions handed to the batch, by id, in order. */
function sentIds() {
  return backend.startNanoBananaRun.mock.calls.flatMap(([/** @type {any} */ call]) => call.regionIds)
}

/**
 * Wait for the run's end. Answers its report.
 *
 * @param {string|null} runId
 * @returns {Promise<any>}
 */
async function ended(runId) {
  await vi.waitFor(() => expect(jobById(/** @type {string} */ (runId))?.status).not.toBe('running'))
  return jobById(/** @type {string} */ (runId))?.nanoBanana
}

/** @param {Record<string, any>} [options] */
function makeBackend(options = {}) {
  let scenario = options.nanoBananaScenario ?? null
  const made = /** @type {any} */ (createMockBackend({ timing: ZERO, get nanoBananaScenario() { return scenario } }))
  made.setScenario = (/** @type {string|null} */ next) => { scenario = next }
  return made
}

/** The mock with cloud engines on, a key, and two pages detected, open in the editor. */
async function prepare() {
  resetJobs()
  setBackend(backend)
  startJobs(backend)
  // Nano Banana is a cloud engine with the person's own key: the consent is
  // granted only with cloud engines on, and a render goes only with a key.
  await backend.writeSettings({ cloudEngines: 'allowed' })
  await backend.storeNanoBananaKey({ key: 'AIzaSyMockMockMockMockMockMockMockMock' })
  const [first, second] = await textPages()
  await detectPage(first)
  await detectPage(second)
  await openChapter()
  editor.pageIndex = first
  editor.run.active = false
  vi.spyOn(backend, 'applyTool')
  vi.spyOn(backend, 'cleanAnyway')
  vi.spyOn(backend, 'startNanoBananaRun')
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
  backend = makeBackend()
  await prepare()
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
  it('hands the native job exactly the detections counted, and follows it as a job', async () => {
    const targets = await detections()
    expect(new Set(targets.map((target) => target.pageIndex)).size).toBe(2)
    const started = startNanoBananaRun('chapter')
    const spec = await answer('confirm')
    expect(spec.props.count).toBe(targets.length)
    const runId = await started
    expect(runId).toBeTruthy()
    expect(sentIds()).toEqual(targets.map((target) => target.id))
    expect(backend.startNanoBananaRun.mock.calls[0][0]).toMatchObject({ chapterId: CHAPTER })
    expect(editor.run).toMatchObject({ active: true, runId })
    expect(jobById(runId)).toMatchObject({ kind: 'nanoBanana', status: 'running' })
    const report = await ended(runId)
    expect(report).toMatchObject({ total: targets.length, cleaned: targets.length, failed: [], left: [] })
    expect(jobById(runId)).toMatchObject({ status: 'completed', nanoBanana: report })
    expect(await detections()).toEqual([])
    expect(backend.cleanAnyway).not.toHaveBeenCalled()
    expect(backend.applyTool).not.toHaveBeenCalled()
  })

  it('holds a page batch to the page on screen', async () => {
    const onScreen = (await detections()).filter((target) => target.pageIndex === editor.pageIndex)
    const started = startNanoBananaRun('page')
    await answer('confirm')
    await ended(await started)
    expect(sentIds()).toEqual(onScreen.map((target) => target.id))
  })

  it('finds a region on a page the editor is not holding', async () => {
    const far = (await detections()).find((target) => target.pageIndex !== editor.pageIndex)
    evictRegions(editor.chapter.pages[/** @type {any} */ (far).pageIndex])
    const started = startNanoBananaRun('chapter')
    await answer('confirm')
    await ended(await started)
    expect(sentIds()).toContain(far?.id)
  })

  it('hands over no more than the person was asked about', async () => {
    const counted = await detections()
    const started = startNanoBananaRun('chapter')
    await vi.waitFor(() => expect(app.modals.length).toBe(1))
    // More text is detected while the question is up.
    const [, , third] = await textPages()
    await detectPage(third)
    expect((await detections()).length).toBeGreaterThan(counted.length)
    closeModal({ id: 'confirm', prompt: '' })
    const report = await ended(await started)
    expect(sentIds()).toEqual(counted.map((target) => target.id))
    expect(report.total).toBe(counted.length)
  })

  it('sends nothing on Cancel, and frees the run slot', async () => {
    const started = startNanoBananaRun('chapter')
    await answer('cancel')
    expect(await started).toBeNull()
    expect(backend.startNanoBananaRun).not.toHaveBeenCalled()
    const token = claimRunStart()
    expect(token).toBeTruthy()
    releaseRunStart(token)
  })

  it('does not start beside a run', async () => {
    const token = claimRunStart()
    expect(await startNanoBananaRun('chapter')).toBeNull()
    expect(app.modals).toEqual([])
    expect(warnings()).toContain('notice.run.busy')
    releaseRunStart(token)
  })

  it('asks nothing when nothing is detected', async () => {
    for (const page of editor.chapter.pages) page.status = 'pending'
    expect(await startNanoBananaRun('chapter')).toBeNull()
    expect(app.modals).toEqual([])
    expect(warnings()).toContain('notice.run.nothingDetected')
  })

  it('says so when the regions cannot be read, and frees the run slot', async () => {
    vi.spyOn(backend, 'loadPages').mockRejectedValue(new Error('disk'))
    expect(await startNanoBananaRun('chapter')).toBeNull()
    expect(app.modals).toEqual([])
    expect(warnings()).toContain('nanoBanana.run.readFailed')
    const token = claimRunStart()
    expect(token).toBeTruthy()
    releaseRunStart(token)
  })

  it('says why when the native side refuses the start', async () => {
    backend.startNanoBananaRun.mockRejectedValueOnce(new Error('decline.reason.nanoBananaNoConsent'))
    const started = startNanoBananaRun('chapter')
    await answer('confirm')
    expect(await started).toBeNull()
    expect(warnings()).toContain('decline.reason.nanoBananaNoConsent')
    expect(editor.run.active).toBe(false)
  })

  it('does not follow the job as the open chapter\'s when the chapter changed while it started', async () => {
    backend.startNanoBananaRun.mockRestore()
    const original = backend.startNanoBananaRun
    vi.spyOn(backend, 'startNanoBananaRun').mockImplementationOnce(async (/** @type {any} */ call) => {
      const handle = await original.call(backend, call)
      editor.chapter = /** @type {any} */ ({ id: 'another-chapter', review: [], pages: [] })
      return handle
    })
    const started = startNanoBananaRun('chapter')
    await answer('confirm')
    expect(await started).toBeNull()
    expect(editor.run.active).toBe(false)
    // The job is the old chapter's, and the jobs list still finds it.
    const runId = (await backend.startNanoBananaRun.mock.results[0].value).runId
    await vi.waitFor(() => expect(jobById(runId)).toMatchObject({ kind: 'nanoBanana', chapterId: CHAPTER }))
    await ended(runId)
  })

  it('sends again only the regions it did not clean, behind a new window', async () => {
    backend = makeBackend({ nanoBananaScenario: 'billing' })
    await prepare()
    const targets = await detections()
    const first = startNanoBananaRun('chapter')
    await answer('confirm')
    const runId = /** @type {string} */ (await first)
    const report = await ended(runId)
    expect(report).toMatchObject({ cleaned: 1, stopped: 'decline.reason.nanoBananaBilling' })
    expect(retryable(report).map((entry) => entry.regionId)).toEqual(targets.slice(1).map((target) => target.id))
    // The account is fixed; the rest goes again.
    backend.setScenario?.(null)
    editor.run.active = false
    const again = retryNanoBanana(runId)
    const spec = await answer('confirm')
    expect(spec.props.count).toBe(targets.length - 1)
    await again
    expect(backend.startNanoBananaRun.mock.calls[1][0].regionIds).toEqual(targets.slice(1).map((target) => target.id))
  })

  it('opens its summary only once the job has a report', async () => {
    const started = startNanoBananaRun('chapter')
    await answer('confirm')
    const runId = /** @type {string} */ (await started)
    expect(openNanoBananaSummary(runId)).toBe(false)
    await ended(runId)
    expect(openNanoBananaSummary(runId)).toBe(true)
    expect(app.modals.at(-1)).toMatchObject({ kind: 'nanoBananaSummary', props: { runId } })
  })
})

describe('the window in front of a Nano Banana request', () => {
  it('opens with the model, the size the model answers at, and the saved instruction', async () => {
    const stub = { grantNanoBanana: vi.fn(async () => ({ grant: 'g', model: 'gemini-3.1-flash-image', count: 1 })), readSettings: vi.fn(async () => ({ nanoBananaModel: 'gemini-nano-banana-2.1', nanoBananaPrompt: 'a' })), writeSettings: vi.fn() }
    const asked = confirmNanoBanana(1, CHAPTER, /** @type {any} */ (stub))
    const spec = await answer('cancel')
    expect(spec.props).toEqual({ count: 1, model: 'gemini-nano-banana-2.1', size: '1K', prompt: 'a' })
    expect(await asked).toBeNull()
    expect(stub.writeSettings).not.toHaveBeenCalled()
  })

  it('keeps what the person wrote, trimmed and cut, before anything is sent', async () => {
    const stub = { grantNanoBanana: vi.fn(async () => ({ grant: 'g', model: 'gemini-3.1-flash-image', count: 1 })), readSettings: vi.fn(async () => ({})), writeSettings: vi.fn(async () => ({})) }
    const asked = confirmNanoBanana(1, CHAPTER, /** @type {any} */ (stub))
    await vi.waitFor(() => expect(app.modals.length).toBe(1))
    closeModal({ id: 'confirm', prompt: `  ${'x'.repeat(900)}  ` })
    expect(await asked).toBe('g')
    expect(stub.writeSettings).toHaveBeenCalledWith({ nanoBananaPrompt: 'x'.repeat(600) })
  })

  it('sends nothing when the instruction could not be saved', async () => {
    const stub = { grantNanoBanana: vi.fn(async () => ({ grant: 'g', model: 'gemini-3.1-flash-image', count: 1 })), readSettings: vi.fn(async () => ({})), writeSettings: vi.fn(async () => { throw new Error('disk') }) }
    const asked = confirmNanoBanana(1, CHAPTER, /** @type {any} */ (stub))
    await answer('confirm')
    expect(await asked).toBeNull()
    expect(warnings()).toContain('nanoBanana.run.promptNotSaved')
  })
})
