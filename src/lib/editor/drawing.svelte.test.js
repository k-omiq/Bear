/**
 * The AI mask brush's seam: a stroke on the page, and the one adapter call it
 * turns into.
 *
 * **It is always `createRegion`.** The tool used to land on either method - it
 * asked a snapping helper which existing region the stroke was "about" and
 * edited that one through `applyTool` where it found one - and that is how a
 * stroke aimed at a leftover *beside* a layer box re-ran the layer box.
 * The painted shape is the mask now, so the
 * stroke makes its own region and never lands on a neighbour's.
 *
 * What must hold either way is that **the engine the user picked travels with
 * the stroke**. It is chosen in the tool window, held in `editor.toolParams`,
 * and read from `params.engine` by `src-tauri/src/region.rs#named_rung`; a
 * gesture that dropped it would clean with whatever the fill mode implied and
 * give no sign of having done so.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
vi.mock('./cloudflow.svelte.js', async (importOriginal) => ({
  // `cloudOutcomeOf` is pure: the real one reads the answers these tests fake.
  ...(/** @type {object} */ (await importOriginal())),
  cloudRefused: vi.fn(() => false),
  requestCloudConsent: vi.fn(),
  runCloudJob: vi.fn(),
}))
import { editor, goToPage, redo, selectedRegion, setToolParam, undo } from '../state/editor.svelte.js'
import { app } from '../state/app.svelte.js'
import { setBackend } from '../api/backend.js'
import { canRedo, canUndo, createHistory, settled } from '../model/history.js'
import { draft, beginDraft, resetDraftState } from './draft.svelte.js'
import { commitDraft } from './drawing.svelte.js'
import { deleteRow } from './maskactions.svelte.js'
import { applyActiveToolToRegion } from './toolapply.svelte.js'
import { requestCloudConsent, runCloudJob } from './cloudflow.svelte.js'

/** A page holding whatever regions a case needs. */
function chapterWith(regions) {
  return {
    id: 'c1',
    review: [],
    pages: [
      {
        id: 'c1-p001',
        index: 0,
        sourceIndex: 0,
        sourceSha: 'fixture-source',
        number: 1,
        status: 'unclean',
        width: 1600,
        height: 2400,
        resident: true,
        regionCount: regions.length,
        regions,
      },
    ],
  }
}

/** Two resident pages side by side, so a result could land on either. */
function twoPages() {
  const chapter = chapterWith([])
  chapter.pages.push({
    ...chapter.pages[0],
    id: 'c1-p002',
    index: 1,
    sourceIndex: 1,
    sourceSha: 'fixture-source-2',
    number: 2,
    regions: [],
    regionCount: 0,
  })
  return chapter
}

/** The stroke: a draft that has already been dragged, ready to commit. */
function stroke(bbox) {
  beginDraft({
    tool: 'aiMaskBrush',
    kind: 'stroke',
    pageId: 'c1-p001',
    points: [{ x: bbox.x, y: bbox.y }],
    bbox,
    mode: 'add',
    keyboard: false,
    moved: true,
  })
}

/** An adapter that answers both region methods and swallows the history push. */
function backend(overrides = {}) {
  return {
    historyPush: vi.fn().mockResolvedValue({ cursor: 1, entries: [] }),
    createRegion: vi.fn(async ({ bbox }) => ({
      region: { id: 'c1-p001-h1', pageId: 'c1-p001', bbox, source: 'hand', outcome: 'cleaned' },
      pageStatus: 'cleaned',
    })),
    applyTool: vi.fn(async () => ({
      status: 'applied',
      region: { id: 'r1', pageId: 'c1-p001', source: 'hand', outcome: 'cleaned' },
      pageStatus: 'cleaned',
    })),
    ...overrides,
  }
}

const SEED_ID = 'c1-p001-h1'
const SEED_MASK = 'c1-p001-h1-m1'
const RENDERED_MASK = 'c1-p001-h1-m2'
const BYSTANDER_ID = 'c1-p001-r9'
const BYSTANDER_MASK = 'c1-p001-r9-m1'
const SEED_BOX = { x: 20, y: 30, w: 12, h: 4 }

/** A region already on the page that no step of a cloud stroke may touch. */
function bystander() {
  return {
    id: BYSTANDER_ID,
    pageId: 'c1-p001',
    bbox: { x: 70, y: 70, w: 10, h: 10 },
    source: 'detected',
    outcome: 'cleaned',
    mask: { id: BYSTANDER_MASK, fillMode: 'match-surround', provenance: { engine: 'fill' } },
  }
}

/** What a cloud stroke stores before its render: the local seed. */
function seed() {
  return {
    id: SEED_ID,
    pageId: 'c1-p001',
    bbox: { ...SEED_BOX },
    source: 'hand',
    outcome: 'cleaned',
    mask: { id: SEED_MASK, fillMode: 'match-surround', provenance: { engine: 'fill' } },
  }
}

/** What the render makes of the seed. */
function rendered() {
  return { ...seed(), mask: { id: RENDERED_MASK, fillMode: 'reconstruct', provenance: { engine: 'flux' } } }
}

/**
 * A cloud stroke whose render waits for `release`, on a page that already
 * holds a bystander, over an adapter with a real journal behind the history:
 * `historyPush` writes at the cursor and drops what was ahead of it,
 * `historyMove` answers the entry it crossed, and `restoreRegion` answers the
 * side it was handed. So `undo` and `redo` run the editor's own replay, and a
 * case asserts what that replay leaves on the page rather than what was
 * pushed.
 *
 * `pages` adds pages after the first, so a page turn can take the stroke's
 * page out of the window.
 *
 * @param {{answer?: () => Promise<any>, consent?: () => Promise<any>, pages?: number}} [options]
 */
function cloudStroke({
  answer = async () => ({ status: 'applied', region: rendered(), pageStatus: 'cleaned' }),
  consent = async () => ({ params: {}, attemptId: 'attempt-journal' }),
  pages = 1,
} = {}) {
  const chapter = chapterWith([bystander()])
  for (let index = 1; index < pages; index += 1) {
    chapter.pages.push({
      ...chapter.pages[0],
      id: `c1-p00${index + 1}`,
      index,
      sourceIndex: index,
      sourceSha: `fixture-source-${index + 1}`,
      number: index + 1,
      regions: [],
      regionCount: 0,
    })
  }
  editor.chapter = chapter
  setToolParam('aiMaskBrush', 'engine', 'cloud')
  /** @type {() => void} */
  let release = () => {}
  const gate = new Promise((resolve) => { release = () => resolve(undefined) })
  /** @type {{entries: any[], cursor: number}} */
  const journal = { entries: [], cursor: 0 }
  const view = () => ({
    cursor: journal.cursor,
    entries: journal.entries.map((entry, index) => ({ seq: index + 1, label: entry.label })),
  })
  const adapter = backend({
    historyPush: vi.fn(async ({ entry }) => {
      journal.entries.length = journal.cursor
      journal.entries.push(structuredClone(entry))
      journal.cursor = journal.entries.length
      return view()
    }),
    historyMove: vi.fn(async ({ direction }) => {
      let entry = null
      if (direction === 'undo' && journal.cursor > 0) entry = journal.entries[--journal.cursor]
      if (direction === 'redo' && journal.cursor < journal.entries.length) entry = journal.entries[journal.cursor++]
      return { ...view(), entry }
    }),
    restoreRegion: vi.fn(async ({ region }) => (region ? structuredClone(region) : null)),
    createRegion: vi.fn(async () => ({ region: seed(), pageStatus: 'cleaned' })),
    applyTool: vi.fn(async () => {
      await gate
      return answer()
    }),
    deleteMask: vi.fn(async () => ({ pageStatus: 'cleaned' })),
    loadPages: vi.fn(async () => []),
  })
  setBackend(/** @type {any} */ (adapter))
  vi.mocked(requestCloudConsent).mockImplementation(consent)
  // As the real one does: a job that throws is a job that stopped, and the
  // caller hears `null`.
  vi.mocked(runCloudJob).mockImplementation(async (_grant, _where, call) => {
    try {
      return await call({})
    } catch {
      return null
    }
  })
  stroke(SEED_BOX)
  const pending = commitDraft()
  return { adapter, journal, pending, release }
}

