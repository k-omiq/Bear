import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  applyRegionState,
  adoptRun,
  consumeResume,
  cycleMaskSelectShape,
  requestResume,
  replaceRegion,
  closeEditorChapter,
  editor,
  jobConflictKey,
  LOCAL_CEILING,
  openEditorChapter,
  redo,
  recordRegionEdit,
  reportJobConflict,
  select,
  selectedRegion,
  setToolBySlot,
  startRun,
  runFinished,
  TOOLS,
  toggleMaskSelectMode,
  undo,
} from './editor.svelte.js'
import { app } from './app.svelte.js'
import { setBackend } from '../api/backend.js'
import { createHistory, push } from '../model/history.js'
import { session } from './session.svelte.js'
import { jobById, resetJobs } from './jobs.svelte.js'

describe('editor history persistence failures', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    app.notices.length = 0
    editor.chapter = { id: 'test-chapter' }
    editor.history = createHistory()
    setBackend(null)
  })

  afterEach(() => {
    vi.useRealTimers()
    setBackend(null)
    editor.chapter = null
  })

  it('historyPush rejecting twice yields one notice and records the error', async () => {
    const pushMock = vi.fn().mockRejectedValue(new Error('disk write failed'))
    setBackend(/** @type {any} */ ({ historyPush: pushMock }))

    const before = { region: null, pageStatus: 'unclean' }
    const after = { region: { id: 'r1', pageId: 'p1' }, pageStatus: 'cleaned' }

    recordRegionEdit('canvas.command.applyTool', 'r1', before, after)
    expect(pushMock).toHaveBeenCalledTimes(1)
    expect(app.notices).toHaveLength(0)

    // Advance timer past retry delay (50ms) to trigger retry and allow promises to settle
    await vi.advanceTimersByTimeAsync(60)

    expect(pushMock).toHaveBeenCalledTimes(2)
    expect(app.notices).toHaveLength(1)
    expect(app.notices[0]).toMatchObject({
      key: 'notice.history.saveFailed',
      tone: 'warn',
    })
    expect(editor.history.error).toBeInstanceOf(Error)
  })

  it('historyPush rejecting once then succeeding yields no notice', async () => {
    const pushMock = vi
      .fn()
      .mockRejectedValueOnce(new Error('temporary disk lock'))
      .mockResolvedValueOnce({
        cursor: 1,
        entries: [{ seq: 1, label: 'canvas.command.applyTool' }],
      })
    setBackend(/** @type {any} */ ({ historyPush: pushMock }))

    const before = { region: null, pageStatus: 'unclean' }
    const after = { region: { id: 'r1', pageId: 'p1' }, pageStatus: 'cleaned' }

    recordRegionEdit('canvas.command.applyTool', 'r1', before, after)
    expect(pushMock).toHaveBeenCalledTimes(1)
    expect(app.notices).toHaveLength(0)

    // Advance timer past retry delay
    await vi.advanceTimersByTimeAsync(60)

    expect(pushMock).toHaveBeenCalledTimes(2)
    expect(app.notices).toHaveLength(0)
    expect(editor.history.entries).toEqual([
      { seq: 1, label: 'canvas.command.applyTool' },
    ])
  })

  it('historyMove rejecting twice during undo yields one notice and records the error', async () => {
    const moveMock = vi.fn().mockRejectedValue(new Error('disk move failed'))
    setBackend(/** @type {any} */ ({ historyMove: moveMock }))

    push(editor.history, { seq: 1, label: 'canvas.command.applyTool' })
    expect(editor.history.cursor).toBe(1)

    undo()
    await vi.advanceTimersByTimeAsync(0)
    expect(moveMock).toHaveBeenCalledTimes(1)
    expect(app.notices).toHaveLength(0)

    await vi.advanceTimersByTimeAsync(60)

    expect(moveMock).toHaveBeenCalledTimes(2)
    expect(app.notices).toHaveLength(1)
    expect(app.notices[0]).toMatchObject({
      key: 'notice.history.saveFailed',
      tone: 'warn',
    })
    expect(editor.history.error).toBeInstanceOf(Error)
  })

  it('historyMove rejecting once then succeeding during undo yields no notice', async () => {
    const moveMock = vi
      .fn()
      .mockRejectedValueOnce(new Error('temporary move lock'))
      .mockResolvedValueOnce({
        cursor: 0,
        entry: null,
      })
    setBackend(/** @type {any} */ ({ historyMove: moveMock }))

    push(editor.history, { seq: 1, label: 'canvas.command.applyTool' })
    expect(editor.history.cursor).toBe(1)

    undo()
    await vi.advanceTimersByTimeAsync(0)
    expect(moveMock).toHaveBeenCalledTimes(1)
    expect(app.notices).toHaveLength(0)

    await vi.advanceTimersByTimeAsync(60)

    expect(moveMock).toHaveBeenCalledTimes(2)
    expect(app.notices).toHaveLength(0)
  })

  it('recordRegionEdit followed immediately by undo preserves cursor 0 when historyPush resolves', async () => {
    /** @type {(value: any) => void} */
    let resolvePush
    const pushPromise = new Promise((res) => {
      resolvePush = res
    })
    const pushMock = vi.fn().mockReturnValue(pushPromise)
    const moveMock = vi.fn().mockResolvedValue({ cursor: 0, entry: null })
    setBackend(/** @type {any} */ ({ historyPush: pushMock, historyMove: moveMock }))

    const before = { region: null, pageStatus: 'unclean' }
    const after = { region: { id: 'r1', pageId: 'p1' }, pageStatus: 'cleaned' }

    recordRegionEdit('canvas.command.applyTool', 'r1', before, after)
    expect(editor.history.cursor).toBe(1)

    undo()
    expect(editor.history.cursor).toBe(0)

    // Push resolves with backend's view (which reported cursor 1 at time of push)
    resolvePush({ cursor: 1, entries: [{ seq: 1, label: 'canvas.command.applyTool' }] })
    await vi.advanceTimersByTimeAsync(0)

    expect(editor.history.cursor).toBe(0)
    expect(moveMock).toHaveBeenCalledWith(expect.objectContaining({ direction: 'undo' }))
  })

  it('delayed retry of first historyPush does not overwrite subsequent edit', async () => {
    const pushCalls = []
    const pushMock = vi.fn().mockImplementation(async ({ entry }) => {
      pushCalls.push(entry.label)
      if (entry.label === 'edit1' && pushCalls.filter((l) => l === 'edit1').length === 1) {
        throw new Error('fail 1')
      }
      if (entry.label === 'edit1') {
        return { cursor: 1, entries: [{ seq: 1, label: 'edit1' }] }
      }
      return {
        cursor: 2,
        entries: [
          { seq: 1, label: 'edit1' },
          { seq: 2, label: 'edit2' },
        ],
      }
    })
    setBackend(/** @type {any} */ ({ historyPush: pushMock }))

    const before = { region: null, pageStatus: 'unclean' }
    const after1 = { region: { id: 'r1', pageId: 'p1' }, pageStatus: 'cleaned' }
    const after2 = { region: { id: 'r2', pageId: 'p1' }, pageStatus: 'cleaned' }

    recordRegionEdit('edit1', 'r1', before, after1)
    expect(pushMock).toHaveBeenCalledTimes(1)

    // While edit 1 is waiting for retry delay, record edit 2
    recordRegionEdit('edit2', 'r2', before, after2)

    // Advance past retry delay and allow queued pushes to complete in order
    await vi.advanceTimersByTimeAsync(100)

    expect(editor.history.cursor).toBe(2)
    expect(editor.history.entries).toEqual([
      { seq: 1, label: 'edit1' },
      { seq: 2, label: 'edit2' },
    ])
  })
})

