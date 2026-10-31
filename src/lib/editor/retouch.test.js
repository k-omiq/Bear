/**
 * The Patch tool and the Gradient tool between the gesture and the commit
 * (`retouch.svelte.js`): a drag holds a selection rather than committing,
 * the Patch's source is proposed and can be moved, the Gradient waits for its
 * line, and Apply sends one `createRegion` that is one undo step.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { editor } from '../state/editor.svelte.js'
import { setBackend } from '../api/backend.js'
import { canUndo, createHistory, settled } from '../model/history.js'
import { beginDraft, draft, resetDraftState } from './draft.svelte.js'
import { commitDraft } from './drawing.svelte.js'
import {
  applicable,
  applySelection,
  editGradientLayer,
  matchSurroundings,
  moveGhost,
  reopenGradient,
  selectionParams,
  setLine,
} from './retouch.svelte.js'

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

const saved = { patch: { ...editor.toolParams.patch }, gradient: { ...editor.toolParams.gradient } }

beforeEach(() => {
  editor.chapter = chapter()
  editor.project = { mode: 'single' }
  editor.pageIndex = 0
  editor.history = createHistory()
})

afterEach(() => {
  resetDraftState()
  editor.toolParams.patch = { ...saved.patch }
  editor.toolParams.gradient = { ...saved.gradient }
  editor.tool = 'autoClean'
})

describe('the Patch tool', () => {
  it('holds the selection, proposes a source, and applies one layer with the ghost offset', async () => {
    const api = backend()
    setBackend(api)
    editor.toolParams.patch = { ...saved.patch, mode: 'normal', direction: 'source', diffusion: 3 }
    lasso('patch')
    expect(await commitDraft()).toBe(false)
    expect(api.createRegion).not.toHaveBeenCalled()
    expect(draft.selection?.tool).toBe('patch')
    await vi.waitFor(() => expect(draft.selection?.offset).toEqual({ x: 30, y: 0 }))
    expect(api.proposePatchSource.mock.calls[0][0]).toMatchObject({ chapterId: 'c1', pageIndex: 0, painted: { kind: 'polygon' } })

    moveGhost({ x: -20, y: 5 })
    expect(applicable(draft.selection)).toBe(true)
    expect(await applySelection()).toBe(true)
    const request = api.createRegion.mock.calls[0][0]
    expect(request.tool).toBe('patch')
    expect(request.params).toMatchObject({ mode: 'normal', direction: 'source', diffusion: 3, offset: { x: -20, y: 5 } })
    // A patch's edge is the solve's, so the selection carries no feather.
    expect(request.params.painted.feather).toBe(0)
    expect(draft.selection).toBeNull()
    await settled(editor.history)
    expect(canUndo(editor.history)).toBe(true)
    expect(api.historyPush.mock.calls[0][0].entry.label).toBe('retouch.command.patch')
  })

  it('applies at once in the modes that read no source', () => {
    const held = /** @type {any} */ ({ tool: 'patch', offset: null })
    expect(applicable(held, { mode: 'auto' })).toBe(true)
    expect(applicable(held, { mode: 'contentAware' })).toBe(true)
    expect(applicable(held, { mode: 'normal' })).toBe(false)
    expect(applicable(null, { mode: 'auto' })).toBe(false)
  })
})

