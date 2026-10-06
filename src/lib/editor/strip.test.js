import { describe, expect, it } from 'vitest'
import {
  OVERSCAN,
  STRIP_GAP,
  VIRTUAL_THRESHOLD,
  centreIndex,
  scrollTopFor,
  stripMetrics,
  stripUnit,
  stripWindow,
  visibleIndices,
} from './strip.js'

/** A strip page 800px wide at ratio 5, i.e. 4000px tall, plus the gap. */
const UNIT = stripUnit({ pageHeight: 4000 })

/** @param {{width: number, height: number}[]} pages */
const metricsOf = (pages, scale = 1) => stripMetrics({ pages, scale, gap: STRIP_GAP })

/** `count` pages all 800×4000 - the shape the fixtures happen to have. */
const uniform = (count) =>
  metricsOf(Array.from({ length: count }, () => ({ width: 800, height: 4000 })))

/**
 * A webtoon chapter's segments, which are **not** uniform by construction:
 * the strip splits at content minima inside a ±500px band, so no two
 * segments are the same height. Heights 4000, 2600 and 3400 give units of
 * 4012, 2612 and 3412 and a column 10036px tall.
 *
 * Sized from page 0 alone the same column is 3 × 4012 = 12036px, page 2 starts
 * at 8024 instead of 6624, and every spacer, every position readout and every
 * `goToPage` is wrong by 1400px with nothing on screen to say so.
 */
const SEGMENTS = [
  { width: 800, height: 4000 },
  { width: 800, height: 2600 },
  { width: 800, height: 3400 },
]

describe('stripUnit', () => {
  it('is the page plus the gap under it', () => {
    expect(UNIT).toBe(4000 + STRIP_GAP)
  })

  it('clamps nonsense to something drawable', () => {
    expect(stripUnit({ pageHeight: 0 })).toBe(1 + STRIP_GAP)
    expect(stripUnit({ pageHeight: NaN, gap: NaN })).toBe(1 + STRIP_GAP)
  })
})

describe('stripMetrics', () => {
  it('gives every page its own height, so the column is the sum and not a multiple', () => {
    const metrics = metricsOf(SEGMENTS)
    expect(metrics.units).toEqual([4000, 2600, 3400])
    expect(metrics.offsets).toEqual([0, 4000, 6600, 10000])
    expect(metrics.total).toBe(10000)
    // What page 0's aspect ratio alone would have said.
    expect(metrics.total).not.toBe(SEGMENTS.length * 4000)
  })

  it('draws a narrow page narrow rather than stretching it to the column', () => {
    // A narrow page sits in a gutter. Drawing it at the wide
    // page's width magnifies it, and every ring, mask and box on it lands in
    // the wrong place.
    const metrics = metricsOf([{ width: 800, height: 4000 }, { width: 700, height: 3500 }])
    expect(metrics.widths).toEqual([800, 700])
    expect(metrics.units).toEqual([4000, 3500])
  })

  it('scales every page by the same factor', () => {
    const metrics = metricsOf(SEGMENTS, 0.5)
    expect(metrics.widths).toEqual([400, 400, 400])
    expect(metrics.units).toEqual([2000 + STRIP_GAP, 1300 + STRIP_GAP, 1700 + STRIP_GAP])
  })

  it('survives a page that declares no size, and a chapter with no pages', () => {
    const metrics = metricsOf([{}, { width: 0, height: 0 }])
    expect(metrics.count).toBe(2)
    expect(metrics.offsets[2]).toBe(metrics.total)
    expect(metrics.total).toBeGreaterThan(0)

    const empty = metricsOf([])
    expect(empty).toMatchObject({ count: 0, total: 0, offsets: [0] })
  })
})