/** The open page's masks by region id. */
function masks() {
  return Object.fromEntries(editor.chapter.pages[0].regions.map((region) => [region.id, region.mask?.id ?? null]))
}

/** The seed as the open page holds it. */
function seedOnPage() {
  return editor.chapter.pages[0].regions.find((region) => region.id === SEED_ID)
}

/** The undo index's labels, oldest first. */
function labels() {
  return editor.history.entries.map((entry) => entry.label)
}

/** @param {() => void} action - `undo` or `redo` */
async function step(action) {
  action()
  await settled(editor.history)
}

/**
 * The bystander is as it was, and nothing was ever sent for it.
 *
 * @param {any} adapter
 */
function expectBystanderUntouched(adapter) {
  expect(editor.chapter.pages[0].regions.find((region) => region.id === BYSTANDER_ID)).toEqual(bystander())
  for (const [spec] of adapter.restoreRegion.mock.calls) expect(spec.regionId).not.toBe(BYSTANDER_ID)
  for (const [spec] of adapter.deleteMask.mock.calls) expect(spec.maskId).not.toBe(BYSTANDER_MASK)
  for (const [spec] of adapter.applyTool.mock.calls) expect(spec.regionId).not.toBe(BYSTANDER_ID)
}

describe('the AI mask brush stroke', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    app.notices.length = 0
    editor.history = createHistory()
    editor.tool = 'aiMaskBrush'
    setToolParam('aiMaskBrush', 'engine', 'fill')
    editor.pageIndex = 0
    editor.selectionId = null
    resetDraftState()
  })

  afterEach(() => {
    setToolParam('aiMaskBrush', 'engine', 'fill')
    setBackend(null)
    editor.chapter = null
    editor.tool = 'autoClean'
    resetDraftState()
  })

  it('creates a region with the engine the tool window picked, where nothing was found', async () => {
    editor.chapter = chapterWith([])
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))
    setToolParam('aiMaskBrush', 'engine', 'lama')

    stroke({ x: 20, y: 30, w: 12, h: 4 })
    expect(await commitDraft()).toBe(true)

    expect(adapter.applyTool).not.toHaveBeenCalled()
    expect(adapter.createRegion).toHaveBeenCalledTimes(1)
    const spec = adapter.createRegion.mock.calls[0][0]
    expect(spec.tool).toBe('aiMaskBrush')
    expect(spec.chapterId).toBe('c1')
    expect(spec.pageIndex).toBe(0)
    expect(spec.sourceIndex).toBe(0)
    expect(spec.sourceSha).toBe('fixture-source')
    expect(spec.params.engine).toBe('lama')
    // The stroke's own box, unchanged: nothing was there to snap to.
    expect(spec.bbox).toEqual({ x: 20, y: 30, w: 12, h: 4 })
  })

  // The stroke overlaps a neighbouring region - a layer box
  // right beside the leftover the user is aiming at - and it must still be its
  // own mask. Retargeting sent it to `applyTool` on that region, which re-ran
  // the neighbour and replaced its patch with the stroke.
  it('never retargets onto a region it merely touches', async () => {
    editor.chapter = chapterWith([
      {
        id: 'r1',
        pageId: 'c1-p001',
        bbox: { x: 22, y: 30, w: 10, h: 4 },
        detected: true,
        outcome: 'cleaned',
      },
    ])
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))
    setToolParam('aiMaskBrush', 'engine', 'lama')

    stroke({ x: 20, y: 30, w: 12, h: 4 })
    expect(await commitDraft()).toBe(true)

    expect(adapter.applyTool).not.toHaveBeenCalled()
    expect(adapter.createRegion).toHaveBeenCalledTimes(1)
    const spec = adapter.createRegion.mock.calls[0][0]
    expect(spec.params.engine).toBe('lama')
    // The stroke's own box, not the union with the region it grazed.
    expect(spec.bbox).toEqual({ x: 20, y: 30, w: 12, h: 4 })
  })

  // A stroke lying wholly inside an existing region is the same answer: a
  // leftover inside a cleaned box is new paint over it, not a re-run of it.
  it('makes its own mask even for a stroke wholly inside a region', async () => {
    editor.chapter = chapterWith([
      {
        id: 'r1',
        pageId: 'c1-p001',
        bbox: { x: 10, y: 10, w: 40, h: 40 },
        detected: true,
        outcome: 'cleaned',
      },
    ])
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))

    stroke({ x: 20, y: 20, w: 6, h: 4 })
    expect(await commitDraft()).toBe(true)

    expect(adapter.applyTool).not.toHaveBeenCalled()
    expect(adapter.createRegion.mock.calls[0][0].bbox).toEqual({ x: 20, y: 20, w: 6, h: 4 })
  })

  // The gesture is one undo step whichever method it landed on, and a creation's
  // "before" side is the absence of the region - `restoreRegion` reads that as
  // "take it away again".
  it('records one undoable edit for the stroke', async () => {
    editor.chapter = chapterWith([])
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))

    stroke({ x: 20, y: 30, w: 12, h: 4 })
    await commitDraft()

    expect(adapter.historyPush).toHaveBeenCalledTimes(1)
    const { entry } = adapter.historyPush.mock.calls[0][0]
    expect(entry.op).toBe('region-state')
    expect(entry.regionId).toBe('c1-p001-h1')
    expect(entry.before.region).toBe(null)
    expect(entry.after.region).toMatchObject({ id: 'c1-p001-h1' })
  })

  it('commits rapid strokes in gesture order, with one history entry each', async () => {
    editor.chapter = chapterWith([])
    let releaseFirst
    const gate = new Promise((resolve) => { releaseFirst = resolve })
    let calls = 0
    const adapter = backend({ createRegion: vi.fn(async ({ bbox }) => {
      const id = `c1-p001-h${++calls}`
      if (calls === 1) await gate
      return { region: { id, pageId: 'c1-p001', bbox, source: 'hand', outcome: 'cleaned' }, pageStatus: 'cleaned' }
    }) })
    setBackend(/** @type {any} */ (adapter))
    stroke({ x: 20, y: 30, w: 12, h: 4 })
    const first = commitDraft()
    stroke({ x: 22, y: 30, w: 12, h: 4 })
    const second = commitDraft()
    await vi.waitFor(() => expect(adapter.createRegion).toHaveBeenCalledTimes(1))
    releaseFirst()
    expect(await first).toBe(true)
    expect(await second).toBe(true)
    expect(adapter.createRegion.mock.calls.map(([spec]) => spec.bbox.x)).toEqual([20, 22])
    expect(adapter.historyPush.mock.calls.map(([{ entry }]) => entry.regionId))
      .toEqual(['c1-p001-h1', 'c1-p001-h2'])
  })

  it('does not apply a late stroke to a different open chapter', async () => {
    editor.chapter = chapterWith([])
    let release
    const gate = new Promise((resolve) => { release = resolve })
    const adapter = backend({ createRegion: vi.fn(async ({ bbox }) => {
      await gate
      return { region: { id: 'c1-p001-h1', pageId: 'c1-p001', bbox }, pageStatus: 'cleaned' }
    }) })
    setBackend(/** @type {any} */ (adapter))
    stroke({ x: 20, y: 30, w: 12, h: 4 })
    const pending = commitDraft()
    await vi.waitFor(() => expect(adapter.createRegion).toHaveBeenCalledTimes(1))
    editor.chapter = { ...chapterWith([]), id: 'c2' }
    release()
    expect(await pending).toBe(true)
    expect(editor.chapter.pages[0].regions).toEqual([])
    expect(editor.selectionId).toBe(null)
    expect(adapter.historyPush).toHaveBeenCalledTimes(1)
    expect(adapter.historyPush.mock.calls[0][0].chapterId).toBe('c1')
  })

  // The selection is what the editor-wide Delete acts on. A stroke that lands
  // after the reader turned the page is kept on the page it was drawn on, and
  // is not selected there: a selection on a page nobody is looking at is one
  // Delete would remove unseen.
  it('selects a stroke only while its page is still the one in view', async () => {
    editor.chapter = twoPages()
    let release
    const gate = new Promise((resolve) => { release = resolve })
    let creates = 0
    const adapter = backend({
      createRegion: vi.fn(async ({ bbox }) => {
        const id = `c1-p001-h${++creates}`
        if (creates === 1) await gate
        return { region: { id, pageId: 'c1-p001', bbox, source: 'hand', outcome: 'cleaned' }, pageStatus: 'cleaned' }
      }),
      loadPages: vi.fn(async () => []),
    })
    setBackend(/** @type {any} */ (adapter))

    stroke({ x: 20, y: 30, w: 12, h: 4 })
    const pending = commitDraft()
    await vi.waitFor(() => expect(adapter.createRegion).toHaveBeenCalledTimes(1))
    goToPage(1)
    release()
    expect(await pending).toBe(true)

    expect(editor.chapter.pages[0].regions.map((region) => region.id)).toEqual(['c1-p001-h1'])
    expect(editor.pageIndex).toBe(1)
    expect(editor.selectionId).toBe(null)
    expect(selectedRegion()).toBe(null)

    // Back on its page, the next stroke there is selected as it lands.
    goToPage(0)
    stroke({ x: 40, y: 30, w: 12, h: 4 })
    expect(await commitDraft()).toBe(true)
    expect(editor.selectionId).toBe('c1-p001-h2')
  })

  // A page turn, not a chapter switch: the chapter is the same one, both pages
  // are in hand, and the result must still land on the page the stroke was
  // drawn on. A stroke queued behind it on the same page follows it there.
  it('attaches strokes pending across a page switch to the page they were drawn on', async () => {
    editor.chapter = twoPages()
    let releaseFirst
    const gate = new Promise((resolve) => { releaseFirst = resolve })
    let creates = 0
    const adapter = backend({
      createRegion: vi.fn(async ({ bbox }) => {
        const id = `c1-p001-h${++creates}`
        if (creates === 1) await gate
        return { region: { id, pageId: 'c1-p001', bbox, source: 'hand', outcome: 'cleaned' }, pageStatus: 'cleaned' }
      }),
      loadPages: vi.fn(async () => []),
    })
    setBackend(/** @type {any} */ (adapter))

    stroke({ x: 20, y: 30, w: 12, h: 4 })
    const first = commitDraft()
    stroke({ x: 40, y: 30, w: 12, h: 4 })
    const second = commitDraft()
    await vi.waitFor(() => expect(adapter.createRegion).toHaveBeenCalledTimes(1))

    goToPage(1)
    expect(editor.pageIndex).toBe(1)
    releaseFirst()
    expect(await first).toBe(true)
    expect(await second).toBe(true)

    // Both sent for the page they were drawn on, the queued one included.
    expect(adapter.createRegion.mock.calls.map(([spec]) => spec.pageIndex)).toEqual([0, 0])
    const [drawnOn, switchedTo] = editor.chapter.pages
    expect(drawnOn.regions.map((region) => region.id)).toEqual(['c1-p001-h1', 'c1-p001-h2'])
    expect(switchedTo.regions).toEqual([])
    expect(switchedTo.status).toBe('unclean')
    // The reader stays where they turned to, and each stroke is one undo step.
    expect(editor.pageIndex).toBe(1)
    expect(adapter.historyPush.mock.calls.map(([{ entry }]) => entry.regionId))
      .toEqual(['c1-p001-h1', 'c1-p001-h2'])
  })

  // The cloud render of a stroke is the window a delete can land in: the local
  // seed is on the page, the render has not answered, and the user removes it.
  // The late answer must not put it back. Two gestures, two steps, in the
  // order they happened: the stroke's creation (as the seed it was when it
  // was deleted), then the delete. Undoing the delete brings the seed back,
  // and one more undo removes it; with the delete alone on the history the
  // seed could never be undone.
  it('does not resurrect a stroke deleted while its render is pending', async () => {
    editor.chapter = chapterWith([])
    setToolParam('aiMaskBrush', 'engine', 'cloud')
    let release
    const gate = new Promise((resolve) => { release = resolve })
    const seed = {
      id: 'c1-p001-h1',
      pageId: 'c1-p001',
      bbox: { x: 20, y: 30, w: 12, h: 4 },
      source: 'hand',
      outcome: 'cleaned',
      mask: { id: 'c1-p001-h1-m1', fillMode: 'match-surround', provenance: { engine: 'fill' } },
    }
    const adapter = backend({
      createRegion: vi.fn(async () => ({ region: seed, pageStatus: 'cleaned' })),
      applyTool: vi.fn(async () => {
        await gate
        return {
          status: 'applied',
          region: { ...seed, mask: { id: 'c1-p001-h1-m2', fillMode: 'reconstruct', provenance: { engine: 'flux' } } },
          pageStatus: 'cleaned',
        }
      }),
      deleteMask: vi.fn(async () => ({ pageStatus: 'unclean' })),
      loadPages: vi.fn(async () => []),
    })
    setBackend(/** @type {any} */ (adapter))
    vi.mocked(requestCloudConsent).mockResolvedValue({ params: {}, attemptId: 'attempt-3' })
    vi.mocked(runCloudJob).mockImplementation(async (_grant, _where, call) => call({}))

    stroke({ x: 20, y: 30, w: 12, h: 4 })
    const pending = commitDraft()
    await vi.waitFor(() => expect(adapter.applyTool).toHaveBeenCalledTimes(1))
    const page = editor.chapter.pages[0]
    expect(page.regions.map((region) => region.id)).toEqual(['c1-p001-h1'])

    expect(await deleteRow(page.regions[0])).toBe(true)
    expect(adapter.deleteMask).toHaveBeenCalledWith({ maskId: 'c1-p001-h1-m1' })
    expect(page.regions).toEqual([])

    release()
    expect(await pending).toBe(false)
    expect(editor.chapter.pages[0].regions).toEqual([])
    expect(editor.selectionId).not.toBe('c1-p001-h1')
    await vi.waitFor(() => expect(adapter.historyPush).toHaveBeenCalledTimes(2))
    const [creation, removal] = adapter.historyPush.mock.calls.map(([{ entry }]) => entry)
    expect(creation.label).toBe('canvas.command.drawMask')
    expect(creation.regionId).toBe('c1-p001-h1')
    expect(creation.before.region).toBe(null)
    expect(creation.after.region).toMatchObject({ id: 'c1-p001-h1', mask: { id: 'c1-p001-h1-m1' } })
    expect(removal.label).toBe('masks.command.deleteMask')
    expect(removal.before.region).toMatchObject({ id: 'c1-p001-h1', mask: { id: 'c1-p001-h1-m1' } })
    expect(removal.after.region).toBe(null)
  })

  // The delete undone before the render answers: the seed is back, and the
  // render lands on it. That is an edit of the seed now, not a second
  // creation from nothing, and a new action: the undone delete is no longer
  // ahead to redo. Undo then walks back to the seed and to nothing, and redo
  // forward again, through the editor's own replay.
  it('lands a render on a seed whose delete was undone as an edit of that seed', async () => {
    const { adapter, pending, release } = cloudStroke()
    await vi.waitFor(() => expect(adapter.applyTool).toHaveBeenCalledTimes(1))
    expect(await deleteRow(/** @type {any} */ (seedOnPage()))).toBe(true)
    await step(undo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: SEED_MASK })

    release()
    expect(await pending).toBe(true)
    await settled(editor.history)
    expect(labels()).toEqual(['canvas.command.drawMask', 'canvas.command.drawMask'])
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: RENDERED_MASK })
    expect(canRedo(editor.history)).toBe(false)
    const moves = adapter.historyMove.mock.calls.length
    await step(redo)
    expect(adapter.historyMove).toHaveBeenCalledTimes(moves)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: RENDERED_MASK })

    await step(undo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: SEED_MASK })
    await step(undo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK })
    expect(canUndo(editor.history)).toBe(false)
    await step(redo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: SEED_MASK })
    await step(redo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: RENDERED_MASK })
    expectBystanderUntouched(adapter)
  })

  // The case above, driven by the editor's own undo: a delete during the
  // render is two steps, and the render that lands after it changes nothing.
  it('undoes a stroke deleted during its render in two steps, and redoes both', async () => {
    const { adapter, pending, release } = cloudStroke()
    await vi.waitFor(() => expect(adapter.applyTool).toHaveBeenCalledTimes(1))
    expect(await deleteRow(/** @type {any} */ (seedOnPage()))).toBe(true)
    release()
    expect(await pending).toBe(false)
    await settled(editor.history)
    expect(labels()).toEqual(['canvas.command.drawMask', 'masks.command.deleteMask'])
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK })

    await step(undo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: SEED_MASK })
    await step(undo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK })
    expect(canUndo(editor.history)).toBe(false)
    await step(redo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: SEED_MASK })
    await step(redo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK })
    expectBystanderUntouched(adapter)
  })

  // A render that starts and then fails, by answer or by throwing inside the
  // job, releases the hold: the seed is one local creation, and a later edit
  // of it is one step of its own rather than a second creation.
  for (const [how, answer] of [
    ['answers failed', async () => ({ status: 'failed', errorCode: 'provider_error' })],
    ['throws', async () => { throw new Error('connection reset') }],
  ]) {
    it(`keeps a stroke whose render ${how} after it starts as one creation`, async () => {
      const { adapter, pending, release } = cloudStroke({ answer })
      await vi.waitFor(() => expect(adapter.applyTool).toHaveBeenCalledTimes(1))
      release()
      expect(await pending).toBe(true)
      await settled(editor.history)
      expect(labels()).toEqual(['canvas.command.drawMask'])
      expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: SEED_MASK })

      expect(await deleteRow(/** @type {any} */ (seedOnPage()))).toBe(true)
      await settled(editor.history)
      expect(labels()).toEqual(['canvas.command.drawMask', 'masks.command.deleteMask'])
      await step(undo)
      expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: SEED_MASK })
      await step(undo)
      expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK })
      await step(redo)
      expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: SEED_MASK })
      expectBystanderUntouched(adapter)
    })
  }

  it('adds nothing when a render fails after its seed was deleted', async () => {
    const { adapter, pending, release } = cloudStroke({
      answer: async () => ({ status: 'failed', errorCode: 'provider_error' }),
    })
    await vi.waitFor(() => expect(adapter.applyTool).toHaveBeenCalledTimes(1))
    expect(await deleteRow(/** @type {any} */ (seedOnPage()))).toBe(true)
    release()
    expect(await pending).toBe(false)
    await settled(editor.history)
    expect(labels()).toEqual(['canvas.command.drawMask', 'masks.command.deleteMask'])
    await step(undo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: SEED_MASK })
    await step(undo)
    expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK })
    expectBystanderUntouched(adapter)
  })

  // The render outlives a page turn far enough to take the stroke's page out
  // of the window. Landed or failed, the stroke is one creation on its
  // chapter's history, and the page in view is left alone.
  for (const [how, answer, mask] of [
    ['lands', async () => ({ status: 'applied', region: rendered(), pageStatus: 'cleaned' }), RENDERED_MASK],
    ['fails', async () => ({ status: 'failed', errorCode: 'provider_error' }), SEED_MASK],
  ]) {
    it(`records a stroke whose render ${how} after its page left the window as one creation`, async () => {
      const { adapter, journal, pending, release } = cloudStroke({ answer, pages: 5 })
      await vi.waitFor(() => expect(adapter.applyTool).toHaveBeenCalledTimes(1))
      goToPage(3)
      const page = editor.chapter.pages[0]
      expect(page.resident).toBe(false)

      release()
      expect(await pending).toBe(true)
      await settled(editor.history)
      expect(labels()).toEqual(['canvas.command.drawMask'])
      expect(journal.entries[0].before.region).toBe(null)
      expect(journal.entries[0].after.region.mask.id).toBe(mask)
      expect(page.regions).toEqual([])
      expect(editor.pageIndex).toBe(3)
      expect(editor.selectionId).toBe(null)
      expect(editor.chapter.pages[3].regions).toEqual([])
    })
  }

  // Something throws before the render could start. The seed is stored all the
  // same, so it goes on the history, and the error is reported, not swallowed.
  it('keeps the seed and reports the error when the render cannot start', async () => {
    const log = vi.spyOn(console, 'error').mockImplementation(() => {})
    try {
      const { adapter, pending } = cloudStroke({ consent: async () => { throw new Error('dialog gone') } })
      expect(await pending).toBe(false)
      expect(adapter.applyTool).not.toHaveBeenCalled()
      expect(app.notices.at(-1)?.key).toBe('notice.mask.rerunFailed')
      await settled(editor.history)
      expect(labels()).toEqual(['canvas.command.drawMask'])
      expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK, [SEED_ID]: SEED_MASK })

      expect(await deleteRow(/** @type {any} */ (seedOnPage()))).toBe(true)
      await settled(editor.history)
      expect(labels()).toEqual(['canvas.command.drawMask', 'masks.command.deleteMask'])
      await step(undo)
      await step(undo)
      expect(masks()).toEqual({ [BYSTANDER_ID]: BYSTANDER_MASK })
      expectBystanderUntouched(adapter)
    } finally { log.mockRestore() }
  })

  it('persists gesture undo entries in order across a chapter switch', async () => {
    editor.chapter = chapterWith([])
    let releaseHistory
    const historyGate = new Promise((resolve) => { releaseHistory = resolve })
    let releaseSecond
    const secondGate = new Promise((resolve) => { releaseSecond = resolve })
    let creates = 0
    const adapter = backend({
      historyPush: vi.fn(async () => {
        if (adapter.historyPush.mock.calls.length === 1) await historyGate
        return { cursor: adapter.historyPush.mock.calls.length, entries: [] }
      }),
      createRegion: vi.fn(async ({ bbox }) => {
        const id = `c1-p001-h${++creates}`
        if (creates === 2) await secondGate
        return { region: { id, pageId: 'c1-p001', bbox }, pageStatus: 'cleaned' }
      }),
    })
    setBackend(/** @type {any} */ (adapter))
    stroke({ x: 20, y: 30, w: 12, h: 4 })
    expect(await commitDraft()).toBe(true)
    await vi.waitFor(() => expect(adapter.historyPush).toHaveBeenCalledTimes(1))
    stroke({ x: 22, y: 30, w: 12, h: 4 })
    const second = commitDraft()
    await vi.waitFor(() => expect(adapter.createRegion).toHaveBeenCalledTimes(2))
    editor.chapter = { ...chapterWith([]), id: 'c2' }
    releaseSecond()
    await Promise.resolve()
    expect(adapter.historyPush).toHaveBeenCalledTimes(1)
    releaseHistory()
    expect(await second).toBe(true)
    expect(adapter.historyPush.mock.calls.map(([{ entry }]) => entry.regionId))
      .toEqual(['c1-p001-h1', 'c1-p001-h2'])
  })

  it('records a cloud stroke as one creation action after attachment', async () => {
    editor.chapter = chapterWith([])
    setToolParam('aiMaskBrush', 'engine', 'cloud')
    const adapter = backend({ applyTool: vi.fn(async () => ({ status: 'applied',
      region: { id: 'c1-p001-h1', pageId: 'c1-p001', source: 'hand', outcome: 'cleaned',
        mask: { id: 'c1-p001-h1-m1', engine: 'flux' } }, pageStatus: 'cleaned' })) })
    setBackend(/** @type {any} */ (adapter))
    vi.mocked(requestCloudConsent).mockResolvedValue({ params: {}, attemptId: 'attempt-1' })
    vi.mocked(runCloudJob).mockImplementation(async (_grant, _where, call) => call({}))

    stroke({ x: 20, y: 30, w: 12, h: 4 })
    expect(await commitDraft()).toBe(true)
    expect(adapter.createRegion.mock.calls[0][0].params.engine).toBeUndefined()
    expect(adapter.applyTool).toHaveBeenCalledTimes(1)
    expect(adapter.historyPush).toHaveBeenCalledTimes(1)
    const { entry } = adapter.historyPush.mock.calls[0][0]
    expect(entry.before.region).toBe(null)
    expect(entry.after.region.mask.engine).toBe('flux')
  })

  it('keeps a cancelled cloud stroke as one local creation action', async () => {
    editor.chapter = chapterWith([])
    setToolParam('aiMaskBrush', 'engine', 'cloud')
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))
    vi.mocked(requestCloudConsent).mockResolvedValue(null)

    stroke({ x: 20, y: 30, w: 12, h: 4 })
    expect(await commitDraft()).toBe(true)
    expect(adapter.applyTool).not.toHaveBeenCalled()
    expect(adapter.historyPush).toHaveBeenCalledTimes(1)
    expect(adapter.historyPush.mock.calls[0][0].entry.before.region).toBe(null)
  })

  it('records a cloud result on its original chapter after a page switch', async () => {
    editor.chapter = chapterWith([])
    setToolParam('aiMaskBrush', 'engine', 'cloud')
    let release
    const gate = new Promise((resolve) => { release = resolve })
    const adapter = backend({ applyTool: vi.fn(async () => {
      await gate
      return { status: 'applied', region: { id: 'c1-p001-h1', pageId: 'c1-p001',
        mask: { id: 'c1-p001-h1-m1', engine: 'flux' } }, pageStatus: 'cleaned' }
    }) })
    setBackend(/** @type {any} */ (adapter))
    vi.mocked(requestCloudConsent).mockResolvedValue({ params: {}, attemptId: 'attempt-2' })
    vi.mocked(runCloudJob).mockImplementation(async (_grant, _where, call) => call({}))

    stroke({ x: 20, y: 30, w: 12, h: 4 })
    const pending = commitDraft()
    await vi.waitFor(() => expect(adapter.applyTool).toHaveBeenCalledTimes(1))
    editor.chapter = { ...chapterWith([]), id: 'c2' }
    release()
    expect(await pending).toBe(true)
    expect(editor.chapter.pages[0].regions).toEqual([])
    expect(adapter.historyPush).toHaveBeenCalledTimes(1)
    expect(adapter.historyPush.mock.calls[0][0].chapterId).toBe('c1')
    expect(adapter.historyPush.mock.calls[0][0].entry.after.region.mask.engine).toBe('flux')
  })

  it('reports an insufficient native input window without creating history', async () => {
    editor.chapter = chapterWith([])
    const adapter = backend({ createRegion: vi.fn().mockRejectedValue(
      new Error('model read footprint exceeds composited window')) })
    setBackend(/** @type {any} */ (adapter))
    const log = vi.spyOn(console, 'error').mockImplementation(() => {})
    try {
      stroke({ x: 20, y: 30, w: 12, h: 4 })
      expect(await commitDraft()).toBe(false)
      expect(adapter.historyPush).not.toHaveBeenCalled()
      expect(app.notices.at(-1)?.key).toBe('notice.mask.rerunFailed')
    } finally { log.mockRestore() }
  })
})

