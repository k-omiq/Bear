/**
 * Region edits whose answer arrives after the reader has moved on.
 *
 * The rule (M1): one gesture is one undo step, and a result that lands after a
 * page turn or a chapter switch goes on **its own chapter's** history, without
 * selecting or showing anything on the page now in view. The native side has
 * already written the patch to disk by the time the answer comes back, so an
 * answer dropped on the floor is an edit the user can see after a reload and
 * can never undo.
 *
 * Three ways to move on while an edit is out:
 *
 * - **A page turn far enough to evict the page.** The window holds the pages
 *   either side of the current one; three pages on, the edited page is a
 *   header with no regions, and the region the answer names is not in hand.
 * - **A chapter switch**, for a local tool click, a local re-run, Clean
 *   anyway, and a cloud re-run.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
vi.mock('./cloudflow.svelte.js', async (importOriginal) => ({
  // `cloudOutcomeOf` is pure: the real one reads the answers these tests fake.
  ...(/** @type {object} */ (await importOriginal())),
  cloudRefused: vi.fn(() => false),
  requestCloudConsent: vi.fn(),
  runCloudJob: vi.fn(),
}))
import { setBackend } from '../api/backend.js'
import { app } from '../state/app.svelte.js'
import {
  closeEditorChapter,
  editor,
  goToPage,
  openEditorChapter,
  redo,
  setToolParam,
  undo,
} from '../state/editor.svelte.js'
import { createHistory, settled } from '../model/history.js'
import { CLOUD_ENGINE } from '../model/masks.js'
import { applyActiveToolToRegion } from './toolapply.svelte.js'
import { cleanAnyway, deleteMask, deleteRow, keepDependencyResult, rerunMask } from './maskactions.svelte.js'
import { requestCloudConsent, runCloudJob } from './cloudflow.svelte.js'

/**
 * The region every case edits: page 1 of its chapter, with a Fill mask.
 *
 * @param {string} chapterId
 * @param {string} [maskId]
 */
function region(chapterId, maskId = `${chapterId}-p001-r1-m1`) {
  return {
    id: `${chapterId}-p001-r1`,
    pageId: `${chapterId}-p001`,
    bbox: { x: 10, y: 10, w: 20, h: 10 },
    source: 'detected',
    outcome: 'cleaned',
    gateSkipCause: null,
    mask: { id: maskId, fillMode: 'match-surround', provenance: { engine: 'fill' } },
  }
}

/**
 * A chapter of five resident pages, the first holding `region`.
 *
 * @param {string} id
 */
function chapter(id) {
  return {
    id,
    review: [],
    pages: Array.from({ length: 5 }, (_, index) => ({
      id: `${id}-p00${index + 1}`,
      index,
      sourceIndex: index,
      sourceSha: `sha-${index}`,
      number: index + 1,
      status: index === 0 ? 'unclean' : 'cleaned',
      width: 1600,
      height: 2400,
      resident: true,
      regionCount: index === 0 ? 1 : 0,
      regions: index === 0 ? [region(id)] : [],
    })),
  }
}

/** A promise and the hand that settles it. */
function deferred() {
  /** @type {(value: any) => void} */
  let resolve = () => {}
  const promise = new Promise((settle) => { resolve = settle })
  return { promise, resolve }
}

/**
 * An adapter whose journal is kept per chapter, so a case can ask which
 * chapter an entry went to, and whose edits wait for `answer`.
 */
function backend() {
  const answer = deferred()
  /** @type {Record<string, any[]>} */
  const journals = {}
  const adapter = {
    answer,
    journals,
    historyPush: vi.fn(async ({ chapterId, entry }) => {
      const journal = (journals[chapterId] ??= [])
      journal.push(entry)
      return { cursor: journal.length, entries: journal.map((e, i) => ({ seq: i + 1, label: e.label })) }
    }),
    loadPages: vi.fn(async () => []),
    applyTool: vi.fn(() => answer.promise),
    rerunMask: vi.fn(() => answer.promise),
    cleanAnyway: vi.fn(() => answer.promise),
    deleteMask: vi.fn(() => answer.promise),
    restoreRegion: vi.fn(() => answer.promise),
    keepDependencyResult: vi.fn(() => answer.promise),
  }
  setBackend(/** @type {any} */ (adapter))
  return adapter
}

