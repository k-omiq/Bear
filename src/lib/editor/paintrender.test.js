import { describe, expect, it } from 'vitest'
import {
  MIN_PREVIEW_RADIUS,
  dabPath,
  dabStops,
  presentStroke,
  previewRadius,
  renderCloneTiles,
  renderDabs,
  rgba,
} from './paintrender.js'
import { planStroke } from './paintplan.js'

/**
 * The repo's vitest runs in `node` and there is no canvas anywhere in it, which
 * is exactly why every drawing routine takes its context as an argument. A
 * recording fake is then the whole test harness: what matters about a preview
 * frame is *which calls were made, in which order, with which numbers*, and
 * that is what a real 2D context would turn into pixels anyway.
 */
function fakeContext() {
  /** @type {Array<{call: string, args: unknown[]}>} */
  const calls = []
  const record = (call) => (...args) => calls.push({ call, args })
  return {
    calls,
    globalAlpha: 1,
    globalCompositeOperation: 'source-over',
    fillStyle: /** @type {any} */ (''),
    save: record('save'),
    restore: record('restore'),
    beginPath: record('beginPath'),
    arc: record('arc'),
    fill: record('fill'),
    clearRect: record('clearRect'),
    drawImage: record('drawImage'),
    createRadialGradient(...args) {
      calls.push({ call: 'createRadialGradient', args })
      /** @type {Array<[number, string]>} */
      const stops = []
      return { stops, addColorStop: (offset, color) => stops.push([offset, color]) }
    },
  }
}

/** @type {import('./paintplan.js').BrushSpec} */
const BRUSH = {
  size: 20,
  hardness: 50,
  flow: 100,
  opacity: 100,
  spacing: 25,
  pressureSize: false,
  pressureOpacity: false,
}

describe('dabStops', () => {
  it('mirrors the Rust falloff: solid core, then the two-piece shell', () => {
    expect(dabStops(0)).toEqual([[0, 1], [0, 1], [0.55, 0.55], [1, 0]])
    expect(dabStops(50)).toEqual([[0, 1], [0.5, 1], [0.775, 0.55], [1, 0]])
  })

  it('is a flat disc at full hardness, like `analytic_radial_coverage`', () => {
    expect(dabStops(100)).toEqual([[0, 1], [1, 1]])
  })
})

describe('rgba', () => {
  it('reads both hex lengths', () => {
    expect(rgba('#E63946', 1)).toBe('rgba(230, 57, 70, 1)')
    expect(rgba('#fff', 0.5)).toBe('rgba(255, 255, 255, 0.5)')
  })

  it('falls back to black rather than throwing inside a frame', () => {
    expect(rgba('rebeccapurple', 1)).toBe('rgba(0, 0, 0, 1)')
    expect(rgba(undefined, 2)).toBe('rgba(0, 0, 0, 1)')
  })
})

describe('previewRadius', () => {
  it('scales native pixels into canvas pixels', () => {
    expect(previewRadius({ radius: 10 }, 0.5)).toBe(5)
  })

  it('floors a sub-pixel dab so a zoomed-out stroke is visible at all', () => {
    expect(previewRadius({ radius: 10 }, 0.01)).toBe(MIN_PREVIEW_RADIUS)
  })
})

describe('renderDabs', () => {
  it('stamps one disc per dab, at the scaled position', () => {
    const ctx = fakeContext()
    const dabs = planStroke([{ x: 0, y: 0, p: 1 }, { x: 40, y: 0, p: 1 }], BRUSH)
    const drawn = renderDabs(ctx, dabs, { scale: 0.5, color: '#000000', hardness: 50 })

    expect(drawn).toBe(dabs.length)
    const arcs = ctx.calls.filter((entry) => entry.call === 'arc')
    expect(arcs).toHaveLength(dabs.length)
    expect(arcs[0].args.slice(0, 3)).toEqual([0, 0, 5])
    // spacing 25% of a 20px brush is 5 native px, so the second dab is at 2.5.
    expect(arcs[1].args.slice(0, 3)).toEqual([2.5, 0, 5])
    expect(ctx.calls.filter((entry) => entry.call === 'fill')).toHaveLength(dabs.length)
  })

  it('draws only what the last frame did not, from `from`', () => {
    const ctx = fakeContext()
    const dabs = planStroke([{ x: 0, y: 0, p: 1 }, { x: 40, y: 0, p: 1 }], BRUSH)
    renderDabs(ctx, dabs, { scale: 1, color: '#000000', hardness: 50, from: dabs.length - 2 })
    expect(ctx.calls.filter((entry) => entry.call === 'arc')).toHaveLength(2)
  })

  it('builds a gradient for a soft tip and a flat fill for a hard one', () => {
    const soft = fakeContext()
    renderDabs(soft, [{ x: 0, y: 0, radius: 4, alpha: 0.5 }], {
      scale: 1,
      color: '#ffffff',
      hardness: 0,
    })
    const gradient = soft.calls.find((entry) => entry.call === 'createRadialGradient')
    expect(gradient?.args).toEqual([0, 0, 0, 0, 0, 4])
    expect(soft.fillStyle.stops).toEqual([
      [0, 'rgba(255, 255, 255, 0.5)'],
      [0, 'rgba(255, 255, 255, 0.5)'],
      [0.55, 'rgba(255, 255, 255, 0.275)'],
      [1, 'rgba(255, 255, 255, 0)'],
    ])

    const hard = fakeContext()
    renderDabs(hard, [{ x: 0, y: 0, radius: 4, alpha: 0.5 }], {
      scale: 1,
      color: '#ffffff',
      hardness: 100,
    })
    expect(hard.calls.some((entry) => entry.call === 'createRadialGradient')).toBe(false)
    expect(hard.fillStyle).toBe('#ffffff')
    expect(hard.globalAlpha).toBe(0.5)
  })

  it('draws nothing at all rather than dividing by a zero scale', () => {
    const ctx = fakeContext()
    expect(renderDabs(ctx, [{ x: 0, y: 0, radius: 4, alpha: 1 }], { scale: 0 })).toBe(1)
    expect(ctx.calls).toHaveLength(0)
  })

  it('skips a dab with no alpha to lay down', () => {
    const ctx = fakeContext()
    renderDabs(ctx, [{ x: 0, y: 0, radius: 4, alpha: 0 }], { scale: 1, hardness: 100 })
    expect(ctx.calls).toHaveLength(0)
  })
})