/**
 * A stroke with a real path: an L, whose bounding box is a square the hand
 * never painted.
 *
 * @param {string} tool
 * @param {Array<{x: number, y: number}>} points
 */
function painted(tool, points) {
  const xs = points.map((point) => point.x)
  const ys = points.map((point) => point.y)
  beginDraft({
    tool,
    kind: 'stroke',
    pageId: 'c1-p001',
    points,
    bbox: {
      x: Math.min(...xs),
      y: Math.min(...ys),
      w: Math.max(...xs) - Math.min(...xs),
      h: Math.max(...ys) - Math.min(...ys),
    },
    mode: 'add',
    keyboard: false,
    moved: true,
  })
}

/** The L: down the left, then along the bottom. Its box is a square. */
const L = [
  { x: 20, y: 20 },
  { x: 20, y: 60 },
  { x: 60, y: 60 },
]

/**
 * **The stroke's own shape crosses the seam.** Every assertion here is about the
 * difference between the path and the rectangle around it - an L and its
 * bounding square have identical bounds, so a bbox assertion cannot tell them
 * apart and this is the only place the defect was ever visible from.
 */
describe('a painted stroke', () => {
  beforeEach(() => {
    app.notices.length = 0
    editor.history = createHistory()
    editor.pageIndex = 0
    editor.chapter = chapterWith([])
    resetDraftState()
  })

  afterEach(() => {
    setBackend(null)
    editor.chapter = null
    editor.tool = 'autoClean'
    resetDraftState()
  })

  it('sends the path and the brush radius, not only the box around them', async () => {
    editor.tool = 'brush'
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))
    setToolParam('brush', 'size', 40)

    painted('brush', L)
    expect(await commitDraft()).toBe(true)

    const { params, bbox } = adapter.createRegion.mock.calls[0][0]
    // The box is still sent - a region is a bbox - and it is still the square.
    expect(bbox).toEqual({ x: 20, y: 20, w: 40, h: 40 })
    // And the shape that is not a square goes with it.
    expect(params.stroke.points).toEqual(L)
    expect(params.stroke.radius).toBe(20)
    // The corner of the bounding box the hand never went near is not on the
    // path - which is the whole of what "not a rectangle" means here.
    expect(params.stroke.points).not.toContainEqual({ x: 60, y: 20 })
  })

  it('carries the AI mask brush’s own shape, over a region or not', async () => {
    editor.tool = 'aiMaskBrush'
    editor.chapter = chapterWith([
      {
        id: 'r1',
        pageId: 'c1-p001',
        bbox: { x: 22, y: 22, w: 30, h: 30 },
        detected: true,
        outcome: 'cleaned',
      },
    ])
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))

    painted('aiMaskBrush', L)
    expect(await commitDraft()).toBe(true)

    const { params } = adapter.createRegion.mock.calls[0][0]
    expect(params.stroke.points).toEqual(L)
    // The default width the tool ships, as a radius.
    expect(params.stroke.radius).toBe(18)
    // Not the square around the L: the shape is what gets cleaned, and it is
    // the whole reason the backend no longer asks a detector what to clean.
    expect(params.stroke.points).not.toContainEqual({ x: 60, y: 20 })
  })

  it('sends no stroke for a gesture that is an area rather than a path', async () => {
    editor.tool = 'shapes'
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))

    shape('rect', [{ x: 20, y: 20 }], { x: 20, y: 20, w: 40, h: 40 })
    expect(await commitDraft()).toBe(true)

    // A shape has no radius to sweep. It sends `painted` instead, which the
    // block below is about, and the two are never both present.
    const { params } = adapter.createRegion.mock.calls[0][0]
    expect(params.stroke).toBeUndefined()
    expect(params.painted).toBeDefined()
  })
})