describe('applyRegionState on non-resident pages', () => {
  beforeEach(() => {
    editor.chapter = null
  })

  it('applyRegionState on an evicted page updates review index without corrupting other entries or mutating regions', () => {
    editor.chapter = {
      id: 'ch1',
      pages: [
        {
          id: 'p0',
          index: 0,
          resident: false,
          regions: [],
          regionCount: 2,
          doneCount: 0,
          reviewCount: 2,
          status: 'cleaned',
        },
      ],
      review: [
        { id: 'r1', pageId: 'p0', pageIndex: 0, reasonKey: 'review.reason.unusuallyLarge' },
        { id: 'r2', pageId: 'p0', pageIndex: 0, reasonKey: 'review.reason.declined' },
      ],
    }

    const updatedR1 = {
      id: 'r1',
      pageId: 'p0',
      bbox: { x: 0, y: 0, w: 1, h: 1 },
      source: 'auto',
      detected: true,
      outcome: 'cleaned',
      gateSkipCause: null,
      declineReason: null,
      unusuallyLarge: false,
      mask: { id: 'r1-m1', regionId: 'r1', sequence: 1, fittingReconstructed: false, cloudOutcome: null },
    }

    applyRegionState('r1', updatedR1, 'cleaned')
    const page = editor.chapter.pages[0]
    expect(page.regions).toEqual([])
    expect(editor.chapter.review).toEqual([
      { id: 'r2', pageId: 'p0', pageIndex: 0, reasonKey: 'review.reason.declined' },
    ])
    expect(page.reviewCount).toBe(1)
  })

  it('applyRegionState with null region on an evicted page removes review entry and decrements counts', () => {
    editor.chapter = {
      id: 'ch1',
      pages: [
        {
          id: 'p0',
          index: 0,
          resident: false,
          regions: [],
          regionCount: 2,
          doneCount: 0,
          reviewCount: 1,
          status: 'cleaned',
        },
      ],
      review: [
        { id: 'r1', pageId: 'p0', pageIndex: 0, reasonKey: 'review.reason.unusuallyLarge' },
      ],
    }

    applyRegionState('r1', null, 'unclean')
    const page = editor.chapter.pages[0]
    expect(editor.chapter.review).toHaveLength(0)
    expect(page.regionCount).toBe(1)
    expect(page.reviewCount).toBe(0)
    expect(page.status).toBe('unclean')
  })
})

/**
 * A run reports regions on pages the reader is nowhere near, and those pages
 * are headers. The Pages list draws a row for every one of them,
 * so the counts on the header - and the chapter-wide review index - have to
 * move without the regions ever arriving.
 */