/** What an edit answers: the region with a new mask, and its page cleaned. */
function applied(chapterId) {
  return {
    status: 'applied',
    region: region(chapterId, `${chapterId}-p001-r1-m2`),
    pageStatus: 'cleaned',
  }
}

beforeEach(() => {
  vi.clearAllMocks()
  app.notices.length = 0
  editor.history = createHistory()
  editor.pageIndex = 0
  editor.selectionId = null
  editor.tool = 'contentAwareFill'
  setToolParam('contentAwareFill', 'engine', 'local')
})

afterEach(() => {
  setToolParam('contentAwareFill', 'engine', 'local')
  setBackend(null)
  editor.chapter = null
  editor.tool = 'autoClean'
})

it('reports a busy chapter when delete and Keep are refused, without recording edits', async () => {
  editor.chapter = chapter('c1')
  const adapter = backend()
  const busy = new Error('job_busy')
  adapter.deleteMask.mockRejectedValue(busy)
  adapter.restoreRegion.mockRejectedValue(busy)
  adapter.keepDependencyResult.mockRejectedValue(busy)
  const masked = /** @type {any} */ (region('c1'))
  const bare = { ...masked, mask: null }
  const reviewed = { ...masked, mask: { ...masked.mask, dependencyReview: true } }

  expect(await deleteMask(masked)).toBe(false)
  expect(await deleteRow(bare)).toBe(false)
  expect(await keepDependencyResult(reviewed)).toBe(false)
  expect(app.notices.filter((notice) => notice.key === 'notice.job.busy')).toHaveLength(3)
  expect(editor.history.entries).toEqual([])
})

describe('an edit answered after its page left the window', () => {
  it('records a local tool click on the chapter history and updates only the page header', async () => {
    editor.chapter = chapter('c1')
    const adapter = backend()
    const pending = applyActiveToolToRegion('c1-p001-r1')
    await vi.waitFor(() => expect(adapter.applyTool).toHaveBeenCalledTimes(1))

    goToPage(3)
    const page = editor.chapter.pages[0]
    expect(page.resident).toBe(false)
    const selection = editor.selectionId

    adapter.answer.resolve(applied('c1'))
    expect(await pending).toBe(true)
    await vi.waitFor(() => expect(adapter.journals.c1).toHaveLength(1))
    const [entry] = adapter.journals.c1
    expect(entry.label).toBe('canvas.command.applyTool')
    expect(entry.regionId).toBe('c1-p001-r1')
    expect(entry.before.region.mask.id).toBe('c1-p001-r1-m1')
    expect(entry.after.region.mask.id).toBe('c1-p001-r1-m2')
    expect(editor.history.entries.map((e) => e.label)).toEqual(['canvas.command.applyTool'])
    // The page's header moves with it; nothing is put on a page not in hand,
    // and nothing is selected on the page in view.
    expect(page.status).toBe('cleaned')
    expect(page.regions).toEqual([])
    expect(editor.selectionId).toBe(selection)
    expect(editor.pageIndex).toBe(3)
  })

  it('records a cloud tool click the same way', async () => {
    editor.chapter = chapter('c1')
    setToolParam('contentAwareFill', 'engine', 'cloud')
    const adapter = backend()
    vi.mocked(requestCloudConsent).mockResolvedValue({ params: {}, attemptId: 'attempt-late' })
    vi.mocked(runCloudJob).mockImplementation(async (_grant, _where, call) => call({}))
    const pending = applyActiveToolToRegion('c1-p001-r1')
    await vi.waitFor(() => expect(adapter.applyTool).toHaveBeenCalledTimes(1))

    goToPage(3)
    adapter.answer.resolve(applied('c1'))
    expect(await pending).toBe(true)
    await vi.waitFor(() => expect(adapter.journals.c1).toHaveLength(1))
    expect(adapter.journals.c1[0].after.region.mask.id).toBe('c1-p001-r1-m2')
    expect(editor.chapter.pages[0].regions).toEqual([])
  })

  it('still refuses an answer for a region deleted from a page that is in hand', async () => {
    editor.chapter = chapter('c1')
    const adapter = backend()
    const pending = applyActiveToolToRegion('c1-p001-r1')
    await vi.waitFor(() => expect(adapter.applyTool).toHaveBeenCalledTimes(1))
    // Gone from a page the window still holds: not a page out of reach.
    editor.chapter.pages[0].regions = []

    adapter.answer.resolve(applied('c1'))
    expect(await pending).toBe(false)
    expect(adapter.historyPush).not.toHaveBeenCalled()
  })
})

