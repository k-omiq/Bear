import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { tick } from 'svelte'
import { setBackend } from '../api/backend.js'
import { tileUrl } from '../api/tile.js'
import { editor, focusRegion, goToPage, reloadPage, replaceRegion, syncPageWindow, zoomActual, zoomFit, zoomIn } from '../state/editor.svelte.js'
import { beginDraft, resetDraftState } from './draft.svelte.js'
import CanvasStage from './CanvasStage.svelte'
import { showOnPage } from './maskactions.svelte.js'
import { STRIP_GAP, stripMetrics } from './strip.js'

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  setBackend(null)
  resetDraftState()
  editor.project = null
  editor.chapter = null
  editor.tool = 'autoClean'
  editor.fit = true
  editor.originalVisible = false
  editor.selectionId = null
  editor.hoverId = null
  editor.stripScope = []
  editor.stripFocus = -1
  editor.stripScrollRequest = null
  editor.viewRequest = null
  editor.zoomCeiling = 128
})

/** The scroll room of the fixture's 600px viewport. */
const ROOM = 300
/** And at the sides of its 1200px width. */
const SIDE = 600

function scrollFixture() {
  const pages = Array.from({ length: 60 }, (_, index) => ({
    id: `scroll-${index}`, index, number: index + 1,
    width: index === 0 ? 1000 : 800, height: 1000,
    status: 'unclean', regions: [], resident: index < 2,
  }))
  const loadPages = vi.fn(async ({ indices }) => indices.map((index) => ({
    ...pages[index], resident: true,
  })))
  setBackend(/** @type {any} */ ({ loadPages }))
  editor.project = /** @type {any} */ ({ id: 'scroll-project', mode: 'longstrip' })
  editor.chapter = /** @type {any} */ ({ id: 'scroll-chapter', pages, review: [] })
  editor.pageIndex = 0
  editor.loading = false
  editor.fit = false
  editor.zoom = 1
  const box = document.createElement('div')
  document.body.append(box)
  Object.defineProperties(box, {
    clientWidth: { configurable: true, value: 1200 },
    clientHeight: { configurable: true, value: 600 },
  })
  // The stage sits under the scroller's 70px of padding and its own scroll
  // room, half the 600px viewport: at rest the scroller is at `ROOM`.
  const rect = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function () {
    return /** @type {DOMRect} */ ({
      top: this.classList.contains('stage') ? 70 + (Number.parseFloat(this.style.marginTop) || 0) - box.scrollTop : 0,
      left: 0, width: 1000, height: 60000,
    })
  })
  const view = render(CanvasStage, { target: box })
  return { box, view, rect, loadPages }
}

