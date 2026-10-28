import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '@testing-library/svelte'
import { tick } from 'svelte'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import PaintLayer from './PaintLayer.svelte'
import { clearDraft, draft, markCommitted, resetDraftState } from './draft.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { pageVersion } from '../api/tile.js'
import { flushPaintPreview } from './paintpreview.js'
import { eraseMasks } from './erasepreview.svelte.js'

const page = (index = 0) => ({ id: `page-${index}`, chapterId: 'c', index, width: 100, height: 100, regions: [] })
// Both strokes are drawn in the layer. A clone stroke shows the cleaned tiles
// through its coverage; a Brush stroke shows its colour.
const stroke = (id, offset = { x: 15, y: 15 }) => ({ pageId: id, kind: 'stroke', tool: 'cloneStamp', points: [{ x: 25, y: 25, p: 1 }],
  clone: { source: { x: 25 + offset.x, y: 25 + offset.y }, offset } })
const brushStroke = (id) => ({ pageId: id, kind: 'stroke', tool: 'brush', points: [{ x: 25, y: 25, p: 1 }] })
/** A canvas's size and place: what has to change in the same step as its pixels. */
const placed = canvas => [canvas.width, canvas.height, canvas.style.left, canvas.style.top, canvas.style.width, canvas.style.height]
/** Every box is the sheet's: 100 by 100 at the origin. */
const box = { left: 0, top: 0, right: 100, bottom: 100, width: 100, height: 100 }

/** Every picture drawn onto a canvas, with the size and place the canvas had and the compositing then. */
let draws = []
/** Every coverage tile put onto a canvas, the same way. */
let puts = []

/** A cleaned tile on the sheet, loaded and on screen. */
function cleanedTile(host) {
  const stack = document.createElement('div'); stack.dataset.artwork = 'cleaned'; host.append(stack)
  const tile = document.createElement('img'); stack.append(tile)
  Object.defineProperty(tile, 'complete', { value: true })
  Object.defineProperty(tile, 'naturalWidth', { value: 100 })
  return tile
}

async function setup(props = {}) {
  editor.chapter = { id: 'c' }
  editor.toolParams.brush = { mode: 'paint', size: 10, color: '#123456' }
  editor.toolParams.cloneStamp = { size: 10, hardness: 100 }
  editor.toolParams.eraser = { size: 10, hardness: 100, opacity: 100 }
  const host = document.createElement('div'); host.className = 'sheet'; document.body.append(host)
  return { host, ...render(PaintLayer, { target: host, props: { page: page(), ...props } }) }
}
beforeEach(() => {
  draws = []; puts = []
  vi.stubGlobal('ImageData', class { constructor(data, width, height) { Object.assign(this, { data, width, height }) } })
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue(/** @type {any} */ (box))
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(/** @type {any} */ (function () {
    const canvas = this
    const context = {
      globalAlpha: 1,
      globalCompositeOperation: 'source-over',
      clearRect: () => {},
      drawImage: (image, x, y, w, h) => draws.push({ image, x, y, w, h, canvas, op: context.globalCompositeOperation, at: placed(canvas) }),
      putImageData: (image, x, y) => puts.push({ image, x, y, canvas, at: placed(canvas) }),
    }
    return context
  }))
})
afterEach(() => { cleanup(); resetDraftState(); editor.chapter = null; vi.restoreAllMocks(); vi.unstubAllGlobals(); document.body.innerHTML = '' })