/**
 * A Shapes gesture, ready to commit.
 *
 * @param {'rect'|'ellipse'|'lasso'|'polygon'} kind
 * @param {Array<{x: number, y: number}>} points
 * @param {{x: number, y: number, w: number, h: number}} bbox
 */
function shape(kind, points, bbox) {
  beginDraft({
    tool: 'shapes',
    kind,
    pageId: 'c1-p001',
    points,
    bbox,
    mode: 'add',
    keyboard: false,
    moved: true,
  })
}

/** An L drawn freehand: its bounding box is a square the hand never filled. */
const LASSO = [
  { x: 20, y: 20 },
  { x: 30, y: 20 },
  { x: 30, y: 50 },
  { x: 60, y: 50 },
  { x: 60, y: 60 },
  { x: 20, y: 60 },
]

/**
 * **What a drawn shape sends.** Two things had to change together: the shape
 * itself has to cross the seam - an ellipse used to be committed as its
 * bounding rectangle and a lasso as the box its curve fitted inside, in
 * the last tool it was open in - and the
 * tool's new `mode` row has to reach the backend as the one field that decides
 * what happens to it: `params.engine` for a clean, `params.paint` for a colour.
 */
describe('a drawn shape', () => {
  beforeEach(() => {
    app.notices.length = 0
    editor.history = createHistory()
    editor.tool = 'shapes'
    editor.pageIndex = 0
    editor.chapter = chapterWith([])
    editor.toolParams.shapes = {
      shape: 'rect',
      mode: 'fill',
      color: '#000000',
      opacity: 100,
      feather: 2,
    }
    resetDraftState()
  })

  afterEach(() => {
    setBackend(null)
    editor.chapter = null
    editor.tool = 'autoClean'
    resetDraftState()
  })

  it('sends the rectangle as its four corners, with the feather', async () => {
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))

    shape('rect', [{ x: 20, y: 20 }], { x: 20, y: 20, w: 40, h: 40 })
    expect(await commitDraft()).toBe(true)

    const { params, bbox } = adapter.createRegion.mock.calls[0][0]
    expect(bbox).toEqual({ x: 20, y: 20, w: 40, h: 40 })
    expect(params.painted).toEqual({
      kind: 'rect',
      points: [
        { x: 20, y: 20 },
        { x: 60, y: 20 },
        { x: 60, y: 60 },
        { x: 20, y: 60 },
      ],
      feather: 2,
    })
  })

  it('sends an ellipse as an ellipse, not as the rectangle it fits in', async () => {
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))
    setToolParam('shapes', 'shape', 'ellipse')

    shape('ellipse', [{ x: 10, y: 10 }], { x: 10, y: 10, w: 20, h: 30 })
    expect(await commitDraft()).toBe(true)

    const { params } = adapter.createRegion.mock.calls[0][0]
    expect(params.painted.kind).toBe('ellipse')
    // The corners of the box it is inscribed in, which is what the drag said.
    expect(params.painted.points).toHaveLength(4)
  })

  it('sends a lasso and a polygon as their own vertices', async () => {
    for (const kind of /** @type {const} */ (['lasso', 'polygon'])) {
      const adapter = backend()
      setBackend(/** @type {any} */ (adapter))

      shape(kind, LASSO, { x: 20, y: 20, w: 40, h: 40 })
      expect(await commitDraft()).toBe(true)

      const { params } = adapter.createRegion.mock.calls[0][0]
      // One kind for both, because a lasso *is* a polygon drawn freehand.
      expect(params.painted.kind).toBe('polygon')
      expect(params.painted.points).toEqual(LASSO)
      // The corner of the bounding box the hand never went near is not a
      // vertex - which is the whole of what "not a rectangle" means here.
      expect(params.painted.points).not.toContainEqual({ x: 60, y: 20 })
    }
  })

  it('refuses to call two clicks an area', async () => {
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))

    shape('polygon', LASSO.slice(0, 2), { x: 20, y: 20, w: 10, h: 0.5 })
    expect(await commitDraft()).toBe(true)

    // No shape crossed, so the backend falls back to the box it also sent
    // rather than being handed an outline that encloses nothing.
    expect(adapter.createRegion.mock.calls[0][0].params.painted).toBeUndefined()
  })

  it('names the rung the mode row picked, for every engine on it', async () => {
    for (const engine of ['fill', 'lama', 'flux']) {
      const adapter = backend()
      setBackend(/** @type {any} */ (adapter))
      setToolParam('shapes', 'mode', engine)

      shape('rect', [{ x: 20, y: 20 }], { x: 20, y: 20, w: 40, h: 40 })
      expect(await commitDraft()).toBe(true)

      const { params } = adapter.createRegion.mock.calls[0][0]
      // `region.rs#named_rung` reads this word and runs that rung.
      expect(params.engine).toBe(engine)
      // A clean is not a paint: the backend's paint branch must not be taken.
      expect(params.paint).toBeUndefined()
    }
  })

  it('sends a solid fill as paint, with the shape as the coverage', async () => {
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))
    setToolParam('shapes', 'mode', 'solid')
    setToolParam('shapes', 'color', '#ff8800')
    setToolParam('shapes', 'opacity', 60)

    shape('ellipse', [{ x: 10, y: 10 }], { x: 10, y: 10, w: 20, h: 30 })
    expect(await commitDraft()).toBe(true)

    const { params } = adapter.createRegion.mock.calls[0][0]
    expect(params.paint).toMatchObject({
      color: '#ff8800',
      opacity: 60,
      // A shape has no soft rim and no build-up: its edge is the feather,
      // which is geometry and travels with the shape.
      hardness: 100,
      flow: 100,
    })
    expect(params.paint.shape).toEqual(params.painted)
    // And it names no engine, because a colour somebody chose is not a rung.
    expect(params.engine).toBeUndefined()
  })

  it('is one undoable edit, whichever mode it was in', async () => {
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))
    setToolParam('shapes', 'mode', 'solid')

    shape('rect', [{ x: 20, y: 20 }], { x: 20, y: 20, w: 40, h: 40 })
    await commitDraft()

    expect(adapter.historyPush).toHaveBeenCalledTimes(1)
    const { entry } = adapter.historyPush.mock.calls[0][0]
    expect(entry.op).toBe('region-state')
    expect(entry.before.region).toBe(null)
    expect(entry.after.region).toMatchObject({ id: 'c1-p001-h1' })
  })
})

