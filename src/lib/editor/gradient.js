/**
 * The Gradient tool's arithmetic on this side of the seam: the live preview,
 * the stops the bar edits, and the payload `region.rs` reads.
 *
 * **One gradient, two copies of its evaluation.** The commit renders natively
 * (`cleaner-core/src/engines/gradient.rs`, `shade`); the preview while a
 * handle is dragged cannot wait for a round trip per frame, so `position` and
 * `colorAt` here are that file's `position` and `at` written again, line for
 * line, and the tests pin them to the same numbers. Dither is the native
 * side's alone: the preview is drawn at a fraction of the page's size, where
 * half a level of noise is invisible.
 *
 * **Two shapes of stop.** The bar edits `#rrggbb` colours and opacities in
 * percent, which is what its controls hold (`editor.toolParams.gradient`);
 * the seam carries `gradient.rs#Gradient` as serde writes it, colours as
 * `[r, g, b]` and opacities 0 to 1, with `start` and `end` in page percent.
 * `wireGradient` and `fromWire` are the only two crossings.
 */

import { hexToRgb, rgbToHex } from '../ui/color.js'

/**
 * @typedef {{at: number, color: string}} ColorStop - `at` 0 to 1
 * @typedef {{at: number, opacity: number}} OpacityStop - `at` 0 to 1, `opacity` 0 to 100
 * @typedef {{x: number, y: number}} Point - page percent
 * @typedef {Object} WireGradient - `gradient.rs#Gradient`
 * @property {'linear'|'radial'|'angle'|'reflected'|'diamond'} kind
 * @property {[number, number]} start
 * @property {[number, number]} end
 * @property {Array<{at: number, color: [number, number, number]}>} colors
 * @property {Array<{at: number, opacity: number}>} opacities
 * @property {boolean} reverse
 * @property {boolean} dither
 */

/** The bar's two stops before anything is picked. */
export const DEFAULT_STOPS = Object.freeze({
  colors: [{ at: 0, color: '#000000' }, { at: 1, color: '#ffffff' }],
  opacities: [{ at: 0, opacity: 100 }, { at: 1, opacity: 100 }],
})

const clamp01 = (/** @type {number} */ v) => Math.min(1, Math.max(0, Number.isFinite(v) ? v : 0))

/**
 * @param {string} hex
 * @returns {[number, number, number]}
 */
function rgbOf(hex) {
  const rgb = hexToRgb(hex)
  return rgb ? [rgb.r, rgb.g, rgb.b] : [0, 0, 0]
}

/**
 * The seam's gradient from the bar's values and the line.
 *
 * @param {Record<string, any>} params - `editor.toolParams.gradient`
 * @param {Point} start
 * @param {Point} end
 * @returns {WireGradient}
 */
export function wireGradient(params, start, end) {
  const colors = (params.colors?.length ? params.colors : DEFAULT_STOPS.colors)
  const opacities = (params.opacities?.length ? params.opacities : DEFAULT_STOPS.opacities)
  return {
    kind: params.kind ?? 'linear',
    start: [round(start.x), round(start.y)],
    end: [round(end.x), round(end.y)],
    colors: colors.map((/** @type {ColorStop} */ stop) => ({ at: clamp01(stop.at), color: rgbOf(stop.color) })),
    opacities: opacities.map((/** @type {OpacityStop} */ stop) => ({ at: clamp01(stop.at), opacity: clamp01(Number(stop.opacity) / 100) })),
    reverse: params.reverse === true,
    dither: params.dither !== false,
  }
}

/** @param {number} v */
function round(v) {
  return Math.round(v * 1000) / 1000
}

/**
 * The bar's values and the line from a seam gradient - what Match
 * surroundings answered, or what a gradient layer recorded.
 *
 * @param {WireGradient} wire
 * @returns {{params: {kind: string, colors: ColorStop[], opacities: OpacityStop[], reverse: boolean, dither: boolean}, start: Point, end: Point}}
 */
