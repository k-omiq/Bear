import { describe, it, expect } from 'vitest'
import {
  defaultGeometry,
  clampPosition,
  clampSize,
  raisedOrder,
  minWidthFor,
  contentSized,
  WINDOW_IDS,
  CONTENT_SIZED,
  DEFAULT_MIN_WIDTH,
  MAX_WIDTH,
  MIN_HEIGHT,
  KEEP_ON_SCREEN,
  MIN_TOP,
  BOTTOM_INSET,
  HEIGHT_INSET,
} from './windows.js'

describe('defaultGeometry', () => {
  it('lays the windows out from the viewport', () => {
    const g = defaultGeometry(1440, 900)
    expect(Object.keys(g).sort()).toEqual([...WINDOW_IDS].sort())
    expect(g.pages).toEqual({ x: 16, y: 62, w: 248, h: 288 })
    // Layers hangs 12px below Pages.
    expect(g.layers.y).toBe(74 + g.pages.h)
    expect(g.layers.h).toBe(306)
    // The bar is centred across the top: half the viewport, less half of the
    // 560 its `w` guesses at until it has measured itself.
    expect(g.tool).toEqual({ x: 440, y: 62, w: 560, h: null })
  })

  it('holds a floor under both left-hand heights on a short viewport', () => {
    const g = defaultGeometry(1440, 400)
    expect(g.pages.h).toBe(200)
    expect(g.layers.h).toBe(180)
  })

  it('never pushes the tool bar off the left of a narrow viewport', () => {
    // Centring a 560px bar in a 600px viewport would put it at 20; on anything
    // narrower than 592 the arithmetic goes negative and the margin holds.
    expect(defaultGeometry(600, 900).tool.x).toBe(20)
    expect(defaultGeometry(400, 900).tool.x).toBe(16)
  })
})