describe('a run over pages the window is not holding', () => {
  /** @type {(event: any) => void} */
  let emit

  const header = (index) => ({
    id: `p${index}`,
    chapterId: 'ch1',
    index,
    number: index + 1,
    status: 'unclean',
    skipReason: null,
    regions: [],
    regionCount: 0,
    doneCount: 0,
    reviewCount: 0,
    resident: false,
  })

  const cleanedRegion = (id, pageId, overrides = {}) => ({
    id,
    pageId,
    bbox: { x: 0, y: 0, w: 1, h: 1 },
    source: 'auto',
    detected: true,
    outcome: 'cleaned',
    gateSkipCause: null,
    declineReason: null,
    unusuallyLarge: false,
    mask: { id: `${id}-m1`, regionId: id, sequence: 1, fittingReconstructed: false, cloudOutcome: null },
    ...overrides,
  })

  beforeEach(async () => {
    editor.chapter = null
    editor.project = null
    setBackend(
      /** @type {any} */ ({
        subscribe: (/** @type {any} */ fn) => {
          emit = fn
          return () => {}
        },
        openChapter: async () => ({
          project: { id: 'pr1', mode: 'single', readingDirection: 'rtl' },
          chapter: {
            id: 'ch1',
            projectId: 'pr1',
            pages: [header(0), header(1), header(2), header(3), header(4)],
            review: [],
          },
          pendingConversion: null,
        }),
        // The window is asked for separately; nothing comes back, so every
        // page stays a header and the counts have only one place to come from.
        loadPages: async () => [],
        historyLoad: async () => ({ cursor: 0, entries: [] }),
      }),
    )
    await openEditorChapter('pr1', 'ch1')
  })

  afterEach(() => {
    setBackend(null)
    editor.chapter = null
    editor.project = null
  })

  it('counts a finished region onto the header of a page it is not holding', () => {
    emit({ type: 'region-done', chapterId: 'ch1', pageIndex: 4, region: cleanedRegion('r1', 'p4') })
    const page = editor.chapter.pages[4]
    expect(page.resident).toBe(false)
    expect(page.regionCount).toBe(1)
    expect(page.doneCount).toBe(1)
    expect(page.reviewCount).toBe(0)
    expect(editor.chapter.review).toHaveLength(0)
  })

  it('puts a flagged region into the chapter-wide index and onto the header', () => {
    emit({
      type: 'region-done',
      chapterId: 'ch1',
      pageIndex: 3,
      region: cleanedRegion('r2', 'p3', { unusuallyLarge: true }),
    })
    const page = editor.chapter.pages[3]
    expect(page.reviewCount).toBe(1)
    expect(page.doneCount).toBe(0)
    expect(editor.chapter.review).toEqual([
      { id: 'r2', pageId: 'p3', pageIndex: 3, reasonKey: 'review.reason.unusuallyLarge' },
    ])
  })

  it('does not count a region twice when the run replaces one it already flagged', () => {
    const flagged = cleanedRegion('r3', 'p3', { unusuallyLarge: true })
    emit({ type: 'region-done', chapterId: 'ch1', pageIndex: 3, region: flagged })
    emit({ type: 'region-done', chapterId: 'ch1', pageIndex: 3, region: flagged })
    expect(editor.chapter.pages[3].reviewCount).toBe(1)
    expect(editor.chapter.review).toHaveLength(1)
  })

  it('takes the whole page-done payload as counts, then drops the regions', () => {
    emit({
      type: 'page-done',
      chapterId: 'ch1',
      page: {
        ...header(2),
        status: 'cleaned',
        resident: true,
        regions: [
          cleanedRegion('a', 'p2'),
          cleanedRegion('b', 'p2'),
          cleanedRegion('c', 'p2', { outcome: 'declined', mask: null }),
        ],
      },
    })
    const page = editor.chapter.pages[2]
    expect(page.resident).toBe(false)
    expect(page.regions).toEqual([])
    expect(page.regionCount).toBe(3)
    expect(page.doneCount).toBe(2)
    expect(page.reviewCount).toBe(1)
    expect(editor.chapter.review.map((entry) => entry.id)).toEqual(['c'])
  })

  it('reloads a page left cleaning when a run ends before page-done', async () => {
    const loadPages = vi.fn(async () => [{ ...header(2), status: 'unclean', resident: false }])
    setBackend(/** @type {any} */ ({ loadPages }))
    editor.run.runId = 'r1'
    emit({ type: 'page-started', chapterId: 'ch1', pageIndex: 2 })
    expect(editor.chapter.pages[2].status).toBe('cleaning')
    emit({ type: 'run-finished', chapterId: 'ch1', runId: 'r1', reason: 'cancelled', nextPageIndex: 2 })
    await vi.waitFor(() => expect(editor.chapter.pages[2].status).toBe('unclean'))
    expect(loadPages).toHaveBeenCalledWith({ chapterId: 'ch1', indices: [2] })
  })

  it('settles a detection waiter when the editor closes', async () => {
    editor.run.runId = 'run-before-home'
    const waiting = runFinished('run-before-home')
    closeEditorChapter()
    await expect(waiting).resolves.toMatchObject({ runId: 'run-before-home', reason: 'editor-closed' })
  })

  it('keeps brush and shape choices when another chapter opens in this app session', async () => {
    const previous = structuredClone(editor.toolParams)
    try {
      editor.toolParams.brush.hardness = 35
      editor.toolParams.brush.color = '#123456'
      editor.toolParams.shapes.feather = 7
      editor.toolParams.shapes.color = '#abcdef'

      await openEditorChapter('pr1', 'another-chapter')

      expect(editor.toolParams.brush).toMatchObject({ hardness: 35, color: '#123456' })
      expect(editor.toolParams.shapes).toMatchObject({ feather: 7, color: '#abcdef' })
    } finally {
      editor.toolParams = previous
    }
  })
})