describe('a clone stroke, drawn in the layer', () => {
  it('draws the cleaned tiles, moved by the offset, through the stroke\'s coverage', async () => {
    const { host } = await setup()
    const tile = cleanedTile(host)
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    const canvas = host.querySelector('.paint')
    // One 10 px dab at 25% of a 100 px page: the first 128 px coverage tile.
    expect(placed(canvas)).toEqual([128, 128, '0%', '0%', '128%', '128%'])
    // The page is drawn 15 px up and left, so canvas pixel 25 shows page
    // pixel 40, and then kept only where the coverage is.
    const onCanvas = draws.filter(draw => draw.canvas === canvas)
    expect(onCanvas.map(({ image, x, y, w, h, op, at }) => [image, x, y, w, h, op, at])).toEqual([
      [tile, -15, -15, 100, 100, 'source-over', placed(canvas)],
      [expect.any(HTMLCanvasElement), 0, 0, undefined, undefined, 'destination-in', placed(canvas)],
    ])
    // The coverage went onto that other canvas, opaque in the dab's middle.
    const mask = onCanvas[1].image
    expect(puts.every(put => put.canvas === mask)).toBe(true)
    const middle = (25 * 128 + 25) * 4
    expect(puts[0].image.data[middle + 3]).toBe(255)

    draft.active.points.push({ x: 40, y: 25, p: 1 }); await tick(); await flushPaintPreview('page-0'); await tick()
    expect(host.querySelector('.paint')).toBe(canvas)
    expect(draws.filter(draw => draw.canvas === canvas)).toHaveLength(4)
  })
  it('moves by whole page pixels, rounded as the commit rounds them', async () => {
    const { host } = await setup()
    const tile = cleanedTile(host)
    draft.active = stroke('page-0', { x: 15.5, y: -2.5 }); await tick(); await flushPaintPreview('page-0'); await tick()
    const first = draws.find(draw => draw.image === tile)
    expect([first.x, first.y]).toEqual([-16, 3])
  })
  it('draws nothing before it has a source', async () => {
    const { host } = await setup()
    draft.active = { ...stroke('page-0'), clone: null }; await tick(); await flushPaintPreview('page-0'); await tick()
    expect(host.querySelector('.paint')).toBeNull()
  })
  it('empties every mask canvas of a stroke whose parameters changed mid-stroke', async () => {
    const { host } = await setup()
    cleanedTile(host)
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    const canvas = host.querySelector('.paint')
    const masksOf = () => [...new Set(draws.filter(draw => draw.canvas === canvas && draw.op === 'destination-in').map(draw => draw.image))]
    const [first] = masksOf()
    expect(first.width).toBeGreaterThan(0)
    // A `[` mid-stroke resizes the stroke: a new mask, drawn on the same canvas.
    editor.toolParams.cloneStamp = { size: 20, hardness: 100 }
    await tick(); await flushPaintPreview('page-0'); await tick()
    expect(host.querySelector('.paint')).toBe(canvas)
    const masks = masksOf()
    expect(masks).toHaveLength(2)
    expect(masks.every(mask => mask.width > 0)).toBe(true)

    clearDraft(); await tick()
    expect(host.querySelector('.paint')).toBeNull()
    expect(masks.map(mask => [mask.width, mask.height])).toEqual([[0, 0], [0, 0]])
  })
  it('leaves out a tile that has not loaded, or is hidden for its replacement', async () => {
    const { host } = await setup()
    const shown = cleanedTile(host)
    const hidden = cleanedTile(host); hidden.style.visibility = 'hidden'
    const stack = host.querySelector('[data-artwork="cleaned"]')
    const loading = document.createElement('img'); stack.append(loading)
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    expect(draws.filter(draw => draw.image instanceof HTMLImageElement).map(draw => draw.image)).toEqual([shown])
  })
})

