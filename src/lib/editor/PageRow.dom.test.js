/**
 * A Pages row's held mark, mounted: a page carrying a candidate shows `◌1`
 * whether its regions are in hand or it is a reopened page's header alone,
 * the mark is hidden from assistive tech because the row's name says the same
 * in words, and a page with nothing held shows no mark.
 */
import { afterEach, describe, expect, it } from 'vitest'
import { cleanup, render } from '@testing-library/svelte'

import { t } from '../i18n/index.js'
import PageRow from './PageRow.svelte'
import { pageRow } from './pagerows.js'

/**
 * @param {string} id
 * @param {object} [spec]
 */
function aRegion(id, spec = {}) {
  return {
    id,
    pageId: 'c1-p001',
    bbox: { x: 10, y: 10, w: 20, h: 10 },
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
    },
    ...spec,
  }
}

const candidate = () => aRegion('held', { outcome: 'candidate', candidateReason: 'review.reason.isolatedMask', mask: null })

/** @param {object[]} regions */
const residentPage = (regions) => ({ id: 'c1-p001', index: 0, status: 'cleaned', resident: true, regions })

/** The same page as a reopened chapter holds it before its regions load. */
const headerPage = () => ({
  id: 'c1-p001', index: 0, status: 'cleaned', resident: false, regions: [],
  regionCount: 2, doneCount: 1, reviewCount: 0, candidateCount: 1,
})

/** @param {any} page */
function show(page) {
  const row = pageRow(page, { index: 0 })
  const view = render(PageRow, { props: { row, total: 1, selected: false, focused: true, onpick: () => {} } })
  return { row, view, button: view.getByRole('option') }
}

afterEach(() => cleanup())

describe('a Pages row with a held candidate', () => {
  const held = t('review.page.candidates', { count: 1 })

  it.each([
    ['in hand', () => residentPage([aRegion('cleaned'), candidate()])],
    ['reopened, header only', headerPage],
  ])('shows ◌1, hidden from assistive tech, and says it in the row name (%s)', (_, page) => {
    const { row, view, button } = show(page())
    expect(row.candidates).toBe(1)
    const mark = /** @type {HTMLElement} */ (view.container.querySelector('.held'))
    expect(mark).not.toBeNull()
    expect(mark.textContent).toBe('◌1')
    expect(mark.getAttribute('aria-hidden')).toBe('true')
    expect(held).toBe('1 area held for your choice')
    expect(button.getAttribute('aria-label')).toContain(held)
    expect(button.getAttribute('title')).toContain(held)
    // The candidate is counted apart: the ratio is the one cleaned region.
    expect(row.total).toBe(1)
    expect(row.cleaned).toBe(1)
  })

  it('shows no held mark and no held words on a page with nothing held', () => {
    const { row, view, button } = show(residentPage([aRegion('cleaned')]))
    expect(row.candidates).toBe(0)
    expect(view.container.querySelector('.held')).toBeNull()
    expect(view.container.textContent).not.toContain('◌')
    expect(button.getAttribute('aria-label')).not.toContain(held)
  })
})