describe('clampPosition', () => {
  const box = { x: 0, y: 0, w: 248 }

  it('leaves a position inside the viewport alone', () => {
    expect(clampPosition({ ...box, x: 400, y: 300 }, 1440, 900)).toEqual({ x: 400, y: 300 })
  })

  it('lets a window hang off the left, keeping a grabbable strip', () => {
    expect(clampPosition({ ...box, x: -9999, y: 300 }, 1440, 900).x).toBe(-(248 - KEEP_ON_SCREEN))
  })

  it('lets a window hang off the right, keeping a grabbable strip', () => {
    expect(clampPosition({ ...box, x: 9999, y: 300 }, 1440, 900).x).toBe(1440 - KEEP_ON_SCREEN)
  })

  it('keeps the header clear of the top and inside the bottom', () => {
    expect(clampPosition({ ...box, y: -50 }, 1440, 900).y).toBe(MIN_TOP)
    expect(clampPosition({ ...box, y: 9999 }, 1440, 900).y).toBe(900 - BOTTOM_INSET)
  })

  it('rounds to whole pixels', () => {
    expect(clampPosition({ ...box, x: 100.6, y: 200.4 }, 1440, 900)).toEqual({ x: 101, y: 200 })
  })

  it('keeps a window wider than the viewport reachable at both ends', () => {
    const wide = { x: 0, y: 300, w: 560 }
    expect(clampPosition({ ...wide, x: -9999 }, 400, 900).x).toBe(-(560 - KEEP_ON_SCREEN))
    expect(clampPosition({ ...wide, x: 9999 }, 400, 900).x).toBe(400 - KEEP_ON_SCREEN)
  })

  // A content-sized window has no width floor, so it can reach this with a
  // `w` of 0 - the value a bar carries until it has measured itself once. The
  // left bound is `-(w - KEEP_ON_SCREEN)`, which inverts below the strip and
  // would push the bar rightwards off its own x instead of leaving it there.
  // The tool bar's width is whatever the selected tool made of it, and it can
  // grow after it was placed - Auto clean puts a run status beside its button
  // while a run is on. A bar centred at 560 that grows to 700 must be pushed
  // back onto the screen rather than left with its run button off the edge;
  // when even the viewport is too narrow, the grip end stays visible.
  it('holds a content-sized window entirely on screen while it fits', () => {
    expect(clampPosition({ x: 500, y: 62, w: 700 }, 1000, 900, 'tool').x).toBe(300)
    expect(clampPosition({ x: -40, y: 62, w: 700 }, 1000, 900, 'tool').x).toBe(0)
    expect(clampPosition({ x: 150, y: 62, w: 700 }, 1000, 900, 'tool').x).toBe(150)
    // Wider than the viewport: the left end, and the grip on it, stays put.
    expect(clampPosition({ x: 100, y: 62, w: 1200 }, 1000, 900, 'tool').x).toBe(0)
    expect(clampPosition({ x: -500, y: 62, w: 1200 }, 1000, 900, 'tool').x).toBe(-200)
    // A resizable window keeps the general rule under the same numbers.
    expect(clampPosition({ x: 500, y: 62, w: 700 }, 1000, 900, 'pages').x).toBe(500)
  })

  // The Text cleanup panel is a few hundred pixels tall, so the bottom bound
  // a header needs is not enough for it: a shell dragged, or grown, near the
  // bottom edge would leave its run button below the window. Held whole above
  // the edge while it fits; taller than the viewport, its top stays reachable.
  it('holds a measured content-sized height above the bottom edge', () => {
    // The minimum supported window, `src-tauri/tauri.conf.json`.
    expect(clampPosition({ x: 16, y: 600, w: 384, h: 480 }, 1000, 640, 'tool').y).toBe(640 - 480 - 62)
    expect(clampPosition({ x: 16, y: 62, w: 384, h: 480 }, 1000, 640, 'tool').y).toBe(62)
    expect(clampPosition({ x: 16, y: -30, w: 384, h: 480 }, 1000, 640, 'tool').y).toBe(MIN_TOP)
    // The pill is held whole too, not merely by its header.
    expect(clampPosition({ x: 16, y: 9999, w: 300, h: 44 }, 1000, 640, 'tool').y).toBe(640 - 44 - MIN_TOP)
    // Taller than the viewport: the top, and the grip on it, stays on screen.
    expect(clampPosition({ x: 16, y: 300, w: 384, h: 900 }, 1000, 640, 'tool').y).toBe(MIN_TOP)
    // Not measured yet: the general bounds.
    expect(clampPosition({ x: 16, y: 9999, w: 384, h: null }, 1000, 640, 'tool').y).toBe(640 - BOTTOM_INSET)
    expect(clampPosition({ x: 16, y: 9999, w: 384 }, 1000, 640, 'tool').y).toBe(640 - BOTTOM_INSET)
    // A resizable window's height is its own business and moves nothing.
    expect(clampPosition({ x: 16, y: 500, w: 248, h: 400 }, 1000, 640, 'pages').y).toBe(500)
  })

  it('does not shove a window narrower than the grabbable strip', () => {
    for (const w of [0, 1, KEEP_ON_SCREEN - 1]) {
      expect(clampPosition({ x: 200, y: 300, w }, 1440, 900).x, `w ${w}`).toBe(200)
      expect(clampPosition({ x: -9999, y: 300, w }, 1440, 900).x, `w ${w}`).toBe(0)
    }
    // At the strip exactly, and above it, the rule is the one it always was.
    expect(clampPosition({ x: -9999, y: 300, w: KEEP_ON_SCREEN }, 1440, 900).x).toBe(0)
    expect(clampPosition({ x: -9999, y: 300, w: 200 }, 1440, 900).x).toBe(-(200 - KEEP_ON_SCREEN))
  })
})

describe('contentSized', () => {
  it('is the tool bar and nothing else', () => {
    expect([...CONTENT_SIZED]).toEqual(['tool'])
    expect(contentSized('tool')).toBe(true)
    for (const id of WINDOW_IDS.filter((other) => other !== 'tool')) {
      expect(contentSized(id), id).toBe(false)
    }
  })

  it('is false for a window it has never heard of, and for none at all', () => {
    expect(contentSized('inspector')).toBe(false)
    expect(contentSized(undefined)).toBe(false)
  })
})

describe('minWidthFor', () => {
  it('holds no floor at all under a content-sized window', () => {
    expect(minWidthFor('tool')).toBe(0)
  })

  it('gives every other window the general floor', () => {
    expect(minWidthFor('pages')).toBe(DEFAULT_MIN_WIDTH)
    expect(minWidthFor('layers')).toBe(DEFAULT_MIN_WIDTH)
    expect(minWidthFor('inspector')).toBe(DEFAULT_MIN_WIDTH)
    expect(minWidthFor(undefined)).toBe(DEFAULT_MIN_WIDTH)
  })
})