describe('stripWindow', () => {
  it('renders a short chapter whole', () => {
    const band = stripWindow({ metrics: uniform(6), columnTop: 0, viewportHeight: 764 })
    expect(band).toEqual({ virtual: false, start: 0, end: 6, padTop: 0, padBottom: 0, extra: null })
  })

  it('windows a long one', () => {
    const metrics = uniform(VIRTUAL_THRESHOLD + 40)
    const band = stripWindow({ metrics, columnTop: -UNIT * 10, viewportHeight: 764 })
    expect(band.virtual).toBe(true)
    expect(band.start).toBe(10 - OVERSCAN)
    expect(band.end).toBe(11 + OVERSCAN)
  })

  // **What this proves, and what it does not.** The identity is still algebra:
  // `padTop` is `offsets[start]` and `padBottom` is `total - offsets[end]`, so
  // it holds for any offsets table at all. What it now also checks is that the
  // band's own pages add up to the gap between the two spacers, which is the
  // thing a single `unit` got wrong on a non-uniform chapter. It still says
  // nothing about the half only a browser can see: that the *rendered* band
  // occupies that many pixels, which depends on `CanvasStage.svelte` giving
  // each slot `margin-bottom: var(--strip-gap)` and giving `.stage` no CSS
  // `gap`, and on the width the sheet is drawn at being `metrics.widths[i]`.
  it('accounts for the whole column at every scroll position', () => {
    // Segment heights that repeat with a period the window's arithmetic does
    // not share, so no two pages are the same and the band moves through them
    // unevenly.
    const pages = Array.from({ length: 60 }, (_, i) => ({
      width: 800,
      height: 2400 + ((i * 317) % 1600),
    }))
    const metrics = metricsOf(pages)
    for (let step = 0; step <= 60; step += 1) {
      const band = stripWindow({
        metrics,
        columnTop: -(metrics.total / 60) * step * 0.97,
        viewportHeight: 900,
      })
      const rendered = metrics.units.slice(band.start, band.end).reduce((a, b) => a + b, 0)
      expect(band.padTop + rendered + band.padBottom).toBe(metrics.total)
    }
  })

  it('clamps past both ends', () => {
    const metrics = uniform(60)
    const above = stripWindow({ metrics, columnTop: 5000, viewportHeight: 900 })
    expect(above.start).toBe(0)
    expect(above.padTop).toBe(0)

    const below = stripWindow({ metrics, columnTop: -UNIT * 500, viewportHeight: 900 })
    expect(below.end).toBe(60)
    expect(below.padBottom).toBe(0)
  })

  it('mounts the pages within the preload margin before they scroll in', () => {
    const metrics = uniform(60)
    const spec = { metrics, columnTop: -UNIT * 10 - 100, viewportHeight: 764, overscan: 0 }
    const near = stripWindow(spec)
    expect([near.start, near.end]).toEqual([10, 11])
    const ahead = stripWindow({ ...spec, margin: UNIT })
    expect([ahead.start, ahead.end]).toEqual([9, 12])
  })

  it('keeps the included page mounted', () => {
    const band = stripWindow({
      metrics: uniform(60),
      columnTop: -UNIT * 40,
      viewportHeight: 900,
      include: 3,
    })
    expect(band.extra).toBe(3)
    expect(band.start).toBe(39)
    expect(band.end).toBe(42)
  })

  it('ignores an include outside the chapter', () => {
    const band = stripWindow({
      metrics: uniform(60),
      columnTop: 0,
      viewportHeight: 900,
      include: 900,
    })
    expect(band.end).toBeLessThan(60)
  })

  it('always renders at least one page', () => {
    const band = stripWindow({ metrics: uniform(60), columnTop: NaN, viewportHeight: 0 })
    expect(band.end).toBeGreaterThan(band.start)
  })

  it('has nothing to render for an empty chapter', () => {
    expect(stripWindow({ metrics: metricsOf([]) })).toEqual({
      virtual: false,
      start: 0,
      end: 0,
      padTop: 0,
      padBottom: 0,
      extra: null,
    })
    expect(stripWindow({})).toEqual({
      virtual: false,
      start: 0,
      end: 0,
      padTop: 0,
      padBottom: 0,
      extra: null,
    })
  })

  /**
   * The defect at the scale it actually shows: page 0 is
   * the tallest segment, so a column sized from it puts every later page too
   * far down and the spacer above the band is too tall by the difference.
   */
  it('spaces a non-uniform chapter by the pages that are actually above it', () => {
    const pages = [...SEGMENTS, ...SEGMENTS, ...SEGMENTS, ...SEGMENTS, ...SEGMENTS]
    const metrics = metricsOf(pages)
    const band = stripWindow({ metrics, columnTop: -20000, viewportHeight: 900 })
    expect(band.padTop).toBe(metrics.offsets[band.start])
    expect(band.padTop + band.padBottom).toBeLessThan(metrics.total)
    // The scroll position is exactly page 6's top; overscan keeps page 5.
    expect(band.start).toBe(6 - OVERSCAN)
  })
})

