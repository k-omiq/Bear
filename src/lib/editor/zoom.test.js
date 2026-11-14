import { describe, expect, it } from 'vitest'
import {
  FALLBACK_NATURAL_WIDTH,
  FALLBACK_RATIO,
  MIN_SHEET_WIDTH,
  FOCUS_MAX_AUTO,
  FOCUS_SMALL_TARGET,
  MAX_DRAWN_EXTENT,
  ZOOM_STOPS,
  anchoredScroll,
  centreAxis,
  clampZoom,
  fitScale,
  fitWidth,
  focusZoom,
  naturalWidth,
  pageRatio,
  placeAxis,
  sheetWidth,
  stepZoom,
  wheelZoom,
  zoomCeiling,
} from './zoom.js'

describe('naturalWidth', () => {
  it('is the page own pixels', () => {
    expect(naturalWidth({ width: 1600 })).toBe(1600)
  })

  it('falls back to the design file NAT', () => {
    expect(naturalWidth(null)).toBe(FALLBACK_NATURAL_WIDTH)
    expect(naturalWidth({})).toBe(FALLBACK_NATURAL_WIDTH)
    expect(naturalWidth({ width: 0 })).toBe(FALLBACK_NATURAL_WIDTH)
    expect(naturalWidth({ width: -5 })).toBe(FALLBACK_NATURAL_WIDTH)
  })
})

describe('pageRatio', () => {
  it('is height over width', () => {
    expect(pageRatio({ width: 1600, height: 2400 })).toBe(1.5)
    expect(pageRatio({ width: 800, height: 4000 })).toBe(5)
  })

  it('falls back to 2/3 when a dimension is missing or absurd', () => {
    expect(pageRatio(null)).toBe(FALLBACK_RATIO)
    expect(pageRatio({ width: 1600 })).toBe(FALLBACK_RATIO)
    expect(pageRatio({ width: 0, height: 100 })).toBe(FALLBACK_RATIO)
  })
})

describe('fitWidth', () => {
  it('fits both dimensions for a single page', () => {
    // 1232 wide, 764 tall, ratio 1.5 -> height is the binding constraint.
    expect(fitWidth({ contentWidth: 1232, contentHeight: 764, ratio: 1.5 })).toBeCloseTo(509.33, 1)
  })

  it('is width-bound when the viewport is tall', () => {
    expect(fitWidth({ contentWidth: 400, contentHeight: 4000, ratio: 1.5 })).toBe(400)
  })

  it('fits width only in longstrip', () => {
    // A 5:1 strip page fitted vertically would be 150px wide.
    expect(fitWidth({ contentWidth: 1232, contentHeight: 764, ratio: 5, longstrip: true })).toBe(1232)
  })

  it('never enlarges past the page own pixels', () => {
    const spec = { contentWidth: 1232, contentHeight: 764, ratio: 5, longstrip: true }
    expect(fitWidth({ ...spec, natural: 800 })).toBe(800)
    // The cap only ever shrinks: a page bigger than the viewport still fits.
    expect(fitWidth({ contentWidth: 1232, contentHeight: 764, ratio: 1.5, natural: 1600 })).toBeCloseTo(509.33, 1)
  })

  it('never goes below the sheet floor', () => {
    expect(fitWidth({ contentWidth: 0, contentHeight: 0, ratio: 1.5 })).toBe(MIN_SHEET_WIDTH)
    expect(fitWidth({ contentWidth: NaN, contentHeight: NaN })).toBe(MIN_SHEET_WIDTH)
  })
})

describe('fitScale', () => {
  it('reports the honest fraction, even below MIN_ZOOM', () => {
    const scale = fitScale({ contentWidth: 1232, contentHeight: 764, ratio: 1.5, natural: 1600 })
    expect(scale).toBe(0.32)
  })

  it('stops at 1:1 for a narrow page in a wide viewport', () => {
    const scale = fitScale({
      contentWidth: 1232,
      contentHeight: 764,
      ratio: 5,
      longstrip: true,
      natural: 800,
    })
    expect(scale).toBe(1)
  })

  it('survives a zero natural width', () => {
    expect(fitScale({ contentWidth: 620, contentHeight: 930, ratio: 1.5, natural: 0 })).toBe(1)
  })
})

describe('clampZoom', () => {
  it('clamps at both ends and rounds to two places', () => {
    expect(clampZoom(0.1, 0.4, 2.4)).toBe(0.4)
    expect(clampZoom(9, 0.4, 2.4)).toBe(2.4)
    expect(clampZoom(1 + 0.15 + 0.15, 0.4, 2.4)).toBe(1.3)
  })

  it('falls to the floor on garbage', () => {
    expect(clampZoom(NaN, 0.4, 2.4)).toBe(0.4)
    expect(clampZoom(undefined, 0.4, 2.4)).toBe(0.4)
  })
})

