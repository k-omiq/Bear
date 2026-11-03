/**
 * The Gradient tool is live (`retouch.svelte.js`): once a held selection has
 * its line, every change reaches the page by itself a moment after the last
 * one, and Apply is Done. A browser test because the change is noticed by an
 * effect, and the node project's server build of Svelte runs none.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '@testing-library/svelte'

import { cancelInteraction, editor } from '../state/editor.svelte.js'
import { app } from '../state/app.svelte.js'
import { setBackend } from '../api/backend.js'
import { layerUrl } from '../api/tile.js'
import { createHistory } from '../model/history.js'
import { beginDraft, draft, resetDraftState } from './draft.svelte.js'
import { commitDraft } from './drawing.svelte.js'
import { layerSettled } from './patchlayers.svelte.js'
import PatchLayers from './PatchLayers.svelte'
import SelectionOverlay from './SelectionOverlay.svelte'
import { applySelection, previewStands, reopenGradient, retouch, setLine } from './retouch.svelte.js'
import { goToPage, setTool, undo } from '../state/editor.svelte.js'

function chapter() {
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
        regionCount: 0,
        regions: [],
      },
    ],
  }
}

function backend(overrides = {}) {
  return {
    historyPush: vi.fn(async ({ entry }) => ({ cursor: 1, entries: [{ seq: 1, label: entry.label }] })),
    loadPages: vi.fn(async () => []),
    proposePatchSource: vi.fn(async () => ({ offset: { x: 30, y: 0 } })),
    fitGradient: vi.fn(async () => ({
      kind: 'linear',
      start: [25, 10],
      end: [25, 40],
      colors: [{ at: 0, color: [40, 30, 30] }, { at: 1, color: [225, 225, 225] }],
      opacities: [{ at: 0, opacity: 1 }, { at: 1, opacity: 1 }],
      reverse: false,
      dither: true,
    })),
    createRegion: vi.fn(async ({ bbox, tool }) => ({
      region: { id: 'c1-p001-h1', pageId: 'c1-p001', bbox, source: 'hand', outcome: 'cleaned', tool },
      pageStatus: 'cleaned',
    })),
    applyTool: vi.fn(async () => ({ status: 'applied', region: { id: 'g1', pageId: 'c1-p001', outcome: 'cleaned' }, pageStatus: 'cleaned' })),
    ...overrides,
  }
}

/** A lasso drag with `tool`, already moved, ready to commit. */
function lasso(tool) {
  editor.tool = tool
  beginDraft({
    tool,
    kind: 'lasso',
    pageId: 'c1-p001',
    points: [{ x: 10, y: 10 }, { x: 40, y: 10 }, { x: 30, y: 40 }],
    bbox: { x: 10, y: 10, w: 30, h: 30 },
    mode: 'add',
    keyboard: false,
    moved: true,
  })
}

const saved = { gradient: { ...editor.toolParams.gradient } }

beforeEach(() => {
  editor.chapter = chapter()
  editor.project = { mode: 'single' }
  editor.pageIndex = 0
  editor.history = createHistory()
  app.notices = []
})

afterEach(() => {
  cleanup()
  delete globalThis.__TAURI_INTERNALS__
  layerSettled.clear()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  editor.areaRuns = []
  resetDraftState()
  editor.toolParams.gradient = { ...saved.gradient }
  editor.tool = 'autoClean'
})

const shown = (key) => app.notices.filter((notice) => notice.key === key).length

/** A promise and the function that settles it. */
function deferred() {
  let resolve = /** @type {(value: any) => void} */ (() => {})
  let reject = /** @type {(error: any) => void} */ (() => {})
  const promise = new Promise((yes, no) => {
    resolve = yes
    reject = no
  })
  return { promise, resolve, reject }
}

const applied = { status: 'applied', region: { id: 'c1-p001-h1', pageId: 'c1-p001', outcome: 'cleaned' }, pageStatus: 'cleaned' }

