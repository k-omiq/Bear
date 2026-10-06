/**
 * A synthetic page for the text-shaped review, for the mock backend.
 *
 * The native review answers with a source page, a SAM mask raster and the
 * evidence found in it, all in source pixels. Outside a Tauri window there is
 * no model and no image decoder, so this draws a small comic page itself:
 * panels, tone, hatching, speech bubbles and blocky lettering from a 5 by 7
 * cell font, and it records which pixels are lettering as it draws them. The
 * mask is exactly those pixels, the components are its lines of lettering and
 * the detector boxes are the bubbles and lines around them, so every tint the
 * review draws can be checked against the page by eye at 1:1.
 *
 * Pure and synchronous: no canvas, no timers, no randomness. The PNGs are
 * written here, 8-bit grayscale, with a run-length fixed-Huffman deflate,
 * which is plenty for a page that is mostly paper.
 */

/* ------------------------------------------------------------------ */
/* PNG                                                                 */
/* ------------------------------------------------------------------ */

const CRC_TABLE = (() => {
  const table = new Uint32Array(256)
  for (let n = 0; n < 256; n += 1) {
    let c = n
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    table[n] = c >>> 0
  }
  return table
})()

/** @param {Uint8Array} bytes */
function crc32(bytes) {
  let c = 0xffffffff
  for (let i = 0; i < bytes.length; i += 1) c = CRC_TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

/** @param {Uint8Array} bytes */
function adler32(bytes) {
  let a = 1
  let b = 0
  for (let start = 0; start < bytes.length; start += 5552) {
    const end = Math.min(bytes.length, start + 5552)
    for (let i = start; i < end; i += 1) {
      a += bytes[i]
      b += a
    }
    a %= 65521
    b %= 65521
  }
  return ((b << 16) | a) >>> 0
}

/** LSB-first bit writer over a growing byte buffer. */
function bitWriter(capacity) {
  let out = new Uint8Array(Math.max(64, capacity))
  let length = 0
  let acc = 0
  let bits = 0
  const byte = (value) => {
    if (length === out.length) {
      const grown = new Uint8Array(out.length * 2)
      grown.set(out)
      out = grown
    }
    out[length] = value
    length += 1
  }
  return {
    byte,
    push(value, count) {
      acc |= value << bits
      bits += count
      while (bits >= 8) {
        byte(acc & 0xff)
        acc >>>= 8
        bits -= 8
      }
    },
    flush() {
      if (bits > 0) byte(acc & 0xff)
      acc = 0
      bits = 0
    },
    bytes: () => out.subarray(0, length),
  }
}

/** The fixed literal/length code of RFC 1951 3.2.6, bit-reversed for an LSB-first writer. */
const FIXED = (() => {
  const codes = new Uint16Array(288)
  const lengths = new Uint8Array(288)
  const reverse = (code, count) => {
    let result = 0
    for (let i = 0; i < count; i += 1) {
      result = (result << 1) | (code & 1)
      code >>= 1
    }
    return result
  }
  for (let symbol = 0; symbol < 288; symbol += 1) {
    const [base, first, count] = symbol < 144 ? [0x30, 0, 8]
      : symbol < 256 ? [0x190, 144, 9]
        : symbol < 280 ? [0, 256, 7]
          : [0xc0, 280, 8]
    codes[symbol] = reverse(base + symbol - first, count)
    lengths[symbol] = count
  }
  return { codes, lengths }
})()

const LENGTH_BASE = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258]
const LENGTH_EXTRA = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0]

/**
 * A zlib stream of `raw`: one fixed-Huffman block whose only matches are runs
 * of the previous byte (distance 1). A page row of paper is one literal and a
 * handful of matches.
 *
 * @param {Uint8Array} raw
 * @returns {Uint8Array}
 */