describe('an edit answered after its chapter was closed', () => {
  /**
   * Switch to chapter two while the edit is out, answer it, and return where
   * each chapter's entries went.
   *
   * @param {(adapter: ReturnType<typeof backend>) => Promise<unknown>} edit
   * @param {any} answer
   * @param {number} [entries] - how many undo entries the edit makes
   */
  async function acrossASwitch(edit, answer, entries = 1) {
    editor.chapter = chapter('c1')
    const adapter = backend()
    const pending = edit(adapter)
    await vi.waitFor(() => expect(
      adapter.applyTool.mock.calls.length + adapter.rerunMask.mock.calls.length +
      adapter.cleanAnyway.mock.calls.length + adapter.deleteMask.mock.calls.length +
      adapter.restoreRegion.mock.calls.length + adapter.keepDependencyResult.mock.calls.length,
    ).toBe(1))
    // What opening a chapter resets (`openEditorChapter`): its own history,
    // and no selection carried over from the last one.
    editor.chapter = chapter('c2')
    editor.history = createHistory()
    editor.selectionId = null
    const other = JSON.stringify(editor.chapter)
    adapter.answer.resolve(answer)
    await pending
    await vi.waitFor(() => expect(adapter.journals.c1 ?? []).toHaveLength(entries))
    // Chapter two is exactly as it was: no entry, no region, no selection.
    expect(adapter.journals.c2).toBeUndefined()
    expect(editor.history.entries).toEqual([])
    expect(JSON.stringify(editor.chapter)).toBe(other)
    expect(editor.selectionId).toBe(null)
    return adapter.journals.c1?.[0] ?? null
  }

  it('records a local tool click on the chapter it was made in', async () => {
    const entry = await acrossASwitch(() => applyActiveToolToRegion('c1-p001-r1'), applied('c1'))
    expect(entry.label).toBe('canvas.command.applyTool')
    expect(entry.after.region.mask.id).toBe('c1-p001-r1-m2')
  })

  it('records a cloud re-run on the chapter it was made in', async () => {
    vi.mocked(requestCloudConsent).mockResolvedValue({ params: {}, attemptId: 'attempt-rerun' })
    vi.mocked(runCloudJob).mockImplementation(async (_grant, _where, call) => call({}))
    const entry = await acrossASwitch(
      () => rerunMask(/** @type {any} */ (region('c1')), 'engine', CLOUD_ENGINE),
      applied('c1'),
    )
    expect(entry.label).toBe('masks.command.rerunMask')
    expect(entry.after.region.mask.id).toBe('c1-p001-r1-m2')
  })

  it('records a local re-run, Clean anyway and a delete on the chapter they were made in', async () => {
    const rerun = await acrossASwitch(() => rerunMask(/** @type {any} */ (region('c1')), 'stronger'), applied('c1'))
    expect(rerun.label).toBe('masks.command.rerunMask')
    const clean = await acrossASwitch(() => cleanAnyway(/** @type {any} */ (region('c1'))), applied('c1'))
    expect(clean.label).toBe('masks.command.cleanAnyway')
    const removal = await acrossASwitch(
      () => deleteMask(/** @type {any} */ (region('c1'))),
      { pageStatus: 'unclean' },
    )
    expect(removal.label).toBe('masks.command.deleteMask')
    expect(removal.after.region).toBe(null)
  })

  // A region with no mask is removed through the same applier undo uses, and
  // that applier touched whatever chapter was open when the backend answered.
  it('removes a warning row on its own chapter, and leaves the open one alone', async () => {
    const warning = { ...region('c1'), mask: null }
    const removal = await acrossASwitch(() => deleteRow(/** @type {any} */ (warning)), null)
    expect(removal.label).toBe('masks.command.deleteRegion')
    expect(removal.after.region).toBe(null)
  })

  // Not an undoable edit, so no entry: only the open chapter to leave alone.
  it('keeps a reviewed result without touching the chapter opened meanwhile', async () => {
    const reviewed = region('c1')
    reviewed.mask = /** @type {any} */ ({ ...reviewed.mask, dependencyReview: { reasonKey: 'masks.dependency.changed' } })
    expect(await acrossASwitch(() => keepDependencyResult(/** @type {any} */ (reviewed)), region('c1'), 0)).toBe(null)
  })

  // An undo moves its own chapter's journal, so its replay lands there too.
  it('replays an undo on the chapter it was pressed in', async () => {
    editor.chapter = chapter('c1')
    const adapter = backend()
    const entry = {
      label: 'masks.command.rerunMask',
      op: 'region-state',
      regionId: 'c1-p001-r1',
      before: { present: true, pageStatus: 'unclean', region: region('c1') },
      after: { present: true, pageStatus: 'cleaned', region: region('c1', 'c1-p001-r1-m2') },
    }
    Object.assign(adapter, { historyMove: vi.fn(async () => ({ cursor: 0, entry })) })
    editor.history = createHistory()
    editor.history.entries = [{ seq: 1, label: entry.label }]
    editor.history.cursor = 1
    undo()
    await vi.waitFor(() => expect(adapter.restoreRegion).toHaveBeenCalledTimes(1))
    expect(adapter.restoreRegion.mock.calls[0][0]).toMatchObject({ regionId: 'c1-p001-r1', region: { mask: { id: 'c1-p001-r1-m1' } } })
    editor.chapter = chapter('c2')
    const other = JSON.stringify(editor.chapter)
    adapter.answer.resolve(region('c1'))
    await vi.waitFor(() => expect(adapter.restoreRegion).toHaveReturned())
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(JSON.stringify(editor.chapter)).toBe(other)
    expect(adapter.loadPages).not.toHaveBeenCalled()
  })
})

