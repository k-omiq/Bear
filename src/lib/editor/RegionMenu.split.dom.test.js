/**
 * The region menu's two splits: Split off part and Split into pieces show on
 * a detection and not on a cleaned layer, and Split into pieces, picked,
 * asks the backend for the pieces of that one detection with no gesture.
 * Against a stub backend.
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { createHistory } from '../model/history.js'
import { editor } from '../state/editor.svelte.js'
import RegionMenu from './RegionMenu.svelte'

const CHAPTER = 'chapter'
const PAGE = `${CHAPTER}-p001`

const detection = () => ({
  id: `${PAGE}-d0`,
  pageId: PAGE,
  bbox: { x: 10, y: 10, w: 40, h: 10 },
  source: 'auto',
  outcome: 'detected',
  detected: true,
  pick: 'lama',
  insideBubble: true,
  gateSkipCause: null,
  mask: { id: `${PAGE}-d0-m1`, provenance: { engine: 'lama' }, layer: {} },
})

const cleaned = () => ({
  id: `${PAGE}-r1`,
  pageId: PAGE,
  bbox: { x: 10, y: 10, w: 20, h: 20 },
  source: 'detected',
  outcome: 'cleaned',
  gateSkipCause: null,
  mask: { id: 'm1', legacyPatchRevision: 'r1', fillMode: 'reconstruct', provenance: { engine: 'lama' }, layer: {} },
})

/** @param {any} target */
function open(target) {
  editor.project = /** @type {any} */ ({ id: 'pr', mode: 'pages' })
  editor.pageIndex = 0
  editor.chapter = /** @type {any} */ ({
    id: CHAPTER,
    review: [],
    pages: [{ id: PAGE, index: 0, width: 700, height: 1000, status: target.outcome, resident: true, regionCount: 1, regions: [target] }],
  })
  return /** @type {any} */ (editor.chapter.pages[0].regions[0])
}

function backend() {
  const adapter = {
    regionVariations: vi.fn(async () => []),
    editDetectionMask: vi.fn(async () => ({ pageStatus: 'detected', changed: [`${PAGE}-d0`], created: [`${PAGE}-d1`], removed: [],
      pages: [{ pageIndex: 0, pageStatus: 'detected' }] })),
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
})

afterEach(() => {
  cleanup()
  setBackend(null)
  editor.chapter = null
  editor.project = null
})

describe('the region menu\'s splits', () => {
  it('show on a detection, and Split into pieces asks for that detection\'s pieces at once', async () => {
    const adapter = backend()
    const onclose = vi.fn()
    render(RegionMenu, { props: { at: { x: 10, y: 10, region: open(detection()) }, onclose } })
    expect(screen.getByRole('menuitem', { name: t('masks.action.splitPart') })).toBeTruthy()
    const pieces = screen.getByRole('menuitem', { name: t('masks.action.splitPieces') })

    await fireEvent.click(pieces)
    await waitFor(() => expect(adapter.editDetectionMask).toHaveBeenCalledWith({
      chapterId: CHAPTER, pageIndex: 0, mode: 'pieces', regionId: `${PAGE}-d0`,
    }))
    expect(onclose).toHaveBeenCalled()
  })

  it('do not show on a cleaned layer', async () => {
    backend()
    render(RegionMenu, { props: { at: { x: 10, y: 10, region: open(cleaned()) }, onclose: () => {} } })
    expect(screen.getAllByRole('menuitem').length).toBeGreaterThan(0)
    expect(screen.queryByRole('menuitem', { name: t('masks.action.splitPieces') })).toBeNull()
    expect(screen.queryByRole('menuitem', { name: t('masks.action.splitPart') })).toBeNull()
  })
})
