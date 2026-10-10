import { describe, expect, it } from 'vitest'
import { edgeMap, magneticPath } from './magneticlasso.js'

describe('regional magnetic lasso', () => {
  it('finds artwork contrast and leaves flat paper quiet', () => {
    const width = 32, height = 32
    const data = new Uint8ClampedArray(width * height * 4)
    for (let y = 0; y < height; y++) for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4
      data[i] = data[i + 1] = data[i + 2] = x < 16 ? 0 : 255
      data[i + 3] = 255
    }
    const map = edgeMap({ data, width, height })
    expect(map.edges[16 * width + 15]).toBe(1)
    expect(map.edges[16 * width + 5]).toBe(0)
  })

  it('bends between anchors along a curved edge instead of joining them with a straight line', () => {
    const width = 96, height = 96, edges = new Float32Array(width * height)
    for (let x = 10; x <= 80; x++) {
      const y = Math.round(20 + 18 * Math.sin((x - 10) / 70 * Math.PI))
      for (let dy = -1; dy <= 1; dy++) edges[(y + dy) * width + x] = 1
    }
    const path = magneticPath({ width, height, edges }, { x: 10 / 95 * 100, y: 20 / 95 * 100 }, { x: 80 / 95 * 100, y: 20 / 95 * 100 })
    expect(path.length).toBeGreaterThan(3)
    expect(Math.max(...path.map((point) => point.y))).toBeGreaterThan(35)
    expect(path.at(-1)).toEqual({ x: 80 / 95 * 100, y: 20 / 95 * 100 })
  })

  it('handles unavailable artwork and coincident anchors without stalling', () => {
    const point = { x: 40, y: 40 }
    expect(magneticPath(null, point, point)).toEqual([point])
    expect(magneticPath({ width: 8, height: 8, edges: new Float32Array(64) }, point, point)).toEqual([point])
  })
})
