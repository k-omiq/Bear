import { describe, expect, it, vi } from 'vitest'
import { STRIP_RADIUS, WINDOW_RADIUS, evictRegions, slideWindow, windowIndices, windowPlan } from './pagewindow.svelte.js'
import { setBackend } from '../api/backend.js'

const page = (index, resident, count = 3) => ({
  index,
  resident,
  regions: resident ? Array.from({ length: count }, (_, n) => ({ id: `r${index}-${n}` })) : [],
  regionCount: count,
})

describe('the resident window', () => {
  it('is previous, current and next', () => {
    expect(windowIndices({ pageIndex: 5, pageCount: 20 })).toEqual([4, 5, 6])
    expect(WINDOW_RADIUS).toBe(1)
  })

  it('clamps at both ends rather than asking for pages that are not there', () => {
    expect(windowIndices({ pageIndex: 0, pageCount: 20 })).toEqual([0, 1])
    expect(windowIndices({ pageIndex: 19, pageCount: 20 })).toEqual([18, 19])
    expect(windowIndices({ pageIndex: 0, pageCount: 1 })).toEqual([0])
    expect(windowIndices({ pageIndex: 3, pageCount: 0 })).toEqual([])
  })

  it('takes the page index it is given, however far out of range', () => {
    expect(windowIndices({ pageIndex: 900, pageCount: 4 })).toEqual([2, 3])
    expect(windowIndices({ pageIndex: -7, pageCount: 4 })).toEqual([0, 1])
  })

  /*
   * The strip's scope *is* the window in a longstrip chapter: those are the
   * pages the Layers panel lists (`scopePageIndices`), so a window that did not
   * contain them would empty the panel at the moment it is being read.
   */
  it('takes in the strip scope in a longstrip chapter', () => {
    expect(
      windowIndices({ pageIndex: 10, pageCount: 40, longstrip: true, stripScope: [10, 16, 17] }),
    ).toEqual([5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17])
  })

  it('keeps the strip\'s warm range resident in a longstrip chapter, five pages a side', () => {
    expect(STRIP_RADIUS).toBe(5)
    expect(windowIndices({ pageIndex: 2, pageCount: 40, longstrip: true })).toEqual([0, 1, 2, 3, 4, 5, 6, 7])
  })

  it('ignores the strip scope in a paginated chapter', () => {
    expect(
      windowIndices({ pageIndex: 5, pageCount: 20, longstrip: false, stripScope: [1, 2, 3] }),
    ).toEqual([4, 5, 6])
  })

  it('drops scope positions that are not pages', () => {
    expect(
      windowIndices({ pageIndex: 1, pageCount: 3, longstrip: true, stripScope: [-1, 2, 99] }),
    ).toEqual([0, 1, 2])
  })
})

describe('the window plan', () => {
  it('loads what is wanted and not held, and evicts what is held and not wanted', () => {
    const pages = [page(0, true), page(1, true), page(2, false), page(3, false)]
    expect(windowPlan(pages, [2, 3])).toEqual({ load: [2, 3], evict: [0, 1] })
  })

  it('asks for nothing when the window has not moved', () => {
    const pages = [page(0, false), page(1, true), page(2, true)]
    expect(windowPlan(pages, [1, 2])).toEqual({ load: [], evict: [] })
  })

  it('evicts nothing on the first slide of a chapter that arrived as headers', () => {
    const pages = [page(0, false), page(1, false), page(2, false)]
    expect(windowPlan(pages, [0, 1])).toEqual({ load: [0, 1], evict: [] })
  })
})

describe('eviction', () => {
  it('keeps the count it is about to stop being able to derive', () => {
    const evicted = evictRegions(page(4, true, 7))
    expect(evicted.regions).toEqual([])
    expect(evicted.resident).toBe(false)
    expect(evicted.regionCount).toBe(7)
  })

  it('keeps the finished and flagged counts the Pages list draws', () => {
    // The row for an evicted page is still drawn, so its three numbers are
    // taken off the regions on the way out rather than lost with them.
    const evicted = evictRegions({
      index: 6,
      resident: true,
      regions: [
        { id: 'a', mask: { fittingReconstructed: false }, outcome: 'cleaned' },
        { id: 'b', mask: { fittingReconstructed: true }, outcome: 'cleaned' },
        { id: 'c', mask: null, outcome: 'declined' },
      ],
    })
    expect(evicted.regionCount).toBe(3)
    expect(evicted.doneCount).toBe(1)
    expect(evicted.reviewCount).toBe(2)
  })

  it('is idempotent - a header evicted again keeps its count', () => {
    const header = { index: 2, resident: false, regions: [], regionCount: 11 }
    expect(evictRegions(header).regionCount).toBe(11)
  })

  /*
   * The page object is mutated rather than replaced, deliberately: it is what
   * `$state` is tracking, and handing back a new object would leave the Pages
   * list pointing at the old one.
   */
  it('mutates the page in place', () => {
    const original = page(1, true)
    expect(evictRegions(original)).toBe(original)
  })
})

describe('slideWindow concurrency', () => {
  it('does not apply in-flight loaded pages if superseded by a zero-load slide', async () => {
    /** @type {(value: any) => void} */
    let resolveLoad
    const loadPromise = new Promise((res) => {
      resolveLoad = res
    })
    setBackend(
      /** @type {any} */ ({
        loadPages: vi.fn().mockReturnValue(loadPromise),
      }),
    )

    const applied = []
    const evicted = []
    const store = {
      chapterId: 'ch1',
      pages: [
        { index: 0, resident: true },
        { index: 1, resident: false },
      ],
      wanted: [1],
      applyPage: (p) => applied.push(p.index),
      evictPage: (i) => evicted.push(i),
    }

    // Call 1: needs to load page 1, evicts 0
    const slide1 = slideWindow(store)

    // Call 2: zero-load slide that evicts page 1 without loading anything
    store.wanted = []
    store.pages = [
      { index: 0, resident: false },
      { index: 1, resident: true },
    ]
    const slide2 = slideWindow(store)
    await slide2
    expect(evicted).toEqual([0, 1])

    // Call 1's backend load now resolves
    resolveLoad([{ index: 1, resident: true, regions: [] }])
    await slide1

    // Page 1 must not be re-applied after being evicted by call 2
    expect(applied).toEqual([])
    setBackend(null)
  })
})