describe('tool parameters defaults', () => {
  it('initializes brush with color, opacity, and flow', () => {
    expect(editor.toolParams.brush).toMatchObject({
      size: 28,
      hardness: 100,
      spacing: 12,
      mode: 'paint',
      color: '#ffffff',
      opacity: 100,
      flow: 100,
    })
  })

  it('initializes cloneHeal with opacity and flow', () => {
    expect(editor.toolParams.cloneHeal).toMatchObject({
      size: 32,
      hardness: 60,
      opacity: 100,
      flow: 100,
      alignment: 'aligned',
      mode: 'heal',
    })
  })

  it('initializes the selection tool to add, with a round brush', () => {
    expect(editor.toolParams.maskSelect).toEqual({ mode: 'add', shape: 'brush', size: 32 })
  })

  it('initializes shapes with white color and feather 0', () => {
    expect(editor.toolParams.shapes).toMatchObject({
      shape: 'rect',
      mode: 'solid',
      color: '#ffffff',
      feather: 0,
    })
  })

  it('initializes autoClean with bubbleColor #ffffff', () => {
    expect(editor.toolParams.autoClean).toMatchObject({
      bubbleColor: '#ffffff',
    })
  })
})


/**
 * A removal has to reach the page even when the page is a header and the
 * region was never flagged - the review index is the only index there is, and
 * it only knows about regions that need review.
 */
describe('a removal on a page the window is not holding', () => {
  beforeEach(() => {
    editor.chapter = null
  })

  afterEach(() => {
    editor.chapter = null
    editor.selectionId = null
    editor.hoverId = null
  })

  /** @returns {any} one evicted page with three regions, one of them flagged */
  function evicted() {
    return {
      id: 'ch1',
      pages: [
        {
          id: 'ch1-p001',
          index: 0,
          resident: false,
          regions: [],
          regionCount: 3,
          doneCount: 2,
          reviewCount: 1,
          status: 'cleaned',
        },
      ],
      review: [
        { id: 'ch1-p001-r2', pageId: 'ch1-p001', pageIndex: 0, reasonKey: 'review.reason.declined' },
      ],
    }
  }

  it('finds the page from the region id when the review index has never heard of it', () => {
    editor.chapter = evicted()
    // `ch1-p001-r0` is an ordinary cleaned region: nothing flagged it, so it
    // is in no index at all. This used to return false and leave the header
    // counting a region that had gone.
    expect(applyRegionState('ch1-p001-r0', null, 'cleaned')).toBe(true)
    const page = editor.chapter.pages[0]
    expect(page.regionCount).toBe(2)
    expect(page.reviewCount).toBe(1)
    expect(page.status).toBe('cleaned')
  })

  it('keeps the header arithmetic possible - done can never exceed what is left', () => {
    editor.chapter = evicted()
    applyRegionState('ch1-p001-r0', null, 'cleaned')
    const page = editor.chapter.pages[0]
    expect(page.doneCount).toBeLessThanOrEqual(page.regionCount - page.reviewCount)
    expect(page.doneCount).toBeGreaterThanOrEqual(0)
  })

  it('still goes through the review index for a flagged region', () => {
    editor.chapter = evicted()
    expect(applyRegionState('ch1-p001-r2', null, 'cleaned')).toBe(true)
    const page = editor.chapter.pages[0]
    expect(editor.chapter.review).toHaveLength(0)
    expect(page.regionCount).toBe(2)
    expect(page.reviewCount).toBe(0)
  })

  it('takes the longest matching page id, because page ids are not prefix-free', () => {
    editor.chapter = evicted()
    editor.chapter.pages.push({
      id: 'ch1-p001-x',
      index: 1,
      resident: false,
      regions: [],
      regionCount: 4,
      doneCount: 0,
      reviewCount: 0,
      status: 'cleaned',
    })
    applyRegionState('ch1-p001-x-r0', null, 'cleaned')
    expect(editor.chapter.pages[0].regionCount).toBe(3)
    expect(editor.chapter.pages[1].regionCount).toBe(3)
  })

  it('answers false for an id that names no page at all', () => {
    editor.chapter = evicted()
    expect(applyRegionState('ch9-p001-r0', null, 'cleaned')).toBe(false)
  })
})

describe('a selection whose region has been removed', () => {
  afterEach(() => {
    editor.chapter = null
    editor.selectionId = null
    editor.hoverId = null
  })

  /** @param {boolean} resident */
  function chapterWith(resident) {
    const region = {
      id: 'ch1-p001-r0',
      pageId: 'ch1-p001',
      bbox: { x: 0, y: 0, w: 10, h: 10 },
      source: 'auto',
      outcome: 'cleaned',
      mask: { id: 'ch1-p001-r0-m1', sequence: 1, provenance: { engine: 'lama' } },
    }
    editor.chapter = {
      id: 'ch1',
      pages: [
        {
          id: 'ch1-p001',
          index: 0,
          resident,
          regions: resident ? [region] : [],
          regionCount: 1,
          doneCount: 1,
          reviewCount: 0,
          status: 'cleaned',
        },
      ],
      review: [],
    }
    return region
  }

  it('is dropped, on the page the window is holding', () => {
    chapterWith(true)
    select('ch1-p001-r0')
    editor.hoverId = 'ch1-p001-r0'
    applyRegionState('ch1-p001-r0', null, 'unclean')
    expect(editor.selectionId).toBe(null)
    expect(editor.hoverId).toBe(null)
  })

  it('is dropped on a page the window is not holding too', () => {
    chapterWith(false)
    select('ch1-p001-r0')
    applyRegionState('ch1-p001-r0', null, 'unclean')
    expect(editor.selectionId).toBe(null)
  })

  it('survives an edit that only replaces the region', () => {
    const region = chapterWith(true)
    select('ch1-p001-r0')
    applyRegionState('ch1-p001-r0', { ...region, outcome: 'declined' }, 'cleaned')
    expect(editor.selectionId).toBe('ch1-p001-r0')
  })

  it('leaves a selection that names some other region alone', () => {
    chapterWith(true)
    select('ch1-p001-r9')
    applyRegionState('ch1-p001-r0', null, 'unclean')
    expect(editor.selectionId).toBe('ch1-p001-r9')
  })
})

