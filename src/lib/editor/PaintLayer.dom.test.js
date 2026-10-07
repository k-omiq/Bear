import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '@testing-library/svelte'
import { tick } from 'svelte'
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
async function setup(request) {
  editor.chapter = { id: 'c' }
  editor.toolParams.brush = { mode: 'paint', size: 10, color: '#123456' }
  vi.stubGlobal('URL', { createObjectURL: vi.fn(() => 'blob:preview'), revokeObjectURL: vi.fn() })
  setBackend({ previewPaint: request })
  const host = document.createElement('div'); host.className = 'sheet'; document.body.append(host)
  return { host, ...render(PaintLayer, { target: host, props: { page: page() } }) }
}
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
  })
  it('rejects a response after the mounted page changes', async () => {
    const late = deferred(), request = vi.fn(() => late.promise)
    const { host, rerender } = await setup(request)
    draft.active = stroke('page-0'); await tick()
    await rerender({ page: page(1) })
    late.resolve(result(request.mock.calls[0][0])); await Promise.resolve(); await tick()
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
    tile.dataset.loadedVersion = pageVersion(committed, 'cleaned')
    document.dispatchEvent(new CustomEvent('paint-tile-loaded', { detail: { pageId: 'page-0' } })); await tick()
    expect(host.querySelector('.paint')).toBeNull()
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
    let made = 0
    vi.stubGlobal('URL', { createObjectURL: vi.fn(() => `blob:${made += 1}`), revokeObjectURL: vi.fn() })
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

    // The second tap starts at once. The first frame stays under it.
    draft.active = stroke('page-0'); await tick(); await flushPaintPreview('page-0'); await tick()
    expect(host.querySelectorAll('.paint')).toHaveLength(2)
    await rerender({ page: page(), busy: 2 }); markCommitted(); clearDraft(); await tick()
    expect(host.querySelectorAll('.paint')).toHaveLength(2)

    // The first commit lands and its tiles load. The second is still in
    // flight, so nothing is let go: the tiles do not show it yet.
    const first = { ...page(), tileRevision: 1 }
    tile.dataset.version = tile.dataset.loadedVersion = pageVersion(first, 'cleaned')
    await rerender({ page: first, busy: 1 })
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
