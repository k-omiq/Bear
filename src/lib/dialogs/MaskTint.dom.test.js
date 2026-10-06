import { afterAll, afterEach, beforeAll, expect, it } from 'vitest'
import { cleanup, render, waitFor } from '@testing-library/svelte'
import MaskTint from './MaskTint.svelte'

/**
 * jsdom decodes no images and has no 2D canvas. The fakes below stand in for
 * both, faithfully enough to check the one thing that matters: which pixels
 * the tint covers. A raster is RGBA rows keyed by its data URL; `drawImage`
 * copies the requested source rectangle and `getImageData` hands it back.
 */
const rasters = new Map()
const calls = []
const RealImage = globalThis.Image
const realGetContext = HTMLCanvasElement.prototype.getContext
let noContext = false

class FakeImage {
  onload = null
  onerror = null
  naturalWidth = 0
  naturalHeight = 0
  set src(value) {
    const raster = rasters.get(value)
    queueMicrotask(() => {
      if (!raster) { this.onerror?.(); return }
      this.raster = raster
      this.naturalWidth = raster.width
      this.naturalHeight = raster.height
      this.onload?.()
    })
  }
}

function recorder(canvas) {
  let copied = null
  const context = {
    imageSmoothingEnabled: true,
    globalCompositeOperation: 'source-over',
    fillStyle: '',
    clearRect() {},
    drawImage(image, sx, sy, sw, sh, dx, dy, dw, dh) {
      calls.push({ canvas, op: 'drawImage', args: [sx, sy, sw, sh, dx, dy, dw, dh], smoothing: context.imageSmoothingEnabled })
      const { raster } = image
      const data = new Uint8ClampedArray(sw * sh * 4)
      for (let y = 0; y < sh; y += 1) {
        for (let x = 0; x < sw; x += 1) {
          const from = ((sy + y) * raster.width + sx + x) * 4
          data.set(raster.rgba.subarray(from, from + 4), (y * sw + x) * 4)
        }
      }
      copied = { width: sw, height: sh, data }
    },
    getImageData: () => copied,
    putImageData(image) { calls.push({ canvas, op: 'putImageData', image: { width: image.width, data: [...image.data] } }) },
    fillRect() { calls.push({ canvas, op: 'fillRect', composite: context.globalCompositeOperation }) },
  }
  return context
}

beforeAll(() => {
  globalThis.Image = /** @type {any} */ (FakeImage)
  HTMLCanvasElement.prototype.getContext = /** @type {any} */ (function () { return noContext ? null : recorder(this) })
})
afterAll(() => {
  globalThis.Image = RealImage
  HTMLCanvasElement.prototype.getContext = realGetContext
})
afterEach(() => {
  cleanup()
  rasters.clear()
  calls.length = 0
  noContext = false
})

/** A grayscale raster: one value per pixel, opaque unless listed in `clear`. */
function gray(src, width, height, values, clear = []) {
  const rgba = new Uint8ClampedArray(width * height * 4)
  values.forEach((value, index) => rgba.set([value, value, value, clear.includes(index) ? 0 : 255], index * 4))
  rasters.set(src, { width, height, rgba })
}

const alphaOf = (data) => data.filter((_value, index) => index % 4 === 3)

it('covers exactly the raster pixels that are not black, one canvas pixel per source pixel', async () => {
  // The fifth pixel is white but fully transparent: not in the set.
  gray('data:w', 3, 2, [0, 255, 7, 0, 255, 255], [4])
  const screen = render(MaskTint, { props: { src: 'data:w', bounds: { x: 30, y: 20, w: 3, h: 2 },
    pageWidth: 64, pageHeight: 48, tone: 'write' } })
  const canvas = screen.container.querySelector('canvas[data-tint="write"]')
  await waitFor(() => expect(canvas.dataset.state).toBe('drawn'))
  expect(canvas.width).toBe(3)
  expect(canvas.height).toBe(2)
  expect(canvas.style.left).toBe(`${30 / 64 * 100}%`)
  expect(canvas.style.top).toBe(`${20 / 48 * 100}%`)
  expect(canvas.style.width).toBe(`${3 / 64 * 100}%`)
  expect(canvas.style.height).toBe(`${2 / 48 * 100}%`)

  const draw = calls.find((call) => call.op === 'drawImage')
  expect(draw.args).toEqual([0, 0, 3, 2, 0, 0, 3, 2])
  expect(draw.smoothing).toBe(false)
  const put = calls.find((call) => call.op === 'putImageData')
  expect(alphaOf(put.image.data)).toEqual([0, 255, 255, 0, 0, 255])
  // Colour comes last, only where the set is: source-in over the alpha.
  expect(calls.find((call) => call.op === 'fillRect').composite).toBe('source-in')
})

it('draws nothing from a raster that is not exactly the size it claims to cover', async () => {
  gray('data:wrong', 4, 2, [255, 255, 255, 255, 255, 255, 255, 255])
  const screen = render(MaskTint, { props: { src: 'data:wrong', bounds: { x: 0, y: 0, w: 3, h: 2 },
    pageWidth: 64, pageHeight: 48, tone: 'evidence' } })
  const canvas = screen.container.querySelector('canvas[data-tint="evidence"]')
  await waitFor(() => expect(canvas.dataset.state).toBe('refused'))
  expect(calls.filter((call) => call.op === 'putImageData')).toHaveLength(0)
})

it('splits a page wider than one tile into canvases that meet without overlap', async () => {
  const width = 3000
  const values = Array.from({ length: width * 2 }, (_value, index) => (index % width === 2047 || index % width === 2048 ? 255 : 0))
  gray('data:wide', width, 2, values)
  const screen = render(MaskTint, { props: { src: 'data:wide', bounds: { x: 0, y: 0, w: width, h: 2 },
    pageWidth: width, pageHeight: 2, tone: 'evidence' } })
  const canvases = [...screen.container.querySelectorAll('canvas')]
  expect(canvases).toHaveLength(2)
  await waitFor(() => expect(canvases.every((canvas) => canvas.dataset.state === 'drawn')).toBe(true))
  expect(canvases.map((canvas) => canvas.width)).toEqual([2048, 952])
  expect(canvases[1].style.left).toBe(`${2048 / width * 100}%`)
  const puts = calls.filter((call) => call.op === 'putImageData')
  // The two lit columns sit either side of the seam: the last of tile one, the first of tile two.
  expect(alphaOf(puts[0].image.data).flatMap((value, index) => (value ? [index % 2048] : []))).toEqual([2047, 2047])
  expect(alphaOf(puts[1].image.data).flatMap((value, index) => (value ? [index % 952] : []))).toEqual([0, 0])
})

it('leaves the page untinted where a 2D canvas is unavailable', async () => {
  noContext = true
  gray('data:w', 1, 1, [255])
  const screen = render(MaskTint, { props: { src: 'data:w', bounds: { x: 0, y: 0, w: 1, h: 1 },
    pageWidth: 10, pageHeight: 10, tone: 'evidence' } })
  const canvas = screen.container.querySelector('canvas')
  await waitFor(() => expect(canvas.dataset.state).toBe('refused'))
})
