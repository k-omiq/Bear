/**
 * The Patch tool and the Gradient tool between the gesture and the commit.
 *
 * Both draw a selection with Region copy's shapes, and neither commits when
 * the drag ends: the selection is **held** (`draft.selection`) while the rest
 * of the edit is decided on the page, and Apply - the bar's button or `Enter`
 * on the page - sends it as one `createRegion`, one layer and one undo step.
 *
 * - **Patch.** The source is proposed as soon as the selection is held
 *   (`proposePatchSource`, `cleaner-core/src/engines/patchsource.rs`) and drawn
 *   as a ghost outline; a drag inside the selection or the ghost moves it.
 *   Only Normal reads a source. Auto, Content-Aware and Smooth fill from
 *   around the selection and ignore the ghost, so Apply is ready at once.
 * - **Gradient.** A drag inside the selection sets the line; its two handles
 *   stay editable, and the preview follows them, until Apply or the next
 *   selection (`SelectionOverlay.svelte`). Match surroundings asks for the line
 *   and the stops at once (`fitGradient`). A gradient layer reopens: its
 *   recorded gradient is held again with the layer's id, and Apply re-renders
 *   that layer in place (`applyTool`) rather than adding one. The **Auto**
 *   type (`Shift+G`) needs no line: the selection is applied as soon as it is
 *   held, and the native side fills it from all round it
 *   (`cleaner-core/src/engines/autogradient.rs`).
 *
 * Nothing here is cloud work: neither tool has a cloud option.
 */

import { getBackend } from '../api/backend.js'
import { decodeImage } from '../api/boundedimage.js'
import { notify } from '../state/app.svelte.js'
import {
  applyRegionState,
  claimPageEdit,
  editor,
  recordBackgroundRegionEdit,
  recordRegionEdit,
  reloadPage,
  scopePageIndices,
  select,
  setToolParam,
} from '../state/editor.svelte.js'
import { rgbToHex } from '../ui/color.js'
import { clearSelection, draft, holdSelection } from './draft.svelte.js'
import { fromWire, wireGradient } from './gradient.js'
import { reportRegionEditFailure } from './maskactions.svelte.js'
import { applyActiveToolToRegion, locateRegion } from './toolapply.svelte.js'

/**
 * The Gradient tool's eyedropper: which colour stop the next click on the page
 * samples, or null. Not part of the held selection, because a stop can be
 * picked before anything is selected.
 */
export const retouch = $state({
  /** @type {number|null} */
  eyedrop: null,
})

/**
 * @param {Record<string, any>} spec - `drawing.svelte.js`'s commit spec
 * @param {{kind: string, points: Array<{x: number, y: number}>, feather: number}|null} painted
 * @param {{points: Array<{x: number, y: number}>, radius: number}|null} stroke
 * @returns {boolean} false: nothing changed yet
 */
export function holdDrawn(spec, painted, stroke) {
  if (!painted && !stroke) return false
  if (spec.tool !== 'patch' && spec.tool !== 'gradient') return false
  holdSelection({
    tool: spec.tool,
    pageId: spec.pageId,
    chapterId: spec.chapterId,
    pageIndex: spec.pageIndex,
    sourceIndex: spec.sourceIndex,
    sourceSha: spec.sourceSha,
    bbox: { ...spec.bbox },
    painted,
    stroke,
    offset: null,
    proposed: false,
    proposing: false,
    start: null,
    end: null,
    regionId: null,
  })
  if (spec.tool === 'patch') void proposeSource()
  else if (editor.toolParams.gradient?.kind === 'auto') void applySelection()
  return false
}

/** @param {import('./draft.svelte.js').HeldSelection} held */
function where(held) {
  return {
    chapterId: /** @type {string} */ (held.chapterId),
    pageIndex: /** @type {number} */ (held.pageIndex),
    ...(held.sourceIndex !== null && held.sourceSha ? { sourceIndex: held.sourceIndex, sourceSha: held.sourceSha } : {}),
    ...(held.painted ? { painted: held.painted } : {}),
    ...(held.stroke ? { stroke: held.stroke } : {}),
  }
}