describe('sheetWidth', () => {
  it('is the natural width times the zoom, in whole pixels', () => {
    expect(sheetWidth({ natural: 1600, zoom: 1 })).toBe(1600)
    expect(sheetWidth({ natural: 1600, zoom: 0.32 })).toBe(512)
  })

  it('never draws a sliver', () => {
    expect(sheetWidth({ natural: 1600, zoom: 0.01 })).toBe(MIN_SHEET_WIDTH)
  })
})

describe('wheelZoom', () => {
  const range = { min: 0.4, max: 2.4 }

  it('zooms in on a negative delta and out on a positive one', () => {
    expect(wheelZoom({ deltaY: -40, zoom: 1, ...range })).toBeGreaterThan(1)
    expect(wheelZoom({ deltaY: 40, zoom: 1, ...range })).toBeLessThan(1)
  })

  it('is multiplicative, so a gesture feels the same at either end', () => {
    const low = wheelZoom({ deltaY: -20, zoom: 0.5, ...range }) / 0.5
    const high = wheelZoom({ deltaY: -20, zoom: 2, ...range }) / 2
    expect(low).toBeCloseTo(high, 1)
  })

  it('bounds one event', () => {
    expect(wheelZoom({ deltaY: -9999, zoom: 1, ...range })).toBe(1.25)
    expect(wheelZoom({ deltaY: 9999, zoom: 1, ...range })).toBe(0.75)
  })

  it('clamps at both ends', () => {
    expect(wheelZoom({ deltaY: -9999, zoom: 2.3, ...range })).toBe(2.4)
    expect(wheelZoom({ deltaY: 9999, zoom: 0.45, ...range })).toBe(0.4)
  })

  it('refuses to enlarge on a zoom-out below the floor', () => {
    // 0.32 is a fit scale, not a chosen zoom; clamping up to 0.4 would make
    // the page bigger on a pinch-out.
    expect(wheelZoom({ deltaY: 40, zoom: 0.32, ...range })).toBeNull()
    expect(wheelZoom({ deltaY: -40, zoom: 0.32, ...range })).toBeGreaterThan(0.32)
  })

  it('is null when nothing would change', () => {
    expect(wheelZoom({ deltaY: 0, zoom: 1, ...range })).toBeNull()
    expect(wheelZoom({ deltaY: -9999, zoom: 2.4, ...range })).toBeNull()
  })
})

describe('anchoredScroll', () => {
  const before = { left: 100, top: 50, width: 400, height: 600 }

  it('keeps the point under the pointer still', () => {
    // Pointer halfway across the sheet; the sheet doubles about its own left
    // edge, so the midpoint moves 200px right and the scroller follows.
    const after = { left: 100, top: 50, width: 800, height: 1200 }
    const next = anchoredScroll({
      pointerX: 300,
      pointerY: 350,
      before,
      after,
      scrollLeft: 0,
      scrollTop: 0,
    })
    expect(next.left).toBe(200)
    expect(next.top).toBe(300)
  })

  it('is a no-op when nothing scaled', () => {
    const next = anchoredScroll({
      pointerX: 300,
      pointerY: 350,
      before,
      after: before,
      scrollLeft: 40,
      scrollTop: 90,
    })
    expect(next).toEqual({ left: 40, top: 90 })
  })

  it('never asks for a negative scroll', () => {
    const after = { left: 100, top: 50, width: 200, height: 300 }
    const next = anchoredScroll({
      pointerX: 300,
      pointerY: 350,
      before,
      after,
      scrollLeft: 0,
      scrollTop: 0,
    })
    expect(next.left).toBe(0)
    expect(next.top).toBe(0)
  })

  it('survives a zero-width box', () => {
    const empty = { left: 0, top: 0, width: 0, height: 0 }
    const next = anchoredScroll({
      pointerX: 10,
      pointerY: 10,
      before: empty,
      after: empty,
      scrollLeft: 5,
      scrollTop: 5,
    })
    expect(next).toEqual({ left: 0, top: 0 })
  })
})