export function zlibDeflate(raw) {
  const out = bitWriter(raw.length >> 4)
  const symbol = (value) => out.push(FIXED.codes[value], FIXED.lengths[value])
  out.byte(0x78)
  out.byte(0x01)
  out.push(1, 1)
  out.push(1, 2)
  let i = 0
  while (i < raw.length) {
    if (i > 0) {
      const previous = raw[i - 1]
      let run = 0
      while (run < 258 && i + run < raw.length && raw[i + run] === previous) run += 1
      if (run >= 3) {
        let code = LENGTH_BASE.length - 1
        while (LENGTH_BASE[code] > run) code -= 1
        symbol(257 + code)
        if (LENGTH_EXTRA[code]) out.push(run - LENGTH_BASE[code], LENGTH_EXTRA[code])
        out.push(0, 5)
        i += run
        continue
      }
    }
    symbol(raw[i])
    i += 1
  }
  symbol(256)
  out.flush()
  const check = adler32(raw)
  out.byte((check >>> 24) & 0xff)
  out.byte((check >>> 16) & 0xff)
  out.byte((check >>> 8) & 0xff)
  out.byte(check & 0xff)
  return out.bytes()
}

/** @param {Uint8Array} bytes */
function base64(bytes) {
  let text = ''
  for (let i = 0; i < bytes.length; i += 0x8000) {
    text += String.fromCharCode.apply(null, /** @type {any} */ (bytes.subarray(i, i + 0x8000)))
  }
  return btoa(text)
}

/**
 * An 8-bit grayscale PNG as a data URL.
 *
 * @param {number} width
 * @param {number} height
 * @param {Uint8Array} pixels - `width * height` samples, row-major
 * @returns {string}
 */
export function grayPngDataUrl(width, height, pixels) {
  const raw = new Uint8Array((width + 1) * height)
  for (let y = 0; y < height; y += 1) {
    raw.set(pixels.subarray(y * width, (y + 1) * width), y * (width + 1) + 1)
  }
  const header = new Uint8Array(13)
  const view = new DataView(header.buffer)
  view.setUint32(0, width)
  view.setUint32(4, height)
  header[8] = 8
  const chunks = [chunk('IHDR', header), chunk('IDAT', zlibDeflate(raw)), chunk('IEND', new Uint8Array(0))]
  const size = 8 + chunks.reduce((sum, part) => sum + part.length, 0)
  const png = new Uint8Array(size)
  png.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
  let at = 8
  for (const part of chunks) {
    png.set(part, at)
    at += part.length
  }
  return `data:image/png;base64,${base64(png)}`
}

/** @param {string} type @param {Uint8Array} data */
function chunk(type, data) {
  const out = new Uint8Array(12 + data.length)
  const view = new DataView(out.buffer)
  view.setUint32(0, data.length)
  for (let i = 0; i < 4; i += 1) out[4 + i] = type.charCodeAt(i)
  out.set(data, 8)
  view.setUint32(8 + data.length, crc32(out.subarray(4, 8 + data.length)))
  return out
}

/* ------------------------------------------------------------------ */
/* Drawing                                                             */
/* ------------------------------------------------------------------ */

const INK = 17
const TONE = 200
const PAPER = 255

