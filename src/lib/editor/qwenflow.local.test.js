/**
 * Guidance before a clean with FLUX on this computer: the same dialog and the
 * same rule by model family as the cloud asks with (`qwenflow.js`). FLUX 3
 * and Qwen always ask, Klein only to redo a FLUX result, a cancel stops the
 * clean, and the answer reaches the native side and is kept on the patch.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { checkedQwenEdit, createMockBackend } from '../api/mock.js'
import { app, clearNotices, closeAllModals, closeModal } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { session, setAnalysisTarget, setCleanTarget } from '../state/session.svelte.js'
import { cleansWithLocalFlux, startCleanRun } from './cloudrun.js'
import { cleanAnyway, cleanDetected, rerunMask, rerunTarget } from './maskactions.svelte.js'
import { askLocalBatchGuidance, asksLocalGuidance, localFluxModel } from './qwenflow.js'
import { applyActiveToolToRegion } from './toolapply.svelte.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const CHAPTER = 'tsuki-to-hane-ch107'
const SFX = { target: 'sound_effect', description: 'A brick wall' }

/** Local storage for the test: the node running the suite has none. */
function memoryStorage() {
  /** @type {Map<string, string>} */
  const values = new Map()
  return {
    get length() { return values.size },
    key: (/** @type {number} */ index) => [...values.keys()][index] ?? null,
    getItem: (/** @type {string} */ key) => values.get(key) ?? null,
    setItem: (/** @type {string} */ key, /** @type {string} */ value) => void values.set(key, String(value)),
    removeItem: (/** @type {string} */ key) => void values.delete(key),
    clear: () => values.clear(),
  }
}

/** A mock with the chapter open on a region that has a local Fill mask. */
async function opened() {
  const backend = createMockBackend({ timing: ZERO })
  setBackend(backend)
  const indices = [0, 1, 2, 3, 4, 5]
  const first = await backend.loadPages({ chapterId: CHAPTER, indices })
  const page = /** @type {any} */ (first.find((candidate) => candidate.regions.length > 0))
  const regionId = page.regions[0].id
  const local = await backend.applyTool({ tool: 'contentAwareFill', params: { engine: 'local' }, chapterId: CHAPTER,
    pageIndex: page.index, regionId })
  expect(local.status).toBe('applied')
  editor.chapter = /** @type {any} */ ({ id: CHAPTER, review: [], pages: await backend.loadPages({ chapterId: CHAPTER, indices }) })
  return { backend, regionId, pageIndex: page.index }
}

/** @param {string} regionId @returns {any} */
function current(regionId) {
  for (const page of editor.chapter?.pages ?? []) {
    const found = page.regions.find((/** @type {any} */ candidate) => candidate.id === regionId)
    if (found) return found
  }
  return null
}

/** The guidance dialog, once the flow has pushed it. */
async function prompted() {
  await vi.waitFor(() => expect(app.modals.at(-1)?.kind).toBe('qwenPrompt'))
  return app.modals.at(-1)
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
  session.fluxModel = ''
})

