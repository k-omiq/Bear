/** A bounded edge map and shortest path for the regional magnetic lasso.
 * All tracing happens on a small display snapshot, never the full scan. */
import { decodeImage } from '../api/boundedimage.js'

export function edgeMap({ data, width, height }) {
  const gray = new Float32Array(width * height)
  const edges = new Float32Array(width * height)
  for (let i = 0; i < gray.length; i++) gray[i] = data[i * 4] * .2126 + data[i * 4 + 1] * .7152 + data[i * 4 + 2] * .0722
  for (let y = 1; y < height - 1; y++) {
    for (let x = 1; x < width - 1; x++) {
      const i = y * width + x
      const gx = gray[i - width + 1] + 2 * gray[i + 1] + gray[i + width + 1] - gray[i - width - 1] - 2 * gray[i - 1] - gray[i + width - 1]
      const gy = gray[i + width - 1] + 2 * gray[i + width] + gray[i + width + 1] - gray[i - width - 1] - 2 * gray[i - width] - gray[i - width + 1]
      edges[i] = Math.min(1, Math.hypot(gx, gy) / 510)
    }
  }
  return { width, height, edges }
}

/** A readable snapshot of the visible native cleaned artwork. */
export async function artworkEdges(sheet) {
  const box = sheet?.getBoundingClientRect()
  if (!box?.width || !box.height) return null
  // A tall webtoon needs the pixels around the visible selection, rather
  // than a whole segment shrunk until its ink is one pixel wide.
  const left = Math.max(box.left, -128), top = Math.max(box.top, -128)
  const right = Math.min(box.right, window.innerWidth + 128)
  const bottom = Math.min(box.bottom, window.innerHeight + 128)
  const extent = { width: right - left, height: bottom - top }
  if (extent.width <= 0 || extent.height <= 0) return null
  const scale = Math.min(1, 768 / Math.max(extent.width, extent.height))
  const canvas = document.createElement('canvas')
  canvas.width = Math.max(1, Math.round(extent.width * scale))
  canvas.height = Math.max(1, Math.round(extent.height * scale))
  const ctx = canvas.getContext('2d', { willReadFrequently: true })
  if (!ctx) return null
  ctx.fillStyle = '#fff'
  ctx.fillRect(0, 0, canvas.width, canvas.height)
  const artwork = sheet.querySelector('[data-artwork="cleaned"]')
  let drawn = false
  try {
    const tiles = [...(artwork?.querySelectorAll('img') ?? [])].filter(node =>
      node.style.visibility !== 'hidden' && node.complete && node.naturalWidth > 0)
    const pictures = await Promise.all(tiles.map(async node => {
      const tile = node.getBoundingClientRect()
      if (tile.right <= left || tile.left >= right || tile.bottom <= top || tile.top >= bottom) return null
      // Drawing a tile:// image element taints a canvas. Decode the fetched
      // blob instead, as the native patch renderer does, to allow edge reads.
      try {
        const response = await fetch(node.currentSrc || node.src)
        if (!response.ok) return null
        return { image: await decodeImage(await response.blob()), tile }
      } catch { return null }
    }))
    for (const picture of pictures) {
      if (!picture) continue
      const { image, tile } = picture
      try {
        ctx.drawImage(image, (tile.left - left) * scale, (tile.top - top) * scale, tile.width * scale, tile.height * scale)
      } finally { image.close?.() }
      drawn = true
    }
    return drawn ? { ...edgeMap(ctx.getImageData(0, 0, canvas.width, canvas.height)),
      area: { x: (left - box.left) / box.width * 100, y: (top - box.top) / box.height * 100,
        w: extent.width / box.width * 100, h: extent.height / box.height * 100 } } : null
  } catch { return null }
}

/** Page-percent points in and out. Curved high-contrast edges cost less than
 * crossing flat artwork. A corridor and a visit limit keep pointer work bounded. */
export function magneticPath(map, from, to) {
  if (!map || !from || !to) return [to]
  const { width, height, edges } = map
  const area = map.area ?? { x: 0, y: 0, w: 100, h: 100 }
  const contains = p => p.x >= area.x && p.y >= area.y && p.x <= area.x + area.w && p.y <= area.y + area.h
  if (!contains(from) || !contains(to)) return [to]
  const pixel = (p) => ({ x: Math.round((p.x - area.x) * (width - 1) / area.w), y: Math.round((p.y - area.y) * (height - 1) / area.h) })
  const a = pixel(from), b = pixel(to)
  if (a.x === b.x && a.y === b.y) return [to]
  const x0 = Math.max(0, Math.min(a.x, b.x) - 24), y0 = Math.max(0, Math.min(a.y, b.y) - 24)
  const x1 = Math.min(width - 1, Math.max(a.x, b.x) + 24), y1 = Math.min(height - 1, Math.max(a.y, b.y) + 24)
  const w = x1 - x0 + 1, h = y1 - y0 + 1
  if (w * h > 65536) return [to]
  const costs = new Float32Array(w * h).fill(Infinity)
  const parents = new Int32Array(w * h).fill(-1)
  const start = (a.y - y0) * w + a.x - x0, goal = (b.y - y0) * w + b.x - x0
  const heap = []
  function push(item) {
    let i = heap.length
    heap.push(item)
    while (i > 0) {
      const p = (i - 1) >> 1
      if (heap[p].score <= item.score) break
      heap[i] = heap[p]; i = p
    }
    heap[i] = item
  }
  function pop() {
    const first = heap[0], last = heap.pop()
    if (heap.length) {
      let i = 0
      while (i * 2 + 1 < heap.length) {
        let child = i * 2 + 1
        if (child + 1 < heap.length && heap[child + 1].score < heap[child].score) child++
        if (heap[child].score >= last.score) break
        heap[i] = heap[child]; i = child
      }
      heap[i] = last
    }
    return first
  }
  costs[start] = 0
  push({ id: start, cost: 0, score: 0 })
  let visits = 0, reached = false
  while (heap.length && visits++ < 12000) {
    const current = pop()
    if (current.cost > costs[current.id] + .001) continue
    if (current.id === goal) { reached = true; break }
    const x = current.id % w, y = Math.floor(current.id / w)
    for (const [dx, dy] of [[-1, 0], [1, 0], [0, -1], [0, 1], [-1, -1], [1, -1], [-1, 1], [1, 1]]) {
      const nx = x + dx, ny = y + dy
      if (nx < 0 || ny < 0 || nx >= w || ny >= h) continue
      const id = ny * w + nx, gx = nx + x0, gy = ny + y0
      const strength = edges[gy * width + gx]
      const cost = current.cost + Math.hypot(dx, dy) * (1 + 7 * (1 - strength) ** 2)
      if (cost >= costs[id]) continue
      costs[id] = cost; parents[id] = current.id
      push({ id, cost: costs[id], score: costs[id] + Math.hypot(b.x - gx, b.y - gy) })
    }
  }
  if (!reached) return [to]
  const path = []
  for (let id = goal; id !== start && id !== -1; id = parents[id]) path.push({ x: area.x + ((id % w + x0) / Math.max(1, width - 1)) * area.w, y: area.y + ((Math.floor(id / w) + y0) / Math.max(1, height - 1)) * area.h })
  path.reverse()
  path[path.length - 1] = to
  // Keep changes in direction, so the native vector payload stays compact.
  return path.filter((p, i) => i === 0 || i === path.length - 1 || Math.abs((p.x - path[i - 1].x) * (path[i + 1].y - p.y) - (p.y - path[i - 1].y) * (path[i + 1].x - p.x)) > .001)
}
