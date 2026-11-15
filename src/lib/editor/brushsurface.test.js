import { describe, expect, it, vi } from 'vitest'
import { MAX_PAGE_SURFACE, brushSurface, coverageAt, proxyScale, surfaceScale, tipDistance } from './brushsurface.js'
import { planStroke } from './paintplan.js'

const brush = (over = {}) => ({
  size: 20, hardness: 100, flow: 100, opacity: 100, spacing: 10, pressureSize: false, pressureOpacity: false, ...over,
})

/** The pixels a surface puts on a canvas, by page pixel, at scale 1. */
function pixels(surface) {
  const puts = []
  const node = /** @type {any} */ ({ style: {}, getContext: () => ({ putImageData: (image, x, y) => puts.push({ image, x, y }) }) })
  vi.stubGlobal('ImageData', class { constructor(data, width, height) { Object.assign(this, { data, width, height }) } })
  surface.paint(node)
  vi.unstubAllGlobals()
  return {
    /** @param {number} x @param {number} y */
    alpha(x, y) {
      const put = puts.find(p => x >= p.x && x < p.x + 128 && y >= p.y && y < p.y + 128)
      return put ? put.image.data[((y - put.y) * 128 + (x - put.x)) * 4 + 3] : 0
    },
    puts,
  }
}

describe('coverageAt', () => {
  it('is the falloff of brush.rs#analytic_radial_coverage', () => {
    expect(coverageAt(0.5, 1)).toBe(1)
    expect(coverageAt(1, 0.5)).toBe(0)
    expect(coverageAt(0.4, 0.5)).toBe(1)
    // Halfway into the soft shell is past the 0.55 knee.
    expect(coverageAt(0.75, 0.5)).toBeCloseTo(1 - (0.5 / 0.55) * 0.45)
    expect(coverageAt(0.5, 0)).toBeCloseTo(1 - (0.5 / 0.55) * 0.45)
    expect(coverageAt(0.8, 0)).toBeCloseTo(0.55 * (1 - (0.8 - 0.55) / 0.45))
  })
})

describe('tipDistance', () => {
  it('is plan.rs#Tip::distance: straight for a disc, the larger axis for a square', () => {
    expect(tipDistance(3, 4)).toBe(5)
    expect(tipDistance(3, 4, 'round')).toBe(5)
    expect(tipDistance(3, -4, 'square')).toBe(4)
    expect(tipDistance(-3, 3, 'square')).toBe(3)
  })
})

describe('proxyScale', () => {
  it('caps the short edge at 1024, like the tiles', () => {
    expect(proxyScale(800, 1200)).toBe(1)
    expect(proxyScale(2048, 3000)).toBe(0.5)
    expect(proxyScale(4096, 1024)).toBe(1)
  })
})

describe('surfaceScale', () => {
  it('is the proxy\'s scale until the page is shown past it', () => {
    expect(surfaceScale(4000, 6000)).toBe(proxyScale(4000, 6000))
    expect(surfaceScale(4000, 6000, false)).toBeLessThan(1)
  })

  it('is the page\'s own pixels once the page is shown past its proxy', () => {
    expect(surfaceScale(4000, 6000, true)).toBe(1)
  })

  it('stays on the proxy for a page too large to hold whole', () => {
    expect(10000 * 10000).toBeGreaterThan(MAX_PAGE_SURFACE)
    expect(surfaceScale(10000, 10000, true)).toBe(proxyScale(10000, 10000))
  })
})