describe('content-aware fill on region click', () => {
  beforeEach(() => {
    app.notices.length = 0
    editor.history = createHistory()
    editor.tool = 'contentAwareFill'
    editor.pageIndex = 0
    resetDraftState()
  })

  afterEach(() => {
    setBackend(null)
    editor.chapter = null
    editor.tool = 'autoClean'
    resetDraftState()
  })

  it('applies content-aware fill to an existing region on click', async () => {
    editor.chapter = chapterWith([
      {
        id: 'r1',
        pageId: 'c1-p001',
        bbox: { x: 10, y: 10, w: 20, h: 20 },
        detected: true,
        outcome: 'unclean',
      },
    ])
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))

    const applied = await applyActiveToolToRegion('r1')
    expect(applied).toBe(true)
    expect(adapter.applyTool).toHaveBeenCalledTimes(1)
    const spec = adapter.applyTool.mock.calls[0][0]
    expect(spec.tool).toBe('contentAwareFill')
    expect(spec.regionId).toBe('r1')
    expect(editor.selectionId).toBe('r1')
  })
})

// Text cleanup is not a per-region tool. Sending its parameters for a region
// named the stored scope and no mode, which the adapter read as a Detect &
// clean over that whole scope; a run starts from the panel instead, and one
// detected region is cleaned by `cleanDetected`.
describe('Text cleanup on an existing region', () => {
  /** @type {Record<string, unknown>|undefined} */
  let params
  afterEach(() => {
    if (params) editor.toolParams.autoClean = params
    setBackend(null)
    editor.chapter = null
  })

  it('applies nothing and starts no run, whatever the panel holds', async () => {
    editor.tool = 'autoClean'
    params = editor.toolParams.autoClean
    editor.toolParams.autoClean = { ...params, scope: 'project', step: 'detect' }
    editor.chapter = chapterWith([{ id: 'r1', pageId: 'c1-p001', bbox: { x: 10, y: 10, w: 20, h: 20 },
      detected: true, outcome: 'unclean' }])
    const adapter = backend()
    const runClean = vi.fn()
    setBackend(/** @type {any} */ ({ ...adapter, runClean }))

    expect(await applyActiveToolToRegion('r1')).toBe(false)
    expect(adapter.applyTool).not.toHaveBeenCalled()
    expect(runClean).not.toHaveBeenCalled()
    expect(editor.run.active).toBe(false)
  })
})