describe('clampSize', () => {
  it('holds width between its bounds', () => {
    expect(clampSize({ w: 10, h: 300 }, 900).w).toBe(DEFAULT_MIN_WIDTH)
    expect(clampSize({ w: 9999, h: 300 }, 900).w).toBe(MAX_WIDTH)
    expect(clampSize({ w: 320, h: 300 }, 900).w).toBe(320)
  })

  // The tool bar's width is a measurement rather than a preference: it is
  // `width: max-content` and writes back whatever the browser made of it, so
  // both the floor and the 560 ceiling would only put the store at odds with
  // the element.
  it('passes a content-sized width through, floor and ceiling alike', () => {
    expect(clampSize({ w: 132, h: null }, 900, 'tool').w).toBe(132)
    expect(clampSize({ w: 980, h: null }, 900, 'tool').w).toBe(980)
    expect(clampSize({ w: 132.4, h: null }, 900, 'tool').w).toBe(132)
    // …and a negative one is still not a width.
    expect(clampSize({ w: -40, h: null }, 900, 'tool').w).toBe(0)
    // Every other window keeps both bounds.
    expect(clampSize({ w: 132, h: 300 }, 900, 'pages').w).toBe(DEFAULT_MIN_WIDTH)
    expect(clampSize({ w: 980, h: 300 }, 900, 'pages').w).toBe(MAX_WIDTH)
  })

  it('holds height between its floor and the viewport', () => {
    expect(clampSize({ w: 248, h: 10 }, 900).h).toBe(MIN_HEIGHT)
    expect(clampSize({ w: 248, h: 9999 }, 900).h).toBe(900 - HEIGHT_INSET)
  })

  it('leaves a content-sized window content-sized', () => {
    expect(clampSize({ w: 306, h: null }, 900).h).toBeNull()
  })

  // The tool shell is 44px as a pill and as tall as its rows as the Text
  // cleanup panel: a measurement, like its width, kept so the position clamp
  // can hold the whole of it on screen. Not clamped, since squeezing it would
  // only put the store at odds with the element; not a number means "not
  // measured yet".
  it('passes a content-sized height through as the measurement it is', () => {
    expect(clampSize({ w: 384, h: 420 }, 900, 'tool').h).toBe(420)
    expect(clampSize({ w: 306, h: 44.4 }, 900, 'tool').h).toBe(44)
    expect(clampSize({ w: 384, h: 2000 }, 640, 'tool').h).toBe(2000)
    expect(clampSize({ w: 306, h: null }, 900, 'tool').h).toBeNull()
    expect(clampSize({ w: 306, h: 0 }, 900, 'tool').h).toBeNull()
    expect(clampSize({ w: 306, h: -5 }, 900, 'tool').h).toBeNull()
    // Every other window still keeps the number it was given, clamped.
    expect(clampSize({ w: 306, h: 420 }, 900, 'pages').h).toBe(420)
    expect(clampSize({ w: 306, h: 10 }, 900, 'pages').h).toBe(MIN_HEIGHT)
  })
})

describe('raisedOrder', () => {
  it('puts the named window on top and closes the gaps', () => {
    expect(raisedOrder({ pages: 1, layers: 2, tool: 3 }, 'pages')).toEqual({
      layers: 1,
      tool: 2,
      pages: 3,
    })
  })

  it('keeps the relative order of everything else', () => {
    // Sparse input ranks collapse to 1..n, and pages still sits under tool.
    expect(raisedOrder({ pages: 5, layers: 2, tool: 9 }, 'layers')).toEqual({
      pages: 1,
      tool: 2,
      layers: 3,
    })
  })

  it('is idempotent on the window already on top', () => {
    const ranks = { pages: 1, layers: 2, tool: 3 }
    expect(raisedOrder(ranks, 'tool')).toEqual(ranks)
  })

  it('never lets a rank climb out of the 1..n band', () => {
    let ranks = { pages: 1, layers: 2, tool: 3 }
    for (let i = 0; i < 50; i += 1) ranks = raisedOrder(ranks, WINDOW_IDS[i % 3])
    expect(Object.values(ranks).sort()).toEqual([1, 2, 3])
  })

  it('ignores an unknown id', () => {
    const ranks = { pages: 1, layers: 2, tool: 3 }
    expect(raisedOrder(ranks, 'nope')).toEqual(ranks)
  })
})
