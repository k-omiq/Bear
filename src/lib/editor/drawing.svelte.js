/**
 * What the five hand tools actually do once a gesture finishes.
 *
 * The geometry arrives already in the region's normalised coordinate space
 * (`gesture.js`), so everything here is about *which* seam call a gesture is:
 *
 * - **Brush, Shapes, Clone / heal** draw a mask where there was none, so they
 *   create a region - `createRegion`, the one method the seam grew for this
 *   task. The Brush paints a colour into it; the other two hand the area to an
 *   engine.
 * - **The AI mask brush** creates a region too, and for the same reason: the
 *   stroke *is* the mask. It used to ask a snapping helper which existing
 *   region the stroke was "about" and edit that one instead, which is how a
 *   stroke aimed at a leftover beside a layer box re-ran the layer box.
 *   A stroke never retargets now.
 * - **Content-aware fill** never reaches here. It fills an existing mask,
 *   which is a click on a region and therefore
 *   `toolapply.svelte.js`'s.
 * - **The selection tool** creates no layer. It adds its gesture to the
 *   page's detected masks or takes it away from them - `editDetectionMask` -
 *   and those masks are what Clean erases later. A detection has no pixels to
 *   put back, so, like deleting one, the edit records no undo entry.
 *
 * **Undo.** Every command is a pair of region snapshots and one
 * `recordRegionEdit` in each direction - `maskactions.svelte.js`'s
 * pattern. A creation's "before" snapshot is `null`, because before the gesture
 * the region was not there; `restoreRegion` takes that and removes it, and the
 * "after" snapshot puts the same region back with the same mask, so redo cannot
 * drift from undo.
 */

import { getBackend } from '../api/backend.js'
import { notify } from '../state/app.svelte.js'
import {
  applyRegionState,
  editor,
  recordBackgroundRegionEdit,
  recordRegionEdit,
  reloadPage,
  reportJobConflict,
  scopePageIndices,
  select,
  hover,
} from '../state/editor.svelte.js'
import { holdCreation, releaseCreation } from '../state/heldcreations.js'
import { cloudRefused } from './cloudflow.svelte.js'
import { draft, clearDraft, setCloneOffset } from './draft.svelte.js'
import { AI_STROKE_PX, cloneOffset, paintedStroke } from './gesture.js'
import { paintParamsOf } from './paint.js'
import { renderRegionInCloud } from './toolapply.svelte.js'
import { reportRegionEditFailure } from './maskactions.svelte.js'
import { SOLID, TOOL_SPECS, toolSpendsCloud } from './tools.js'

export { paintParamsOf }

/** Per-page promise tails. Enqueue synchronously when the gesture ends. */
const gestureTails = new Map()

/**
 * Commit whatever the draft describes. Clears the draft either way: a gesture
 * that finished is over, whether or not it changed anything.
 *
 * @returns {Promise<boolean>} whether the chapter changed
 */
export async function commitDraft() {
  const active = draft.active
  if (!active?.bbox) {
    clearDraft()
    return false
  }
  const spec = {
    tool: active.tool,
    kind: active.kind,
    pageId: active.pageId,
    bbox: active.bbox,
    mode: active.mode,
    // The path itself, copied out of the reactive draft before it is dropped.
    // It is what makes a stroke a stroke on the far side of the seam rather
    // than the rectangle around it.
    points: active.points.map((point) => ({
      x: point.x,
      y: point.y,
      p: typeof point.p === 'number' && point.p > 0 ? point.p : 0.5,
    })),
    start: active.points[0] ?? { x: active.bbox.x, y: active.bbox.y },
    chapterId: editor.chapter?.id ?? null,
    pageIndex: pageOf(active.pageId)?.index ?? null,
    sourceIndex: pageOf(active.pageId)?.sourceIndex ?? null,
    sourceSha: pageOf(active.pageId)?.sourceSha ?? null,
    params: paramsOf(active.tool),
    cloneSource: draft.cloneSource ? { ...draft.cloneSource } : null,
    cloneOffset: draft.cloneOffset ? { ...draft.cloneOffset } : null,
  }
  clearDraft()
  if (!spec.chapterId || spec.pageIndex === null) return false
  const key = `${spec.chapterId}:${spec.pageId}`
  const previous = gestureTails.get(key) ?? Promise.resolve()
  const run = () => {
    switch (spec.tool) {
    case 'brush':
      return createMask(spec)
    case 'cloneHeal':
      return cloneInto(spec)
    case 'shapes':
      return createMask(spec, shapeEngine(spec))
    case 'maskSelect':
      return editMask(spec)
    default:
      return createMask(spec)
    }
  }
  const next = previous.catch(() => false).then(run).catch(reportRegionEditFailure)
  gestureTails.set(key, next)
  void next.then(
    () => { if (gestureTails.get(key) === next) gestureTails.delete(key) },
    () => { if (gestureTails.get(key) === next) gestureTails.delete(key) },
  )
  return next
}