describe('dabPath', () => {
  it('is the union of the discs, one sub-path each', () => {
    /** @type {Array<{call: string, args: unknown[]}>} */
    const calls = []
    class FakePath {
      moveTo(...args) {
        calls.push({ call: 'moveTo', args })
      }
      arc(...args) {
        calls.push({ call: 'arc', args })
      }
    }
    const path = dabPath(/** @type {any} */ (FakePath), [
      { x: 10, y: 20, radius: 4, alpha: 1 },
      { x: 30, y: 20, radius: 4, alpha: 1 },
    ], { scale: 0.5 })

    expect(path).toBeInstanceOf(FakePath)
    expect(calls.filter((entry) => entry.call === 'arc')).toHaveLength(2)
    expect(calls[0].args).toEqual([7, 10])
    expect(calls[1].args.slice(0, 3)).toEqual([5, 10, 2])
  })
})

describe('renderCloneTiles', () => {
  it('places each tile over its own share of the canvas, shifted by the offset', () => {
    const ctx = fakeContext()
    const image = { nodeName: 'IMG' }
    const drawn = renderCloneTiles(
      ctx,
      [
        { image, left: 0, top: 0, width: 1, height: 0.5 },
        { image, left: 0, top: 0.5, width: 1, height: 0.5 },
      ],
      { width: 400, height: 800, dx: -20, dy: -40 },
    )

    expect(drawn).toBe(2)
    expect(ctx.calls.map((entry) => entry.args)).toEqual([
      [image, -20, -40, 400, 400],
      [image, -20, 360, 400, 400],
    ])
  })

  it('draws each layer at its own opacity over the tiles, and leaves the context as it found it', () => {
    const ctx = fakeContext()
    /** @type {number[]} */
    const alphas = []
    ctx.drawImage = (...args) => {
      alphas.push(ctx.globalAlpha)
      ctx.calls.push({ call: 'drawImage', args })
    }
    const tile = { nodeName: 'IMG' }
    const layer = { nodeName: 'CANVAS' }
    renderCloneTiles(
      ctx,
      [
        { image: tile, left: 0, top: 0, width: 1, height: 1 },
        { image: layer, left: 0.25, top: 0.5, width: 0.1, height: 0.05, alpha: 0.4 },
      ],
      { width: 400, height: 800, dx: 0, dy: 0 },
    )
    expect(alphas).toEqual([1, 0.4])
    expect(ctx.calls.map((entry) => entry.args[0])).toEqual([tile, layer])
    expect(ctx.globalAlpha).toBe(1)
  })

  it('draws nothing where the page has no proxy tiles in the document', () => {
    const ctx = fakeContext()
    expect(renderCloneTiles(ctx, [], { width: 400, height: 800, dx: 0, dy: 0 })).toBe(0)
    expect(ctx.calls).toHaveLength(0)
  })
})

describe('presentStroke', () => {
  it('clears and blits the stroke canvas at the opacity ceiling', () => {
    const ctx = fakeContext()
    const stroke = { width: 400, height: 800 }
    presentStroke(ctx, stroke, { width: 400, height: 800, opacity: 40 })

    expect(ctx.calls[0]).toEqual({ call: 'clearRect', args: [0, 0, 400, 800] })
    expect(ctx.calls.some((entry) => entry.call === 'drawImage')).toBe(true)
    expect(ctx.globalAlpha).toBe(0.4)
  })

  it('clears and stops at zero opacity, so nothing lingers', () => {
    const ctx = fakeContext()
    presentStroke(ctx, { width: 4, height: 4 }, { width: 400, height: 800, opacity: 0 })
    expect(ctx.calls).toEqual([{ call: 'clearRect', args: [0, 0, 400, 800] }])
  })
})
