/**
 * The browser mock's Patch and Gradient seam, held to what
 * `region/retouch.rs` records: the proposal and the fit answer in page
 * percent, a patch layer is the clone engine with `mode: "patch"`, a gradient
 * layer is the paint engine with `mode: "gradient"` and the gradient itself,
 * and both are refused on a colour mode the paint path cannot write.
 */

import { describe, expect, it } from 'vitest'

import { createMockBackend } from './mock.js'
import { applyToolToRegion } from './tools.js'

const CHAPTER = 'tsuki-to-hane-ch107'
const LASSO = { kind: 'polygon', points: [{ x: 20, y: 20 }, { x: 30, y: 20 }, { x: 25, y: 30 }], feather: 0 }
const BBOX = { x: 20, y: 20, w: 10, h: 10 }

function makeMock() {
  return createMockBackend({ timing: { method: 0, cloud: 0, provision: 0, analysis: 0, region: 0, pageTail: 0 } })
}

describe('the Patch tool on the mock', () => {
  it('proposes a source one selection away, on the page', async () => {
    const answer = await makeMock().proposePatchSource({ chapterId: CHAPTER, pageIndex: 0, painted: LASSO })
    expect(answer).toEqual({ offset: { x: 10, y: 0 } })
  })

  it('answers null for a page it does not hold', async () => {
    expect(await makeMock().proposePatchSource({ chapterId: CHAPTER, pageIndex: 999, painted: LASSO })).toBeNull()
  })

  it('records a clone layer with the patch options', async () => {
    const made = await makeMock().createRegion({
      chapterId: CHAPTER,
      pageIndex: 0,
      bbox: BBOX,
      tool: 'patch',
      params: { mode: 'normal', direction: 'destination', diffusion: 2, offset: { x: 10, y: 0 }, painted: LASSO },
    })
    const provenance = made?.region.mask.provenance
    expect(provenance.engine).toBe('clone')
    expect(provenance.params_snapshot).toMatchObject({
      mode: 'patch',
      patch: { mode: 'normal', direction: 'destination', diffusion: 2, structure: 4, color: 0, offset: { x: 10, y: 0 } },
      painted: LASSO,
    })
  })
})

describe('the Gradient tool on the mock', () => {
  it('fits a dark-to-light line straight down the selection', async () => {
    const fit = await makeMock().fitGradient({ chapterId: CHAPTER, pageIndex: 0, painted: LASSO })
    expect(fit).toMatchObject({ kind: 'linear', start: [25, 20], end: [25, 30] })
    expect(fit.colors).toHaveLength(2)
  })

  it('records a paint layer that keeps its gradient, so the tool can reopen it', async () => {
    const gradient = { kind: 'radial', start: [25, 25], end: [30, 30], colors: [{ at: 0, color: [0, 0, 0] }], opacities: [], reverse: false, dither: true }
    const made = await makeMock().createRegion({
      chapterId: CHAPTER,
      pageIndex: 0,
      bbox: BBOX,
      tool: 'gradient',
      params: { gradient, asScreentone: true, opacity: 60, painted: { ...LASSO, feather: 3 } },
    })
    const mask = made?.region.mask
    expect(mask.provenance.engine).toBe('paint')
    expect(mask.provenance.params_snapshot).toMatchObject({ mode: 'gradient', gradient, asScreentone: true, feather: 3, layer: { opacity: 60 } })
    expect(mask.layer.opacity).toBe(60)
  })
})

describe('both tools', () => {
  it.each(['patch', 'gradient'])('refuse an indexed page for %s', (tool) => {
    const region = { id: 'r', bbox: { ...BBOX } }
    const refused = applyToolToRegion(/** @type {any} */ (region), /** @type {any} */ ({ colorMode: 'Indexed8' }), /** @type {any} */ ({}), { tool, params: {} })
    expect(refused.mask).toBeNull()
    expect(/** @type {any} */ (region).declineReason).toBe('decline.reason.paintUnsupportedMode')
  })
})