describe('mounted preview lifecycle', () => {
  it('holds final pixels until all affected tiles load the committed revision', async () => {
    const { host, rerender } = await setup()
    const tile = cleanedTile(host)
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    markCommitted(); clearDraft(); await tick()
    const committed = { ...page(), tileRevision: 1 }
    tile.dataset.version = pageVersion(committed, 'cleaned')
    await rerender({ page: committed })
    expect(host.querySelector('.paint')).not.toBeNull()
    tile.dataset.loadedVersion = pageVersion(page(), 'cleaned')
    document.dispatchEvent(new CustomEvent('paint-tile-loaded', { detail: { pageId: 'page-0' } })); await tick()
    expect(host.querySelector('.paint')).not.toBeNull()
    tile.dataset.loadedVersion = pageVersion(committed, 'cleaned')
    document.dispatchEvent(new CustomEvent('paint-tile-loaded', { detail: { pageId: 'page-0' } })); await tick()
    expect(host.querySelector('.paint')).toBeNull()
  })
  it('waits for affected neighboring-page tiles as well as the anchor', async () => {
    const { host, rerender } = await setup()
    const stage = document.createElement('div'); stage.className = 'stage'; host.replaceWith(stage); stage.append(host)
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
    const { host, rerender } = await setup()
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    markCommitted(); clearDraft(); await tick()
    expect(host.querySelector('.paint')).not.toBeNull()
    await rerender({ page: page(), dropped: 1 })
    expect(host.querySelector('.paint')).toBeNull()
  })
  it('drops the frame of a stroke that was abandoned, not committed', async () => {
    const { host } = await setup()
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    clearDraft(); await tick()
    expect(host.querySelector('.paint')).toBeNull()
  })
  it('keeps the live frame\'s canvas, not drawn again, when the frame is held', async () => {
    const { host } = await setup()
    cleanedTile(host)
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    const canvas = host.querySelector('.paint')
    const before = draws.length
    markCommitted(); clearDraft(); await tick()
    expect(host.querySelectorAll('.paint')).toHaveLength(1)
    expect(host.querySelector('.paint')).toBe(canvas)
    // The tiles under a held clone may already be the committed ones, and a
    // redraw would read the stroke's own pixels back.
    document.dispatchEvent(new CustomEvent('paint-tile-loaded')); await new Promise(requestAnimationFrame); await tick()
    expect(draws).toHaveLength(before)
  })
  // The stamp is used in quick taps. Each tap is a commit of its own, and the
  // next one starts before the tiles of the last have caught up.
  it('keeps a held frame while the next stroke is drawn and committed', async () => {
    const { host, rerender } = await setup({ busy: 0 })
    const tile = cleanedTile(host)
    tile.dataset.version = tile.dataset.loadedVersion = pageVersion(page(), 'cleaned')

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
    const { host, rerender } = await setup()
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    await rerender({ page: page(), busy: 2 }); markCommitted(); clearDraft(); await tick()
    await rerender({ page: page(), busy: 1, dropped: 1 })
    expect(host.querySelector('.paint')).not.toBeNull()
    await rerender({ page: page(), busy: 0, dropped: 2 })
    expect(host.querySelector('.paint')).toBeNull()
  })
  it('lays nothing opaque over the page', async () => {
    const { host } = await setup()
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    markCommitted(); clearDraft(); await tick()
    draft.active = brushStroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    // A held frame and a live one. Each is a canvas, which is see-through
    // wherever nothing was drawn on it, and nothing gives one a background.
    const layer = host.querySelector('.paints')
    const nodes = [...layer.querySelectorAll('*')]
    expect(nodes.map(node => node.tagName)).toEqual(['CANVAS', 'CANVAS'])
    for (const node of [layer, ...nodes]) expect([node.style.background, node.style.backgroundColor]).toEqual(['', ''])
    // From the project root: under jsdom `import.meta.url` is not a file URL.
    const source = readFileSync(join(process.cwd(), 'src/lib/editor/PaintLayer.svelte'), 'utf8')
    const css = source.slice(source.indexOf('<style>'))
    expect(css).toMatch(/\.paint\s*\{/)
    expect(css).not.toMatch(/background/)
  })
})

describe('a Brush stroke, drawn in the layer', () => {
  it('draws as the points arrive, with no native round trip', async () => {
    const { host } = await setup()
    draft.active = brushStroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    const canvas = host.querySelector('.paint')
    expect(canvas).not.toBeNull()
    expect(puts.length).toBeGreaterThan(0)
    expect(puts.every(put => put.canvas === canvas)).toBe(true)
    // One dab of a 10 px brush on a 100 px page, at 25%: the canvas covers
    // the first 128 px tile, which is past the page's own edge.
    expect(placed(canvas)).toEqual([128, 128, '0%', '0%', '128%', '128%'])
    // A paint pixel in the dab's middle carries the brush colour, opaque.
    const tile = puts[0].image
    const middle = (25 * 128 + 25) * 4
    expect([...tile.data.slice(middle, middle + 4)]).toEqual([0x12, 0x34, 0x56, 255])

    const before = puts.length
    draft.active.points.push({ x: 40, y: 25, p: 1 }); await tick(); await flushPaintPreview('page-0'); await tick()
    expect(host.querySelector('.paint')).toBe(canvas)
    expect(puts.length).toBe(before + 1)
  })
  it('holds the stroke\'s canvas once committed and lets it go when the tiles show it', async () => {
    const { host, rerender } = await setup()
    const tile = cleanedTile(host)
    draft.active = brushStroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    const canvas = host.querySelector('.paint')
    markCommitted(); clearDraft(); await tick()
    expect(host.querySelector('.paint')).toBe(canvas)
    const committed = { ...page(), tileRevision: 1 }
    tile.dataset.version = tile.dataset.loadedVersion = pageVersion(committed, 'cleaned')
    await rerender({ page: committed })
    document.dispatchEvent(new CustomEvent('paint-tile-loaded')); await tick()
    expect(host.querySelector('.paint')).toBeNull()
  })
  it('drops an abandoned stroke', async () => {
    const { host } = await setup()
    draft.active = brushStroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    clearDraft(); await tick()
    expect(host.querySelector('.paint')).toBeNull()
  })
})

describe('an eraser stroke', () => {
  const eraserStroke = (id) => ({ pageId: id, kind: 'stroke', tool: 'eraser', points: [{ x: 25, y: 25, p: 1 }] })

  it('draws its coverage on a canvas that is never shown, for the Brush layers to take off', async () => {
    const { host } = await setup()
    draft.active = eraserStroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    const canvas = host.querySelector('.paint')
    expect(canvas.classList.contains('erase')).toBe(true)
    const [mask] = eraseMasks.get('page-0')
    expect(mask.canvas).toBe(canvas)
    expect(mask.box).toEqual({ x: 0, y: 0, w: 128, h: 128 })
    // Coverage, opaque in the dab's middle.
    const middle = (25 * 128 + 25) * 4
    expect(puts[0].image.data[middle + 3]).toBe(255)

    draft.active.points.push({ x: 40, y: 25, p: 1 }); await tick(); await flushPaintPreview('page-0'); await tick()
    expect(eraseMasks.get('page-0')[0].tick).toBeGreaterThan(mask.tick)
  })

  it('is held only while its commit is in flight', async () => {
    const { host, rerender } = await setup({ busy: 1 })
    draft.active = eraserStroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    markCommitted(); clearDraft(); await tick()
    expect(eraseMasks.get('page-0')).toHaveLength(1)
    await rerender({ busy: 0 }); await tick()
    expect(host.querySelector('.paint')).toBeNull()
    expect(eraseMasks.has('page-0')).toBe(false)
  })

  it('drops an abandoned stroke', async () => {
    await setup()
    draft.active = eraserStroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    clearDraft(); await tick()
    expect(eraseMasks.has('page-0')).toBe(false)
  })
})