/**
 * Ask for the held Patch selection's source and put the ghost there. A drag
 * that landed meanwhile wins; a selection replaced meanwhile is left alone.
 * Where no source fits, the ghost starts one selection-width to the side and
 * the bar says so.
 *
 * @returns {Promise<boolean>} whether a proposal arrived
 */
export async function proposeSource() {
  const held = draft.selection
  if (!held || held.tool !== 'patch' || held.chapterId === null || held.pageIndex === null) return false
  held.proposing = true
  /** @type {{offset: {x: number, y: number}}|null} */
  let answer = null
  try {
    answer = await getBackend().proposePatchSource(where(held))
  } catch (error) {
    console.error('no patch source was proposed', error)
  }
  if (draft.selection !== held) return false
  held.proposing = false
  if (held.offset) return false
  if (answer?.offset && Number.isFinite(answer.offset.x) && Number.isFinite(answer.offset.y)) {
    held.offset = { x: answer.offset.x, y: answer.offset.y }
    held.proposed = true
    return true
  }
  const side = held.bbox.x + held.bbox.w * 2 <= 100 ? held.bbox.w : -held.bbox.w
  held.offset = { x: side, y: 0 }
  held.proposed = false
  notify({ key: 'retouch.notice.noSource', tone: 'info' })
  return false
}

/**
 * The ghost dragged: the source (or, for Destination, where the selection
 * goes), in page percent from the selection.
 *
 * @param {{x: number, y: number}} offset
 */
export function moveGhost(offset) {
  const held = draft.selection
  if (!held || held.tool !== 'patch') return
  held.offset = { x: offset.x, y: offset.y }
  held.proposed = false
}

/**
 * The gradient's line, in page percent.
 *
 * @param {{x: number, y: number}} start
 * @param {{x: number, y: number}} end
 */
export function setLine(start, end) {
  const held = draft.selection
  if (!held || held.tool !== 'gradient') return
  held.start = { x: start.x, y: start.y }
  held.end = { x: end.x, y: end.y }
}

/**
 * Whether Apply would send anything: a Gradient needs its line, and Normal
 * needs a source.
 *
 * @param {import('./draft.svelte.js').HeldSelection|null} held
 * @param {Record<string, any>} [params]
 * @returns {boolean}
 */
export function applicable(held, params = editor.toolParams[held?.tool ?? ''] ?? {}) {
  if (!held) return false
  if (held.tool === 'gradient') return params.kind === 'auto' || !!(held.start && held.end)
  return params.mode !== 'normal' || !!held.offset
}

/**
 * The seam payload for the held selection: the tool's own values, the
 * selection, and the offset or the gradient.
 *
 * @param {import('./draft.svelte.js').HeldSelection} held
 * @param {Record<string, any>} params
 * @returns {Record<string, unknown>}
 */
export function selectionParams(held, params) {
  const { colors: _colors, opacities: _opacities, reverse: _reverse, dither: _dither, ...rest } = params
  const own = held.tool === 'gradient' ? rest : params
  /** @type {Record<string, unknown>} */
  const out = { ...own }
  if (held.painted) {
    // The feather is the bar's as it is now, not as it was when the
    // selection was drawn. A patch has none: its edge is the Poisson solve's.
    out.painted = { ...held.painted, feather: held.tool === 'gradient' ? Math.max(0, Number(params.feather ?? 0)) : 0 }
  }
  if (held.stroke) out.stroke = held.stroke
  if (held.tool === 'patch') out.offset = held.offset ?? { x: 0, y: 0 }
  else if (params.kind !== 'auto' && held.start && held.end) out.gradient = wireGradient(params, held.start, held.end)
  return out
}

/**
 * Apply the held selection: one layer, one undo step. A reopened gradient
 * re-renders its own layer instead.
 *
 * @returns {Promise<boolean>} whether the chapter changed
 */
