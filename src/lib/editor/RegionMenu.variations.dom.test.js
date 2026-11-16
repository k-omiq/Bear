/**
 * The region menu's Variations on a layer a model that answers differently
 * each time made: the "Variation 2 of 3" row with a previous and a next
 * button, reached from the keyboard, that picks the one beside it and leaves
 * the menu open; and Make n variations at the remembered count. A LaMa layer
 * has neither. Against a stub backend.
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { createHistory } from '../model/history.js'
import { editor } from '../state/editor.svelte.js'
import { setVariationCount } from './maskactions.svelte.js'
import RegionMenu from './RegionMenu.svelte'

const CHAPTER = 'chapter'
const ID = `${CHAPTER}-p001-r1`

/** @param {string} engine @param {string} revision */
function region(engine, revision) {
  return {
    id: ID,
    pageId: `${CHAPTER}-p001`,
    bbox: { x: 10, y: 10, w: 20, h: 20 },
    source: 'detected',
    outcome: 'cleaned',
    gateSkipCause: null,
    mask: { id: 'm1', legacyPatchRevision: revision, fillMode: 'reconstruct', provenance: { engine }, layer: {} },
  }
}

/** @param {any} target */
function open(target) {
  editor.project = /** @type {any} */ ({ id: 'pr', mode: 'pages' })
  editor.pageIndex = 0
  editor.chapter = /** @type {any} */ ({
    id: CHAPTER,
    review: [],
    pages: [{ id: `${CHAPTER}-p001`, index: 0, width: 700, height: 1000, status: 'cleaned', resident: true, regionCount: 1, regions: [target] }],
  })
  return /** @type {any} */ (editor.chapter.pages[0].regions[0])
}

function backend() {
  const adapter = {
    regionVariations: vi.fn(async () => ['r0', 'r1', 'r2'].map((revision, at) => ({
      revision, engine: 'nanobanana', model: null, created: at, current: revision === editor.chapter?.pages[0].regions[0].mask?.legacyPatchRevision,
    }))),
    pickVariation: vi.fn(async ({ revision }) => ({
      region: region('nanobanana', revision),
      pageStatus: 'cleaned',
      history: { limit: 500, cursor: 1, entries: [{ seq: 1, label: 'masks.command.pickVariation', page: 0, undone: false }] },
    })),
    loadPages: vi.fn(async () => []),
    historyLoad: vi.fn(async () => ({ limit: 500, cursor: 0, entries: [] })),
  }
  setBackend(/** @type {any} */ (adapter))
  return adapter
}

beforeEach(() => {
  editor.history = createHistory()
  editor.run.active = false
  editor.run.busyPages = []
  setVariationCount(3)
})

afterEach(() => {
  cleanup()
  setBackend(null)
  editor.chapter = null
  editor.project = null
})

describe('the region menu\'s Variations', () => {
  it('show which one the layer shows, step to the next without closing, and offer Make 3 variations', async () => {
    const adapter = backend()
    const onclose = vi.fn()
    render(RegionMenu, { props: { at: { x: 10, y: 10, region: open(region('nanobanana', 'r1')) }, onclose } })
    const row = await screen.findByRole('group', { name: t('masks.variations.position', { current: 2, count: 3 }) })
    expect(adapter.regionVariations).toHaveBeenCalledWith({ regionId: ID })
    expect(screen.getByRole('menuitem', { name: t('masks.variations.make', { n: 3 }) })).toBeTruthy()

    const next = screen.getByRole('button', { name: t('masks.variations.next') })
    expect(row.contains(next)).toBe(true)
    await fireEvent.click(next)
    await waitFor(() => expect(adapter.pickVariation).toHaveBeenCalledWith({ regionId: ID, revision: 'r2' }))
    expect(onclose).not.toHaveBeenCalled()
    await screen.findByRole('group', { name: t('masks.variations.position', { current: 3, count: 3 }) })
  })

  it('are reached from the keyboard: the row takes the focus on its buttons, Left and Right go between them', async () => {
    backend()
    render(RegionMenu, { props: { at: { x: 10, y: 10, region: open(region('nanobanana', 'r1')) }, onclose: () => {} } })
    const row = await screen.findByRole('group', { name: t('masks.variations.position', { current: 2, count: 3 }) })
    const menu = screen.getByRole('menu')
    const previous = screen.getByRole('button', { name: t('masks.variations.previous') })
    const next = screen.getByRole('button', { name: t('masks.variations.next') })
    for (let step = 0; step < 12 && !row.contains(document.activeElement); step += 1) {
      await fireEvent.keyDown(document.activeElement ?? menu, { key: 'ArrowDown' })
    }
    expect(document.activeElement).toBe(previous)
    await fireEvent.keyDown(previous, { key: 'ArrowRight' })
    expect(document.activeElement).toBe(next)
    await fireEvent.keyDown(next, { key: 'ArrowLeft' })
    expect(document.activeElement).toBe(previous)
  })

  it('show no position row with one variation, and a LaMa layer gets the position row alone', async () => {
    const adapter = backend()
    adapter.regionVariations.mockResolvedValue([{ revision: 'r1', engine: 'nanobanana', model: null, created: 0, current: true }])
    render(RegionMenu, { props: { at: { x: 10, y: 10, region: open(region('nanobanana', 'r1')) }, onclose: () => {} } })
    await waitFor(() => expect(adapter.regionVariations).toHaveBeenCalled())
    expect(screen.getByRole('menuitem', { name: t('masks.variations.make', { n: 3 }) })).toBeTruthy()
    expect(screen.queryByRole('group', { name: /1/ })).toBeNull()
    cleanup()

    // A LaMa layer's variations are its wider tries: read, stepped through,
    // and nothing makes n of them. It has no Try again either.
    adapter.regionVariations.mockClear()
    adapter.regionVariations.mockResolvedValue([
      { revision: 'r0', engine: 'lama', model: null, created: 0, current: false },
      { revision: 'r1', engine: 'lama', model: null, created: 1, current: true },
    ])
    render(RegionMenu, { props: { at: { x: 10, y: 10, region: open(region('lama', 'r1')) }, onclose: () => {} } })
    await waitFor(() => expect(adapter.regionVariations).toHaveBeenCalled())
    await waitFor(() => expect(screen.getByRole('group', { name: t('masks.variations.position', { current: 2, count: 2 }) })).toBeTruthy())
    expect(screen.queryByRole('menuitem', { name: t('masks.variations.make', { n: 3 }) })).toBeNull()
    expect(screen.queryByRole('menuitem', { name: t('masks.action.retry') })).toBeNull()
    expect(screen.getByRole('menuitem', { name: t('masks.action.retryWider') })).toBeTruthy()
  })
})