/** Five by seven cells, `X` inked. Only the letters the page says. */
const GLYPHS = Object.freeze({
  A: ['.XXX.', 'X...X', 'X...X', 'XXXXX', 'X...X', 'X...X', 'X...X'],
  D: ['XXXX.', 'X...X', 'X...X', 'X...X', 'X...X', 'X...X', 'XXXX.'],
  E: ['XXXXX', 'X....', 'X....', 'XXXX.', 'X....', 'X....', 'XXXXX'],
  G: ['.XXX.', 'X...X', 'X....', 'X.XXX', 'X...X', 'X...X', '.XXX.'],
  H: ['X...X', 'X...X', 'X...X', 'XXXXX', 'X...X', 'X...X', 'X...X'],
  I: ['XXXXX', '..X..', '..X..', '..X..', '..X..', '..X..', 'XXXXX'],
  M: ['X...X', 'XX.XX', 'X.X.X', 'X.X.X', 'X...X', 'X...X', 'X...X'],
  N: ['X...X', 'XX..X', 'X.X.X', 'X..XX', 'X...X', 'X...X', 'X...X'],
  O: ['.XXX.', 'X...X', 'X...X', 'X...X', 'X...X', 'X...X', '.XXX.'],
  R: ['XXXX.', 'X...X', 'X...X', 'XXXX.', 'X.X..', 'X..X.', 'X...X'],
  S: ['.XXXX', 'X....', 'X....', '.XXX.', '....X', '....X', 'XXXX.'],
  T: ['XXXXX', '..X..', '..X..', '..X..', '..X..', '..X..', '..X..'],
  W: ['X...X', 'X...X', 'X...X', 'X.X.X', 'X.X.X', 'XX.XX', 'X...X'],
  '?': ['.XXX.', 'X...X', '....X', '...X.', '..X..', '.....', '..X..'],
  '!': ['..X..', '..X..', '..X..', '..X..', '..X..', '.....', '..X..'],
})

/** Width of a line of lettering, in cells. */
function lineCells(text) {
  let cells = 0
  for (let i = 0; i < text.length; i += 1) {
    cells += text[i] === ' ' ? 3 : 5
    if (i < text.length - 1) cells += 1
  }
  return cells
}

function surface(width, height) {
  const gray = new Uint8Array(width * height).fill(PAPER)
  const inside = (x, y) => x >= 0 && y >= 0 && x < width && y < height
  const set = (x, y, value) => { if (inside(x, y)) gray[y * width + x] = value }
  return {
    gray,
    fill(x, y, w, h, value) {
      for (let row = Math.max(0, y); row < Math.min(height, y + h); row += 1) {
        gray.fill(value, row * width + Math.max(0, x), row * width + Math.min(width, x + w))
      }
    },
    frame(x, y, w, h, thickness, value) {
      this.fill(x, y, w, thickness, value)
      this.fill(x, y + h - thickness, w, thickness, value)
      this.fill(x, y, thickness, h, value)
      this.fill(x + w - thickness, y, thickness, h, value)
    },
    /** A filled ellipse with a ring of `stroke` on it; `fill` null leaves the inside as it is. */
    ellipse(cx, cy, rx, ry, fill, stroke, thickness) {
      const ox = rx + thickness / 2
      const oy = ry + thickness / 2
      const ix = rx - thickness / 2
      const iy = ry - thickness / 2
      for (let y = Math.floor(cy - oy); y <= Math.ceil(cy + oy); y += 1) {
        for (let x = Math.floor(cx - ox); x <= Math.ceil(cx + ox); x += 1) {
          const dx = x + 0.5 - cx
          const dy = y + 0.5 - cy
          if ((dx / ox) ** 2 + (dy / oy) ** 2 > 1) continue
          const inner = (dx / ix) ** 2 + (dy / iy) ** 2 <= 1
          if (!inner) set(x, y, stroke)
          else if (fill !== null) set(x, y, fill)
        }
      }
    },
    /** A straight stroke `thickness` wide, clipped to a rectangle. */
    line(x0, y0, x1, y1, thickness, value, clip) {
      const steps = Math.max(Math.abs(x1 - x0), Math.abs(y1 - y0), 1)
      const half = Math.floor(thickness / 2)
      for (let step = 0; step <= steps; step += 1) {
        const x = Math.round(x0 + (x1 - x0) * step / steps)
        const y = Math.round(y0 + (y1 - y0) * step / steps)
        for (let dy = -half; dy < thickness - half; dy += 1) {
          for (let dx = -half; dx < thickness - half; dx += 1) {
            const px = x + dx
            const py = y + dy
            if (px < clip.x || py < clip.y || px >= clip.x + clip.w || py >= clip.y + clip.h) continue
            set(px, py, value)
          }
        }
      }
    },
    /**
     * Letter `text` from its top-left corner and answer the inked pixels as
     * page indices, each once.
     */
    letter(text, left, top, cell) {
      const pixels = []
      let x = left
      for (const char of text) {
        if (char === ' ') { x += 4 * cell; continue }
        const rows = GLYPHS[char]
        if (rows) {
          for (let gy = 0; gy < 7; gy += 1) {
            for (let gx = 0; gx < 5; gx += 1) {
              if (rows[gy][gx] !== 'X') continue
              for (let py = top + gy * cell; py < top + (gy + 1) * cell; py += 1) {
                for (let px = x + gx * cell; px < x + (gx + 1) * cell; px += 1) {
                  if (!inside(px, py) || gray[py * width + px] === INK) continue
                  gray[py * width + px] = INK
                  pixels.push(py * width + px)
                }
              }
            }
          }
        }
        x += 6 * cell
      }
      return pixels
    },
  }
}