describe('selectedRegion', () => {
  afterEach(() => {
    editor.chapter = null
    editor.selectionId = null
  })

  it('is the region the selection names, so the editor-wide delete has one to give', () => {
    editor.chapter = {
      id: 'ch1',
      pages: [
        {
          id: 'ch1-p001',
          index: 0,
          regions: [{ id: 'ch1-p001-r0', pageId: 'ch1-p001', mask: null }],
          status: 'cleaned',
        },
      ],
      review: [],
    }
    select('ch1-p001-r0')
    expect(selectedRegion()?.id).toBe('ch1-p001-r0')
  })

  it('is null with nothing selected, and null for an id nothing holds', () => {
    editor.chapter = { id: 'ch1', pages: [], review: [] }
    select(null)
    expect(selectedRegion()).toBe(null)
    select('ch1-p001-r0')
    expect(selectedRegion()).toBe(null)
  })
})

describe('startRun local engine ceiling boundary', () => {
  const initialToolParams = structuredClone(editor.toolParams)

  beforeEach(() => {
    editor.chapter = { id: 'ch-test-1', pages: [{ id: 'p0', index: 0, regions: [], status: 'unclean' }] }
    editor.pageIndex = 0
    editor.run = {
      active: false,
      runId: null,
      scope: null,
      queued: 0,
      pagesDone: 0,
      currentPageIndex: null,
      nextPageIndex: null,
    }
  })

  afterEach(() => {
    setBackend(null)
    editor.chapter = null
    editor.run = {
      active: false,
      runId: null,
      scope: null,
      queued: 0,
      pagesDone: 0,
      currentPageIndex: null,
      nextPageIndex: null,
    }
    editor.toolParams = structuredClone(initialToolParams)
  })

  it.each([
    { inputScope: undefined, expectedScope: 'page', paramCeiling: undefined },
    { inputScope: 'page', expectedScope: 'page', paramCeiling: 'cloud' },
    { inputScope: 'chapter', expectedScope: 'chapter', paramCeiling: 'flux' },
    { inputScope: 'project', expectedScope: 'project', paramCeiling: 'cloud' },
  ])(
    'always passes LOCAL_CEILING ("lama") to runClean for scope $inputScope (effective: $expectedScope)',
    async ({ inputScope, expectedScope, paramCeiling }) => {
      if (paramCeiling) {
        editor.toolParams.autoClean = {
          ...editor.toolParams.autoClean,
          engineCeiling: paramCeiling,
        }
      }

      const runClean = vi.fn().mockResolvedValue({
        runId: 'mock-run-id',
        pages: [{ id: 'p0' }],
      })
      setBackend(/** @type {any} */ ({ runClean }))

      const runId = await startRun(inputScope)

      expect(runId).toBe('mock-run-id')
      expect(runClean).toHaveBeenCalledTimes(1)
      expect(runClean).toHaveBeenCalledWith({
        scope: expectedScope,
        mode: 'auto',
        chapterId: 'ch-test-1',
        pageIndex: 0,
        engineCeiling: 'lama',
        bubbleEngine: 'fill',
        outsideEngine: 'lama',
        outsideBubbles: 'review',
        bubbleColor: '#ffffff',
        maskPaddingPx: 0,
        detection: { ...session.detection },
        detectorModels: [...session.detectorModels],
        geometryPolicy: 'legacy',
        textPolicy: 'legacy_gate',
        ocrRescue: session.ocrRescue,
        analysisTargets: { ...session.analysisTargets },
      })
      expect(runClean.mock.calls[0][0].engineCeiling).toBe(LOCAL_CEILING)
      expect(editor.run).toMatchObject({
        active: true,
        runId: 'mock-run-id',
        scope: expectedScope,
        queued: 1,
      })
    },
  )

  it('runs all-text automatic cleaning with the selected detection models', async () => {
    const previousPolicy = session.textPolicy
    session.textPolicy = 'all_text'
    const runClean = vi.fn().mockResolvedValue({ runId: 'all-text-run', pages: [{ id: 'p0' }] })
    setBackend(/** @type {any} */ ({ runClean }))
    app.modals.length = 0
    try {
      expect(await startRun()).toBe('all-text-run')
      expect(runClean).toHaveBeenCalledWith(expect.objectContaining({
        textPolicy: 'all_text', detectorModels: [...session.detectorModels],
      }))
    } finally {
      session.textPolicy = previousPolicy
      app.modals.length = 0
    }
  })

  // The status line beside Cancel names the step, and a Detect run changes
  // no pixel: the run keeps the mode it was started with.
  it('keeps the step a run was started with', async () => {
    const runClean = vi.fn().mockResolvedValue({ runId: 'detect-run', pages: [{ id: 'p0' }] })
    setBackend(/** @type {any} */ ({ runClean }))
    expect(await startRun('chapter', { mode: 'detect' })).toBe('detect-run')
    expect(editor.run).toMatchObject({ active: true, runId: 'detect-run', mode: 'detect' })
    editor.run.active = false
    expect(adoptRun({ runId: 'render-run', pages: [] }, 'page', 'clean')).toBe('render-run')
    expect(editor.run.mode).toBe('clean')
    editor.run.active = false
    expect(adoptRun({ runId: 'plain-run', pages: [] }, 'page')).toBe('plain-run')
    expect(editor.run.mode).toBe('auto')
  })

  // Detect on its own finds all text for the review; the detection half of a
  // Detect & clean (a Detect run whose clean follows on the cloud) keeps the
  // panel's choices.
  it('finds all text on a Detect run and keeps the choices for Detect & clean', async () => {
    const runClean = vi.fn().mockResolvedValue({ runId: 'r', pages: [{ id: 'p0' }] })
    setBackend(/** @type {any} */ ({ runClean }))
    const previous = editor.toolParams.autoClean
    try {
      for (const [step, expected] of [
        ['detect', { textPolicy: 'all_text', outsideBubbles: 'clean' }],
        ['auto', { textPolicy: session.textPolicy, outsideBubbles: 'review' }],
      ]) {
        editor.run.active = false
        runClean.mockClear()
        editor.toolParams.autoClean = { ...(previous ?? {}), step, outsideBubbles: 'review' }
        await startRun('page', { mode: 'detect' })
        expect(runClean.mock.calls[0][0], step).toMatchObject(expected)
      }
    } finally {
      editor.toolParams.autoClean = previous
      editor.run.active = false
    }
  })

  // A resumed run is the step it was taking when it stopped, and says so.
  it('resumes a run in the mode the native side resumed it in', async () => {
    for (const [answered, expected] of [['detect', 'detect'], ['clean', 'clean'], [undefined, 'auto']]) {
      editor.run.active = false
      const resumeJob = vi.fn(async () => ({ runId: `resume-${expected}`, pages: [{ id: 'p0' }], resumedFrom: 0,
        ...(answered ? { mode: answered } : {}) }))
      setBackend(/** @type {any} */ ({ resumeJob }))
      requestResume('project-1', 'ch-test-1')
      expect(await consumeResume()).toBe(`resume-${expected}`)
      expect(editor.run).toMatchObject({ active: true, scope: 'chapter', mode: expected })
    }
  })

  it('does not adopt another chapter\'s already-running handle', async () => {
    const runClean = vi.fn(async () => ({ runId: 'chapter-a-run', alreadyRunning: true, pages: [] }))
    setBackend(/** @type {any} */ ({ runClean }))
    app.notices.length = 0
    expect(await startRun()).toBeNull()
    expect(editor.run.active).toBe(false)
    expect(adoptRun({ runId: 'chapter-a-run', alreadyRunning: true }, 'page')).toBeNull()
    expect(app.notices.at(-1)?.key).toBe('notice.run.busy')
  })

  it('rejects a second local start while the first handle is pending', async () => {
    let answer
    const runClean = vi.fn(() => new Promise((resolve) => { answer = resolve }))
    setBackend(/** @type {any} */ ({ runClean }))
    const first = startRun()
    expect(await startRun()).toBeNull()
    expect(runClean).toHaveBeenCalledTimes(1)
    answer({ runId: 'first', pages: [] })
    expect(await first).toBe('first')
  })
})