describe('paint integration payload contract', () => {
  beforeEach(() => {
    app.notices.length = 0
    editor.history = createHistory()
    editor.chapter = chapterWith([])
    editor.pageIndex = 0
    resetDraftState()
  })

  afterEach(() => {
    setBackend(null)
    editor.chapter = null
    editor.tool = 'autoClean'
    resetDraftState()
  })

  it('builds params.paint for brush in paint mode', async () => {
    editor.tool = 'brush'
    editor.toolParams.brush = {
      size: 30,
      hardness: 80,
      spacing: 15,
      mode: 'paint',
      color: '#ff0000',
      opacity: 90,
      flow: 85,
    }
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))

    const strokePoints = [
      { x: 10, y: 10, p: 0.8 },
      { x: 20, y: 30, p: 0.6 },
    ]
    painted('brush', strokePoints, 30)
    expect(await commitDraft()).toBe(true)

    const { params, tool } = adapter.createRegion.mock.calls[0][0]
    expect(tool).toBe('brush')
    expect(params.mode).toBe('paint')
    expect(params.color).toBe('#ff0000')
    expect(params.opacity).toBe(90)
    expect(params.flow).toBe(85)
    expect(params.stroke).toEqual({
      points: [{ x: 10, y: 10 }, { x: 20, y: 30 }],
      radius: 15,
    })
    expect(params.paint).toMatchObject({
      points: [
        { x: 10, y: 10, p: 0.8 },
        { x: 20, y: 30, p: 0.6 },
      ],
      color: '#ff0000',
      opacity: 90,
      flow: 85,
      hardness: 80,
      spacing: 15,
      pressureSize: true,
      pressureOpacity: false,
    })
    expect(typeof params.paint.seed).toBe('number')
    expect(params.paint.seed).toBeGreaterThanOrEqual(0)
  })

  it('does not build params.paint for brush in add mode', async () => {
    editor.tool = 'brush'
    editor.toolParams.brush = {
      size: 28,
      hardness: 70,
      spacing: 12,
      mode: 'add',
      color: '#000000',
      opacity: 100,
      flow: 100,
    }
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))

    painted('brush', [{ x: 10, y: 10 }, { x: 20, y: 20 }], 28)
    expect(await commitDraft()).toBe(true)

    const { params } = adapter.createRegion.mock.calls[0][0]
    expect(params.mode).toBe('add')
    expect(params.paint).toBeUndefined()
  })

  it('builds params.paint with seed and points for cloneHeal', async () => {
    editor.tool = 'cloneHeal'
    editor.toolParams.cloneHeal = {
      size: 32,
      hardness: 60,
      alignment: 'aligned',
      mode: 'heal',
      opacity: 75,
      flow: 80,
    }
    const adapter = backend()
    setBackend(/** @type {any} */ (adapter))

    // Set clone source
    draft.cloneSource = { pageId: 'c1-p001', x: 50, y: 50 }

    const strokePoints = [
      { x: 10, y: 10, p: 0.7 },
      { x: 15, y: 15, p: 0.9 },
    ]
    painted('cloneHeal', strokePoints, 32)
    expect(await commitDraft()).toBe(true)

    const { params, tool } = adapter.createRegion.mock.calls[0][0]
    expect(tool).toBe('cloneHeal')
    expect(params.opacity).toBe(75)
    expect(params.flow).toBe(80)
    expect(params.mode).toBe('heal')
    expect(params.alignment).toBe('aligned')
    expect(params.stroke).toBeDefined()
    expect(params.cloneSource).toEqual({ x: 50, y: 50 })
    expect(params.paint).toMatchObject({
      points: [
        { x: 10, y: 10, p: 0.7 },
        { x: 15, y: 15, p: 0.9 },
      ],
    })
    expect(typeof params.paint.seed).toBe('number')
  })
})