/** @param {number[]} pixels @param {number} width */
function boundsOf(pixels, width) {
  let left = Infinity
  let top = Infinity
  let right = -Infinity
  let bottom = -Infinity
  for (const at of pixels) {
    const x = at % width
    const y = Math.floor(at / width)
    if (x < left) left = x
    if (y < top) top = y
    if (x + 1 > right) right = x + 1
    if (y + 1 > bottom) bottom = y + 1
  }
  return left === Infinity ? null : { x: left, y: top, w: right - left, h: bottom - top }
}

/** The page layouts `pagebuilder.js` uses, as a fallback when a page has none. */
const DEFAULT_PANELS = Object.freeze([
  { x: 6, y: 4, w: 88, h: 27 },
  { x: 6, y: 34, w: 42, h: 30 },
  { x: 52, y: 34, w: 42, h: 30 },
  { x: 6, y: 67, w: 88, h: 29 },
])

/**
 * `reasons` per line are native `ReviewReason`s: the first bubble's second
 * line stands in for lettering whose mask reaches past the bubble edge.
 */
const BUBBLES = Object.freeze([
  { panel: 0, fx: 0.3, fy: 0.42, lines: ['WHERE DID', 'IT GO?!'], reasons: [[], ['crossesBalloon']] },
  { panel: 1, fx: 0.55, fy: 0.36, lines: ['THE MOON', 'IS GONE'], reasons: [[], []] },
  { panel: 2, fx: 0.4, fy: 0.3, lines: ['WAIT'], reasons: [[]] },
])

/** A stable `tg-` id with the native shape: twelve hex digits. */
const groupId = (index) => `tg-${index.toString(16).padStart(12, '0')}`

/** Union of rectangles; `null` for none. */
function hull(rects) {
  if (!rects.length) return null
  const x = Math.min(...rects.map((r) => r.x))
  const y = Math.min(...rects.map((r) => r.y))
  return { x, y, w: Math.max(...rects.map((r) => r.x + r.w)) - x, h: Math.max(...rects.map((r) => r.y + r.h)) - y }
}

/**
 * @typedef {Object} ReviewPage
 * @property {number} width
 * @property {number} height
 * @property {string} sourceDataUrl - grayscale PNG of the page
 * @property {string} maskDataUrl - grayscale PNG, 255 on every lettering pixel
 * @property {number} maskPixels
 * @property {Array<Object>} components - SAM components, one per line of lettering
 * @property {Array<Object>} regions - detector boxes: bubbles, lines, one box with no lettering
 * @property {Array<Object>} links
 * @property {Array<Object>} groups - text groups as native fusion reports them: one per bubble,
 *   the sound effect held as a candidate, the empty text box
 * @property {Map<string, number[]>} pixels - each component's page indices
 */