export async function applySelection() {
  const held = draft.selection
  if (!held) return false
  const params = /** @type {Record<string, any>} */ ($state.snapshot(editor.toolParams[held.tool] ?? {}))
  if (!applicable(held, params)) {
    notify({ key: held.tool === 'gradient' ? 'retouch.notice.drawLine' : 'retouch.notice.noSource', tone: 'warn' })
    return false
  }
  if (held.pageIndex === null || !claimPageEdit(held.pageIndex)) return false
  const payload = selectionParams(/** @type {any} */ ($state.snapshot(held)), params)
  const snapshot = /** @type {import('./draft.svelte.js').HeldSelection} */ ($state.snapshot(held))
  clearSelection()
  retouch.eyedrop = null
  if (snapshot.regionId) return applyActiveToolToRegion(snapshot.regionId, { ...payload, bbox: snapshot.bbox })
  return createLayer(snapshot, payload)
}

/**
 * @param {import('./draft.svelte.js').HeldSelection} held
 * @param {Record<string, unknown>} params
 * @returns {Promise<boolean>}
 */
async function createLayer(held, params) {
  const chapter = editor.chapter
  const page = chapter?.pages.find((candidate) => candidate.id === held.pageId)
  if (!chapter || !page || chapter.id !== held.chapterId || page.index !== held.pageIndex) return false
  const before = page.status
  /** @type {any} */
  let result
  try {
    result = await getBackend().createRegion({
      chapterId: chapter.id,
      pageIndex: page.index,
      ...(held.sourceIndex !== null && held.sourceSha ? { sourceIndex: held.sourceIndex, sourceSha: held.sourceSha } : {}),
      bbox: held.bbox,
      tool: held.tool,
      params,
    })
  } catch (error) {
    return reportRegionEditFailure(error)
  }
  if (!result) return false
  const label = held.tool === 'patch' ? 'retouch.command.patch' : 'retouch.command.gradient'
  const created = /** @type {any} */ ($state.snapshot(result.region))
  const beforeState = { region: null, pageStatus: before }
  const afterState = { region: created, pageStatus: result.pageStatus }
  if (editor.chapter?.id !== held.chapterId) {
    return recordBackgroundRegionEdit(/** @type {string} */ (held.chapterId), label, created.id, beforeState, afterState)
  }
  applyRegionState(result.region.id, result.region, result.pageStatus)
  if (scopePageIndices().includes(page.index)) select(created.id)
  recordRegionEdit(label, created.id, beforeState, afterState)
  void reloadPage(page.index).catch(() => {})
  return true
}

/**
 * Match surroundings: the type, the line and the stops fitted to the ring
 * around the held selection, drawn at once.
 *
 * @returns {Promise<boolean>} whether a fit arrived
 */
export async function matchSurroundings() {
  const held = draft.selection
  if (!held || held.tool !== 'gradient' || held.chapterId === null || held.pageIndex === null) {
    notify({ key: 'retouch.notice.selectFirst', tone: 'info' })
    return false
  }
  /** @type {any} */
  let answer = null
  try {
    answer = await getBackend().fitGradient(where(held))
  } catch (error) {
    console.error('the gradient could not be fitted', error)
  }
  if (draft.selection !== held) return false
  if (!answer?.start || !answer?.end || !answer?.colors?.length) {
    notify({ key: 'retouch.notice.noFit', tone: 'warn' })
    return false
  }
  const { params, start, end } = fromWire(answer)
  setToolParam('gradient', 'kind', params.kind)
  setToolParam('gradient', 'colors', params.colors)
  setToolParam('gradient', 'opacities', params.opacities)
  setToolParam('gradient', 'reverse', false)
  setLine(start, end)
  return true
}

/**
 * Hold a gradient layer again: its line, its stops and its options back on
 * the bar, and Apply re-rendering that layer.
 *
 * @param {any} region
 * @returns {boolean} whether it could be reopened
 */