describe('the Gradient tool', () => {
  it('waits for its line, then sends the gradient as the seam reads it', async () => {
    const api = backend()
    setBackend(api)
    editor.toolParams.gradient = { ...saved.gradient, kind: 'radial', feather: 6, opacity: 70 }
    lasso('gradient')
    expect(await commitDraft()).toBe(false)
    expect(applicable(draft.selection)).toBe(false)
    expect(await applySelection()).toBe(false)
    expect(api.createRegion).not.toHaveBeenCalled()

    setLine({ x: 15, y: 15 }, { x: 35, y: 35 })
    expect(await applySelection()).toBe(true)
    const request = api.createRegion.mock.calls[0][0]
    expect(request.tool).toBe('gradient')
    expect(request.params.gradient).toMatchObject({ kind: 'radial', start: [15, 15], end: [35, 35], dither: true })
    expect(request.params.gradient.colors[0].color).toEqual([0, 0, 0])
    // The stops travel inside the gradient, not beside it.
    expect(request.params.colors).toBeUndefined()
    expect(request.params.painted.feather).toBe(6)
    expect(request.params.opacity).toBe(70)
  })

  it('fills at once from all round the selection when its type is Auto', async () => {
    const api = backend()
    setBackend(api)
    editor.toolParams.gradient = { ...saved.gradient, kind: 'auto' }
    lasso('gradient')
    expect(await commitDraft()).toBe(false)
    await vi.waitFor(() => expect(api.createRegion).toHaveBeenCalledTimes(1))
    const request = api.createRegion.mock.calls[0][0]
    expect(request.tool).toBe('gradient')
    expect(request.params.kind).toBe('auto')
    // no line and no stops: the native side reads the page round it
    expect(request.params.gradient).toBeUndefined()
    expect(request.params.painted).toMatchObject({ kind: 'polygon' })
    expect(draft.selection).toBeNull()
    expect(applicable(/** @type {any} */ ({ tool: 'gradient', start: null, end: null }), { kind: 'auto' })).toBe(true)
  })

  it('takes the line and the stops from Match surroundings', async () => {
    setBackend(backend())
    lasso('gradient')
    await commitDraft()
    expect(await matchSurroundings()).toBe(true)
    expect(draft.selection?.start).toEqual({ x: 25, y: 10 })
    expect(editor.toolParams.gradient.colors).toEqual([{ at: 0, color: '#281e1e' }, { at: 1, color: '#e1e1e1' }])
  })

  it('reopens a gradient layer and re-renders that layer on Apply', async () => {
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
    await applySelection()
    expect(api.createRegion).not.toHaveBeenCalled()
    expect(api.applyTool).toHaveBeenCalledTimes(1)
    expect(api.applyTool.mock.calls[0][0].params.gradient).toMatchObject({ kind: 'diamond', start: [20, 20] })
  })

  it('reopens a gradient layer from any tool, an Auto one as Linear with a fitted line', async () => {
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
            gradient: null,
            auto: true,
            painted: { kind: 'rect', points: [{ x: 10, y: 10 }, { x: 40, y: 40 }], feather: 0 },
          },
        },
      },
    }
    editor.chapter.pages[0].regions = [region]
    editor.chapter.pages[0].regionCount = 1
    editor.toolParams.gradient = { ...editor.toolParams.gradient, kind: 'auto' }
    editor.tool = 'brush'
    expect(editGradientLayer(region)).toBe(true)
    expect(editor.tool).toBe('gradient')
    expect(editor.selectionId).toBe('g1')
    expect(editor.toolParams.gradient.kind).toBe('linear')
    expect(draft.selection).toMatchObject({ tool: 'gradient', regionId: 'g1', start: null })
    await vi.waitFor(() => expect(draft.selection?.start).toEqual({ x: 25, y: 10 }))
    await applySelection()
    expect(api.createRegion).not.toHaveBeenCalled()
    expect(api.applyTool.mock.calls[0][0].params).toMatchObject({ kind: 'linear', gradient: { kind: 'linear', start: [25, 10] } })
  })

  it('records no Patch fields on a gradient, and no gradient on a patch', () => {
    const held = /** @type {any} */ ({ tool: 'patch', painted: { kind: 'rect', points: [], feather: 9 }, stroke: null, offset: { x: 1, y: 2 }, start: null, end: null })
    expect(selectionParams(held, { mode: 'auto' })).toEqual({ mode: 'auto', painted: { kind: 'rect', points: [], feather: 0 }, offset: { x: 1, y: 2 } })
  })

  /** A placed Auto layer, as the native side records it. */
  function autoLayer(layer = undefined) {
    const region = {
      id: 'g1',
      pageId: 'c1-p001',
      bbox: { x: 10, y: 10, w: 30, h: 30 },
      outcome: 'cleaned',
      mask: {
        ...(layer ? { layer } : {}),
        provenance: {
          engine: 'paint',
          params_snapshot: {
            mode: 'gradient',
            gradient: null,
            auto: true,
            painted: { kind: 'rect', points: [{ x: 10, y: 10 }, { x: 40, y: 40 }], feather: 0 },
          },
        },
      },
    }
    editor.chapter.pages[0].regions = [region]
    editor.chapter.pages[0].regionCount = 1
    return region
  }

  it('keeps the Auto layer in the undo record of a reopened one, so Undo can hold it again', async () => {
    const api = backend()
    setBackend(api)
    editGradientLayer(autoLayer())
    await vi.waitFor(() => expect(draft.selection?.start).toEqual({ x: 25, y: 10 }))
    await applySelection()
    await settled(editor.history)
    const entry = api.historyPush.mock.calls.at(-1)?.[0]?.entry
    expect(entry.before.region.mask.provenance.params_snapshot.auto).toBe(true)
    expect(entry.after.region.id).toBe('g1')
  })

  it('a line dragged while the fit is on its way wins, as Patch\'s ghost does', async () => {
    /** @type {(answer: any) => void} */
    let answer = () => {}
    const api = backend({ fitGradient: vi.fn(() => new Promise((resolve) => { answer = resolve })) })
    setBackend(api)
    editGradientLayer(autoLayer())
    setLine({ x: 11, y: 12 }, { x: 13, y: 14 })
    answer({ kind: 'linear', start: [25, 10], end: [25, 40], colors: [{ at: 0, color: [0, 0, 0] }], opacities: [], reverse: false, dither: true })
    await vi.waitFor(() => expect(api.fitGradient).toHaveBeenCalled())
    await new Promise((resolve) => setTimeout(resolve))
    expect(draft.selection?.start).toEqual({ x: 11, y: 12 })
  })

  it('a moved Auto layer fits the surroundings where it now sits', async () => {
    const api = backend()
    setBackend(api)
    // 160 page pixels on a 1600 pixel page: ten percent to the right.
    editGradientLayer(autoLayer({ offsetX: 160 }))
    await vi.waitFor(() => expect(api.fitGradient).toHaveBeenCalled())
    expect(api.fitGradient.mock.calls[0][0].painted.points[0].x).toBeCloseTo(20)
  })

  it('an Auto layer reopens as Linear whatever type was on the bar', async () => {
    const api = backend({ fitGradient: vi.fn(async () => null) })
    setBackend(api)
    editor.toolParams.gradient = { ...editor.toolParams.gradient, kind: 'radial' }
    editGradientLayer(autoLayer())
    await vi.waitFor(() => expect(api.fitGradient).toHaveBeenCalled())
    expect(editor.toolParams.gradient.kind).toBe('linear')
  })

  it('a layer that cannot reopen leaves the tool out as it was', () => {
    setBackend(backend())
    const region = autoLayer()
    region.mask.provenance.params_snapshot.auto = undefined
    editor.tool = 'brush'
    expect(editGradientLayer(region)).toBe(false)
    expect(editor.tool).toBe('brush')
  })
})