/**
 * Draw the page and find its evidence.
 *
 * @param {{ width: number, height: number, panels?: Array<{x: number, y: number, w: number, h: number}> }} spec
 *   - `panels` in percent of the page, as the mock's pages carry them
 * @returns {ReviewPage}
 */
export function buildReviewPage({ width, height, panels }) {
  const page = surface(width, height)
  const cell = Math.max(2, Math.round(width / 320))
  const stroke = Math.max(3, Math.round(width / 320))
  const layout = (panels?.length >= 3 ? panels : DEFAULT_PANELS).map((panel) => ({
    x: Math.round(panel.x / 100 * width),
    y: Math.round(panel.y / 100 * height),
    w: Math.round(panel.w / 100 * width),
    h: Math.round(panel.h / 100 * height),
  }))

  // Art first: tone along the bottom of the first panel, a moon in the
  // second, hatching behind the sound effect in the last.
  const first = layout[0]
  page.fill(first.x, first.y + Math.round(first.h * 0.72), first.w, Math.round(first.h * 0.28), TONE)
  page.line(first.x, first.y + Math.round(first.h * 0.72), first.x + first.w, first.y + Math.round(first.h * 0.72),
    Math.max(2, stroke - 2), INK, first)
  const second = layout[1]
  page.ellipse(second.x + second.w * 0.22, second.y + second.h * 0.72, second.w * 0.13, second.w * 0.13, null, INK, stroke)
  const last = layout[layout.length - 1]
  for (let x = last.x - last.h; x < last.x + last.w; x += cell * 4) {
    page.line(x, last.y + last.h, x + Math.round(last.h * 0.6), last.y, Math.max(1, cell - 2), 120, last)
  }
  for (const panel of layout) page.frame(panel.x, panel.y, panel.w, panel.h, stroke, INK)

  const mask = new Uint8Array(width * height)
  const components = []
  const regions = []
  const links = []
  const groups = []
  const pixels = new Map()
  const addComponent = (lettered, extra) => {
    const id = `sam-${String(components.length + 1).padStart(5, '0')}`
    for (const at of lettered) mask[at] = 255
    pixels.set(id, lettered)
    components.push({ id, bounds: boundsOf(lettered, width), pixels: lettered.length, rtTextIds: [], rtBubbleIds: [],
      cooIds: [], reviewReasons: [], groupId: null, ...extra })
    return components[components.length - 1]
  }
  const addGroup = ({ origin, disposition, members, boxes, bubble, reasons }) => {
    const id = groupId(groups.length + 1)
    for (const component of members) component.groupId = id
    const bounds = hull(members.map((component) => component.bounds)) ?? hull(boxes.map((box) => box.bounds))
    groups.push({
      id, order: groups.length, origin, disposition, bounds,
      anchor: hull(boxes.map((box) => box.bounds)),
      bubble: bubble?.id ?? null, bubbleRect: bubble?.bounds ?? null,
      componentIds: members.map((component) => component.id),
      boxIds: boxes.map((box) => box.id),
      models: [...(members.length ? ['samTsL'] : []), ...(boxes.length ? ['ogkaluFull'] : [])],
      letteringPixels: members.reduce((sum, component) => sum + component.pixels, 0),
      score: boxes.length ? Math.max(...boxes.map((box) => box.score)) : null,
      reasons: [...new Set([...reasons, ...members.flatMap((component) => component.reviewReasons)])].sort(),
      split: null,
    })
    // As native fusion does: a member carries its group's lettering reasons.
    const lettering = groups[groups.length - 1].reasons.filter((reason) => reason !== 'maskMissingUnderTextBox')
    for (const component of members) component.reviewReasons = [...new Set([...component.reviewReasons, ...lettering])].sort()
  }

  const lineHeight = 7 * cell
  const lineGap = 3 * cell
  BUBBLES.forEach((bubble, index) => {
    const panel = layout[bubble.panel] ?? layout[0]
    const blockW = Math.max(...bubble.lines.map(lineCells)) * cell
    const blockH = bubble.lines.length * lineHeight + (bubble.lines.length - 1) * lineGap
    const rx = Math.round(blockW / 2 * 1.45 + 2 * cell)
    const ry = Math.round(blockH / 2 * 1.45 + 3 * cell)
    const margin = stroke * 3
    const cx = Math.round(Math.min(Math.max(panel.x + panel.w * bubble.fx, panel.x + rx + margin), panel.x + panel.w - rx - margin))
    const cy = Math.round(Math.min(Math.max(panel.y + panel.h * bubble.fy, panel.y + ry + margin), panel.y + panel.h - ry - margin))
    page.ellipse(cx, cy, rx, ry, PAPER, INK, Math.max(2, stroke - 1))
    const bubbleId = `rt-${String(index).padStart(4, '0')}`
    const bubbleRegion = { id: bubbleId, kind: 'bubble_context', bounds: { x: cx - rx, y: cy - ry, w: rx * 2, h: ry * 2 },
      score: 0.93, componentIds: [], detectorOnly: false }
    regions.push(bubbleRegion)
    const members = []
    const boxes = []
    bubble.lines.forEach((text, row) => {
      const left = Math.round(cx - lineCells(text) * cell / 2)
      const top = Math.round(cy - blockH / 2 + row * (lineHeight + lineGap))
      const textId = `rt-${String(10 + regions.length).padStart(4, '0')}`
      const component = addComponent(page.letter(text, left, top, cell),
        { rtTextIds: [textId], rtBubbleIds: [bubbleId], reviewReasons: [...bubble.reasons[row]] })
      const box = component.bounds
      const textRegion = { id: textId, kind: 'text_bubble', bounds: { x: box.x - cell, y: box.y - cell, w: box.w + 2 * cell, h: box.h + 2 * cell },
        score: 0.88, componentIds: [component.id], detectorOnly: false }
      regions.push(textRegion)
      members.push(component)
      boxes.push(textRegion)
      bubbleRegion.componentIds.push(component.id)
      links.push({ componentId: component.id, regionId: textId, sharedPixels: component.pixels })
    })
    // A lone speck after the second bubble's last line: one cell, selectable
    // from the list or with the keyboard when it is too small to click. It
    // sits beside the line, so it joins the bubble's group as punctuation.
    if (index === 1) {
      const speck = []
      const lastLine = bubble.lines[bubble.lines.length - 1]
      const sx = Math.round(cx + lineCells(lastLine) * cell / 2) + cell
      const sy = Math.round(cy + blockH / 2) - cell
      for (let y = sy; y < sy + cell; y += 1) {
        for (let x = sx; x < sx + cell; x += 1) {
          if (x < 0 || y < 0 || x >= width || y >= height) continue
          page.gray[y * width + x] = INK
          speck.push(y * width + x)
        }
      }
      const component = addComponent(speck, { rtBubbleIds: [bubbleId] })
      bubbleRegion.componentIds.push(component.id)
      members.push(component)
    }
    // Lines in one bubble are one text group: one job and one layer.
    addGroup({ origin: 'textBox', disposition: 'clean', members, boxes, bubble: bubbleRegion, reasons: [] })
  })

  // A sound effect over the hatching, outside every bubble: held until the
  // review's own permission allows it. No text box claims it, so detection
  // lists its group as a candidate rather than cleaning it.
  const sfxCell = cell * 3
  const sfx = 'DOOM'
  const effect = addComponent(page.letter(sfx,
    Math.round(last.x + last.w * 0.55 - lineCells(sfx) * sfxCell / 2),
    Math.round(last.y + last.h * 0.55 - 7 * sfxCell / 2), sfxCell), { reviewReasons: ['unassignedComponent'] })
  addGroup({ origin: 'maskLayout', disposition: 'candidate', members: [effect], boxes: [], bubble: null, reasons: [] })

  // A detector box with nothing inside it for SAM: a locator, never pixels.
  const third = layout[2] ?? layout[0]
  const empty = { id: 'rt-0099', kind: 'text_free',
    bounds: { x: third.x + Math.round(third.w * 0.62), y: third.y + Math.round(third.h * 0.74), w: 18 * cell, h: 8 * cell },
    score: 0.41, componentIds: [], detectorOnly: true }
  regions.push(empty)
  addGroup({ origin: 'textBox', disposition: 'clean', members: [], boxes: [empty], bubble: null, reasons: ['maskMissingUnderTextBox'] })

  return {
    width,
    height,
    sourceDataUrl: grayPngDataUrl(width, height, page.gray),
    maskDataUrl: grayPngDataUrl(width, height, mask),
    maskPixels: components.reduce((sum, component) => sum + component.pixels, 0),
    components,
    regions,
    links,
    groups,
    pixels,
  }
}