/**
 * What a Shapes commit adds to its parameters: the rung it was pointed at.
 *
 * Shapes' `mode` row is one list of six - a solid colour, or one of the five
 * cleaning rungs (`tools.js#SHAPE_MODES`) - and the seam has a field for
 * exactly one of those: `params.engine`, which
 * `src-tauri/src/region.rs#named_rung` reads and runs. A solid fill names no
 * engine at all, because it is not a clean: it crosses as `params.paint`, and
 * the backend's paint branch never reaches the ladder.
 *
 * @returns {Record<string, unknown>}
 */
function shapeEngine(spec) {
  const mode = String(spec.params?.mode ?? SOLID)
  return mode === SOLID ? {} : { engine: mode }
}

/**
 * @typedef {Object} CommitSpec
 * @property {string} tool
 * @property {string} kind - the draft's shape: `stroke` is the only one that carries a path
 * @property {string} pageId
 * @property {import('./gesture.js').Bbox} bbox
 * @property {'add'|'erase'|'paint'} mode - `erase` is the selection tool's remove
 * @property {Array<{x: number, y: number}>} points
 * @property {{x: number, y: number}} start
 * @property {string|null} chapterId
 * @property {number|null} pageIndex
 * @property {number|null} sourceIndex
 * @property {string|null} sourceSha
 * @property {Record<string, unknown>} params
 * @property {any} cloneSource
 * @property {any} cloneOffset
 */

/**
 * The **path** a gesture swept, or `null` where it swept none.
 *
 * Only a `stroke` draft has one: a brush stroke is a swept disc, and the box
 * around it is the defect this records. A Shapes
 * gesture describes an area rather than a path and sends `paintedShape`
 * below instead; the two payloads are exclusive.
 *
 * The size falls back to the same number `DrawLayer` draws the preview at, so
 * what was on screen and what crosses the seam are one width.
 *
 * @param {CommitSpec} spec
 * @returns {{points: Array<{x: number, y: number}>, radius: number}|null}
 */
function strokeOf(spec) {
  if (spec.kind !== 'stroke') return null
  const size = (spec.params ?? paramsOf(spec.tool)).size ?? (spec.tool === 'aiMaskBrush' ? AI_STROKE_PX : 0)
  return paintedStroke(spec.points, Number(size))
}

/**
 * The **area** a Shapes gesture describes, in the shape the seam carries:
 * the outline and the feather, not the
 * rectangle around them.
 *
 * Until this, only the bbox crossed - so an ellipse cleaned its bounding
 * rectangle and a lasso cleaned the box its curve fitted inside, a defect
 * in the one tool it had not yet been
 * fixed in. Three kinds cover the four shapes: a lasso *is* a polygon, drawn
 * freehand rather than clicked.
 *
 * **Vector, and in the same two spaces `paintedStroke` uses.** The vertices
 * are page percent, like every other `bbox` on the seam; `feather` is page
 * pixels, because it is a distance on the image rather than on the screen and
 * the backend is the only side that knows the page's real size.
 *
 * The selection tool's lasso and rectangle cross in the same shape, because
 * they are the same gestures: `editDetectionMask` reads `painted` exactly as
 * `createRegion` does. It has no feather row, so its feather is 0.
 *
 * @param {CommitSpec} spec
 * @returns {{kind: 'rect'|'ellipse'|'polygon', points: Array<{x: number, y: number}>, feather: number}|null}
 */
