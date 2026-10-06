/**
 * The Layers header's count, mounted: what it says for a page holding a
 * cleaned layer, a flagged layer, a detection and a held candidate, what it
 * says under the review filter, and that it agrees with the page's own counts
 * (`model/status.js#pageCounts`) before a reopen reduces the page to its
 * header and after its regions are back.
 */
import { afterEach, describe, expect, it } from 'vitest'
import { cleanup, render } from '@testing-library/svelte'
import { tick } from 'svelte'

import { setBackend } from '../api/backend.js'
import { pageCounts } from '../model/status.js'
import { editor } from '../state/editor.svelte.js'
import { t } from '../i18n/index.js'
import LayersWindow from './LayersWindow.svelte'

/**
 * A region on the page, with a cleaned mask unless `spec` says otherwise.
 *
 * @param {string} id
 * @param {object} [spec]
 */
function aRegion(id, spec = {}) {
  const bbox = { x: 10, y: 10, w: 20, h: 10 }
  return {
    id,
    pageId: 'c1-p001',
    bbox,
    source: 'auto',
    outcome: 'cleaned',
    gateSkipCause: null,
    declineReason: null,
    unusuallyLarge: false,
    detected: true,
    mask: {
      id: `${id}-m1`,
      regionId: id,
      sequence: 1,
      fillMode: 'match-surround',
      elapsedMs: 0,
      fittingReconstructed: false,
      cloudOutcome: null,
      provenance: { engine: 'fill', params_snapshot: {}, cloud: null },
      layer: { opacity: 100, offsetX: 0, offsetY: 0, rotation: 0, locked: false },
      sourceBbox: bbox,
    },
    ...spec,
  }
}

/** The four regions, fresh each time, as a load would hand them back. */
function regions() {
  return [
    aRegion('cleaned'),
    aRegion('flagged', { unusuallyLarge: true }),
    aRegion('found', { outcome: 'detected' }),
    aRegion('held', { outcome: 'candidate', candidateReason: 'review.reason.unassignedMask', mask: null }),
  ]
}

/** The page in hand, with its regions. */
function resident() {
  return { id: 'c1-p001', chapterId: 'c1', index: 0, width: 1600, height: 2400, status: 'cleaned', resident: true, regions: regions() }
}

/**
 * The same page as a reopened chapter first has it: the header the native
 * side reads out of the manifest, with no regions. `regionCount` is every
 * region listed, candidates included, as the native header counts it.
 */
function header() {
  return {
    id: 'c1-p001', chapterId: 'c1', index: 0, width: 1600, height: 2400, status: 'cleaned',
    resident: false, regions: [], regionCount: 4, doneCount: 1, reviewCount: 1, candidateCount: 1,
  }
}

afterEach(() => {
  cleanup()
  setBackend(null)
  editor.chapter = null
  editor.pageIndex = 0
  editor.reviewFilter = false
  editor.selectionId = null
  editor.hoverId = null
})

/** @param {ReturnType<typeof render>} view */
const meta = (view) => view.container.querySelector('.meta')?.textContent ?? ''

/** What the header should read for a page with these counts and two applied layers. */
function expected(counts, { filtered = false } = {}) {
  if (filtered) return t('editor.meta.masksFlagged', { count: counts.review })
  return t('review.meta.withCandidates', { base: t('editor.meta.masksApplied', { count: 2 }), count: counts.candidates })
}

describe('the Layers header count', () => {
  it('counts both layers applied and the candidate held apart, and only the flagged one under the review filter', async () => {
    editor.chapter = /** @type {any} */ ({ id: 'c1', review: [], pages: [resident()] })
    const view = render(LayersWindow)
    const counts = pageCounts(editor.chapter.pages[0])
    expect(counts).toEqual({ total: 3, done: 1, review: 1, candidates: 1 })
    expect(meta(view)).toBe('2 masks · 1 held')
    expect(meta(view)).toBe(expected(counts))

    editor.reviewFilter = true
    await tick()
    expect(meta(view)).toBe('1 flagged')
    expect(meta(view)).toBe(expected(counts, { filtered: true }))
  })

  it('agrees with the page counts before a reopen reduces the page to its header and after its regions are back', async () => {
    editor.chapter = /** @type {any} */ ({ id: 'c1', review: [], pages: [resident()] })
    const view = render(LayersWindow)
    const before = pageCounts(editor.chapter.pages[0])
    expect(meta(view)).toBe(expected(before))

    // Reopened: the header alone, counted from the manifest.
    editor.chapter.pages[0] = /** @type {any} */ (header())
    await tick()
    expect(pageCounts(editor.chapter.pages[0])).toEqual(before)

    // The page loads again.
    editor.chapter.pages[0] = /** @type {any} */ (resident())
    await tick()
    const after = pageCounts(editor.chapter.pages[0])
    expect(after).toEqual(before)
    expect(meta(view)).toBe(expected(after))

    editor.reviewFilter = true
    await tick()
    expect(meta(view)).toBe(expected(after, { filtered: true }))
  })
})
