import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '@testing-library/svelte'
import { tick } from 'svelte'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import PaintLayer from './PaintLayer.svelte'
import { clearDraft, draft, markCommitted, resetDraftState } from './draft.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { setBackend } from '../api/backend.js'
import { pageVersion } from '../api/tile.js'
import { flushPaintPreview } from './paintpreview.js'

const page = (index = 0) => ({ id: `page-${index}`, chapterId: 'c', index, width: 100, height: 100, regions: [] })
const stroke = (id) => ({ pageId: id, kind: 'stroke', tool: 'brush', points: [{ x: 25, y: 25, p: 1 }] })
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r }); return { promise, resolve } }
const result = spec => ({ ...spec, png: [1, 2], bounds: { x: 20, y: 20, w: 10, h: 10 } })
const bitmap = (width = 4, height = 3) => ({ width, height, close: vi.fn() })
/** A canvas's size and place: what has to change in the same step as its pixels. */
const placed = canvas => [canvas.width, canvas.height, canvas.style.left, canvas.style.top, canvas.style.width, canvas.style.height]
/** Past every microtask: whatever an answer already given sets off has happened. */
const settled = () => new Promise(resolve => setTimeout(resolve))

/** Every picture decoded, in order. */
let bitmaps = []
/** Every draw onto a canvas, with the size and place the canvas had at that moment. */
let draws = []

async function setup(request) {
  editor.chapter = { id: 'c' }
  editor.toolParams.brush = { mode: 'paint', size: 10, color: '#123456' }
  setBackend({ previewPaint: request })
  const host = document.createElement('div'); host.className = 'sheet'; document.body.append(host)
  return { host, ...render(PaintLayer, { target: host, props: { page: page() } }) }
}
beforeEach(() => {
  bitmaps = []; draws = []
  // The context can draw a picture and nothing else: a fill would throw.
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(/** @type {any} */ (function () {
    const canvas = this
    return { drawImage: (image, x, y) => draws.push({ image, x, y, canvas, at: placed(canvas) }) }
  }))
  vi.stubGlobal('createImageBitmap', vi.fn(async () => { const made = bitmap(); bitmaps.push(made); return made }))
})
afterEach(() => { cleanup(); resetDraftState(); editor.chapter = null; setBackend(null); vi.restoreAllMocks(); vi.unstubAllGlobals(); document.body.innerHTML = '' })