export function paintedShape(spec) {
  if (spec.tool !== 'shapes' && spec.tool !== 'maskSelect') return null
  const box = spec.bbox
  if (!box) return null
  const feather = Math.max(0, Number((spec.params ?? paramsOf(spec.tool)).feather ?? 0) || 0)

  if (spec.kind === 'line') {
    const points = (spec.points?.length ?? 0) >= 2
      ? spec.points.filter((point) => point && Number.isFinite(point.x) && Number.isFinite(point.y))
      : [
          { x: box.x, y: box.y },
          { x: box.x + box.w, y: box.y + box.h },
        ]
    if (points.length < 2) return null
    return {
      kind: 'line',
      points: [place(points[0]), place(points.at(-1))],
      feather: Math.max(0.5, Number(spec.params?.outlineWidth ?? 2) / 2),
    }
  }

  // A rectangle and an ellipse are their box: the drag says two corners, and
  // the keyboard route says a rectangle outright. The corners travel anyway,
  // so one payload describes all four shapes and the backend has one parser.
  if (spec.kind === 'rect' || spec.kind === 'ellipse') {
    return {
      kind: spec.kind,
      points: corners(box).map(place),
      feather,
    }
  }

  // A lasso and a clicked polygon are the vertices themselves, closed by the
  // backend. Fewer than three is not an area - the surface refuses to commit
  // one, and this is the same rule where the payload is built.
  const points = (spec.points ?? []).filter(
    (point) => point && Number.isFinite(point.x) && Number.isFinite(point.y),
  )
  if (points.length < 3) return null
  return { kind: 'polygon', points: points.map(place), feather }
}

/**
 * @param {import('./gesture.js').Bbox} box
 * @returns {Array<{x: number, y: number}>} clockwise from the top left
 */
function corners(box) {
  return [
    { x: box.x, y: box.y },
    { x: box.x + box.w, y: box.y },
    { x: box.x + box.w, y: box.y + box.h },
    { x: box.x, y: box.y + box.h },
  ]
}

/**
 * @param {{x: number, y: number}} point
 * @returns {{x: number, y: number}} two decimal places, as `paintedStroke`
 *   rounds a path: enough for a 1600px page, and it keeps the payload short
 */
function place(point) {
  return { x: Math.round(point.x * 100) / 100, y: Math.round(point.y * 100) / 100 }
}

/**
 * @param {string} pageId
 * @returns {import('../api/backend.js').ApiPage|null}
 */
function pageOf(pageId) {
  return editor.chapter?.pages.find((page) => page.id === pageId) ?? null
}

/**
 * @param {string} tool
 * @returns {Record<string, unknown>}
 */
function paramsOf(tool) {
  return /** @type {any} */ ($state.snapshot(editor.toolParams[tool] ?? {}))
}

/* ------------------------------------------------------------------ */
/* Creating a mask                                                     */
/* ------------------------------------------------------------------ */

/**
 * A hand-drawn mask: what the gesture painted, cleaned where it was painted.
 *
 * @param {CommitSpec} spec
 * @param {Record<string, unknown>} [extraParams]
 * @returns {Promise<boolean>}
 */