export function reopenGradient(region) {
  const snapshot = region?.mask?.provenance?.params_snapshot
  if (snapshot?.mode !== 'gradient' || !snapshot.gradient) return false
  const located = locateRegion(region.id)
  const page = editor.chapter?.pages.find((candidate) => candidate.index === located?.pageIndex)
  if (!located || !page || (!snapshot.painted && !snapshot.stroke)) return false
  const { params, start, end } = fromWire(snapshot.gradient)
  for (const [key, value] of Object.entries(params)) setToolParam('gradient', key, value)
  setToolParam('gradient', 'asScreentone', snapshot.asScreentone === true)
  setToolParam('gradient', 'feather', Number(snapshot.feather ?? snapshot.painted?.feather ?? 0))
  setToolParam('gradient', 'opacity', Number(snapshot.layer?.opacity ?? 100))
  if (snapshot.shape) setToolParam('gradient', 'shape', snapshot.shape === 'polygon' ? 'lasso' : snapshot.shape)
  holdSelection({
    tool: 'gradient',
    pageId: page.id,
    chapterId: editor.chapter?.id ?? null,
    pageIndex: page.index,
    sourceIndex: page.sourceIndex ?? null,
    sourceSha: page.sourceSha ?? null,
    bbox: { ...region.bbox },
    painted: snapshot.painted ?? null,
    stroke: snapshot.stroke ?? null,
    offset: null,
    proposed: false,
    proposing: false,
    start,
    end,
    regionId: region.id,
  })
  return true
}

/**
 * Arm the eyedropper for colour stop `index`, or put it down again.
 *
 * @param {number|null} index
 */
export function armEyedrop(index) {
  retouch.eyedrop = retouch.eyedrop === index ? null : index
}

/**
 * The colour of the page under a client point, as the sheet draws it: the
 * cleaned artwork's tile under the point, decoded from its own bytes (an
 * image element drawn straight into a canvas would taint it, which is
 * `magneticlasso.js#artworkEdges`'s reason too).
 *
 * @param {Element|null|undefined} sheet
 * @param {number} clientX
 * @param {number} clientY
 * @returns {Promise<string|null>} `#rrggbb`
 */
export async function colorAt(sheet, clientX, clientY) {
  const artwork = sheet?.querySelector('[data-artwork="cleaned"]')
  const tiles = [...(artwork?.querySelectorAll('img') ?? [])]
    .filter((node) => node.style.visibility !== 'hidden' && node.complete && node.naturalWidth > 0)
    .reverse()
  for (const node of tiles) {
    const box = node.getBoundingClientRect()
    if (clientX < box.left || clientX >= box.right || clientY < box.top || clientY >= box.bottom) continue
    try {
      const response = await fetch(node.currentSrc || node.src)
      if (!response.ok) continue
      const image = await decodeImage(await response.blob())
      try {
        const canvas = document.createElement('canvas')
        canvas.width = 1
        canvas.height = 1
        const ctx = canvas.getContext('2d', { willReadFrequently: true })
        if (!ctx) return null
        const sx = ((clientX - box.left) / box.width) * image.width
        const sy = ((clientY - box.top) / box.height) * image.height
        ctx.drawImage(image, Math.floor(sx), Math.floor(sy), 1, 1, 0, 0, 1, 1)
        const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data
        return rgbToHex(r, g, b)
      } finally {
        /** @type {any} */ (image).close?.()
      }
    } catch {
      continue
    }
  }
  return null
}

/**
 * The armed eyedropper's click: the stop takes the page's colour there.
 *
 * @param {Element|null|undefined} sheet
 * @param {number} clientX
 * @param {number} clientY
 * @returns {Promise<boolean>}
 */
export async function sampleEyedrop(sheet, clientX, clientY) {
  const index = retouch.eyedrop
  retouch.eyedrop = null
  if (index === null) return false
  const color = await colorAt(sheet, clientX, clientY)
  if (!color) {
    notify({ key: 'retouch.notice.noColor', tone: 'warn' })
    return false
  }
  const stops = [...(/** @type {any[]} */ ($state.snapshot(editor.toolParams.gradient?.colors ?? [])))]
  if (!stops[index]) return false
  stops[index] = { ...stops[index], color }
  setToolParam('gradient', 'colors', stops)
  return true
}