describe('longstrip scrolling', () => {
  it('does not measure layout on ordinary scroll or resize the stage to the mounted band', async () => {
    const { box, view, rect } = scrollFixture()
    await tick()
    rect.mockClear()
    box.scrollTop = ROOM + 40170
    box.dispatchEvent(new Event('scroll'))
    await tick()
    expect(rect).not.toHaveBeenCalled()
    expect(editor.pageIndex).toBe(40)
    expect(view.container.querySelector('.stage').style.width).toBe('1000px')
    expect(view.container.querySelector('.stage').style.height).toBe('60000px')
  })

  it('mounts only the destination band on a large jump and loads it once', async () => {
    const { box, view, loadPages } = scrollFixture()
    await tick()
    loadPages.mockClear()
    const mounted = []
    const observer = new MutationObserver((records) => {
      for (const record of records) for (const node of record.addedNodes) {
        if (node instanceof HTMLElement && node.matches('.slot')) mounted.push(node)
      }
    })
    observer.observe(view.container.querySelector('.stage'), { childList: true })
    box.scrollTop = ROOM + 40170
    box.dispatchEvent(new Event('scroll'))
    await tick()
    await tick()
    observer.disconnect()
    // Page 40 is in view and the warm range keeps five pages either side of
    // it; the viewport band (38 to 42) is inside that.
    expect(mounted.length).toBeLessThanOrEqual(11)
    expect([...view.container.querySelectorAll('.slot')].map((slot) => slot.getAttribute('data-strip-index')))
      .toEqual(['35', '36', '37', '38', '39', '40', '41', '42', '43', '44', '45'])
    expect(loadPages).toHaveBeenCalledTimes(1)
    // The mounted band and a neighbour either side, so every mounted sheet
    // has its patch layers before it scrolls into view.
    expect(loadPages).toHaveBeenCalledWith({
      chapterId: 'scroll-chapter',
      indices: [34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46],
    })
  })

  it('retains a focused sheet and its resident data without widening the band', async () => {
    const { box, view } = scrollFixture()
    const button = view.container.querySelector('.sheet button')
    button.focus()
    await tick()
    box.scrollTop = ROOM + 40170
    box.dispatchEvent(new Event('scroll'))
    await tick()
    expect(document.activeElement).toBe(button)
    expect(view.container.querySelectorAll('.slot')).toHaveLength(12)
    expect(editor.chapter.pages[0].resident).toBe(true)
    expect(editor.stripScope).toEqual([40])
    button.blur()
    await tick()
    expect(view.container.querySelectorAll('.slot')).toHaveLength(11)
    expect(editor.chapter.pages[0].resident).toBe(false)
  })

  it('uses the cached origin for navigation and refreshes it on resize', async () => {
    let resize
    const disconnect = vi.fn()
    vi.stubGlobal('ResizeObserver', class {
      constructor(callback) { resize = callback }
      observe() {}
      disconnect = disconnect
    })
    const { box, view, rect } = scrollFixture()
    goToPage(40)
    await tick()
    expect(box.scrollTop).toBe(ROOM + 40070)
    expect(editor.pageIndex).toBe(40)
    Object.defineProperty(box, 'clientHeight', { value: 2400 })
    resize()
    await tick()
    expect(editor.stripScope).toEqual([40, 41, 42])
    rect.mockClear()
    // The viewport is 2400px now, and its room 1200.
    expect(box.scrollTop).toBe(1200 + 40070)
    box.scrollTop = 1200 + 50170
    box.dispatchEvent(new Event('scroll'))
    await tick()
    expect(rect).not.toHaveBeenCalled()
    expect(editor.pageIndex).toBe(51)
    view.unmount()
    expect(disconnect).toHaveBeenCalledTimes(1)
  })

  it('leaves ordinary paginated wheel scrolling alone and anchors modifier zoom', async () => {
    const { box, rect } = scrollFixture()
    editor.project.mode = 'single'
    await tick()
    rect.mockClear()
    const wheel = new WheelEvent('wheel', { deltaY: 50, cancelable: true })
    box.dispatchEvent(wheel)
    await tick()
    expect(wheel.defaultPrevented).toBe(false)
    expect(editor.zoom).toBe(1)
    expect(rect).not.toHaveBeenCalled()
    // The stage's own side margin centres the sheet: 700px at 100%, 645px at 111%.
    rect.mockImplementation(function () {
      return /** @type {DOMRect} */ ({
        top: 70 + ROOM - box.scrollTop,
        left: 104 + (Number.parseFloat(this.style.marginLeft) || 0) - box.scrollLeft,
        width: 1000 * editor.zoom, height: 1000 * editor.zoom,
      })
    })
    const pinch = new WheelEvent('wheel', {
      deltaY: -10, ctrlKey: true, cancelable: true, clientX: 400, clientY: 500,
    })
    box.dispatchEvent(pinch)
    expect(pinch.defaultPrevented).toBe(true)
    await waitFor(() => expect(editor.zoom).toBe(1.11))
    await waitFor(() => expect(box.scrollTop).toBeCloseTo(ROOM + 47.3))
    expect(box.scrollLeft).toBeCloseTo(SIDE - 33.44)
    expect(rect).toHaveBeenCalledTimes(2)
  })
})