/**
 * The background journal is only worth writing if the chapter can use it: a
 * result that landed while its chapter was closed has to undo and redo like
 * any other edit once that chapter is open again. Driven through the real
 * open, undo and redo, over a fake library that keeps each chapter's pages
 * and journal the way the native side does.
 */
describe('a late result, once its chapter is open again', () => {
  /** A library of two chapters on "disk", with a journal each. */
  function library() {
    const disk = { c1: chapter('c1'), c2: chapter('c2') }
    /** @type {Record<string, {entries: any[], cursor: number}>} */
    const journals = { c1: { entries: [], cursor: 0 }, c2: { entries: [], cursor: 0 } }
    const answer = deferred()
    const view = (/** @type {string} */ id) => ({
      cursor: journals[id].cursor,
      entries: journals[id].entries.map((entry, index) => ({ seq: index + 1, label: entry.label })),
    })
    /** Put a region, or its absence, and its page's status on disk. */
    const write = (/** @type {string} */ regionId, /** @type {any} */ found, /** @type {string|undefined} */ pageStatus) => {
      const page = Object.values(disk).flatMap((entry) => entry.pages).find((candidate) => regionId.startsWith(`${candidate.id}-`))
      if (!page) return null
      page.regions = page.regions.filter((candidate) => candidate.id !== regionId)
      if (found) page.regions.push(structuredClone(found))
      page.regionCount = page.regions.length
      if (pageStatus) page.status = pageStatus
      return found ? structuredClone(found) : null
    }
    const adapter = {
      disk,
      journals,
      answer,
      subscribe: () => () => {},
      openChapter: vi.fn(async ({ chapterId }) => ({ project: { id: 'p1', mode: 'single' }, chapter: structuredClone(disk[chapterId]) })),
      loadPages: vi.fn(async ({ chapterId, indices }) =>
        indices.map((index) => disk[chapterId].pages[index]).filter(Boolean).map((page) => structuredClone(page))),
      historyLoad: vi.fn(async ({ chapterId }) => view(chapterId)),
      historyPush: vi.fn(async ({ chapterId, entry }) => {
        const journal = journals[chapterId]
        journal.entries.length = journal.cursor
        journal.entries.push(structuredClone(entry))
        journal.cursor = journal.entries.length
        return view(chapterId)
      }),
      historyMove: vi.fn(async ({ chapterId, direction }) => {
        const journal = journals[chapterId]
        let entry = null
        if (direction === 'undo' && journal.cursor > 0) entry = journal.entries[--journal.cursor]
        if (direction === 'redo' && journal.cursor < journal.entries.length) entry = journal.entries[journal.cursor++]
        return { ...view(chapterId), entry }
      }),
      restoreRegion: vi.fn(async ({ regionId, region: found, pageStatus }) => write(regionId, found, pageStatus)),
      rerunMask: vi.fn(async () => {
        const result = await answer.promise
        write(result.region.id, result.region, result.pageStatus)
        return result
      }),
    }
    setBackend(/** @type {any} */ (adapter))
    return adapter
  }

  beforeEach(() => {
    /** @type {Map<string, string>} */
    const values = new Map()
    vi.stubGlobal('localStorage', {
      getItem: (/** @type {string} */ key) => values.get(key) ?? null,
      setItem: (/** @type {string} */ key, /** @type {string} */ value) => void values.set(key, String(value)),
      removeItem: (/** @type {string} */ key) => void values.delete(key),
    })
  })

  afterEach(() => {
    closeEditorChapter()
    vi.unstubAllGlobals()
  })

  it('undoes and redoes a re-run that landed while its chapter was closed', async () => {
    const lib = library()
    expect(await openEditorChapter('p1', 'c1')).toBe(true)
    const pending = rerunMask(/** @type {any} */ (editor.chapter.pages[0].regions[0]), 'stronger')
    await vi.waitFor(() => expect(lib.rerunMask).toHaveBeenCalledTimes(1))

    expect(await openEditorChapter('p1', 'c2')).toBe(true)
    lib.answer.resolve(applied('c1'))
    expect(await pending).toBe(true)
    await vi.waitFor(() => expect(lib.journals.c1.entries).toHaveLength(1))
    expect(lib.journals.c2.entries).toHaveLength(0)
    expect(editor.history.entries).toEqual([])

    expect(await openEditorChapter('p1', 'c1')).toBe(true)
    const shown = () => {
      const page = editor.chapter.pages[0]
      return { mask: page.regions.find((candidate) => candidate.id === 'c1-p001-r1')?.mask?.id ?? null, status: page.status }
    }
    expect(editor.history.entries.map((entry) => entry.label)).toEqual(['masks.command.rerunMask'])
    expect(shown()).toEqual({ mask: 'c1-p001-r1-m2', status: 'cleaned' })

    undo()
    await settled(editor.history)
    expect(shown()).toEqual({ mask: 'c1-p001-r1-m1', status: 'unclean' })
    expect(lib.disk.c1.pages[0].status).toBe('unclean')
    redo()
    await settled(editor.history)
    expect(shown()).toEqual({ mask: 'c1-p001-r1-m2', status: 'cleaned' })
    expect(lib.disk.c1.pages[0].regions[0].mask.id).toBe('c1-p001-r1-m2')
    // Chapter two was never written.
    expect(lib.disk.c2).toEqual(chapter('c2'))
  })
})