describe('text-shaped revision history', () => {
  afterEach(() => {
    setBackend(null)
    editor.chapter = null
    editor.history = createHistory()
  })

  it('undoes and redoes successive immutable patch revisions by exact identity', async () => {
    const regionAt = (revision) => ({ id: 'p1-hreview-sam-1', pageId: 'p1', outcome: 'cleaned',
      mask: { id: `mask-${revision}`, textShapePatchRevision: `patch-revision-${revision}` } })
    const first = regionAt(1)
    const second = regionAt(2)
    const journal = []
    let cursor = 0
    let currentRegion = second
    const restoreRegion = vi.fn(async ({ region }) => {
      currentRegion = region
      return region
    })
    setBackend(/** @type {any} */ ({
      historyPush: async ({ entry }) => {
        journal.push(entry)
        cursor = journal.length
        return { cursor, entries: journal.map((item, index) => ({ seq: index + 1, label: item.label })) }
      },
      historyMove: async ({ direction }) => {
        if (direction === 'undo') cursor = Math.max(0, cursor - 1)
        else cursor = Math.min(journal.length, cursor + 1)
        const entry = direction === 'undo' ? journal[cursor] : journal[cursor - 1]
        return { cursor, entry }
      },
      restoreRegion,
      loadPages: async () => [],
    }))
    editor.chapter = { id: 'revision-chapter', pages: [{ id: 'p1', index: 0, resident: true,
      status: 'cleaned', regions: [second], regionCount: 1, doneCount: 1, reviewCount: 0 }], review: [] }
    editor.history = createHistory()

    recordRegionEdit('canvas.command.applyTool', second.id, { region: null, pageStatus: 'unclean' },
      { region: first, pageStatus: 'cleaned' })
    await editor.history.running
    recordRegionEdit('canvas.command.applyTool', second.id, { region: first, pageStatus: 'cleaned' },
      { region: second, pageStatus: 'cleaned' })
    await editor.history.running

    undo()
    await editor.history.running
    expect(currentRegion.mask.textShapePatchRevision).toBe('patch-revision-1')
    undo()
    await editor.history.running
    expect(currentRegion).toBeNull()
    redo()
    await editor.history.running
    expect(currentRegion.mask.textShapePatchRevision).toBe('patch-revision-1')
    redo()
    await editor.history.running
    expect(currentRegion.mask.textShapePatchRevision).toBe('patch-revision-2')
    expect(restoreRegion.mock.calls.map(([call]) => call.region?.mask?.textShapePatchRevision ?? null)).toEqual([
      'patch-revision-1', null, 'patch-revision-1', 'patch-revision-2',
    ])
  })

  it('carries legacy patch revisions through undo and redo', async () => {
    const regionAt = (revision) => ({
      id: 'p1-r1', pageId: 'p1', outcome: 'cleaned',
      mask: { id: 'p1-r1-m1', legacyPatchRevision: revision },
    })
    const first = regionAt('old-pixels')
    const second = regionAt('new-pixels')
    const entries = []
    let cursor = 0
    const restoreRegion = vi.fn(async ({ region }) => region)
    setBackend(/** @type {any} */ ({
      historyPush: async ({ entry }) => {
        entries.push(entry)
        cursor = entries.length
        return { cursor, entries: entries.map((item, index) => ({ seq: index + 1, label: item.label })) }
      },
      historyMove: async ({ direction }) => {
        cursor += direction === 'undo' ? -1 : 1
        return { cursor, entry: direction === 'undo' ? entries[cursor] : entries[cursor - 1] }
      },
      restoreRegion,
      loadPages: async () => [],
    }))
    editor.chapter = { id: 'legacy-chapter', pages: [{ id: 'p1', index: 0, resident: true,
      status: 'cleaned', regions: [second], regionCount: 1, doneCount: 1, reviewCount: 0 }], review: [] }
    editor.history = createHistory()

    recordRegionEdit('canvas.command.applyTool', first.id,
      { region: first, pageStatus: 'cleaned' }, { region: second, pageStatus: 'cleaned' })
    await editor.history.running
    undo()
    await editor.history.running
    redo()
    await editor.history.running
    expect(restoreRegion.mock.calls.map(([call]) => call.region.mask.legacyPatchRevision))
      .toEqual(['old-pixels', 'new-pixels'])
  })
})