/**
 * The write support W of one component: its pixels grown by a round
 * `paddingPx`, plus the additions, minus the removals. Mirrors the native
 * `MaskPlan` closely enough for a preview; it is not the native plan.
 *
 * @param {{ pixels: number[], width: number, height: number, paddingPx: number,
 *   additions?: {bounds: {x: number, y: number, w: number, h: number}, bits: number[]},
 *   removals?: {bounds: {x: number, y: number, w: number, h: number}, bits: number[]} }} spec
 * @returns {{ count: number, bounds: {x: number, y: number, w: number, h: number}|null, dataUrl: string|null, checksum: number }}
 */
export function supportOf({ pixels, width, height, paddingPx, additions, removals }) {
  const r = Math.max(0, Math.min(64, Math.trunc(paddingPx) || 0))
  const own = new Set(pixels)
  const support = new Set(pixels)
  if (r > 0) {
    const disc = []
    for (let dy = -r; dy <= r; dy += 1) {
      for (let dx = -r; dx <= r; dx += 1) if (dx * dx + dy * dy <= r * r) disc.push(dx, dy)
    }
    for (const at of pixels) {
      const x = at % width
      const y = Math.floor(at / width)
      // Only the edge grows the support; the inside is already in it.
      const edge = x === 0 || y === 0 || x === width - 1 || y === height - 1 ||
        !own.has(at - 1) || !own.has(at + 1) || !own.has(at - width) || !own.has(at + width)
      if (!edge) continue
      for (let i = 0; i < disc.length; i += 2) {
        const nx = x + disc[i]
        const ny = y + disc[i + 1]
        if (nx >= 0 && ny >= 0 && nx < width && ny < height) support.add(ny * width + nx)
      }
    }
  }
  walkRaster(additions, width, height, (at) => support.add(at))
  walkRaster(removals, width, height, (at) => support.delete(at))
  const list = [...support]
  const bounds = boundsOf(list, width)
  if (!bounds) return { count: 0, bounds: null, dataUrl: null, checksum: 0 }
  const raster = new Uint8Array(bounds.w * bounds.h)
  let checksum = 0
  for (const at of list) {
    raster[(Math.floor(at / width) - bounds.y) * bounds.w + (at % width) - bounds.x] = 255
    checksum = (checksum + at * 2654435761) % 4294967296
  }
  return { count: list.length, bounds, dataUrl: grayPngDataUrl(bounds.w, bounds.h, raster), checksum }
}

function walkRaster(raster, width, height, visit) {
  const bounds = raster?.bounds
  if (!bounds || !Array.isArray(raster.bits)) return
  for (let y = 0; y < bounds.h; y += 1) {
    for (let x = 0; x < bounds.w; x += 1) {
      if (!(raster.bits[y * bounds.w + x] > 0)) continue
      const px = bounds.x + x
      const py = bounds.y + y
      if (px >= 0 && py >= 0 && px < width && py < height) visit(py * width + px)
    }
  }
}