describe('longstrip zoom', () => {
  /** Slot indices removed from the stage while `act` runs. */
  async function removedDuring(view, act) {
    const removed = []
    const observer = new MutationObserver((records) => {
      for (const record of records) for (const node of record.removedNodes) {
        if (node instanceof HTMLElement && node.matches('.slot')) removed.push(Number(node.getAttribute('data-strip-index')))
      }
    })
    observer.observe(view.container.querySelector('.stage'), { childList: true })
    await act()
    observer.disconnect()
    return removed
  }

  /** The scroller position that puts page 40 at the top, plus `extra`. */
  async function atPage40(box, extra = 100) {
    box.scrollTop = ROOM + 40070 + extra
    box.dispatchEvent(new Event('scroll'))
    await tick()
    await tick()
  }

  it('keeps the pages under a pinch mounted and the point under the pointer still', async () => {
    const { box, view } = scrollFixture()
    await tick()
    await atPage40(box)
    const pages = editor.chapter.pages
    const removed = await removedDuring(view, async () => {
      for (const zoom of [1.11, 1.22]) {
        box.dispatchEvent(new WheelEvent('wheel', { deltaY: -10, ctrlKey: true, cancelable: true, clientX: 500, clientY: 300 }))
        await waitFor(() => expect(editor.zoom).toBe(zoom))
        await tick()
      }
    })
    // Pages 38 to 42 were the viewport's band before and after; none of them
    // may leave the document on any frame of the gesture.
    expect(removed.filter((index) => index >= 38 && index <= 42)).toEqual([])
    // The content under the pointer (300px into the scroller, 40% down page
    // 40) is still under it at the new scale.
    const after = stripMetrics({ pages, scale: editor.zoom })
    const columnTop = 300 - (after.offsets[40] + 0.4 * after.units[40])
    expect(box.scrollTop).toBeCloseTo(70 + ROOM - columnTop, 0)
    expect(editor.pageIndex).toBe(40)
  })

  it('keeps the centre page when the zoom changes without a pointer', async () => {
    const { box, view } = scrollFixture()
    await tick()
    await atPage40(box)
    const removed = await removedDuring(view, async () => {
      editor.zoom = 1.5
      await tick()
      await tick()
    })
    expect(removed.filter((index) => index >= 38 && index <= 42)).toEqual([])
    expect(editor.pageIndex).toBe(40)
    const after = stripMetrics({ pages: editor.chapter.pages, scale: 1.5 })
    expect(box.scrollTop).toBeCloseTo(70 + ROOM - (300 - (after.offsets[40] + 0.4 * after.units[40])), 0)
  })
})

describe('scroll room', () => {
  it('keeps half a viewport on every side of a zoomed page, none around a fitted one, and holds the page still', async () => {
    const { box, view } = scrollFixture()
    editor.project.mode = 'single'
    editor.fit = true
    await tick()
    const stage = /** @type {HTMLElement} */ (view.container.querySelector('.stage'))
    expect([stage.style.marginTop, editor.scrollRoom, box.scrollTop]).toEqual(['', 0, 0])
    expect([stage.style.marginLeft, editor.scrollRoomSide, box.scrollLeft]).toEqual(['', 0, 0])

    // Leaving fit adds the room and moves the scroller by as much.
    editor.fit = false
    await tick()
    expect([stage.style.marginTop, stage.style.marginBottom]).toEqual(['300px', '300px'])
    expect([editor.scrollRoom, box.scrollTop]).toEqual([ROOM, ROOM])
    // At the sides the margin also centres the 1000px sheet in the 1200px
    // viewport, which the viewport no longer does for a stage this wide.
    expect([stage.style.marginLeft, stage.style.marginRight]).toEqual(['700px', '700px'])
    expect([editor.scrollRoomSide, box.scrollLeft]).toEqual([SIDE, SIDE])

    // A zoom changes the margin and leaves the scroller alone; past the
    // viewport's width there is nothing left to centre.
    editor.zoom = 1.1
    await tick()
    expect([stage.style.marginLeft, box.scrollLeft]).toEqual(['650px', SIDE])
    editor.zoom = 1.5
    await tick()
    expect([stage.style.marginLeft, box.scrollLeft]).toEqual(['600px', SIDE])

    // The reader's place survives going back to fit and out again.
    box.scrollTop = ROOM + 120
    box.scrollLeft = SIDE + 45
    editor.fit = true
    await tick()
    expect([stage.style.marginTop, editor.scrollRoom, box.scrollTop]).toEqual(['', 0, 120])
    expect([stage.style.marginLeft, editor.scrollRoomSide, box.scrollLeft]).toEqual(['', 0, 45])
    view.unmount()
    expect([editor.scrollRoom, editor.scrollRoomSide]).toEqual([0, 0])
  })
})