describe('visibleIndices', () => {
  it('is one page when the viewport is inside one', () => {
    expect(visibleIndices({ metrics: uniform(6), columnTop: -UNIT, viewportHeight: 764 })).toEqual([
      1,
    ])
  })

  it('is every page the viewport overlaps', () => {
    const metrics = metricsOf(Array.from({ length: 6 }, () => ({ width: 300, height: 288 })))
    expect(metrics.units[0]).toBe(288)
    expect(visibleIndices({ metrics, columnTop: -250, viewportHeight: 700 })).toEqual([0, 1, 2, 3])
  })

  it('overlaps a page it merely touches the top of, and not one it stops at', () => {
    const metrics = metricsOf(Array.from({ length: 6 }, () => ({ width: 300, height: 288 })))
    // A viewport ending exactly on page 2's first row is inside pages 0 and 1.
    expect(visibleIndices({ metrics, columnTop: 0, viewportHeight: 576 })).toEqual([0, 1])
    expect(visibleIndices({ metrics, columnTop: 0, viewportHeight: 577 })).toEqual([0, 1, 2])
  })

  it('clamps at the end of the column', () => {
    const metrics = metricsOf(Array.from({ length: 3 }, () => ({ width: 300, height: 288 })))
    expect(visibleIndices({ metrics, columnTop: -800, viewportHeight: 700 })).toEqual([2])
  })

  it('is empty only for an empty chapter', () => {
    expect(visibleIndices({ metrics: metricsOf([]) })).toEqual([])
    expect(visibleIndices({})).toEqual([])
    expect(visibleIndices({ metrics: uniform(4), columnTop: 0, viewportHeight: 0 })).toEqual([0])
  })

  it('scopes to the segments the reader can actually see, not to page 0 multiples', () => {
    const metrics = metricsOf(SEGMENTS)
    // 6000..6900 spans the join between segment 1 (ends at 6624) and 2.
    expect(visibleIndices({ metrics, columnTop: -6000, viewportHeight: 900 })).toEqual([1, 2])
  })
})

describe('centreIndex', () => {
  it('follows the viewport centre, not its top', () => {
    // Page 0 spans 0..4012 and page 1 starts there; a viewport 900 tall whose
    // top is at 3800 has its centre at 4250, inside page 1.
    expect(centreIndex({ metrics: uniform(6), columnTop: -3800, viewportHeight: 900 })).toBe(1)
    expect(centreIndex({ metrics: uniform(6), columnTop: -3400, viewportHeight: 900 })).toBe(0)
  })

  it('reads the position off the pages that are there', () => {
    const metrics = metricsOf(SEGMENTS)
    // Centre at 7000: past segment 2's top at 6624, so the pill says 3 of 3.
    // Page 0's unit alone would answer 1, because 7000 / 4012 floors to 1.
    expect(centreIndex({ metrics, columnTop: -6550, viewportHeight: 900 })).toBe(2)
  })

  it('clamps to the chapter', () => {
    const metrics = metricsOf(Array.from({ length: 3 }, () => ({ width: 300, height: 288 })))
    expect(centreIndex({ metrics, columnTop: 900, viewportHeight: 700 })).toBe(0)
    expect(centreIndex({ metrics, columnTop: -9000, viewportHeight: 700 })).toBe(2)
  })

  it('is 0 for an empty chapter', () => {
    expect(centreIndex({ metrics: metricsOf([]) })).toBe(0)
    expect(centreIndex({})).toBe(0)
  })
})

describe('scrollTopFor', () => {
  it('puts the asked-for page at the top of the viewport', () => {
    // The column starts 70px below the viewport top and nothing is scrolled.
    const metrics = uniform(6)
    expect(scrollTopFor({ index: 0, metrics, scrollTop: 0, columnTop: 70 })).toBe(70)
    expect(scrollTopFor({ index: 2, metrics, scrollTop: 0, columnTop: 70 })).toBe(70 + 2 * UNIT)
  })

  it('is the same answer wherever the scroller already is', () => {
    const metrics = uniform(6)
    const scrolled = scrollTopFor({ index: 2, metrics, scrollTop: 5000, columnTop: 70 - 5000 })
    expect(scrolled).toBe(70 + 2 * UNIT)
  })

  it('never asks for a negative scroll', () => {
    expect(scrollTopFor({ index: 0, metrics: uniform(6), scrollTop: 0, columnTop: -400 })).toBe(0)
  })

  /**
   * `goToPage` is the loudest of the three: the Pages list, the bottom pill and
   * `stepReview` all scroll through it, so a column sized from page 0 lands the
   * reader 1400px past the segment they asked for and the strip looks like it
   * ignored the click.
   */
  it('lands on the top of the page asked for, not on a multiple of page 0', () => {
    const metrics = metricsOf(SEGMENTS)
    expect(scrollTopFor({ index: 1, metrics, scrollTop: 0, columnTop: 70 })).toBe(70 + 4000)
    expect(scrollTopFor({ index: 2, metrics, scrollTop: 0, columnTop: 70 })).toBe(70 + 6600)
    // The end of the column is a position too: `index === count` is the bottom.
    expect(scrollTopFor({ index: 3, metrics, scrollTop: 0, columnTop: 70 })).toBe(70 + 10000)
    expect(scrollTopFor({ index: 99, metrics, scrollTop: 0, columnTop: 70 })).toBe(70 + 10000)
  })
})