export function fromWire(wire) {
  return {
    params: {
      kind: wire.kind ?? 'linear',
      colors: (wire.colors ?? []).map((stop) => ({ at: clamp01(stop.at), color: rgbToHex(...stop.color) })),
      opacities: (wire.opacities?.length ? wire.opacities : [{ at: 0, opacity: 1 }, { at: 1, opacity: 1 }])
        .map((stop) => ({ at: clamp01(stop.at), opacity: Math.round(clamp01(stop.opacity) * 100) })),
      reverse: wire.reverse === true,
      dither: wire.dither !== false,
    },
    start: { x: wire.start[0], y: wire.start[1] },
    end: { x: wire.end[0], y: wire.end[1] },
  }
}

/**
 * Position 0 to 1 of a point along a gradient, all in **page pixels** -
 * `gradient.rs#Gradient::position`.
 *
 * @param {{kind: string, start: [number, number], end: [number, number], reverse?: boolean}} g
 * @param {number} x
 * @param {number} y
 * @returns {number}
 */
export function position(g, x, y) {
  const dx = g.end[0] - g.start[0]
  const dy = g.end[1] - g.start[1]
  const len = Math.sqrt(Math.max(dx * dx + dy * dy, 1e-9))
  const px = x - g.start[0]
  const py = y - g.start[1]
  const along = (px * dx + py * dy) / len
  const across = (px * dy - py * dx) / len
  let t
  switch (g.kind) {
    case 'reflected': t = Math.abs(along) / len; break
    case 'radial': t = Math.hypot(px, py) / len; break
    case 'diamond': t = (Math.abs(along) + Math.abs(across)) / len; break
    case 'angle': {
      const turn = Math.PI * 2
      t = (((Math.atan2(across, along) % turn) + turn) % turn) / turn
      break
    }
    default: t = along / len
  }
  t = clamp01(t)
  return g.reverse ? 1 - t : t
}

/**
 * @template {number[]} V
 * @param {Array<[number, V]>} stops
 * @param {number} t
 * @param {V} empty
 * @returns {V}
 */
function interpolate(stops, t, empty) {
  if (stops.length === 0) return empty
  const sorted = [...stops].sort((a, b) => a[0] - b[0])
  if (t <= sorted[0][0]) return sorted[0][1]
  for (let i = 1; i < sorted.length; i += 1) {
    const [a, ca] = sorted[i - 1]
    const [b, cb] = sorted[i]
    if (t <= b) {
      const f = b > a ? (t - a) / (b - a) : 1
      return /** @type {V} */ (ca.map((v, k) => v + (cb[k] - v) * f))
    }
  }
  return sorted[sorted.length - 1][1]
}

/**
 * Colour (0 to 255) and opacity (0 to 1) at position `t` -
 * `gradient.rs#Gradient::at`.
 *
 * @param {WireGradient} g
 * @param {number} t
 * @returns {{color: number[], alpha: number}}
 */
export function colorAt(g, t) {
  const color = interpolate(g.colors.map((stop) => [stop.at, stop.color]), t, [0, 0, 0])
  const [alpha] = interpolate(g.opacities.map((stop) => [stop.at, [stop.opacity]]), t, [1])
  return { color, alpha }
}

/**
 * `gradient` with its line in page pixels rather than page percent.
 *
 * @param {WireGradient} g
 * @param {number} width
 * @param {number} height
 * @returns {WireGradient}
 */
export function inPixels(g, width, height) {
  return {
    ...g,
    start: [(g.start[0] / 100) * width, (g.start[1] / 100) * height],
    end: [(g.end[0] / 100) * width, (g.end[1] / 100) * height],
  }
}

/**
 * Where a drag from `from` to `to` ends with `Shift` held: the same length,
 * turned to the nearest 15 degrees **on the page**, which is pixels and not
 * percent - a page is rarely square.
 *
 * @param {Point} from
 * @param {Point} to
 * @param {number} width
 * @param {number} height
 * @param {number} [step] - degrees
 * @returns {Point}
 */