async function createMask(spec, extraParams) {
  const chapter = editor.chapter
  const page = pageOf(spec.pageId)
  if (!chapter || !page || chapter.id !== spec.chapterId || page.index !== spec.pageIndex) return false

  const before = page.status
  const stroke = strokeOf(spec)
  const shape = paintedShape(spec)
  const mergedParams = {
    ...spec.params,
    ...(stroke ? { stroke } : {}),
    // The drawn area, for the tools whose gesture is an area rather than a
    // path. `painted` and `stroke` are never both present: a shape has no
    // radius and a stroke has no vertices.
    ...(shape ? { painted: shape } : {}),
    ...(extraParams ?? {}),
  }
  const paint = paintParamsOf(spec.tool, mergedParams, spec.points)
  const params = {
    ...mergedParams,
    ...(paint ? { paint } : {}),
  }

  // The same refusal the other two ways into the seam make, so the gate is on
  // all three rather than on the two that happen to need it today. No
  // `DRAWING_TOOLS` parameter carries `cloud: true` at the moment - the one
  // that does belongs to Content-aware fill, which does not draw - so this
  // cannot fire; one cloud option added to a drawing tool's specs would
  // otherwise be a silent hole.
  if (cloudRefused(spec.tool, params)) return false

  // Consent is bound to a stored region, so a cloud gesture creates its local
  // seed first. Both steps share one undo entry after the cloud result settles.
  const cloud = toolSpendsCloud(spec.tool, params)
  const result = await getBackend().createRegion({
    chapterId: chapter.id,
    pageIndex: spec.pageIndex,
    ...(spec.sourceIndex !== null && spec.sourceSha ? {
      sourceIndex: spec.sourceIndex, sourceSha: spec.sourceSha,
    } : {}),
    bbox: spec.bbox,
    tool: spec.tool,
    params: cloud ? withoutCloud(spec.tool, params) : params,
  })
  if (!result) return false

  // The redo half is a snapshot, like every other half of every other pair:
  // `applyRegionState` puts this same object into reactive state, and a
  // command must not hold a handle on state that later edits can move under it.
  const created = /** @type {any} */ ($state.snapshot(result.region))
  const beforeState = { region: null, pageStatus: before }
  if (editor.chapter?.id !== spec.chapterId) {
    return recordBackgroundRegionEdit(spec.chapterId, 'canvas.command.drawMask', created.id,
      beforeState, { region: created, pageStatus: result.pageStatus })
  }
  applyRegionState(result.region.id, result.region, result.pageStatus)
  // Selected only while its page is still the one in view. A result that
  // arrives after a page turn is on a page the reader has left, and a
  // selection there is one a Delete shortcut would act on unseen.
  if (scopePageIndices().includes(spec.pageIndex)) select(created.id)
  // Before the gesture the region was not there at all, and that is what the
  // delta's `before` side says: `present: false`, which `restoreRegion` reads
  // as "take it away again".
  if (cloud) {
    // The render records the creation when it lands, from nothing to the
    // rendered region: one gesture, one entry. Until then the entry is held
    // (`state/heldcreations.js`). An edit to the seed meanwhile - a Delete
    // above all - records the seed's creation first, and the render, if it
    // still lands, is then an edit of the seed: `renderBefore` is read when
    // the render records, not now.
    const renderBefore = { region: null, pageStatus: before }
    holdCreation(created.id, renderBefore, () => {
      // Into the stroke's own chapter, whichever is open when the seed is edited.
      recordRegionEdit('canvas.command.drawMask', created.id, beforeState,
        { region: created, pageStatus: result.pageStatus }, spec.chapterId)
      renderBefore.region = created
      renderBefore.pageStatus = result.pageStatus
    })
    let applied = false
    /** @type {{error: unknown}|null} */
    let thrown = null
    try {
      applied = await renderRegionInCloud(created.id, spec.tool, spec.params, renderBefore)
    } catch (error) {
      // The seed is on the page whatever went wrong, so its creation is kept
      // like a declined render's below, and the error is reported after.
      thrown = { error }
    }
    const waiting = releaseCreation(created.id, renderBefore)
    if (applied) return true
    // A page the window has evicted since is a header with no regions to look
    // in; the seed was stored and nothing recorded its removal, so it is there.
    const seedHere = () => {
      const page = pageOf(spec.pageId)
      if (!page) return false
      return page.resident === false || page.regions.some((region) => region.id === created.id)
    }
    /** @returns {Promise<boolean>} */
    const keepSeed = async () => {
      // An edit to the seed already put its creation on the history.
      if (!waiting) return seedHere()
      // A failed/declined cloud attempt leaves the local creation as one action.
      if (editor.chapter?.id !== spec.chapterId) {
        return recordBackgroundRegionEdit(spec.chapterId, 'canvas.command.drawMask', created.id,
          beforeState, { region: created, pageStatus: result.pageStatus })
      }
      if (!seedHere()) return false
      recordRegionEdit('canvas.command.drawMask', created.id, beforeState,
        { region: created, pageStatus: result.pageStatus })
      void reloadPage(spec.pageIndex).catch(() => {})
      return true
    }
    const kept = await keepSeed()
    if (thrown) throw thrown.error
    return kept
  }
  recordRegionEdit('canvas.command.drawMask', created.id, beforeState,
    { region: created, pageStatus: result.pageStatus })
  void reloadPage(spec.pageIndex).catch(() => {})
  return true
}

/* ------------------------------------------------------------------ */
/* Editing the detected masks                                          */
/* ------------------------------------------------------------------ */

/**
 * The selection tool's commit: the gesture added to the page's detected masks
 * or taken away from them.
 *
 * The page is read again afterwards rather than patched here, because an add
 * can grow one detection and a remove can empty and delete several, and the
 * native side is the one that knows which pixels went where. The reloaded
 * page moves the mask URL of each detection the edit changed, and only those
 * are fetched again (`api/tile.js#detectionMaskUrl`).
 *
 * A result that arrives after its chapter was closed leaves the chapter now
 * open alone. A selection the edit deleted is let go of, so no shortcut acts
 * on a region that is not there.
 *
 * @param {CommitSpec} spec
 * @returns {Promise<boolean>}
 */