describe('preloading', () => {
  /** A Tauri window, as far as `tile.js` can tell: tiles get URLs. */
  function tauriTiles() {
    vi.stubGlobal('__TAURI_INTERNALS__', { convertFileSrc: (_path, scheme) => `${scheme}://localhost/` })
  }

  it('fetches a strip page\'s tiles a screen ahead, and only those, without lazy loading', async () => {
    tauriTiles()
    const pages = Array.from({ length: 3 }, (_, index) => ({
      id: `tall-${index}`, index, number: index + 1, chapterId: 'tall', sourceSha: `s${index}`,
      width: 800, height: 20000, status: 'unclean', regions: [],
    }))
    setBackend(/** @type {any} */ ({ loadPages: vi.fn(async () => []) }))
    editor.project = /** @type {any} */ ({ id: 'tall-project', mode: 'longstrip' })
    editor.chapter = /** @type {any} */ ({ id: 'tall', pages, review: [] })
    editor.pageIndex = 0
    editor.loading = false
    editor.fit = false
    editor.zoom = 1
    const box = document.createElement('div')
    document.body.append(box)
    Object.defineProperties(box, {
      clientWidth: { configurable: true, value: 1200 },
      clientHeight: { configurable: true, value: 600 },
    })
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function () {
      return /** @type {DOMRect} */ ({ top: this.classList.contains('stage') ? 70 : 0, left: 0, width: 800, height: 60000 })
    })
    const view = render(CanvasStage, { target: box })
    await tick()
    const tiles = () => [...view.container.querySelectorAll('img.scan')].map((image) =>
      `${image.closest('[data-strip-index]').getAttribute('data-strip-index')}:${new URL(image.src).pathname.split('/').slice(-2).join('/')}`)

    // 20000px pages cut into ten 2048px tiles: the viewport and the screen
    // below it reach only the first tile of page 0.
    // Both variants are native-composited managed tiles.
    expect(tiles()).toEqual(['0:source/0', '0:cleaned/0'])
    expect(view.container.querySelector('img.scan[loading="lazy"]')).toBeNull()

    // Near the join, the last tile of page 0 and the first of page 1.
    box.scrollTop = 19770
    box.dispatchEvent(new Event('scroll'))
    await tick()
    expect(tiles()).toEqual(['0:source/9', '0:cleaned/9', '1:source/0', '1:cleaned/0'])
  })

  /** A paginated chapter of one-tile pages, opened on `pageIndex`. */
  function paginatedFixture(pageIndex, count = 6) {
    tauriTiles()
    const pages = Array.from({ length: count }, (_, index) => ({
      id: `single-${index}`, index, number: index + 1, chapterId: 'single', sourceSha: `s${index}`,
      width: 800, height: 1200, status: 'unclean', regions: [],
    }))
    editor.project = /** @type {any} */ ({ id: 'single-project', mode: 'single' })
    editor.chapter = /** @type {any} */ ({ id: 'single', pages, review: [] })
    editor.pageIndex = pageIndex
    editor.loading = false
    // The order the tiles enter the document, which is the order the webview
    // asks for them in. A record's node is read after later records have
    // added to it, so a tile counts under the last record that holds it.
    const seen = new Set()
    const entered = []
    const note = (records) => {
      const fresh = records.map(() => [])
      for (let at = records.length - 1; at >= 0; at -= 1) {
        for (const node of records[at].addedNodes) {
          if (!(node instanceof Element)) continue
          for (const image of node.matches('img.scan') ? [node] : node.querySelectorAll('img.scan')) {
            if (seen.has(image)) continue
            seen.add(image)
            fresh[at].push(new URL(/** @type {HTMLImageElement} */ (image).src).pathname)
          }
        }
      }
      entered.push(...fresh.flat())
    }
    const observer = new MutationObserver(note)
    observer.observe(document.body, { childList: true, subtree: true })
    const asked = () => {
      note(observer.takeRecords())
      return entered
    }
    const view = render(CanvasStage)
    const sheet = (index) => /** @type {HTMLElement} */ (view.container.querySelector(`[data-page-id="single-${index}"]`))
    const stack = (index, variant) => /** @type {HTMLElement} */ (sheet(index).querySelector(`[data-artwork="${variant}"]`))
    const tile = (index, variant) => /** @type {HTMLImageElement} */ (stack(index, variant).querySelector('img'))
    const paths = () => [...view.container.querySelectorAll('img.scan')].map((image) => new URL(image.src).pathname)
    return { view, asked, sheet, stack, tile, paths, stop: () => observer.disconnect() }
  }

  it('asks for the page on screen first, the stack in view before the covered one, and the neighbours once it is shown', async () => {
    const { asked, sheet, tile, paths, stop } = paginatedFixture(3)
    await tick()
    // A jump lands on a cold page: its neighbours' sheets are there, hidden,
    // and ask for nothing while the page on screen is still loading.
    expect(asked()).toEqual(['/single/3/cleaned/0', '/single/3/source/0'])
    expect(sheet(2).hidden && sheet(4).hidden).toBe(true)

    await fireEvent.load(tile(3, 'cleaned'))
    await waitFor(() => expect(paths()).toHaveLength(6))
    expect(asked().slice(2)).toEqual([
      '/single/2/cleaned/0', '/single/4/cleaned/0',
      '/single/2/source/0', '/single/4/source/0',
    ])

    // A turn to a neighbour that has not loaded yet: it is the page on screen
    // now, and the page after it waits for it in turn.
    editor.pageIndex = 4
    await tick()
    expect(asked()).toHaveLength(6)
    expect(sheet(5).querySelector('img')).toBeNull()
    await fireEvent.load(tile(4, 'cleaned'))
    await waitFor(() => expect(asked().slice(6)).toEqual(['/single/5/cleaned/0', '/single/5/source/0']))
    stop()
  })

  it('makes the neighbours wait again for a page left and come back to', async () => {
    const { asked, tile, paths, stop } = paginatedFixture(1, 8)
    await tick()
    await fireEvent.load(tile(1, 'cleaned'))
    await waitFor(() => expect(paths()).toHaveLength(6))

    // Away to a page that never gets to show, and back: every sheet is new.
    editor.pageIndex = 5
    await tick()
    editor.pageIndex = 1
    await tick()
    expect(asked().slice(-2)).toEqual(['/single/1/cleaned/0', '/single/1/source/0'])
    expect(paths()).toHaveLength(2)
    stop()
  })

  it('asks for the source first while the original is shown, and shows the page without its cleaned tiles', async () => {
    editor.originalVisible = true
    const { asked, stack, tile, stop } = paginatedFixture(0)
    await tick()
    expect(asked()).toEqual(['/single/0/source/0', '/single/0/cleaned/0'])
    expect(stack(0, 'source').style.visibility).toBe('hidden')
    await fireEvent.load(tile(0, 'source'))
    await waitFor(() => expect(stack(0, 'source').style.visibility).toBe(''))
    expect(tile(0, 'cleaned').dataset.loadedVersion).toBeUndefined()
    stop()
  })

  it('keeps a cleaned view hidden until its cleaned tiles decode, whatever the source has done', async () => {
    const { stack, tile, stop } = paginatedFixture(0)
    await tick()
    await fireEvent.load(tile(0, 'source'))
    await waitFor(() => expect(tile(0, 'source').dataset.loadedVersion).toBe(tile(0, 'source').dataset.version))
    expect(stack(0, 'source').style.visibility).toBe('hidden')
    expect(stack(0, 'cleaned').style.visibility).toBe('hidden')
    await fireEvent.load(tile(0, 'cleaned'))
    await waitFor(() => expect(stack(0, 'cleaned').style.visibility).toBe(''))
    expect(stack(0, 'source').style.visibility).toBe('')
    stop()
  })

  it('keeps a cold page\'s cleaned tile while its regions arrive, and moves it when an edit answer lands', async () => {
    tauriTiles()
    const region = (layer) => ({
      id: 'cold-1-r0', pageId: 'cold-1', bbox: { x: 10, y: 10, w: 20, h: 20 }, source: 'auto', outcome: 'cleaned',
      mask: {
        id: 'cold-1-r0-m1', regionId: 'cold-1-r0', sequence: 1, fillMode: 'reconstruct', elapsedMs: 0,
        fittingReconstructed: false, cloudOutcome: null, appearance: layer,
        provenance: { engine: 'lama', engine_version: '1.0', mask_sha256: 'm', source_sha256: 's',
          created: '2026-10-02', execution_provider: 'cpu', params_snapshot: {} },
      },
    })
    const header = (index) => ({
      id: `cold-${index}`, index, number: index + 1, chapterId: 'cold', sourceSha: `s${index}`,
      width: 800, height: 1200, status: 'cleaned', appearance: `page-${index}-a`, regions: [], resident: false,
    })
    // What the manifest holds for page 1: its digest and its one layer's.
    let saved = { appearance: 'page-1-a', layer: 'layer-a' }
    setBackend(/** @type {any} */ ({ loadPages: vi.fn(async ({ indices }) => indices.map((index) => ({
      ...header(index), resident: true,
      ...(index === 1 ? { appearance: saved.appearance, regions: [region(saved.layer)] } : {}),
    }))) }))
    editor.project = /** @type {any} */ ({ id: 'cold-project', mode: 'single' })
    editor.chapter = /** @type {any} */ ({ id: 'cold', pages: Array.from({ length: 6 }, (_, index) => header(index)), review: [] })
    editor.pageIndex = 1
    editor.loading = false
    const view = render(CanvasStage)
    await tick()
    const cleaned = () => /** @type {HTMLImageElement} */ (
      view.container.querySelector('[data-page-id="cold-1"] [data-artwork="cleaned"] img[data-version]'))
    const pending = cleaned()

    // The window brings the regions in: the same saved page, the same tile.
    await syncPageWindow()
    await tick()
    expect(editor.chapter.pages[1].regions).toHaveLength(1)
    expect(cleaned()).toBe(pending)

    // An edit's answer replaces the layer before the page is read again.
    replaceRegion(/** @type {any} */ (region('layer-b')))
    await tick()
    const answered = cleaned().src
    expect(answered).not.toBe(pending.src)

    // The reload names the edit, and the header the window leaves agrees.
    saved = { appearance: 'page-1-b', layer: 'layer-b' }
    await reloadPage(1)
    await tick()
    const settled = cleaned().src
    expect([pending.src, answered]).not.toContain(settled)
    goToPage(4)
    expect(editor.chapter.pages[1].resident).toBe(false)
    expect(tileUrl(editor.chapter.pages[1], 'cleaned', 0)).toBe(settled)
    await tick()
  })

  it('decodes both variants of adjacent pages and retains their nodes on navigation', async () => {
    const { view, sheet, tile, paths, stop } = paginatedFixture(1, 4)
    await tick()
    await fireEvent.load(tile(1, 'cleaned'))
    await waitFor(() => expect(paths().sort()).toEqual([
      '/single/0/cleaned/0', '/single/0/source/0',
      '/single/1/cleaned/0', '/single/1/source/0',
      '/single/2/cleaned/0', '/single/2/source/0',
    ]))
    const next = sheet(2)
    expect(next.hidden).toBe(true)
    const cleaned = tile(2, 'cleaned')
    await fireEvent.load(cleaned)
    await waitFor(() => expect(next.querySelector('[data-artwork="cleaned"]').style.visibility).toBe(''))
    editor.pageIndex = 2
    await tick()
    expect(next.hidden).toBe(false)
    expect(next.querySelector('[data-artwork="cleaned"] img')).toBe(cleaned)
    expect(view.container.querySelector('[data-page-id="single-0"]')).toBeNull()
    // The neighbour was loaded, so it is shown as it arrives and the page
    // after it is asked for without waiting on anything.
    await waitFor(() => expect(paths()).toContain('/single/3/cleaned/0'))
    expect(view.container.querySelectorAll('.paginated')).toHaveLength(3)
    expect(view.container.querySelectorAll('.paginated:not([hidden])')).toHaveLength(1)
    stop()
  })
})