export function snapped(from, to, width, height, step = 15) {
  const dx = ((to.x - from.x) / 100) * width
  const dy = ((to.y - from.y) / 100) * height
  const length = Math.hypot(dx, dy)
  if (length === 0) return { ...to }
  const unit = (step * Math.PI) / 180
  const angle = Math.round(Math.atan2(dy, dx) / unit) * unit
  return {
    x: from.x + ((Math.cos(angle) * length) / width) * 100,
    y: from.y + ((Math.sin(angle) * length) / height) * 100,
  }
}

/**
 * The two presets. Foreground is the colour the Brush paints with, which is
 * the one paint colour the app keeps; background is white beside a dark
 * foreground and black beside a light one, so the pair is never one colour
 * twice.
 *
 * @param {'foregroundToBackground'|'foregroundToTransparent'} preset
 * @param {string} [foreground]
 * @returns {{colors: ColorStop[], opacities: OpacityStop[]}}
 */
export function presetStops(preset, foreground = '#000000') {
  const fg = rgbToHex(...rgbOf(foreground))
  const [r, g, b] = rgbOf(fg)
  const background = 0.299 * r + 0.587 * g + 0.114 * b < 128 ? '#ffffff' : '#000000'
  if (preset === 'foregroundToTransparent') {
    return {
      colors: [{ at: 0, color: fg }, { at: 1, color: fg }],
      opacities: [{ at: 0, opacity: 100 }, { at: 1, opacity: 0 }],
    }
  }
  return {
    colors: [{ at: 0, color: fg }, { at: 1, color: background }],
    opacities: [{ at: 0, opacity: 100 }, { at: 1, opacity: 100 }],
  }
}

/**
 * A stop added half way into the widest gap, with the value the gradient
 * already has there, so adding one changes nothing until it is moved or
 * edited.
 *
 * @template {ColorStop|OpacityStop} S
 * @param {S[]} stops
 * @param {(at: number) => S} make
 * @returns {S[]}
 */
export function addStop(stops, make) {
  const ats = [...stops.map((stop) => stop.at), 0, 1].sort((a, b) => a - b)
  let best = 0.5
  let gap = -1
  for (let i = 1; i < ats.length; i += 1) {
    if (ats[i] - ats[i - 1] > gap) {
      gap = ats[i] - ats[i - 1]
      best = (ats[i] + ats[i - 1]) / 2
    }
  }
  return [...stops, make(Math.round(best * 100) / 100)].sort((a, b) => a.at - b.at)
}

/**
 * The colour stop at `at` of the bar's colour stops, as `#rrggbb`.
 *
 * @param {ColorStop[]} stops
 * @param {number} at
 * @returns {string}
 */
export function colorBetween(stops, at) {
  const color = interpolate(stops.map((stop) => [stop.at, rgbOf(stop.color)]), at, [0, 0, 0])
  return rgbToHex(color[0], color[1], color[2])
}

/**
 * The opacity (percent) at `at` of the bar's opacity stops.
 *
 * @param {OpacityStop[]} stops
 * @param {number} at
 * @returns {number}
 */
export function opacityBetween(stops, at) {
  return Math.round(interpolate(stops.map((stop) => [stop.at, [Number(stop.opacity)]]), at, [100])[0])
}

/**
 * Paint the gradient into `image` (an `ImageData` covering `box`, page pixels,
 * at `scale` image pixels per page pixel). Alpha is the gradient's own
 * opacity; the selection's outline is cut afterwards by the caller.
 *
 * @param {ImageData} image
 * @param {WireGradient} g - in page pixels
 * @param {{x: number, y: number}} box - the image's top left, page pixels
 * @param {number} scale
 */
export function paintGradient(image, g, box, scale) {
  const { data, width, height } = image
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const t = position(g, box.x + (x + 0.5) / scale, box.y + (y + 0.5) / scale)
      const { color, alpha } = colorAt(g, t)
      const i = (y * width + x) * 4
      data[i] = color[0]
      data[i + 1] = color[1]
      data[i + 2] = color[2]
      data[i + 3] = Math.round(alpha * 255)
    }
  }
}
