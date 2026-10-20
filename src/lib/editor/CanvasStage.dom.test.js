import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { tick } from 'svelte'
import { setBackend } from '../api/backend.js'
import { tileUrl } from '../api/tile.js'
import { editor, goToPage, reloadPage, replaceRegion, syncPageWindow } from '../state/editor.svelte.js'
import { beginDraft, resetDraftState } from './draft.svelte.js'
import CanvasStage from './CanvasStage.svelte'
import { stripMetrics } from './strip.js'

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