describe('stepZoom', () => {
  const range = { min: 0.4, max: 128 }

  it('climbs the ladder to 12800% and back', () => {
    const up = [0.4]
    while (up.at(-1) < 128) up.push(stepZoom({ ...range, zoom: up.at(-1), direction: 'in' }))
    expect(up).toEqual([...ZOOM_STOPS])
    const down = [128]
    while (down.at(-1) > 0.4) down.push(stepZoom({ ...range, zoom: down.at(-1), direction: 'out' }))
    expect(down).toEqual([...ZOOM_STOPS].reverse())
  })

  it('goes from between two stops to the nearer one in the direction asked', () => {
    expect(stepZoom({ ...range, zoom: 0.26, direction: 'in' })).toBe(0.4)
    expect(stepZoom({ ...range, zoom: 7.43, direction: 'in' })).toBe(8)
    expect(stepZoom({ ...range, zoom: 7.43, direction: 'out' })).toBe(6)
    // A zoom a rounding away from a stop is that stop.
    expect(stepZoom({ ...range, zoom: 0.999, direction: 'in' })).toBe(1.25)
  })

  it('stays inside the range, a lower ceiling included', () => {
    expect(stepZoom({ ...range, zoom: 128, direction: 'in' })).toBe(128)
    expect(stepZoom({ ...range, zoom: 0.4, direction: 'out' })).toBe(0.4)
    expect(stepZoom({ min: 0.4, max: 10, zoom: 8, direction: 'in' })).toBe(10)
  })
})

describe('zoomCeiling', () => {
  it('is the maximum for a page', () => {
    expect(zoomCeiling({ extent: 2400, max: 128 })).toBe(128)
  })

  it('keeps a long strip column inside what a browser lays out', () => {
    // Fifty 4000px segments: 200000px at 1:1.
    const ceiling = zoomCeiling({ extent: 200_000, max: 128 })
    expect(ceiling).toBe(80)
    expect(ceiling * 200_000).toBeLessThanOrEqual(MAX_DRAWN_EXTENT)
  })

  it('falls back to the maximum when there is nothing to measure', () => {
    expect(zoomCeiling({ extent: 0, max: 128 })).toBe(128)
    expect(zoomCeiling({ extent: Number.NaN, max: 128 })).toBe(128)
  })
})

describe('focusZoom', () => {
  const view = { contentWidth: 800, contentHeight: 600, min: 0.4, max: 128 }

  it('keeps the zoom when the region fits and can be seen', () => {
    expect(focusZoom({ ...view, zoom: 1, regionWidth: 200, regionHeight: 100 })).toBeNull()
  })

  it('zooms out until a region that does not fit takes most of the view', () => {
    const zoom = focusZoom({ ...view, zoom: 4, regionWidth: 400, regionHeight: 200 })
    expect(zoom).toBeCloseTo(1.4)
    expect(400 * zoom).toBeLessThanOrEqual(800)
  })

  it('zooms in on a region too small to see, and no further than it needs', () => {
    // 20px at 26% is 5px on screen.
    const zoom = focusZoom({ ...view, zoom: 0.26, regionWidth: 20, regionHeight: 10 })
    expect(zoom).toBe(FOCUS_MAX_AUTO)
    expect(focusZoom({ ...view, zoom: 0.26, regionWidth: 80, regionHeight: 60 })).toBe(FOCUS_SMALL_TARGET / 80)
  })

  it('never enlarges a page to fit a region that is already all on screen', () => {
    // A fitted page under the floor, and a region nearly the whole page.
    expect(focusZoom({ ...view, zoom: 0.26, regionWidth: 1600, regionHeight: 2300 })).toBeNull()
  })

  it('stays inside the range', () => {
    expect(focusZoom({ ...view, zoom: 1, regionWidth: 0, regionHeight: 0, max: 2 })).toBe(2)
    expect(focusZoom({ ...view, zoom: 2, regionWidth: 6000, regionHeight: 6000 })).toBe(0.4)
  })
})

describe('placeAxis', () => {
  const view = { viewStart: 70, viewSize: 600 }

  it('centres a page that fits', () => {
    // Page at 100..300, view centre at 370: the scroller moves back by 170.
    expect(placeAxis({ ...view, start: 100, size: 200, scroll: 500 })).toBe(330)
  })

  it('pulls a larger page back over the view, and no further', () => {
    // Its top edge is 30px into the view.
    expect(placeAxis({ ...view, start: 100, size: 2000, scroll: 500 })).toBe(530)
    // Its bottom edge is 100px short of the view's.
    expect(placeAxis({ ...view, start: -1430, size: 2000, scroll: 500 })).toBe(400)
    // It covers the view: nothing to do.
    expect(placeAxis({ ...view, start: -100, size: 2000, scroll: 500 })).toBe(500)
  })
})

describe('centreAxis', () => {
  it('moves the point to the middle of the view', () => {
    expect(centreAxis({ point: 900, viewStart: 70, viewSize: 600, scroll: 1000 })).toBe(1530)
    expect(centreAxis({ point: 0, viewStart: 70, viewSize: 600, scroll: 100 })).toBe(0)
  })
})