describe('mounted authoritative preview lifecycle', () => {
  it('renders the first frame and discards canceled late frames', async () => {
    const late = deferred(), request = vi.fn().mockImplementationOnce(async s => result(s)).mockReturnValueOnce(late.promise)
    const { host } = await setup(request)
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    expect(host.querySelector('.paint')).not.toBeNull()
    draft.active.points.push({ x: 30, y: 30 }); await tick()
    draft.active = null; await tick()
    late.resolve(result(request.mock.calls[1][0])); await flushPaintPreview('page-0'); await tick()
    expect(host.querySelector('.paint')).toBeNull()
    // The late frame was decoded before it was discarded, and nothing drew it.
    expect(bitmaps).toHaveLength(2)
    expect(draws.map(({ image }) => image)).toEqual([bitmaps[0]])
  })
  it('rejects a response after the mounted page changes', async () => {
    const late = deferred(), request = vi.fn(() => late.promise)
    const { host, rerender } = await setup(request)
    draft.active = stroke('page-0'); await tick()
    await rerender({ page: page(1) })
    late.resolve(result(request.mock.calls[0][0])); await settled(); await tick()
    expect(bitmaps).toHaveLength(1)
    expect(host.querySelector('.paint')).toBeNull()
  })
  it('holds final pixels until all affected tiles load the committed revision', async () => {
    const { host, rerender } = await setup(vi.fn(async s => result(s)))
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({ left: 0, top: 0, right: 100, bottom: 100 })
    const stack = document.createElement('div'); stack.dataset.artwork = 'cleaned'; host.append(stack)
    const tile = document.createElement('img'); stack.append(tile)
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    markCommitted(); clearDraft(); await tick()
    const committed = { ...page(), tileRevision: 1 }
    tile.dataset.version = pageVersion(committed, 'cleaned')
    await rerender({ page: committed })
    expect(host.querySelector('.paint')).not.toBeNull()
    tile.dataset.loadedVersion = pageVersion(page(), 'cleaned')
    document.dispatchEvent(new CustomEvent('paint-tile-loaded', { detail: { pageId: 'page-0' } })); await tick()
    expect(host.querySelector('.paint')).not.toBeNull()
    expect(bitmaps[0].close).not.toHaveBeenCalled()
    tile.dataset.loadedVersion = pageVersion(committed, 'cleaned')
    document.dispatchEvent(new CustomEvent('paint-tile-loaded', { detail: { pageId: 'page-0' } })); await tick()
    expect(host.querySelector('.paint')).toBeNull()
    expect(bitmaps[0].close).toHaveBeenCalledTimes(1)
  })
  it('waits for affected neighboring-page tiles as well as the anchor', async () => {
    const { host, rerender } = await setup(vi.fn(async s => result(s)))
    const stage = document.createElement('div'); stage.className = 'stage'; host.replaceWith(stage); stage.append(host)
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({ left: 0, top: 0, right: 100, bottom: 100 })
    const stack = document.createElement('div'); stack.dataset.artwork = 'cleaned'; stage.append(stack)
    const first = document.createElement('img'), neighbor = document.createElement('img'); stack.append(first, neighbor)
    first.dataset.paintKey = 'page-0:0'; neighbor.dataset.paintKey = 'page-1:0'
    first.dataset.version = first.dataset.loadedVersion = pageVersion(page(), 'cleaned')
    neighbor.dataset.version = neighbor.dataset.loadedVersion = 'neighbor-old'
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    markCommitted(); clearDraft(); await tick()
    const committed = { ...page(), tileRevision: 1 }
    first.dataset.version = first.dataset.loadedVersion = pageVersion(committed, 'cleaned')
    await rerender({ page: committed })
    expect(host.querySelector('.paint')).not.toBeNull()
    // The neighbor has not received its commit token yet: old loaded pixels do not qualify.
    neighbor.dataset.version = 'neighbor-committed'
    document.dispatchEvent(new CustomEvent('paint-tile-loaded')); await tick()
    expect(host.querySelector('.paint')).not.toBeNull()
    neighbor.dataset.loadedVersion = neighbor.dataset.version
    document.dispatchEvent(new CustomEvent('paint-tile-loaded', { detail: { pageId: 'page-1' } })); await tick()
    expect(host.querySelector('.paint')).toBeNull()
  })
  it('clears a held overlay when commit fails', async () => {
    const { host, rerender } = await setup(vi.fn(async s => result(s)))
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    markCommitted(); clearDraft(); await tick()
    expect(host.querySelector('.paint')).not.toBeNull()
    await rerender({ page: page(), dropped: 1 })
    expect(host.querySelector('.paint')).toBeNull()
  })
  it('drops the frame of a stroke that was abandoned, not committed', async () => {
    const { host } = await setup(vi.fn(async s => result(s)))
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    clearDraft(); await tick()
    expect(host.querySelector('.paint')).toBeNull()
  })
  // The stamp is used in quick taps. Each tap is a commit of its own, and the
  // next one starts before the tiles of the last have caught up.
  it('keeps a held frame while the next stroke is drawn and committed', async () => {
    setBackend({ previewPaint: vi.fn(async s => result(s)) })
    editor.chapter = { id: 'c' }
    editor.toolParams.brush = { mode: 'paint', size: 10, color: '#123456' }
    const host = document.createElement('div'); host.className = 'sheet'; document.body.append(host)
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({ left: 0, top: 0, right: 100, bottom: 100 })
    const stack = document.createElement('div'); stack.dataset.artwork = 'cleaned'; host.append(stack)
    const tile = document.createElement('img'); stack.append(tile)
    tile.dataset.version = tile.dataset.loadedVersion = pageVersion(page(), 'cleaned')
    const { rerender } = render(PaintLayer, { target: host, props: { page: page(), busy: 0 } })

    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    await rerender({ page: page(), busy: 1 }); markCommitted(); clearDraft(); await tick()
    expect(host.querySelectorAll('.paint')).toHaveLength(1)
    const first = host.querySelector('.paint')

    // The second tap starts at once. The first frame stays under it.
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    expect(host.querySelectorAll('.paint')).toHaveLength(2)
    expect(host.querySelector('.paint')).toBe(first)
    await rerender({ page: page(), busy: 2 }); markCommitted(); clearDraft(); await tick()
    expect(host.querySelectorAll('.paint')).toHaveLength(2)

    // The first commit lands and its tiles load. The second is still in
    // flight, so nothing is let go: the tiles do not show it yet.
    const landed = { ...page(), tileRevision: 1 }
    tile.dataset.version = tile.dataset.loadedVersion = pageVersion(landed, 'cleaned')
    await rerender({ page: landed, busy: 1 })
    document.dispatchEvent(new CustomEvent('paint-tile-loaded')); await tick()
    expect(host.querySelectorAll('.paint')).toHaveLength(2)

    const second = { ...page(), tileRevision: 2 }
    tile.dataset.version = tile.dataset.loadedVersion = pageVersion(second, 'cleaned')
    await rerender({ page: second, busy: 0 })
    document.dispatchEvent(new CustomEvent('paint-tile-loaded')); await tick()
    expect(host.querySelectorAll('.paint')).toHaveLength(0)
  })
  it('keeps the frames when one commit changes nothing while another is in flight', async () => {
    const { host, rerender } = await setup(vi.fn(async s => result(s)))
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    await rerender({ page: page(), busy: 2 }); markCommitted(); clearDraft(); await tick()
    await rerender({ page: page(), busy: 1, dropped: 1 })
    expect(host.querySelector('.paint')).not.toBeNull()
    await rerender({ page: page(), busy: 0, dropped: 2 })
    expect(host.querySelector('.paint')).toBeNull()
  })
})