describe('longstrip mask visibility', () => {
  it('draws no line where one position meets the next, and lifts no page over another for a spanning draft', async () => {
    const first = {
      id: 'c1-p001', index: 0, number: 1, width: 800, height: 1000,
      status: 'unclean', regions: [],
    }
    const second = {
      id: 'c1-p002', index: 1, number: 2, width: 700, height: 2000,
      status: 'unclean', regions: [],
    }
    editor.project = /** @type {any} */ ({ id: 'p1', mode: 'longstrip' })
    editor.chapter = /** @type {any} */ ({ id: 'c1', pages: [first, second], review: [] })
    editor.pageIndex = 0
    editor.fit = false
    editor.zoom = 1
    editor.tool = 'shapes'

    const view = render(CanvasStage)
    const slots = view.container.querySelectorAll('.slot')
    expect(slots).toHaveLength(2)
    // The pages meet edge to edge, with no mark between them.
    expect(view.container.querySelector('.seam')).toBeNull()
    expect(/** @type {HTMLElement} */ (slots[1]).style.top).toBe('1000px')

    beginDraft({
      tool: 'shapes', kind: 'rect', pageId: first.id,
      points: [{ x: 20, y: 95 }], bbox: { x: 20, y: 95, w: 40, h: 17 },
      mode: 'add', keyboard: false, moved: true,
    })
    await waitFor(() => expect(slots[0].querySelector('.preview')).not.toBeNull())
    // No page lifts itself over its neighbour any more: the sheets make no
    // stacking context, and every page's layers share the one z-index scale
    // (`PageSheet.svelte`), so the draft hangs over the next page's artwork
    // with nothing to switch on.
    for (const slot of slots) {
      expect(slot.className).toBe(slot.className.split(' ').filter((name) => !name.endsWith('-origin')).join(' '))
      expect(/** @type {HTMLElement} */ (slot).style.zIndex).toBe('')
    }
    expect(/** @type {HTMLElement} */ (slots[0].querySelector('.sheet')).style.getPropertyValue('--sheet-w')).toBe('800px')
  })
})

