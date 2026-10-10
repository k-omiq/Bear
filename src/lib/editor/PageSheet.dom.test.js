import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { editor } from '../state/editor.svelte.js'
import { layerPreviews } from './layerpreview.svelte.js'
import { forgetLayerImages } from './patchlayers.svelte.js'
import PageSheet from './PageSheet.svelte'

afterEach(() => {
  cleanup()
  editor.chapter = null
  editor.project = null
  editor.selectionId = null
  layerPreviews.clear()
  forgetLayerImages()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

it('shows movable artwork for a selected regional copy and retains it until the flattened commit loads', async () => {
  vi.stubGlobal('__TAURI_INTERNALS__', { convertFileSrc: (_path, scheme) => `${scheme}://localhost/` })
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({ drawImage: vi.fn() })
  vi.stubGlobal('createImageBitmap', async () => ({ width: 160, height: 100 }))
  vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true, status: 200,
    headers: { get: () => '80,100,160,100' }, blob: async () => new Blob() })))
  const bbox = { x: 10, y: 10, w: 20, h: 10 }
  editor.project = { mode: 'single' }
  editor.chapter = { id: 'c1', pages: [{ id: 'c1-p001', chapterId: 'c1', index: 0,
    width: 800, height: 1000, sourceSha: 'aa', appearance: 'one', resident: true,
    regions: [{ id: 'r1', pageId: 'c1-p001', outcome: 'cleaned', bbox, source: 'hand',
      mask: { id: 'm1', regionId: 'r1', layerKey: 'one', sourceBbox: bbox,
        layer: { opacity: 100, offsetX: 0, offsetY: 0, rotation: 0 },
        provenance: { engine: 'paint', params_snapshot: { mode: 'region-copy' } },
        capabilities: { transform: 'movable', opacity: true, lock: true } } }] }] }
  editor.selectionId = 'r1'
  editor.tool = 'cloneStamp'
  editor.toolParams = { cloneStamp: { mode: 'region', shape: 'rect' } }
  editor.wipe = 100
  editor.originalVisible = false
  const page = editor.chapter.pages[0]
  const view = render(PageSheet, { page, width: 800 })
  await waitFor(() => expect(view.container.querySelector('canvas[data-layer="r1"]')?.width).toBe(160))
  for (const image of view.container.querySelectorAll('img[data-version]')) await fireEvent.load(image)
  const preview = view.container.querySelector('.regional-artwork')
  expect(preview.style.visibility).toBe('hidden')
  expect(preview.querySelector('img').src).toContain('/source/0?')
  expect(view.container.querySelector('.surface')).toBeNull()
  layerPreviews.set('r1', { offsetX: 80, offsetY: 0, rotation: 15 })
  await waitFor(() => expect(preview.style.visibility).toBe(''))
  await waitFor(() => expect(preview.querySelector('canvas').style.transform).toContain('rotate(15deg)'))
  page.appearance = 'two'
  page.regions[0].mask.layerKey = 'two'
  page.regions[0].mask.layer.offsetX = 80
  page.regions[0].mask.layer.rotation = 15
  layerPreviews.delete('r1')
  editor.selectionId = null
  await waitFor(() => expect(view.container.querySelector('.surface')).not.toBeNull())
  expect(view.container.querySelector('.regional-artwork')).toBe(preview)
  const committed = view.container.querySelector('.cleaned > [data-artwork="cleaned"] img[data-version]')
  await fireEvent.load(committed)
  await waitFor(() => expect(view.container.querySelector('.regional-artwork')).toBeNull())
})
