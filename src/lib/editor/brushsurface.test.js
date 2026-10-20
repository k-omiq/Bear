import { describe, expect, it, vi } from 'vitest'
import { brushSurface, coverageAt, proxyScale } from './brushsurface.js'
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

describe('proxyScale', () => {
  it('caps the short edge at 1024, like the tiles', () => {
    expect(proxyScale(800, 1200)).toBe(1)
    expect(proxyScale(2048, 3000)).toBe(0.5)
    expect(proxyScale(4096, 1024)).toBe(1)
  })
})

describe('brushSurface', () => {
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