describe('brushSurface', () => {
  // `paint/mod.rs#a_dab_is_centred_on_its_point`: pixel k spans k to k+1
  // and is sampled at its centre, so a dab 20 across at 50 covers 40 to 59,
  // ten pixels each side, not 41 to 59 and one pixel down and right.
  it('centres a dab on its point: each pixel is sampled at its centre', () => {
    const surface = brushSurface({ brush: brush(), color: '#000000', pageWidth: 200, pageHeight: 200 })
    surface.add({ x: 50, y: 50, p: 1 })
    const out = pixels(surface)
    expect(out.alpha(40, 50)).toBe(255)
    expect(out.alpha(59, 50)).toBe(255)
    expect(out.alpha(39, 50)).toBe(0)
    expect(out.alpha(60, 50)).toBe(0)
    expect(out.alpha(50, 40)).toBe(255)
    expect(out.alpha(50, 39)).toBe(0)
  })

  it('draws on page pixels once the page is shown past its proxy', () => {
    const page = brushSurface({ brush: brush(), color: '#000000', pageWidth: 4000, pageHeight: 6000, pagePixels: true })
    page.add({ x: 50, y: 50, p: 1 })
    const out = pixels(page)
    // The same twenty pixels a small page gets, one surface pixel each.
    expect(out.alpha(40, 50)).toBe(255)
    expect(out.alpha(59, 50)).toBe(255)
    expect(out.alpha(60, 50)).toBe(0)
    // On the proxy the same dab is a quarter as many surface pixels, and
    // nothing lands at the page-pixel place.
    const proxy = brushSurface({ brush: brush(), color: '#000000', pageWidth: 4000, pageHeight: 6000 })
    proxy.add({ x: 50, y: 50, p: 1 })
    expect(pixels(proxy).alpha(59, 50)).toBe(0)
  })
  it('stamps a hard dab solid inside its radius and nothing outside', () => {
    const surface = brushSurface({ brush: brush(), color: '#000000', pageWidth: 200, pageHeight: 200 })
    surface.add({ x: 50, y: 50, p: 1 })
    const out = pixels(surface)
    expect(out.alpha(50, 50)).toBe(255)
    expect(out.alpha(59, 50)).toBe(255)
    expect(out.alpha(61, 50)).toBe(0)
  })

  it('never builds a soft rim past its own coverage where dabs overlap', () => {
    const one = brushSurface({ brush: brush({ hardness: 0 }), color: '#000000', pageWidth: 200, pageHeight: 200 })
    one.add({ x: 50, y: 50, p: 1 })
    const many = brushSurface({ brush: brush({ hardness: 0 }), color: '#000000', pageWidth: 200, pageHeight: 200 })
    // The same place, over and over: a canvas drawn dab over dab goes solid.
    for (let i = 0; i < 20; i += 1) many.add({ x: 50 + (i % 2) * 0.01, y: 50, p: 1 })
    expect(pixels(many).alpha(56, 50)).toBe(pixels(one).alpha(56, 50))
  })

  it('caps the stroke at its opacity', () => {
    const surface = brushSurface({ brush: brush({ opacity: 40 }), color: '#ff0000', pageWidth: 200, pageHeight: 200 })
    surface.add({ x: 50, y: 50, p: 1 })
    surface.add({ x: 80, y: 50, p: 1 })
    expect(pixels(surface).alpha(65, 50)).toBe(Math.round(0.4 * 255))
  })

  // The same dabs as `paint/mod.rs#a_square_tip_covers_the_corners_a_round_one_leaves`:
  // the preview and the commit cover the same pixels, pixel for pixel. At
  // 9.5, the middle of pixel 9, a dab 7 across covers pixels 6 to 12 whole.
  it('stamps a square tip over its corners, the 7 by 7 a hard dab 7 across covers', () => {
    const square = brushSurface({ brush: brush({ size: 7, tip: 'square' }), color: '#000000', pageWidth: 200, pageHeight: 200 })
    square.add({ x: 9.5, y: 9.5, p: 1 })
    const round = brushSurface({ brush: brush({ size: 7 }), color: '#000000', pageWidth: 200, pageHeight: 200 })
    round.add({ x: 9.5, y: 9.5, p: 1 })
    const out = pixels(square)
    const covered = []
    for (let y = 0; y < 20; y += 1) for (let x = 0; x < 20; x += 1) if (out.alpha(x, y) > 0) covered.push([x, y])
    expect(covered).toHaveLength(49)
    expect(covered.every(([x, y]) => x >= 6 && x <= 12 && y >= 6 && y <= 12)).toBe(true)
    expect(out.alpha(12, 12)).toBe(255)
    expect(pixels(round).alpha(12, 12)).toBe(0)
  })

  // `paint/mod.rs#a_soft_square_fades_by_its_axis_distance`'s dab.
  it('softens a square by its axis distance, as hardness softens a disc', () => {
    const at = (/** @type {'round'|'square'} */ tip) => {
      const surface = brushSurface({ brush: brush({ size: 11, hardness: 50, tip }), color: '#000000', pageWidth: 200, pageHeight: 200 })
      surface.add({ x: 9.5, y: 9.5, p: 1 })
      const out = pixels(surface)
      return (/** @type {number} */ x, /** @type {number} */ y) => out.alpha(x, y)
    }
    const square = at('square')
    const edge = Math.round(coverageAt(3 / 5.5, 0.5) * 255)
    expect(square(9, 9)).toBe(255)
    expect(square(12, 9)).toBe(edge)
    expect(square(12, 12)).toBe(edge)
    expect(square(6, 12)).toBe(edge)
    expect(square(13, 13)).toBeLessThan(edge)
    const round = at('round')
    expect(round(12, 9)).toBe(edge)
    expect(round(12, 12)).toBeLessThan(edge)
  })

  // `paint/erase.rs#a_square_eraser_reaches_its_corners`: a corner is capped
  // at the stroke's opacity however often the stroke covers it.
  it('caps a square stroke at its opacity, corners and all', () => {
    const surface = brushSurface({ brush: brush({ opacity: 40, tip: 'square' }), color: '#000000', pageWidth: 200, pageHeight: 200 })
    surface.add({ x: 50, y: 50, p: 1 })
    surface.add({ x: 70, y: 50, p: 1 })
    const out = pixels(surface)
    expect(out.alpha(59, 59)).toBe(102)
    expect(out.alpha(41, 41)).toBe(102)
    expect(out.alpha(60, 50 + 11)).toBe(0)
  })

  it('stays inside the page unless it is a longstrip stroke', () => {
    const page = brushSurface({ brush: brush(), color: '#000000', pageWidth: 200, pageHeight: 200 })
    page.add({ x: 100, y: 2, p: 1 })
    expect(page.bounds()?.y).toBe(0)
    const strip = brushSurface({ brush: brush(), color: '#000000', pageWidth: 200, pageHeight: 200, clipY: false })
    strip.add({ x: 100, y: 2, p: 1 })
    expect(strip.bounds()?.y).toBeLessThan(0)
  })

  it('puts only the tiles that changed since the last paint', () => {
    const surface = brushSurface({ brush: brush(), color: '#000000', pageWidth: 400, pageHeight: 400 })
    surface.add({ x: 20, y: 20, p: 1 })
    surface.add({ x: 300, y: 20, p: 1 })
    const node = /** @type {any} */ ({ style: {}, getContext: () => ({ putImageData: () => { painted += 1 } }) })
    let painted = 0
    vi.stubGlobal('ImageData', class {})
    surface.paint(node)
    const first = painted
    surface.paint(node)
    expect(painted).toBe(first)
    surface.add({ x: 300, y: 30, p: 1 })
    surface.paint(node)
    vi.unstubAllGlobals()
    // The stroke crossed every tile from the first dab to the second, and the
    // last move touched one of them.
    expect(first).toBeGreaterThanOrEqual(3)
    expect(painted - first).toBe(1)
  })

  // `render_clone` stamps with `build_up`: each dab takes a pixel part of the
  // way to the opacity, by its flow.
  it('builds a clone stroke\'s coverage towards the opacity, dab by dab', () => {
    const spec = brush({ flow: 20, spacing: 1 })
    const points = [{ x: 50, y: 50, p: 1 }, { x: 51, y: 50, p: 1 }]
    // Every dab of the stroke covers (50, 50): a 10 px hard radius, 1 px of travel.
    const dabs = planStroke(points, spec).length
    expect(dabs).toBeGreaterThan(2)
    for (const [opacity, buildUp, expected] of [
      [100, true, 1 - 0.8 ** dabs],
      [50, true, 0.5 * (1 - 0.8 ** dabs)],
      [100, false, 0.2],
    ]) {
      const surface = brushSurface({ brush: { ...spec, opacity }, color: '#000000', pageWidth: 200, pageHeight: 200, buildUp })
      for (const point of points) surface.add(point)
      expect(pixels(surface).alpha(50, 50)).toBe(Math.round(expected * 255))
    }
  })

  it('draws a clone as the page\'s tiles, moved by the offset, kept where the coverage is', () => {
    const surface = brushSurface({ brush: brush(), color: '#000000', pageWidth: 200, pageHeight: 400, buildUp: true })
    surface.add({ x: 150, y: 300, p: 1 })
    const calls = []
    const context = {
      globalAlpha: 1,
      globalCompositeOperation: 'source-over',
      clearRect: (...args) => calls.push(['clear', ...args]),
      drawImage: (image, x, y, w, h) => calls.push(['draw', image.name, x, y, w, h, context.globalCompositeOperation]),
      putImageData: () => {},
    }
    const mask = /** @type {any} */ ({ name: 'mask', style: {}, getContext: () => context })
    const node = /** @type {any} */ ({ width: 0, height: 0, style: {}, getContext: () => context })
    const tiles = [
      { image: { name: 'top' }, left: 0, top: 0, width: 1, height: 0.5 },
      { image: { name: 'bottom' }, left: 0, top: 0.5, width: 1, height: 0.5 },
    ]
    vi.stubGlobal('ImageData', class {})
    surface.paintClone(node, { mask, tiles, shift: { x: -40, y: 10 } })
    vi.unstubAllGlobals()
    // The dab touched the tile at (128, 256); the canvas is that tile.
    expect([node.width, node.height]).toEqual([128, 128])
    expect(node.style).toEqual({ left: '64%', top: '64%', width: '64%', height: '32%' })
    // Canvas pixel 0 is page pixel 128, whose source is 40 px left of it.
    expect(calls).toEqual([
      ['clear', 0, 0, 128, 128],
      ['draw', 'top', -128 + 40, -256 - 10, 200, 200, 'source-over'],
      ['draw', 'bottom', -128 + 40, 200 - 256 - 10, 200, 200, 'source-over'],
      ['draw', 'mask', 0, 0, undefined, undefined, 'destination-in'],
    ])
    expect(context.globalCompositeOperation).toBe('source-over')
  })
})