/**
 * A longstrip page learns of a layer edit on a neighbour without a reload:
 * `page.liveAppearance` chains the changed layer's digest onto every page its
 * old and new boxes reach. It moves only when a tile would change, and when
 * the old box is not known it reaches every page.
 */
// Last, because opening a chapter subscribes to the backend once per module,
// and the tests above count on being the ones that do.
describe('a chapter write refused for another writer', () => {
  afterEach(() => {
    setBackend(null)
    editor.chapter = null
    editor.project = null
    app.notices.length = 0
  })

  it('finds the code wherever a wrapping layer left it', () => {
    expect(jobConflictKey(new Error('job_busy: another Manga Cleaner process is using /l/c1.mtclean'))).toBe(
      'notice.job.busy',
    )
    expect(jobConflictKey('/l/p1/c1.mtclean: job_stale: /l/p1/c1.mtclean changed on disk')).toBe(
      'notice.job.stale',
    )
    expect(jobConflictKey(new Error('disk write failed'))).toBeNull()
    expect(jobConflictKey(new Error('not_job_busy_at_all'))).toBeNull()
  })

  it('says a stale chapter in its own words and reads it again', async () => {
    const openChapter = vi.fn(async () => ({
      project: { id: 'pr1', mode: 'single', readingDirection: 'rtl' },
      chapter: { id: 'ch1', projectId: 'pr1', pages: [], review: [] },
      pendingConversion: null,
    }))
    setBackend(
      /** @type {any} */ ({
        subscribe: () => () => {},
        openChapter,
        loadPages: async () => [],
        historyLoad: async () => ({ cursor: 0, entries: [] }),
      }),
    )
    await openEditorChapter('pr1', 'ch1')
    expect(openChapter).toHaveBeenCalledTimes(1)
    app.notices.length = 0

    expect(reportJobConflict(new Error('/x.mtclean: job_stale: /x.mtclean changed on disk'))).toBe(true)
    await vi.waitFor(() => expect(openChapter).toHaveBeenCalledTimes(2))
    expect(openChapter).toHaveBeenLastCalledWith({ projectId: 'pr1', chapterId: 'ch1', convert: false })
    expect(app.notices.map((notice) => notice.key)).toEqual(['notice.job.stale'])

    expect(reportJobConflict(new Error('disk write failed'))).toBe(false)
  })

  it('says a busy chapter instead of an undo history failure', async () => {
    vi.useFakeTimers()
    try {
      editor.chapter = { id: 'test-chapter' }
      editor.history = createHistory()
      const busy = new Error('job_busy: another Manga Cleaner process is using /l/c1.mtclean')
      setBackend(/** @type {any} */ ({ historyPush: vi.fn().mockRejectedValue(busy) }))
      recordRegionEdit('canvas.command.applyTool', 'r1', { region: null, pageStatus: 'unclean' }, {
        region: { id: 'r1', pageId: 'p1' },
        pageStatus: 'cleaned',
      })
      await vi.advanceTimersByTimeAsync(60)
      expect(app.notices.map((notice) => notice.key)).toEqual(['notice.job.busy'])
    } finally {
      vi.useRealTimers()
    }
  })
})

