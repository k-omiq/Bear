/**
 * The Gradient tool is live (`retouch.svelte.js`): once a held selection has
 * its line, every change reaches the page by itself a moment after the last
 * one, and Apply is Done. A browser test because the change is noticed by an
 * effect, and the node project's server build of Svelte runs none.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { editor } from '../state/editor.svelte.js'
import { setBackend } from '../api/backend.js'
import { createHistory } from '../model/history.js'
import { beginDraft, draft, resetDraftState } from './draft.svelte.js'
import { commitDraft } from './drawing.svelte.js'
import { applySelection, reopenGradient, setLine } from './retouch.svelte.js'
import { undo } from '../state/editor.svelte.js'

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
})

afterEach(() => {
  editor.areaRuns = []
  resetDraftState()
  editor.toolParams.gradient = { ...saved.gradient }
  editor.tool = 'autoClean'
})

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
})