/**
 * One page in a 1200x600 viewport with the shell's real padding, laid out the
 * way the browser does it: the stage under the padding and its own margins,
 * moved by the scroller, as wide as the sheet and as tall as its ratio says,
 * and centred across while it is narrower than the view.
 * The view is the content box: 992x464 from (104, 70), centred on (600, 302).
 *
 * @param {{width: number, height: number}} size the page at 1:1
 */
function pageFixture(size) {
  const pages = [0, 1].map((index) => ({
    id: `view-${index}`, index, number: index + 1, chapterId: 'view-chapter', ...size, status: 'unclean', resident: true,
    regions: [
      { id: `small-${index}`, pageId: `view-${index}`, bbox: { x: 80, y: 10, w: 1, h: 1 }, outcome: 'declined', mask: null },
      { id: `mid-${index}`, pageId: `view-${index}`, bbox: { x: 10, y: 70, w: 20, h: 10 }, outcome: 'declined', mask: null },
    ],
  }))
  setBackend(/** @type {any} */ ({ loadPages: vi.fn(async ({ indices }) => indices.map((index) => pages[index])) }))
  editor.project = /** @type {any} */ ({ id: 'view-project', mode: 'single' })
  editor.chapter = /** @type {any} */ ({ id: 'view-chapter', pages, review: [] })
  editor.pageIndex = 0
  editor.loading = false
  editor.fit = true
  editor.zoom = 1
  const box = document.createElement('div')
  box.style.padding = '70px 104px 66px'
  document.body.append(box)
  Object.defineProperties(box, {
    clientWidth: { configurable: true, value: 1200 },
    clientHeight: { configurable: true, value: 600 },
  })
  const view = render(CanvasStage, { target: box })
  const stage = /** @type {HTMLElement} */ (view.container.querySelector('.stage'))
  const sheet = () => {
    const width = Number.parseFloat(/** @type {HTMLElement} */ (stage.querySelector('.paginated:not([hidden]) .sheet')).style.width)
    return { width, height: width * (size.height / size.width) }
  }
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function () {
    if (this !== stage) return /** @type {DOMRect} */ ({ top: 0, left: 0, width: 1200, height: 600 })
    const { width, height } = sheet()
    const side = Number.parseFloat(stage.style.marginLeft) || 0
    // `justify-content: safe center`: a stage narrower than the content box,
    // margins and all, is centred in it.
    const centred = Math.max(0, (992 - (width + 2 * side)) / 2)
    return /** @type {DOMRect} */ ({
      top: 70 + (Number.parseFloat(stage.style.marginTop) || 0) - box.scrollTop,
      left: 104 + centred + side - box.scrollLeft,
      width,
      height,
    })
  })
  /** Where page percent `(x, y)` is on screen. */
  const at = (x, y) => {
    const rect = stage.getBoundingClientRect()
    return { x: rect.left + (x / 100) * rect.width, y: rect.top + (y / 100) * rect.height }
  }
  return { box, stage, at, pages }
}