describe('the tool slots', () => {
  afterEach(() => {
    editor.tool = 'autoClean'
  })

  it('numbers the six tools in rail order, the selection tool last', () => {
    expect([...TOOLS]).toEqual(['autoClean', 'brush', 'shapes', 'aiMaskBrush', 'cloneHeal', 'maskSelect'])
  })

  it('arms the selection tool from slot 6, and nothing from a slot past it', () => {
    setToolBySlot(6)
    expect(editor.tool).toBe('maskSelect')
    setToolBySlot(7)
    expect(editor.tool).toBe('maskSelect')
  })

  it('arms the selection tool on S or X first, then steps its shape and swaps its mode', () => {
    editor.toolParams.maskSelect = { mode: 'add', shape: 'brush', size: 32 }
    cycleMaskSelectShape()
    expect(editor.tool).toBe('maskSelect')
    expect(editor.toolParams.maskSelect.shape).toBe('brush')
    cycleMaskSelectShape()
    cycleMaskSelectShape()
    expect(editor.toolParams.maskSelect.shape).toBe('rect')
    cycleMaskSelectShape()
    expect(editor.toolParams.maskSelect.shape).toBe('brush')

    editor.tool = 'brush'
    toggleMaskSelectMode()
    expect(editor.tool).toBe('maskSelect')
    expect(editor.toolParams.maskSelect.mode).toBe('add')
    toggleMaskSelectMode()
    expect(editor.toolParams.maskSelect.mode).toBe('remove')
    toggleMaskSelectMode()
    expect(editor.toolParams.maskSelect.mode).toBe('add')
  })
})

/**
 * A run belongs to the backend, not to the editor: leaving the chapter does
 * not end it, the jobs list follows it, and opening the chapter again picks
 * it up from what the backend lists.
 */
describe('a run that outlives the editor', () => {
  const page = (index) => ({ id: `bg${index}`, chapterId: 'ch-bg', index, number: index + 1, status: 'unclean',
    skipReason: null, regions: [], regionCount: 0, doneCount: 0, reviewCount: 0, resident: false })

  /** @param {any[]} listed */
  function backendListing(listed, extra = {}) {
    return /** @type {any} */ ({
      subscribe: () => () => {},
      openChapter: async () => ({
        project: { id: 'pr-bg', name: 'Background', mode: 'single', readingDirection: 'rtl' },
        chapter: { id: 'ch-bg', projectId: 'pr-bg', name: 'Night Shift', number: 9, pages: [page(0), page(1)], review: [] },
        pendingConversion: null,
      }),
      loadPages: async () => [],
      historyLoad: async () => ({ cursor: 0, entries: [] }),
      listJobs: vi.fn(async () => listed),
      listProjects: async () => [],
      ...extra,
    })
  }

  beforeEach(() => {
    resetJobs()
    closeEditorChapter()
    app.notices.length = 0
  })

  afterEach(() => {
    closeEditorChapter()
    resetJobs()
    setBackend(null)
  })

  it('adopts the run the chapter already has when it opens', async () => {
    setBackend(backendListing([{ runId: 'bg-run', kind: 'detect', chapterId: 'ch-bg', done: 1, total: 2 }]))
    await openEditorChapter('pr-bg', 'ch-bg')
    expect(editor.run).toMatchObject({ active: true, runId: 'bg-run', mode: 'detect', queued: 2, pagesDone: 1 })
    expect(jobById('bg-run')).toMatchObject({ kind: 'detect', status: 'running', projectName: 'Background', chapterName: 'Night Shift' })
  })

  it('adopts nothing the backend does not list, even when the jobs list still shows it', async () => {
    setBackend(backendListing([]))
    await openEditorChapter('pr-bg', 'ch-bg')
    expect(editor.run.active).toBe(false)
  })

  it('puts a started run on the jobs list, and says when the backend is at its limit', async () => {
    const runClean = vi.fn(async () => ({ runId: 'fresh', pages: [{ pageIndex: 0 }, { pageIndex: 1 }] }))
    setBackend(backendListing([], { runClean }))
    await openEditorChapter('pr-bg', 'ch-bg')
    expect(await startRun('chapter', { mode: 'detect' })).toBe('fresh')
    expect(jobById('fresh')).toMatchObject({ kind: 'detect', chapterId: 'ch-bg', total: 2, projectId: 'pr-bg', chapterNumber: 9 })

    // Leaving the editor ends nothing on the jobs list.
    closeEditorChapter()
    expect(jobById('fresh')?.status).toBe('running')

    setBackend(backendListing([], { runClean: async () => ({ runId: null, pages: [], alreadyRunning: true, atCapacity: true }) }))
    await openEditorChapter('pr-bg', 'ch-bg')
    expect(await startRun('chapter')).toBeNull()
    expect(app.notices.at(-1)?.key).toBe('notice.run.atCapacity')
    expect(editor.run.active).toBe(false)
  })
})