async function editMask(spec) {
  const chapter = editor.chapter
  const page = pageOf(spec.pageId)
  if (!chapter || !page || chapter.id !== spec.chapterId || page.index !== spec.pageIndex) return false
  const stroke = strokeOf(spec)
  const painted = stroke ? null : paintedShape(spec)
  if (!stroke && !painted) return false

  /** @type {Awaited<ReturnType<import('../api/backend.js').Backend['editDetectionMask']>>} */
  let result
  try {
    result = await getBackend().editDetectionMask({
      chapterId: spec.chapterId,
      pageIndex: spec.pageIndex,
      ...(spec.sourceIndex !== null && spec.sourceSha ? {
        sourceIndex: spec.sourceIndex, sourceSha: spec.sourceSha,
      } : {}),
      mode: spec.mode === 'erase' ? 'remove' : 'add',
      ...(stroke ? { stroke } : { painted: /** @type {any} */ (painted) }),
    })
  } catch (error) {
    return reportMaskEditFailure(error)
  }
  if (!result) return false
  const touched = [...(result.changed ?? []), ...(result.created ?? []), ...(result.removed ?? [])]
  // Nothing changed and nothing was written: there is nothing to read again.
  if (touched.length === 0) return false
  if (editor.chapter?.id !== spec.chapterId) return true
  const removed = new Set(result.removed ?? [])
  if (editor.selectionId && removed.has(editor.selectionId)) select(null)
  if (editor.hoverId && removed.has(editor.hoverId)) hover(null)
  try {
    await reloadPage(spec.pageIndex)
  } catch (error) {
    console.error('the page could not be read again after a mask edit', error)
  }
  return true
}

/**
 * Say a refused mask edit. A chapter another process holds, or one that moved
 * under this window, is said in its own words; anything else is the one
 * sentence that says nothing changed, and the detail goes to the console.
 *
 * @param {unknown} error
 * @returns {false}
 */
function reportMaskEditFailure(error) {
  console.error('a mask edit was rejected', error)
  if (reportJobConflict(error)) return false
  notify({ key: 'notice.mask.selectionFailed', tone: 'warn' })
  return false
}

/**
 * The parameters with every cloud choice taken out, so the backend's own
 * default renders the region instead.
 *
 * @param {string} tool
 * @param {Record<string, unknown>} params
 * @returns {Record<string, unknown>}
 */
function withoutCloud(tool, params) {
  const local = { ...params }
  const spec = TOOL_SPECS.find((candidate) => candidate.id === tool)
  for (const param of spec?.params ?? []) {
    if (param.kind !== 'choice') continue
    if (param.options.some((option) => option.cloud && option.value === local[param.key])) delete local[param.key]
  }
  return local
}

/* ------------------------------------------------------------------ */
/* Clone / heal                                                        */
/* ------------------------------------------------------------------ */

/**
 * Paint with the stamp. The source must have been sampled first - alt-click on
 * the page, or `S` on the focused drawing surface, which is the sticky
 * equivalent the modifier needs.
 *
 * `aligned` holds the offset between source and stroke once a stroke has
 * established it, so successive strokes read on from one another; `nonAligned`
 * re-anchors to the sampled point on every stroke. The
 * arithmetic is `gesture.js#cloneOffset`; what is recorded on the mask is
 * which of the two was in force and where it read from.
 *
 * @param {CommitSpec} spec
 * @returns {Promise<boolean>}
 */
async function cloneInto(spec) {
  const params = spec.params
  const source = spec.cloneSource?.pageId === spec.pageId ? spec.cloneSource : null
  if (!source) {
    notify({ key: 'notice.tool.cloneNeedsSource', tone: 'warn' })
    return false
  }
  const resolved = cloneOffset({
    source: { x: source.x, y: source.y },
    strokeStart: spec.start,
    alignment: String(params.alignment ?? 'aligned'),
    offset: spec.cloneOffset,
  })
  if (!resolved) return false
  setCloneOffset(resolved.offset)
  return createMask(spec, {
    cloneSource: resolved.source,
    cloneOffset: resolved.offset,
  })
}