/** The centre of the fixture's view. */
const VIEW = { x: 600, y: 302 }

describe('where a zoom leaves the view', () => {
  it('centres a fitted page that its width binds, rather than hanging it from the top', async () => {
    const { at } = pageFixture({ width: 3000, height: 1000 })
    await tick()
    const centre = at(50, 50)
    expect(centre.x).toBeCloseTo(VIEW.x)
    expect(centre.y).toBeCloseTo(VIEW.y)
  })

  it('puts the page in the middle at 100%, whatever the reader panned to', async () => {
    const { box, at } = pageFixture({ width: 600, height: 400 })
    editor.fit = false
    editor.zoom = 2
    await tick()
    box.scrollLeft += 380
    box.scrollTop -= 120
    zoomActual()
    await tick()
    await tick()
    const centre = at(50, 50)
    expect(centre.x).toBeCloseTo(VIEW.x)
    expect(centre.y).toBeCloseTo(VIEW.y)
  })

  it('keeps the view\'s centre at 100% on a page larger than the view, and shows no empty canvas', async () => {
    const { box, at } = pageFixture({ width: 1600, height: 2400 })
    await tick()
    // From fit the view's centre is the page's: 100% keeps it there.
    zoomActual()
    await tick()
    await tick()
    expect(at(50, 50).x).toBeCloseTo(VIEW.x)
    expect(at(50, 50).y).toBeCloseTo(VIEW.y)
    // Pulled down past its top, the page goes back up to the view's top edge.
    box.scrollTop -= 1500
    zoomActual()
    await tick()
    await tick()
    expect(at(0, 0).y).toBeCloseTo(70)
  })

  it('zooms in and out about the view\'s centre, along the ladder', async () => {
    const { box, at } = pageFixture({ width: 1600, height: 2400 })
    editor.fit = false
    await tick()
    box.scrollLeft += 210
    box.scrollTop += 340
    const before = { x: 0, y: 0 }
    {
      const origin = at(0, 0)
      const end = at(100, 100)
      before.x = ((VIEW.x - origin.x) / (end.x - origin.x)) * 100
      before.y = ((VIEW.y - origin.y) / (end.y - origin.y)) * 100
    }
    for (const zoom of [1.25, 1.5, 2]) {
      zoomIn()
      await tick()
      await tick()
      expect(editor.zoom).toBe(zoom)
      const point = at(before.x, before.y)
      expect(point.x).toBeCloseTo(VIEW.x)
      expect(point.y).toBeCloseTo(VIEW.y)
    }
  })

  it('fit puts the page back in the middle after a pan', async () => {
    const { box, at } = pageFixture({ width: 1600, height: 2400 })
    editor.fit = false
    await tick()
    box.scrollLeft += 500
    zoomFit()
    await tick()
    await tick()
    expect(at(50, 50).x).toBeCloseTo(VIEW.x)
    expect(at(50, 50).y).toBeCloseTo(VIEW.y)
  })

  it('marks the stage for nearest-neighbour drawing only past 100%', async () => {
    const { stage } = pageFixture({ width: 1600, height: 2400 })
    await tick()
    expect(stage.hasAttribute('data-pixels')).toBe(false)
    editor.fit = false
    editor.zoom = 1
    await tick()
    expect(stage.hasAttribute('data-pixels')).toBe(false)
    editor.zoom = 16
    await tick()
    expect(stage.hasAttribute('data-pixels')).toBe(true)
  })
})