/** A placed gradient layer `g1`, with a layer picture of its own. */
function gradientLayer(layerKey = 'k1') {
  return {
    id: 'g1',
    pageId: 'c1-p001',
    bbox: { x: 10, y: 10, w: 30, h: 30 },
    outcome: 'cleaned',
    mask: {
      id: 'g1-m1',
      regionId: 'g1',
      layerKey,
      order: 0,
      layer: { opacity: 50 },
      provenance: {
        engine: 'paint',
        params_snapshot: {
          mode: 'gradient',
          shape: 'rect',
          gradient: { kind: 'linear', start: [15, 15], end: [35, 35], colors: [{ at: 0, color: [0, 0, 0] }], opacities: [], reverse: false, dither: false },
          feather: 0,
          layer: { opacity: 50 },
          painted: { kind: 'rect', points: [{ x: 10, y: 10 }, { x: 40, y: 40 }], feather: 0 },
        },
      },
    },
  }
}

describe('the live Gradient tool', () => {
  it('a reopened layer re-renders by itself when a value changes', async () => {
    const api = backend()
    setBackend(api)
    const region = {
      id: 'g1',
      pageId: 'c1-p001',
      bbox: { x: 10, y: 10, w: 30, h: 30 },
      outcome: 'cleaned',
      mask: {
        provenance: {
          engine: 'paint',
          params_snapshot: {
            mode: 'gradient',
            shape: 'ellipse',
            gradient: { kind: 'diamond', start: [20, 20], end: [30, 30], colors: [{ at: 0, color: [255, 0, 0] }], opacities: [], reverse: true, dither: false },
            asScreentone: true,
            feather: 4,
            layer: { opacity: 50 },
            painted: { kind: 'ellipse', points: [{ x: 10, y: 10 }, { x: 40, y: 40 }], feather: 4 },
          },
        },
      },
    }
    editor.chapter.pages[0].regions = [region]
    editor.chapter.pages[0].regionCount = 1
    // Edit gradient is on the Gradient tool's own bar.
    editor.tool = 'gradient'
    expect(reopenGradient(region)).toBe(true)
    expect(editor.toolParams.gradient).toMatchObject({ kind: 'diamond', reverse: true, dither: false, asScreentone: true, feather: 4, opacity: 50, shape: 'ellipse' })
    expect(draft.selection).toMatchObject({ tool: 'gradient', regionId: 'g1', start: { x: 20, y: 20 } })
    // What the layer already shows is not sent again.
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.applyTool).not.toHaveBeenCalled()
    // A change re-renders that layer by itself, with no Apply.
    editor.toolParams.gradient.opacity = 80
    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(1), { timeout: 2000 })
    expect(api.createRegion).not.toHaveBeenCalled()
    expect(api.applyTool.mock.calls[0][0].params.gradient).toMatchObject({ kind: 'diamond', start: [20, 20] })
    expect(api.applyTool.mock.calls[0][0].params.opacity).toBe(80)
    await applySelection()
    expect(api.applyTool).toHaveBeenCalledTimes(1)
    expect(draft.selection).toBeNull()
  })

  it('is live: the line makes the layer, a later change re-renders it, and Done sends what waits', async () => {
    const api = backend({
      applyTool: vi.fn(async () => ({ status: 'applied', region: { id: 'c1-p001-h1', pageId: 'c1-p001', outcome: 'cleaned' }, pageStatus: 'cleaned' })),
    })
    setBackend(api)
    lasso('gradient')
    await commitDraft()
    setLine({ x: 15, y: 15 }, { x: 35, y: 35 })
    await vi.waitFor(() => expect(api.createRegion).toHaveBeenCalledTimes(1), { timeout: 2000 })
    await vi.waitFor(() => expect(draft.selection?.regionId).toBe('c1-p001-h1'))
    expect(draft.selection?.liveKey).not.toBeNull()

    editor.toolParams.gradient.reverse = true
    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(1), { timeout: 2000 })
    expect(api.applyTool.mock.calls[0][0].regionId).toBe('c1-p001-h1')
    expect(api.applyTool.mock.calls[0][0].params.gradient.reverse).toBe(true)

    // Done does not wait for the pause: the change goes at once.
    editor.toolParams.gradient.opacity = 40
    await applySelection()
    expect(api.applyTool).toHaveBeenCalledTimes(2)
    expect(api.applyTool.mock.calls[1][0].params.opacity).toBe(40)
    expect(api.createRegion).toHaveBeenCalledTimes(1)
    expect(draft.selection).toBeNull()
  })

  /** A held gradient on the page whose first commit has landed. */
  async function heldLive(api) {
    lasso('gradient')
    await commitDraft()
    setLine({ x: 15, y: 15 }, { x: 35, y: 35 })
    await vi.waitFor(() => expect(api.createRegion).toHaveBeenCalledTimes(1), { timeout: 2000 })
    await vi.waitFor(() => expect(draft.selection?.liveKey).toBeTruthy())
  }

  it('keeps the selection on Done while a run holds the page, and sends it once the page is free', async () => {
    const api = backend({
      applyTool: vi.fn(async () => ({ status: 'applied', region: { id: 'c1-p001-h1', pageId: 'c1-p001', outcome: 'cleaned' }, pageStatus: 'cleaned' })),
    })
    setBackend(api)
    await heldLive(api)
    editor.areaRuns = [{ pageIndex: 0, runId: 'r1' }]
    editor.toolParams.gradient.opacity = 30
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.applyTool).not.toHaveBeenCalled()
    expect(await applySelection()).toBe(false)
    expect(draft.selection).not.toBeNull()
    editor.areaRuns = []
    await applySelection()
    expect(api.applyTool).toHaveBeenCalledTimes(1)
    expect(api.applyTool.mock.calls[0][0].params.opacity).toBe(30)
    expect(draft.selection).toBeNull()
  })

  it('drops a waiting change when the type turns Auto, and on Undo lets the selection go', async () => {
    const api = backend()
    setBackend(api)
    await heldLive(api)
    editor.toolParams.gradient.opacity = 30
    editor.toolParams.gradient.kind = 'auto'
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.applyTool).not.toHaveBeenCalled()

    editor.toolParams.gradient.kind = 'linear'
    editor.toolParams.gradient.opacity = 60
    undo()
    expect(draft.selection).toBeNull()
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.applyTool).not.toHaveBeenCalled()
  })

  it('sends nothing for the selection shape or brush size, which draw nothing new', async () => {
    const api = backend()
    setBackend(api)
    await heldLive(api)
    editor.toolParams.gradient.shape = 'ellipse'
    editor.toolParams.gradient.size = 90
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.applyTool).not.toHaveBeenCalled()
  })
  it('sends a change a run refused once the run ends, with one notice for the hold', async () => {
    const api = backend({ applyTool: vi.fn(async () => applied) })
    setBackend(api)
    await heldLive(api)
    editor.areaRuns = [{ pageIndex: 0, runId: 'r1' }]
    editor.toolParams.gradient.opacity = 30
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.applyTool).not.toHaveBeenCalled()
    expect(shown('notice.run.pageBusy')).toBe(1)
    // A second pause in the same hold says nothing more.
    app.notices = []
    editor.toolParams.gradient.opacity = 35
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(shown('notice.run.pageBusy')).toBe(0)
    // What waits is what the preview shows.
    expect(retouch.pending).toBe(true)
    expect(previewStands(draft.selection, editor.toolParams.gradient)).toBe(true)

    editor.areaRuns = []
    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(1), { timeout: 2000 })
    expect(api.applyTool.mock.calls[0][0].params.opacity).toBe(35)
    await vi.waitFor(() => expect(retouch.pending).toBe(false))
    expect(previewStands(draft.selection, editor.toolParams.gradient)).toBe(false)
    expect(draft.selection).not.toBeNull()
  })

  it('keeps the selection and the change on a backend error at Done, and says so', async () => {
    const api = backend({ applyTool: vi.fn(async () => { throw new Error('decline.reason.rungUnavailable') }) })
    setBackend(api)
    await heldLive(api)
    editor.toolParams.gradient.opacity = 45
    expect(await applySelection()).toBe(false)
    expect(api.applyTool).toHaveBeenCalled()
    expect(draft.selection).not.toBeNull()
    expect(retouch.pending).toBe(true)
    expect(shown('notice.mask.rerunFailed')).toBeGreaterThan(0)

    api.applyTool.mockImplementation(async () => applied)
    expect(await applySelection()).toBe(true)
    expect(api.applyTool.mock.calls.at(-1)[0].params.opacity).toBe(45)
    expect(draft.selection).toBeNull()
  })

  it('sends a change still waiting on Escape before the selection goes', async () => {
    const api = backend({ applyTool: vi.fn(async () => applied) })
    setBackend(api)
    await heldLive(api)
    editor.toolParams.gradient.opacity = 55
    expect(cancelInteraction()).toBe(true)
    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(1))
    expect(api.applyTool.mock.calls[0][0].params.opacity).toBe(55)
    await vi.waitFor(() => expect(draft.selection).toBeNull())
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.applyTool).toHaveBeenCalledTimes(1)
  })

  it('lets the selection go at once on Escape when nothing waits', async () => {
    const api = backend({ applyTool: vi.fn(async () => applied) })
    setBackend(api)
    await heldLive(api)
    expect(cancelInteraction()).toBe(true)
    expect(draft.selection).toBeNull()
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.applyTool).not.toHaveBeenCalled()
  })

  it('still sends a new selection\'s change made while the last one\'s commit was on its way', async () => {
    let made = 0
    const first = deferred()
    const api = backend({
      createRegion: vi.fn(async ({ bbox, tool }) => ({
        region: { id: `c1-p001-h${++made}`, pageId: 'c1-p001', bbox, source: 'hand', outcome: 'cleaned', tool },
        pageStatus: 'cleaned',
      })),
      applyTool: vi.fn(() => first.promise),
    })
    setBackend(api)
    await heldLive(api)
    const a = draft.selection
    editor.toolParams.gradient.opacity = 65
    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(1), { timeout: 2000 })

    // B is drawn and changed while A's commit is still on its way.
    lasso('gradient')
    await commitDraft()
    expect(draft.selection).not.toBe(a)
    setLine({ x: 12, y: 12 }, { x: 30, y: 30 })
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.createRegion).toHaveBeenCalledTimes(1)

    first.resolve(applied)
    await vi.waitFor(() => expect(api.createRegion).toHaveBeenCalledTimes(2), { timeout: 2000 })
    await vi.waitFor(() => expect(draft.selection?.regionId).toBe('c1-p001-h2'))
    expect(api.createRegion.mock.calls[1][0].params.gradient.start).toEqual([12, 12])
  })

  it('sends a change still waiting before a tool switch, as the gradient', async () => {
    const api = backend({ applyTool: vi.fn(async () => applied) })
    setBackend(api)
    await heldLive(api)
    editor.toolParams.gradient.opacity = 70
    setTool('brush')
    expect(editor.tool).toBe('brush')
    expect(draft.selection).toBeNull()
    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(1))
    expect(api.applyTool.mock.calls[0][0]).toMatchObject({ tool: 'gradient', regionId: 'c1-p001-h1', pageIndex: 0 })
    expect(api.applyTool.mock.calls[0][0].params.opacity).toBe(70)
    expect(api.historyPush).toHaveBeenCalled()
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.applyTool).toHaveBeenCalledTimes(1)
  })

  it('sends a change still waiting before a page turn, onto the page it was made on', async () => {
    const api = backend({ applyTool: vi.fn(async () => applied) })
    setBackend(api)
    editor.chapter.pages.push({ ...chapter().pages[0], id: 'c1-p002', index: 1, number: 2 })
    await heldLive(api)
    editor.toolParams.gradient.opacity = 25
    goToPage(1)
    expect(editor.pageIndex).toBe(1)
    expect(draft.selection).toBeNull()
    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(1))
    expect(api.applyTool.mock.calls[0][0]).toMatchObject({ tool: 'gradient', regionId: 'c1-p001-h1', pageIndex: 0 })
    expect(api.applyTool.mock.calls[0][0].params.opacity).toBe(25)
  })

  it('sends the last change after a commit on its way when the tool switches, though the page has left the window', async () => {
    const first = deferred()
    const api = backend({ applyTool: vi.fn(() => first.promise) })
    setBackend(api)
    await heldLive(api)
    editor.toolParams.gradient.opacity = 65
    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(1), { timeout: 2000 })
    editor.toolParams.gradient.opacity = 75
    setTool('eraser')
    // The page that held the layer is evicted while the first commit is out.
    editor.chapter.pages[0].regions = []
    editor.chapter.pages[0].resident = false
    first.resolve(applied)
    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(2))
    expect(api.applyTool.mock.calls[1][0]).toMatchObject({ tool: 'gradient', regionId: 'c1-p001-h1' })
    expect(api.applyTool.mock.calls[1][0].params.opacity).toBe(75)
    expect(api.createRegion).toHaveBeenCalledTimes(1)
  })

  it('sends a change a run refused at a tool switch once the run ends', async () => {
    const api = backend({ applyTool: vi.fn(async () => applied) })
    setBackend(api)
    await heldLive(api)
    editor.areaRuns = [{ pageIndex: 0, runId: 'r1' }]
    editor.toolParams.gradient.opacity = 15
    setTool('brush')
    await new Promise((resolve) => setTimeout(resolve, 400))
    expect(api.applyTool).not.toHaveBeenCalled()
    expect(shown('notice.run.pageBusy')).toBe(1)
    editor.areaRuns = []
    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(1), { timeout: 2000 })
    expect(api.applyTool.mock.calls[0][0]).toMatchObject({ tool: 'gradient' })
    expect(api.applyTool.mock.calls[0][0].params.opacity).toBe(15)
  })

  it('previews until the page has drawn the layer the change made, and not with the page\'s dots', async () => {
    globalThis.__TAURI_INTERNALS__ = { convertFileSrc: (_path, scheme) => `${scheme}://localhost/` }
    const answer = deferred()
    const api = backend({ applyTool: vi.fn(() => answer.promise) })
    setBackend(api)
    const page = editor.chapter.pages[0]
    page.chapterId = 'c1'
    page.regions = [gradientLayer('k1')]
    page.regionCount = 1
    editor.tool = 'gradient'
    expect(reopenGradient(page.regions[0])).toBe(true)
    const held = draft.selection
    // Nothing waits and the page shows the layer as it is: no preview.
    layerSettled.set('c1-p001|g1', layerUrl(page, page.regions[0]))
    expect(previewStands(held, editor.toolParams.gradient)).toBe(false)

    editor.toolParams.gradient.opacity = 80
    await vi.waitFor(() => expect(retouch.pending).toBe(true))
    expect(previewStands(held, editor.toolParams.gradient)).toBe(true)
    // The page's dots are found on the native side: no preview for them.
    editor.toolParams.gradient.asScreentone = true
    expect(previewStands(held, editor.toolParams.gradient)).toBe(false)
    editor.toolParams.gradient.asScreentone = false

    await vi.waitFor(() => expect(api.applyTool).toHaveBeenCalledTimes(1), { timeout: 2000 })
    answer.resolve({ status: 'applied', region: gradientLayer('k2'), pageStatus: 'cleaned' })
    await vi.waitFor(() => expect(retouch.pending).toBe(false))
    // Landed, but the page still shows the old picture: the preview stays.
    expect(previewStands(held, editor.toolParams.gradient)).toBe(true)
    const region = editor.chapter.pages[0].regions.find((candidate) => candidate.id === 'g1')
    layerSettled.set('c1-p001|g1', layerUrl(editor.chapter.pages[0], region))
    expect(previewStands(held, editor.toolParams.gradient)).toBe(false)
  })

  it('draws the preview over the selection only while it stands', async () => {
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(/** @type {any} */ (() => null))
    const api = backend()
    setBackend(api)
    lasso('gradient')
    await commitDraft()
    setLine({ x: 15, y: 15 }, { x: 35, y: 35 })
    const view = render(SelectionOverlay, {
      props: { held: draft.selection, params: editor.toolParams.gradient, pageWidth: 1600, pageHeight: 2400 },
    })
    await vi.waitFor(() => expect(retouch.pending).toBe(true))
    await vi.waitFor(() => expect(view.container.querySelector('canvas.preview')).not.toBeNull())
    editor.toolParams.gradient.asScreentone = true
    await vi.waitFor(() => expect(view.container.querySelector('canvas.preview')).toBeNull())
  })

  it('hides the held layer\'s old pixels under the preview, and only that layer', async () => {
    globalThis.__TAURI_INTERNALS__ = { convertFileSrc: (_path, scheme) => `${scheme}://localhost/` }
    vi.stubGlobal('fetch', vi.fn(() => new Promise(() => {})))
    const page = { ...editor.chapter.pages[0], chapterId: 'c1', regions: [gradientLayer('k1'), { ...gradientLayer('k9'), id: 'g2', mask: { ...gradientLayer('k9').mask, id: 'g2-m1', regionId: 'g2' } }] }
    const view = render(PatchLayers, { props: { page, hide: 'g1' } })
    const canvas = (id) => /** @type {HTMLCanvasElement} */ (view.container.querySelector(`canvas[data-layer="${id}"]`))
    expect(canvas('g1').style.visibility).toBe('hidden')
    expect(canvas('g2').style.visibility).toBe('')
  })
})