/**
 * **The selection tool edits the detected masks, and records nothing.** Its
 * gesture is a brush stroke or a lasso or a rectangle, the same payloads
 * `createRegion` carries, sent to `editDetectionMask` with the mode the row
 * picked. It makes no layer and no undo entry - a detection has no pixels to
 * put back - and reads the page again afterwards, because only the native side
 * knows which detections grew, appeared or went.
 */
describe('the selection tool', () => {
  const DETECTED_ID = 'c1-p001-d1'
  const detected = () => ({
    id: DETECTED_ID,
    pageId: 'c1-p001',
    bbox: { x: 10, y: 10, w: 10, h: 10 },
    source: 'auto',
    outcome: 'detected',
    mask: { id: `${DETECTED_ID}-m1`, fillMode: 'match-surround', provenance: { engine: 'fill', mask_sha256: 'aa' } },
  })

  /**
   * @param {string} kind
   * @param {Array<{x: number, y: number}>} points
   * @param {{x: number, y: number, w: number, h: number}} bbox
   * @param {'add'|'erase'} [mode]
   */
  function selection(kind, points, bbox, mode = 'add') {
    beginDraft({ tool: 'maskSelect', kind, pageId: 'c1-p001', points, bbox, mode, keyboard: false, moved: true })
  }

  /** An adapter whose mask edit answers `answer`, and whose page reload answers the page as `regions`. */
  function maskBackend(answer, regions = [detected()], overrides = {}) {
    return backend({
      editDetectionMask: vi.fn(async () => answer),
      loadPages: vi.fn(async () => [{ ...chapterWith(regions).pages[0], status: answer?.pageStatus ?? 'detected' }]),
      ...overrides,
    })
  }

  beforeEach(() => {
    vi.clearAllMocks()
    app.notices.length = 0
    editor.history = createHistory()
    editor.tool = 'maskSelect'
    editor.pageIndex = 0
    editor.selectionId = null
    editor.hoverId = null
    editor.chapter = chapterWith([detected()])
    setToolParam('maskSelect', 'size', 32)
    resetDraftState()
  })

  afterEach(() => {
    setBackend(null)
    editor.chapter = null
    editor.tool = 'autoClean'
    resetDraftState()
  })

  it('adds a brush stroke as its path and radius, and records no history', async () => {
    const adapter = maskBackend({ pageStatus: 'detected', changed: [DETECTED_ID], created: [], removed: [] })
    setBackend(/** @type {any} */ (adapter))

    selection('stroke', [{ x: 12, y: 12 }, { x: 30, y: 12 }], { x: 11, y: 11, w: 20, h: 2 })
    expect(await commitDraft()).toBe(true)

    expect(adapter.editDetectionMask).toHaveBeenCalledTimes(1)
    expect(adapter.editDetectionMask.mock.calls[0][0]).toEqual({
      chapterId: 'c1',
      pageIndex: 0,
      sourceIndex: 0,
      sourceSha: 'fixture-source',
      mode: 'add',
      stroke: { points: [{ x: 12, y: 12 }, { x: 30, y: 12 }], radius: 16 },
    })
    // Not a layer, and not an undoable edit.
    expect(adapter.createRegion).not.toHaveBeenCalled()
    expect(adapter.historyPush).not.toHaveBeenCalled()
    expect(editor.history.entries).toHaveLength(0)
    // The page is read again, which moves the edited mask's URL.
    expect(adapter.loadPages).toHaveBeenCalledWith({ chapterId: 'c1', indices: [0] })
  })

  it('removes with a lasso as its own vertices and a rectangle as its corners', async () => {
    const adapter = maskBackend({ pageStatus: 'detected', changed: [DETECTED_ID], created: [], removed: [] })
    setBackend(/** @type {any} */ (adapter))

    selection('lasso', LASSO, { x: 20, y: 20, w: 40, h: 40 }, 'erase')
    expect(await commitDraft()).toBe(true)
    selection('rect', [{ x: 5, y: 5 }], { x: 5, y: 5, w: 10, h: 20 }, 'erase')
    expect(await commitDraft()).toBe(true)

    const [lasso, rect] = adapter.editDetectionMask.mock.calls.map(([spec]) => spec)
    expect(lasso.mode).toBe('remove')
    expect(lasso.stroke).toBeUndefined()
    expect(lasso.painted).toEqual({ kind: 'polygon', points: LASSO, feather: 0 })
    expect(rect.mode).toBe('remove')
    expect(rect.painted).toEqual({
      kind: 'rect',
      points: [{ x: 5, y: 5 }, { x: 15, y: 5 }, { x: 15, y: 25 }, { x: 5, y: 25 }],
      feather: 0,
    })
  })

  it('lets go of a selection the edit deleted', async () => {
    const adapter = maskBackend({ pageStatus: 'unclean', changed: [], created: [], removed: [DETECTED_ID] }, [])
    setBackend(/** @type {any} */ (adapter))
    editor.selectionId = DETECTED_ID
    editor.hoverId = DETECTED_ID

    selection('stroke', [{ x: 15, y: 15 }], { x: 14, y: 14, w: 2, h: 2 }, 'erase')
    expect(await commitDraft()).toBe(true)

    expect(editor.selectionId).toBeNull()
    expect(editor.hoverId).toBeNull()
    expect(editor.chapter.pages[0].regions).toEqual([])
    expect(editor.chapter.pages[0].status).toBe('unclean')
  })

  it('reads nothing again for an edit that changed nothing', async () => {
    const adapter = maskBackend({ pageStatus: 'detected', changed: [], created: [], removed: [] })
    setBackend(/** @type {any} */ (adapter))

    selection('stroke', [{ x: 80, y: 80 }], { x: 79, y: 79, w: 2, h: 2 }, 'erase')
    expect(await commitDraft()).toBe(false)
    expect(adapter.loadPages).not.toHaveBeenCalled()
  })

  it('leaves a chapter opened meanwhile alone', async () => {
    /** @type {(value: any) => void} */
    let answer = () => {}
    const adapter = maskBackend(null, [detected()], {
      editDetectionMask: vi.fn(() => new Promise((resolve) => { answer = resolve })),
    })
    setBackend(/** @type {any} */ (adapter))

    selection('stroke', [{ x: 15, y: 15 }], { x: 14, y: 14, w: 2, h: 2 }, 'erase')
    const pending = commitDraft()
    await vi.waitFor(() => expect(adapter.editDetectionMask).toHaveBeenCalledTimes(1))
    const other = { ...chapterWith([{ ...detected() }]), id: 'c2' }
    editor.chapter = other
    editor.selectionId = DETECTED_ID
    answer({ pageStatus: 'unclean', changed: [], created: [], removed: [DETECTED_ID] })
    await pending

    expect(adapter.loadPages).not.toHaveBeenCalled()
    expect(editor.chapter).toBe(other)
    expect(editor.selectionId).toBe(DETECTED_ID)
  })

  it('says a refused edit in one sentence and changes nothing', async () => {
    const adapter = maskBackend(null, [detected()], {
      editDetectionMask: vi.fn(async () => { throw new Error('mask_edit_mode_invalid') }),
    })
    setBackend(/** @type {any} */ (adapter))
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    try {
      selection('stroke', [{ x: 15, y: 15 }], { x: 14, y: 14, w: 2, h: 2 })
      expect(await commitDraft()).toBe(false)
    } finally {
      error.mockRestore()
    }
    expect(app.notices.map((notice) => notice.key)).toContain('notice.mask.selectionFailed')
    expect(adapter.loadPages).not.toHaveBeenCalled()
    expect(editor.chapter.pages[0].regions).toEqual([detected()])
  })
})