describe('show on page', () => {
  it('turns to the region\'s page, selects it and centres it, at the zoom on screen', async () => {
    const { at, pages } = pageFixture({ width: 1600, height: 2400 })
    editor.fit = false
    editor.zoom = 1
    await tick()
    const region = pages[1].regions[1]
    // The Layers row's action, as the row calls it.
    showOnPage(/** @type {any} */ (region))
    await tick()
    await tick()
    expect(editor.pageIndex).toBe(1)
    expect(editor.selectionId).toBe(region.id)
    expect(editor.zoom).toBe(1)
    const centre = at(20, 75)
    expect(centre.x).toBeCloseTo(VIEW.x)
    expect(centre.y).toBeCloseTo(VIEW.y)
    expect(editor.viewRequest).toBeNull()
  })

  it('zooms in on a region too small to see, then centres it', async () => {
    const { at, pages } = pageFixture({ width: 1600, height: 2400 })
    editor.fit = false
    editor.zoom = 1
    await tick()
    // 16x24 page pixels: 16px on screen at 100%.
    focusRegion(pages[0].regions[0])
    await tick()
    await tick()
    await tick()
    expect(editor.zoom).toBe(4)
    const centre = at(80.5, 10.5)
    expect(centre.x).toBeCloseTo(VIEW.x)
    expect(centre.y).toBeCloseTo(VIEW.y)
  })

  it('zooms out until a region that does not fit does', async () => {
    const { at, pages } = pageFixture({ width: 1600, height: 2400 })
    editor.fit = false
    editor.zoom = 32
    await tick()
    focusRegion(pages[0].regions[1])
    await tick()
    await tick()
    await tick()
    // 320x240 page pixels in a 992x464 view.
    expect(editor.zoom).toBeCloseTo((464 * 0.7) / 240, 2)
    const centre = at(20, 75)
    expect(centre.x).toBeCloseTo(VIEW.x)
    expect(centre.y).toBeCloseTo(VIEW.y)
  })

  it('centres a region far down a strip, from the column\'s geometry', async () => {
    const { box } = scrollFixture()
    await tick()
    const pages = editor.chapter.pages
    pages[40].regions = [{ id: 'deep', pageId: pages[40].id, bbox: { x: 10, y: 40, w: 30, h: 20 }, outcome: 'declined', mask: null }]
    focusRegion(pages[40].regions[0])
    await tick()
    await tick()
    expect(editor.selectionId).toBe('deep')
    expect(editor.zoom).toBe(1)
    // The fixture's stage is 70px below the scroller's top, moved by it, and
    // the view is the whole 1200x600 scroller.
    const stage = /** @type {HTMLElement} */ (box.querySelector('.stage'))
    const metrics = stripMetrics({ pages, scale: 1 })
    const top = 70 + (Number.parseFloat(stage.style.marginTop) || 0) - box.scrollTop
    expect(top + metrics.offsets[40] + 0.5 * (metrics.units[40] - STRIP_GAP)).toBeCloseTo(300)
    // Across: page 40 is 800px wide in the 1000px stage, which the fixture
    // holds still at the left, so the region's centre is at 100 + 200 = 300
    // and the scroller moves left by 300 from its place at rest.
    expect(box.scrollLeft).toBeCloseTo(SIDE - 300)
  })
})