// The preview is a see-through layer of the painted pixels alone. It used to
// be an opaque crop of the stroke's whole box over a paper background, which
// redrew the untouched art inside the box at another scale.
describe('a paint frame on its canvas', () => {
  it('lays nothing opaque over the page', async () => {
    const { host } = await setup(vi.fn(async s => result(s)))
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    markCommitted(); clearDraft(); await tick()
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    // A held frame and a live one. Each is a canvas, which is see-through
    // wherever nothing was drawn on it, and nothing gives one a background.
    const layer = host.querySelector('.paints')
    const nodes = [...layer.querySelectorAll('*')]
    expect(nodes.map(node => node.tagName)).toEqual(['CANVAS', 'CANVAS'])
    for (const node of [layer, ...nodes]) expect([node.style.background, node.style.backgroundColor]).toEqual(['', ''])
    // All that goes onto one is the renderer's own picture, once.
    expect(draws.map(({ image, x, y, canvas }) => [image, x, y, canvas])).toEqual([[bitmaps[0], 0, 0, nodes[0]], [bitmaps[1], 0, 0, nodes[1]]])
    // From the project root: under jsdom `import.meta.url` is not a file URL.
    const source = readFileSync(join(process.cwd(), 'src/lib/editor/PaintLayer.svelte'), 'utf8')
    const css = source.slice(source.indexOf('<style>'))
    expect(css).toMatch(/\.paint\s*\{/)
    expect(css).not.toMatch(/background/)
  })
  it('changes its size, place and pixels together, and only once the frame has decoded', async () => {
    const decodes = []
    vi.stubGlobal('createImageBitmap', vi.fn(() => { const next = deferred(); decodes.push(next); return next.promise }))
    const boxes = [{ x: 20, y: 20, w: 10, h: 10 }, { x: 15, y: 18, w: 20, h: 14 }]
    let asked = 0
    const { host } = await setup(vi.fn(async s => ({ ...result(s), bounds: boxes[asked++] })))

    // Answered, and still decoding: nothing is on the sheet yet.
    draft.active = stroke('page-0'); await tick()
    await vi.waitFor(() => expect(decodes).toHaveLength(1)); await settled(); await tick()
    expect(host.querySelector('.paint')).toBeNull()
    const first = bitmap(40, 30)
    decodes[0].resolve(first); await flushPaintPreview('page-0'); await tick()
    const canvas = host.querySelector('.paint')
    expect(placed(canvas)).toEqual([40, 30, '20%', '20%', '10%', '10%'])
    expect(draws).toEqual([{ image: first, x: 0, y: 0, canvas, at: [40, 30, '20%', '20%', '10%', '10%'] }])

    // The stroke moves on. Its next frame is another size in another place,
    // and until that has decoded the canvas is exactly what it was.
    draft.active.points.push({ x: 30, y: 30 }); await tick()
    await vi.waitFor(() => expect(decodes).toHaveLength(2)); await settled(); await tick()
    expect(host.querySelector('.paint')).toBe(canvas)
    expect(placed(canvas)).toEqual([40, 30, '20%', '20%', '10%', '10%'])
    expect(draws).toHaveLength(1)

    // Nothing below waits on a timer or forces a render, so the canvas is whole
    // again within the task the decode finished in. The draw saw the new size
    // and place.
    const second = bitmap(80, 56)
    decodes[1].resolve(second); await flushPaintPreview('page-0')
    expect(host.querySelectorAll('.paint')).toHaveLength(1)
    expect(host.querySelector('.paint')).toBe(canvas)
    expect(placed(canvas)).toEqual([80, 56, '15%', '18%', '20%', '14%'])
    expect(draws).toHaveLength(2)
    expect(draws[1]).toEqual({ image: second, x: 0, y: 0, canvas, at: [80, 56, '15%', '18%', '20%', '14%'] })
  })
  it('does not finish a flush until the final frame has decoded and is there to hold', async () => {
    const decoding = deferred()
    vi.stubGlobal('createImageBitmap', vi.fn(() => decoding.promise))
    const { host } = await setup(vi.fn(async s => result(s)))
    draft.active = stroke('page-0'); await tick()
    let flushed = false
    const flush = flushPaintPreview('page-0').then(() => { flushed = true })
    await vi.waitFor(() => expect(globalThis.createImageBitmap).toHaveBeenCalledTimes(1)); await settled()
    expect(flushed).toBe(false)
    decoding.resolve(bitmap()); await flush
    // The commit seam drops the draft straight after the flush, with no wait.
    markCommitted(); clearDraft(); await tick()
    expect(host.querySelectorAll('.paint')).toHaveLength(1)
    expect(draws).toHaveLength(1)
  })
  it('keeps the live frame\'s canvas, untouched, when the frame is held', async () => {
    const { host } = await setup(vi.fn(async s => result(s)))
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    const canvas = host.querySelector('.paint')
    expect(draws).toHaveLength(1)
    markCommitted(); clearDraft(); await tick()
    expect(host.querySelectorAll('.paint')).toHaveLength(1)
    expect(host.querySelector('.paint')).toBe(canvas)
    // Not drawn again either: a redraw sets the size, which clears a canvas.
    expect(draws).toHaveLength(1)
    expect(bitmaps[0].close).not.toHaveBeenCalled()
  })
})

describe('a decoded frame is closed when it is let go', () => {
  it('replaced by the next frame, dropped while held, or abandoned', async () => {
    const { rerender } = await setup(vi.fn(async s => result(s)))
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    draft.active.points.push({ x: 30, y: 30 }); await tick(); await flushPaintPreview('page-0'); await tick()
    expect(bitmaps).toHaveLength(2)
    expect(bitmaps[0].close).toHaveBeenCalledTimes(1)
    expect(bitmaps[1].close).not.toHaveBeenCalled()

    markCommitted(); clearDraft(); await tick()
    expect(bitmaps[1].close).not.toHaveBeenCalled()
    await rerender({ page: page(), dropped: 1 })
    expect(bitmaps[1].close).toHaveBeenCalledTimes(1)

    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    expect(bitmaps[2].close).not.toHaveBeenCalled()
    clearDraft(); await tick()
    expect(bitmaps.map(made => made.close.mock.calls.length)).toEqual([1, 1, 1])
  })
  it('superseded while it was on its way', async () => {
    const first = deferred()
    const request = vi.fn().mockReturnValueOnce(first.promise).mockImplementation(async s => result(s))
    await setup(request)
    draft.active = stroke('page-0'); await tick()
    draft.active.points.push({ x: 30, y: 30 }); await tick()
    first.resolve(result(request.mock.calls[0][0])); await flushPaintPreview('page-0'); await tick()
    expect(request).toHaveBeenCalledTimes(2)
    expect(bitmaps.map(made => made.close.mock.calls.length)).toEqual([1, 0])
    expect(draws.map(({ image }) => image)).toEqual([bitmaps[1]])
  })
  it('canceled while it was on its way, as soon as it has decoded', async () => {
    const late = deferred(), request = vi.fn(() => late.promise)
    const { unmount } = await setup(request)
    draft.active = stroke('page-0'); await tick()
    clearDraft(); await tick()
    late.resolve(result(request.mock.calls[0][0])); await settled()
    // The layer is still mounted and no other frame is asked for: nothing
    // later would have closed it.
    expect(bitmaps).toHaveLength(1)
    expect(bitmaps[0].close).toHaveBeenCalledTimes(1)
    expect(draws).toHaveLength(0)
    unmount()
    expect(bitmaps[0].close).toHaveBeenCalledTimes(1)
  })
  it('decoded after the layer is gone', async () => {
    const late = deferred(), request = vi.fn(() => late.promise)
    const { unmount } = await setup(request)
    draft.active = stroke('page-0'); await tick()
    unmount()
    late.resolve(result(request.mock.calls[0][0])); await settled()
    expect(bitmaps).toHaveLength(1)
    expect(bitmaps[0].close).toHaveBeenCalledTimes(1)
  })
})
