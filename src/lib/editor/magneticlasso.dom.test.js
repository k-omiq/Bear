import { afterEach, expect, it, vi } from 'vitest'
import { artworkEdges } from './magneticlasso.js'

afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks() })

it('reads native artwork through decoded blobs, without drawing tainted protocol image elements', async () => {
  const blob = new Blob(['pixels'])
  const image = { close: vi.fn() }
  const drawn = vi.fn()
  vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true, blob: async () => blob })))
  vi.stubGlobal('createImageBitmap', vi.fn(async () => image))
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({
    fillRect: vi.fn(), drawImage: drawn,
    getImageData: () => ({ width: 10, height: 10, data: new Uint8ClampedArray(400) }),
  })
  const rect = { left: 0, top: 0, right: 10, bottom: 10, width: 10, height: 10 }
  const node = { src: 'tile://localhost/c1/0/cleaned/0', complete: true, naturalWidth: 10,
    style: {}, getBoundingClientRect: () => rect }
  const sheet = { getBoundingClientRect: () => rect,
    querySelector: () => ({ querySelectorAll: () => [node] }) }
  const map = await artworkEdges(sheet)
  expect(fetch).toHaveBeenCalledWith(node.src)
  expect(createImageBitmap).toHaveBeenCalledWith(blob)
  expect(drawn.mock.calls[0][0]).toBe(image)
  expect(image.close).toHaveBeenCalledOnce()
  expect(map.area).toEqual({ x: 0, y: 0, w: 100, h: 100 })
})
