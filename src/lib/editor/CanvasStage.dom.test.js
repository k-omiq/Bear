import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { tick } from 'svelte'
import { setBackend } from '../api/backend.js'
import { editor, goToPage } from '../state/editor.svelte.js'
import { beginDraft, resetDraftState } from './draft.svelte.js'
import CanvasStage from './CanvasStage.svelte'

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
  editor.selectionId = null
  editor.hoverId = null
  editor.stripScope = []
  editor.stripFocus = -1
  editor.stripScrollRequest = null
})

/** The scroll room of the fixture's 600px viewport. */
const ROOM = 300

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
    // Page 40 is in view; 39 and 41 are within a screen of it, 38 and 42 are
    // the overscan beyond that.
    expect(mounted.length).toBeLessThanOrEqual(5)
    expect([...view.container.querySelectorAll('.slot')].map((slot) => slot.getAttribute('data-strip-index')))
      .toEqual(['38', '39', '40', '41', '42'])
    expect(loadPages).toHaveBeenCalledTimes(1)
    // The mounted band and a neighbour either side, so every mounted sheet
    // has its patch layers before it scrolls into view.
    expect(loadPages).toHaveBeenCalledWith({ chapterId: 'scroll-chapter', indices: [37, 38, 39, 40, 41, 42, 43] })
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
    expect(view.container.querySelectorAll('.slot')).toHaveLength(6)
    expect(editor.chapter.pages[0].resident).toBe(true)
    expect(editor.stripScope).toEqual([40])
    button.blur()
    await tick()
    expect(view.container.querySelectorAll('.slot')).toHaveLength(5)
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
    rect.mockImplementation(() => /** @type {DOMRect} */ ({
      top: 70 + ROOM - box.scrollTop, left: 104 - box.scrollLeft,
      width: 1000 * editor.zoom, height: 1000 * editor.zoom,
    }))
    const pinch = new WheelEvent('wheel', {
      deltaY: -10, ctrlKey: true, cancelable: true, clientX: 400, clientY: 500,
    })
    box.dispatchEvent(pinch)
    expect(pinch.defaultPrevented).toBe(true)
    await waitFor(() => expect(editor.zoom).toBe(1.11))
    await waitFor(() => expect(box.scrollTop).toBeCloseTo(ROOM + 47.3))
    expect(box.scrollLeft).toBeCloseTo(32.56)
    expect(rect).toHaveBeenCalledTimes(2)
  })
})

describe('scroll room', () => {
  it('keeps half a viewport above and below a zoomed page, none around a fitted one, and holds the page still', async () => {
    const { box, view } = scrollFixture()
    editor.project.mode = 'single'
    editor.fit = true
    await tick()
    const stage = /** @type {HTMLElement} */ (view.container.querySelector('.stage'))
    expect([stage.style.marginTop, editor.scrollRoom, box.scrollTop]).toEqual(['', 0, 0])

    // Leaving fit adds the room and moves the scroller by as much.
    editor.fit = false
    await tick()
    expect([stage.style.marginTop, stage.style.marginBottom]).toEqual(['300px', '300px'])
    expect([editor.scrollRoom, box.scrollTop]).toEqual([ROOM, ROOM])

    // The reader's place survives going back to fit and out again.
    box.scrollTop = ROOM + 120
    editor.fit = true
    await tick()
    expect([stage.style.marginTop, editor.scrollRoom, box.scrollTop]).toEqual(['', 0, 120])
    view.unmount()
    expect(editor.scrollRoom).toBe(0)
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

  it('decodes both variants of adjacent pages and retains their nodes on navigation', async () => {
    tauriTiles()
    const pages = Array.from({ length: 4 }, (_, index) => ({
      id: `single-${index}`, index, number: index + 1, chapterId: 'single', sourceSha: `s${index}`,
      width: 800, height: 1200, status: 'unclean', regions: [],
    }))
    editor.project = /** @type {any} */ ({ id: 'single-project', mode: 'single' })
    editor.chapter = /** @type {any} */ ({ id: 'single', pages, review: [] })
    editor.pageIndex = 1
    editor.loading = false
    const view = render(CanvasStage)
    await tick()
    const images = [...view.container.querySelectorAll('img.scan')]
    expect(images.map(image => new URL(image.src).pathname).sort()).toEqual([
      '/single/0/cleaned/0', '/single/0/source/0',
      '/single/1/cleaned/0', '/single/1/source/0',
      '/single/2/cleaned/0', '/single/2/source/0',
    ])
    const next = view.container.querySelector('[data-page-id="single-2"]')
    expect(next.hidden).toBe(true)
    const cleaned = next.querySelector('[data-artwork="cleaned"] img')
    await fireEvent.load(cleaned)
    await waitFor(() => expect(next.querySelector('[data-artwork="cleaned"]').style.visibility).toBe(''))
    editor.pageIndex = 2
    await tick()
    expect(next.hidden).toBe(false)
    expect(next.querySelector('[data-artwork="cleaned"] img')).toBe(cleaned)
    expect(view.container.querySelector('[data-page-id="single-0"]')).toBeNull()
    expect(view.container.querySelectorAll('.paginated')).toHaveLength(3)
    expect(view.container.querySelectorAll('.paginated:not([hidden])')).toHaveLength(1)
  })
})

describe('longstrip mask visibility', () => {
  it('draws no line where one position meets the next, and keeps a spanning draft above the next page', async () => {
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
    await waitFor(() => expect(slots[0].classList.contains('gesture-origin')).toBe(true))
    expect(slots[1].classList.contains('gesture-origin')).toBe(false)
  })
})