afterEach(() => {
  closeAllModals()
  clearNotices()
  session.fluxModel = ''
  editor.chapter = null
  setBackend(null)
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('which local models ask', () => {
  it('FLUX 3 and Qwen always ask; Klein and a model placed by hand ask only to redo a FLUX result', async () => {
    const { regionId } = await opened()
    expect(localFluxModel()).toBe('flux2-klein-4b')
    for (const model of ['flux3-image-klein', 'qwen-image-edit-2511']) {
      expect(asksLocalGuidance(regionId, model)).toBe(true)
      expect(asksLocalGuidance(null, model)).toBe(true)
    }
    for (const model of ['flux2-klein-4b', 'flux2-klein-9b', 'flux2-klein-2b']) {
      expect(asksLocalGuidance(regionId, model)).toBe(false)
      expect(asksLocalGuidance(null, model)).toBe(false)
    }
    current(regionId).mask.provenance.engine = 'flux'
    expect(asksLocalGuidance(regionId, 'flux2-klein-4b')).toBe(true)
    current(regionId).mask.provenance.engine = 'cloud'
    expect(asksLocalGuidance(regionId, 'flux2-klein-9b')).toBe(true)
  })

  it('a re-run asks only where it lands on FLUX', async () => {
    const { regionId } = await opened()
    const mask = current(regionId).mask
    expect(rerunTarget(mask, 'engine', 'flux')).toBe('flux')
    expect(rerunTarget(mask, 'retry')).toBe('fill')
    expect(rerunTarget(mask, 'cycleFill')).toBeNull()
    mask.provenance.engine = 'lama'
    expect(rerunTarget(mask, 'stronger')).toBe('flux')
    mask.provenance.engine = 'flux'
    expect(rerunTarget(mask, 'retryWider')).toBe('flux')
    expect(rerunTarget(mask, 'simpler')).toBe('lama')
    expect(rerunTarget(mask, 'reopenInTool')).toBeNull()
  })

  it('a batch asks for FLUX 3 and Qwen with Automatic, and never for Klein', async () => {
    expect(await askLocalBatchGuidance('flux2-klein-4b')).toBeUndefined()
    const asked = askLocalBatchGuidance('qwen-image-edit-2511')
    const modal = await prompted()
    expect(modal.props).toMatchObject({ batch: true, flux: false, initial: { target: 'auto', description: '' } })
    closeModal({ target: 'auto', description: ' Small letters ' })
    expect(await asked).toEqual({ target: 'auto', description: 'Small letters' })
  })
})

describe('one region cleaned with FLUX here', () => {
  it('FLUX 3 asks before the re-run, sends the answer and keeps it on the patch for next time', async () => {
    const { backend, regionId } = await opened()
    session.fluxModel = 'flux3-image-klein'
    const rerun = vi.spyOn(backend, 'rerunMask')
    const work = rerunMask(current(regionId), 'engine', 'flux')
    const modal = await prompted()
    expect(modal.props.flux).toBe(true)
    expect(rerun).not.toHaveBeenCalled()
    closeModal({ target: 'sound_effect', description: '  A brick wall  ' })
    expect(await work).toBe(true)
    expect(rerun.mock.calls[0][0]).toMatchObject({ kind: 'engine', engine: 'flux', params: { qwenEdit: SFX } })
    expect(current(regionId).mask.provenance.engine).toBe('flux')
    expect(current(regionId).mask.provenance.params_snapshot.qwen_edit).toEqual(SFX)

    // Klein now, redoing the FLUX result: it asks, opened on the last answer.
    session.fluxModel = 'flux2-klein-4b'
    const again = rerunMask(current(regionId), 'retry')
    expect((await prompted()).props.initial).toEqual(SFX)
    closeModal({ target: 'other', description: '' })
    expect(await again).toBe(true)
    expect(rerun.mock.calls[1][0].params).toEqual({ qwenEdit: { target: 'other', description: '' } })
  })

  it('a cancel runs nothing', async () => {
    const { backend, regionId } = await opened()
    session.fluxModel = 'qwen-image-edit-2511'
    const rerun = vi.spyOn(backend, 'rerunMask')
    const before = current(regionId).mask.id
    const work = rerunMask(current(regionId), 'engine', 'flux')
    expect((await prompted()).props.flux).toBe(false)
    closeModal(null)
    expect(await work).toBe(false)
    expect(rerun).not.toHaveBeenCalled()
    expect(current(regionId).mask.id).toBe(before)
  })

  it('Klein keeps its one click on a first FLUX clean, and nothing local asks for another engine', async () => {
    const { backend, regionId } = await opened()
    const rerun = vi.spyOn(backend, 'rerunMask')
    expect(await rerunMask(current(regionId), 'engine', 'flux')).toBe(true)
    expect(rerun.mock.calls[0][0].params).toBeUndefined()
    expect(current(regionId).mask.provenance.params_snapshot.qwen_edit).toBeUndefined()
    session.fluxModel = 'flux3-image-klein'
    expect(await rerunMask(current(regionId), 'engine', 'lama')).toBe(true)
    expect(rerun.mock.calls[1][0].params).toBeUndefined()
    expect(app.modals).toEqual([])
  })

  it('a tool pointed at FLUX asks before it is applied, and a cancel applies nothing', async () => {
    const { backend, regionId } = await opened()
    session.fluxModel = 'flux3-image-klein'
    const apply = vi.spyOn(backend, 'applyTool')
    editor.tool = 'aiMaskBrush'
    editor.toolParams = { ...editor.toolParams, aiMaskBrush: { ...(editor.toolParams.aiMaskBrush ?? {}), engine: 'flux' } }
    const cancelled = applyActiveToolToRegion(regionId)
    await prompted()
    closeModal(null)
    expect(await cancelled).toBe(false)
    expect(apply).not.toHaveBeenCalled()

    const applied = applyActiveToolToRegion(regionId)
    await prompted()
    closeModal(SFX)
    expect(await applied).toBe(true)
    expect(apply.mock.calls[0][0].params).toMatchObject({ engine: 'flux', qwenEdit: SFX })
    expect(current(regionId).mask.provenance.params_snapshot.qwen_edit).toEqual(SFX)
  })

  it('Clean anyway with FLUX asks first, sends the answer, and a cancel cleans nothing', async () => {
    const { backend, regionId } = await opened()
    session.fluxModel = 'flux3-image-klein'
    const clean = vi.spyOn(backend, 'cleanAnyway')
    const cancelled = cleanAnyway(current(regionId), 'flux')
    await prompted()
    closeModal(null)
    expect(await cancelled).toBe(false)
    expect(clean).not.toHaveBeenCalled()
    const cleaned = cleanAnyway(current(regionId), 'flux')
    await prompted()
    closeModal(SFX)
    expect(await cleaned).toBe(true)
    expect(clean.mock.calls[0][0]).toMatchObject({ engine: 'flux', params: { exact: true, qwenEdit: SFX } })
    expect(current(regionId).mask.provenance.params_snapshot.qwen_edit).toEqual(SFX)
  })

  it('a detection whose pick is FLUX asks before its clean', async () => {
    const { backend, regionId } = await opened()
    session.fluxModel = 'qwen-image-edit-2511'
    const apply = vi.spyOn(backend, 'applyTool').mockResolvedValue({ status: 'blocked' })
    const region = { ...current(regionId), mask: null, pick: 'flux' }
    const cancelled = cleanDetected(region)
    await prompted()
    closeModal(null)
    expect(await cancelled).toBe(false)
    expect(apply).not.toHaveBeenCalled()
    const asked = cleanDetected(region)
    await prompted()
    closeModal(SFX)
    await asked
    expect(apply.mock.calls[0][0].params).toMatchObject({ qwenEdit: SFX })
    await cleanDetected({ ...region, pick: 'lama' })
    expect(apply.mock.calls[1][0].params.qwenEdit).toBeUndefined()
  })
})

describe('a run cleaned with FLUX here', () => {
  /** @type {any} */
  let api
  beforeEach(() => {
    api = { runClean: vi.fn(async () => ({ runId: 'run-1', pages: [{ index: 0 }] })) }
    setBackend(api)
    editor.project = /** @type {any} */ ({ id: 'p', mode: 'paginated' })
    editor.chapter = /** @type {any} */ ({ id: 'c1', pages: [{ index: 0, number: 1, regions: [] }], review: [] })
    editor.pageIndex = 0
    setAnalysisTarget('rtFull', 'local')
    setAnalysisTarget('samTs', 'local')
    setCleanTarget('local')
  })
  afterEach(() => {
    editor.run = { ...editor.run, active: false, runId: null }
    editor.project = null
  })

  /** @param {Record<string, unknown>} picks */
  function picks(picks) {
    editor.toolParams = { ...editor.toolParams, autoClean: { ...(editor.toolParams.autoClean ?? {}), step: 'auto',
      bubbleEngine: 'fill', outsideEngine: 'lama', solidOnly: false, ...picks } }
  }

  it('asks before a run whose picks name FLUX, and sends the answer with it', async () => {
    session.fluxModel = 'flux3-image-klein'
    picks({ outsideEngine: 'flux' })
    const started = startCleanRun('page')
    const modal = await prompted()
    expect(modal.props).toMatchObject({ batch: true, flux: true })
    expect(api.runClean).not.toHaveBeenCalled()
    closeModal(SFX)
    expect(await started).toBe('run-1')
    expect(api.runClean.mock.calls[0][0]).toMatchObject({ mode: 'auto', outsideEngine: 'flux', qwenEdit: SFX })
  })

  it('a cancel starts no run', async () => {
    session.fluxModel = 'qwen-image-edit-2511'
    picks({ bubbleEngine: 'flux', step: 'clean' })
    const started = startCleanRun('chapter')
    closeModal((await prompted(), null))
    expect(await started).toBeNull()
    expect(api.runClean).not.toHaveBeenCalled()
  })

  it('a run with no FLUX pick, a Klein run, a Detect and Solid bubbles only do not ask', async () => {
    // Each run is let go before the next starts, as a finished one is.
    const start = async () => {
      expect(await startCleanRun('page')).toBe('run-1')
      editor.run = { ...editor.run, active: false, runId: null }
    }
    session.fluxModel = 'flux3-image-klein'
    picks({})
    await start()
    picks({ bubbleEngine: 'flux', step: 'detect' })
    await start()
    picks({ bubbleEngine: 'flux', solidOnly: true })
    await start()
    session.fluxModel = 'flux2-klein-4b'
    picks({ bubbleEngine: 'flux' })
    await start()
    expect(app.modals).toEqual([])
    expect(api.runClean).toHaveBeenCalledTimes(4)
    expect(api.runClean.mock.calls.every((/** @type {any} */ [spec]) => spec.qwenEdit === undefined)).toBe(true)
  })

  it('a clean that goes to the cloud GPU is not asked as a local one', () => {
    const params = { bubbleEngine: 'flux', outsideEngine: 'lama', solidOnly: false }
    expect(cleansWithLocalFlux('auto', { detect: false, clean: false }, params)).toBe(true)
    expect(cleansWithLocalFlux('clean', { detect: false, clean: true }, params)).toBe(false)
    expect(cleansWithLocalFlux('detect', { detect: false, clean: false }, params)).toBe(false)
  })
})

describe('the mock checks guidance as the native side does', () => {
  it('takes a target and a trimmed description, and refuses anything else', () => {
    expect(checkedQwenEdit(undefined)).toBeNull()
    expect(checkedQwenEdit(null)).toBeNull()
    expect(checkedQwenEdit({ target: 'dialogue', description: ' a ' })).toEqual({ target: 'dialogue', description: 'a' })
    for (const bad of [{ target: 'all', description: '' }, { target: 'auto' }, { target: 'auto', description: '', more: 1 },
      { target: 'auto', description: 'x'.repeat(501) }, { target: 'auto', description: 'a\u0007b' }, 'auto']) {
      expect(() => checkedQwenEdit(bad)).toThrow('qwen_edit_invalid')
    }
  })

  it('a run takes its guidance only where it cleans here', async () => {
    const backend = createMockBackend({ timing: ZERO })
    await expect(backend.runClean({ scope: 'chapter', chapterId: CHAPTER, mode: 'clean',
      qwenEdit: { target: 'auto', description: 'a\u0007b' } })).rejects.toThrow('qwen_edit_invalid')
  })
})
