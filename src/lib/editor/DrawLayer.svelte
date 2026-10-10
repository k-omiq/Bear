<script>
  import { claimPageEdit, editor, hover, select, setToolParam } from '../state/editor.svelte.js'
  import { notify } from '../state/app.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { modifierHeld } from '../shortcuts.js'
  import {
    draft,
    addPoint,
    beginDraft,
    clearDraft,
    markMoved,
    setCloneSource,
    setCloneStroke,
    setDraftBbox,
  } from './draft.svelte.js'
  import {
    AI_STROKE_PX,
    MIN_SPAN,
    TAP_SLOP,
    boundsOf,
    brushRadius,
    centredBbox,
    clientCentre,
    cloneOffset,
    draftKeyIntent,
    menuPoint,
    moveBbox,
    pointIn,
    rectBetween,
    stripBoundsOf,
    stripRectBetween,
    regionAt,
    resizeBbox,
    shouldStamp,
    steppedSize,
  } from './gesture.js'
  import { commitDraft } from './drawing.svelte.js'
  import { draftShape, toolSpec } from './tools.js'
  import { pageVersion } from '../api/tile.js'
  import { t } from '../i18n/index.js'
  import CloneSource from './CloneSource.svelte'
  import DraftPreview from './DraftPreview.svelte'
  import PaintLayer from './PaintLayer.svelte'
  import { flushPaintPreview } from './paintpreview.js'
  import RegionMenu from './RegionMenu.svelte'

  /**
   * The drawing surface: one element over the sheet that is both the pointer
   * target for every drag tool and the keyboard route to the same result.
   *
   * It is mounted only for the tools whose gesture is a drag
   * (`DRAWING_TOOLS`); Auto clean and Content-aware fill act on a region that
   * already exists, and the surface would only take their click away.
   *
   * **The selection tool draws with the others' gestures.** Its brush is the
   * round stroke every brush tool paints, and its lasso and rectangle are
   * Shapes' own drags (`tools.js#draftShape`); what differs is the commit
   * (`drawing.svelte.js#editMask`) and the preview, where remove reads as a
   * cut rather than as more mask. In its Detect mode the drag is a rectangle
   * and the commit is a Detect held to it (`drawing.svelte.js#detectDrawn`).
   *
   * **A tap is not a drag.** Below `TAP_SLOP` of movement the gesture is
   * treated as a click on whatever region is under it, which is exactly what
   * `RegionLayer`'s own buttons do and what keeps Task 9's region-click seam
   * alive underneath a surface that covers them. **The clone stamp is the
   * exception**: a click of a stamp is one dab, so its tap paints.
   *
   * **The keyboard route** is the same element:
   * activating it opens a draft rectangle at the centre of the page, the arrows
   * move it and `Shift`+arrows resize it, `Enter` commits and `Escape`
   * abandons. A brush stroke is not paintable by arrow keys and does not need
   * to be - but a mask must be creatable, adjustable and removable without a
   * pointer, and this is the creation half. Every arrow the surface handles is
   * stopped as well as prevented, or the editor's global ← / → would page the
   * chapter underneath it.
   *
   * **The clone stamp** is driven the way every raster editor's is. A click
   * with the sample modifier held picks the source - which modifier is
   * `session.cloneSourceModifier`, set in Settings › Shortcuts - and after
   * that a click stamps one dab and a drag paints. `Shift`-click draws a
   * straight line from where the last stroke ended, and `Shift` held during a
   * drag keeps it horizontal or vertical. Where a stroke reads from is fixed
   * when it starts and rides on the draft (`resolveClone`), so the preview and
   * the commit cannot disagree. On the focused surface `S` samples without a
   * modifier, `[` and `]` step the brush's size and, with `Shift`, its
   * hardness; `Enter` opens one dab that the arrows move and `Enter` stamps,
   * which is the stamp without a pointer.
   */

  /**
   * @type {{
   *   page: import('../api/backend.js').ApiPage,
   *   tabbable?: boolean,
   *   strip?: boolean,
   *   stripMinY?: number,
   *   stripMaxY?: number,
   * }}
   */
  let { page, tabbable = true, strip = false, stripMinY = 0, stripMaxY = 100 } = $props()

  /** How much of the page a keyboard draft starts as, and how far a key moves it. */
  const KEY_DRAFT = { w: 18, h: 11 }
  const KEY_STEP = 1
  /** How close a polygon's last vertex must land to its first to close it. */
  const CLOSE_WITHIN = 2.5

  /** @type {HTMLElement|undefined} */
  let surfaceEl = $state()
  /** @type {number|null} */
  let pointerId = null
  /** @type {{x: number, y: number}|null} */
  let downAt = null
  /**
   * Where the round cursor is, in page percent.
   *
   * A brush whose cursor is a crosshair is a brush nobody can aim: the one
   * thing a person needs to see before pressing down is **how much paper the
   * next stamp covers**, and until this the answer was only visible after the
   * stroke had been committed. Null when the pointer is not over the sheet, so
   * nothing is drawn where nothing is hovering.
   *
   * @type {{x: number, y: number}|null}
   */
  let cursorAt = $state(null)
  /**
   * The open region context menu - the same one `RegionLayer` raises, because
   * while a drag tool is armed this surface is what the pointer reaches and
   * the region buttons under it are inert.
   *
   * @type {{x: number, y: number, region: import('../api/backend.js').ApiRegion} | null}
   */
  let menu = $state(null)

  const tool = $derived(editor.tool)
  const params = $derived(editor.toolParams[tool] ?? {})
  /** The area a drag draws, or null where the drag is a round brush stroke. */
  const shape = $derived(draftShape(tool, params))
  const polygonal = $derived(shape === 'polygon')
  const active = $derived(draft.active?.pageId === page.id ? draft.active : null)
  /** The tools, and the selection tool's brush, that paint a round stroke rather than drag a shape. */
  const round = $derived(shape === null)

  /**
   * The two tools whose preview is pixels rather than an outline
   * (`PaintLayer.svelte`). Paint lays down colour and clone reads the page from
   * somewhere else; for both of them a tinted capsule says nothing about what
   * the commit will actually look like.
   */
  const livePixels = $derived(tool === 'cloneStamp' || tool === 'brush')
  const stamping = $derived(tool === 'cloneStamp')
  const detecting = $derived(tool === 'maskSelect' && params.mode === 'detect')

  /** Detection submits another area; other gestures edit the held page. */
  function claimGesture() {
    return detecting || claimPageEdit(page.index)
  }

  /**
   * How many of this page's gestures are still being committed, and how many
   * came back having changed nothing. `PaintLayer` holds a committed stroke's
   * last frame until the page shows it, and these two are how it knows when
   * that can happen and when it never will.
   */
  let inFlight = $state(0)
  let dropped = $state(0)
  /**
   * Sends the gesture that has ended and is waiting on its last preview
   * frame. A stamp is used in quick taps: the next press calls this first, so
   * a tap is never replaced by the one after it.
   *
   * @type {(() => void)|null}
   */
  let settle = null

  /** Commit the draft, once the preview has caught up with where it ended. */
  async function commit() {
    const pending = draft.active
    if (!pending) return
    if (pending.clone && pending.points.length > 0) {
      setCloneStroke(page.id, pending.clone.offset, /** @type {any} */ (pending.points.at(-1)))
    }
    inFlight += 1
    /** @type {Promise<boolean>} */
    let sent = Promise.resolve(false)
    const send = () => {
      settle = null
      // Escape may have dropped the gesture while its last frame was fetched.
      if (draft.active === pending) sent = commitDraft()
    }
    settle = send
    await flushPaintPreview(page.id)
    if (settle === send) send()
    const applied = await sent
    inFlight -= 1
    if (!applied) dropped += 1
  }

  /**
   * Where a clone stroke starting at `start` reads from, or null until a
   * source has been sampled on this page.
   *
   * @param {{x: number, y: number}} start
   */
  function resolveClone(start) {
    const sampled = draft.cloneSource?.pageId === page.id ? draft.cloneSource : null
    return cloneOffset({
      source: sampled ? { x: sampled.x, y: sampled.y } : null,
      strokeStart: start,
      alignment: String(params.alignment ?? 'aligned'),
      offset: draft.cloneOffset,
    })
  }

  /** The sample modifier is down: the next click picks a source. */
  let sampling = $state(false)
  /**
   * A stroke held to one axis by `Shift`: the stroke's last point when the
   * hold began, in client pixels and on the page, and which way the drag went
   * from there once it had said.
   *
   * @type {{x: number, y: number, at: {x: number, y: number}, axis: 'x'|'y'|null}|null}
   */
  let lock = null
  /** The stroke began as a `Shift`-click line, so `Shift` is not an axis lock. */
  let joined = false
  /** Where the keyboard's stamp was last put, so the next one opens there. */
  let keyAt = /** @type {{x: number, y: number}|null} */ (null)

  /**
   * The stroke half-width in page percent, per axis. Zero for a drag that
   * draws an area: the selection tool keeps its brush `size` while its lasso
   * or rectangle is armed, and that size must neither space the lasso's
   * vertices (a 160 px brush put them tens of pixels apart) nor pad its box
   * nor draw a footprint.
   */
  const radius = $derived(
    round
      ? brushRadius(
          Number(params.size ?? (tool === 'aiMaskBrush' ? AI_STROKE_PX : 0)),
          page.width ?? 1600,
          page.height ?? 2400,
        )
      : { rx: 0, ry: 0 },
  )

  /**
   * The round cursor: under the pointer, or on the keyboard's stamp while one
   * is open.
   */
  const ringAt = $derived(stamping && active?.keyboard ? (active.points[0] ?? null) : cursorAt)

  /**
   * Where the stamp reads from for a brush at the ring: the stroke's own
   * source moved along with it while one is being drawn, and otherwise what a
   * stroke started here would read. With no brush on the sheet it is the
   * sampled point itself.
   */
  const cloneFrom = $derived.by(() => {
    if (!stamping) return null
    if (!ringAt) return draft.cloneSource?.pageId === page.id ? draft.cloneSource : null
    const offset = active?.clone?.offset
    if (offset) return { x: ringAt.x + offset.x, y: ringAt.y + offset.y }
    return resolveClone(ringAt)?.source ?? null
  })

  /** @returns {'rect'|'ellipse'|'lasso'|'polygon'|'line'|'stroke'} */
  function draftKind() {
    return shape ?? 'stroke'
  }

  /**
   * What the draft does: the Brush paints, the selection tool's remove mode
   * erases from the detected masks, its Detect mode asks the detector about
   * the area, and everything else adds.
   *
   * @returns {'add'|'erase'|'paint'|'detect'}
   */
  function draftMode() {
    if (tool === 'brush') return 'paint'
    if (tool === 'maskSelect' && params.mode === 'remove') return 'erase'
    if (tool === 'maskSelect' && params.mode === 'detect') return 'detect'
    return 'add'
  }

  /** @returns {DOMRect|null} the sheet's own box - the drawn scale, measured */
  function sheetRect() {
    return surfaceEl?.getBoundingClientRect() ?? null
  }

  /**
   * @param {PointerEvent} event
   * @returns {{x: number, y: number, p?: number}|null}
   */
  function at(event) {
    const rect = sheetRect()
    if (!rect) return null
    // Mouse pressure is fixed at 0.5 while pressed, which otherwise halves
    // pressure-sensitive brush size. Mouse and touch use the full footprint;
    // a pen keeps its measured pressure.
    const pressure = event.pointerType === 'pen' ? event.pressure : 1
    const point = pointIn(event.clientX, event.clientY, rect, pressure, !strip)
    if (strip) point.y = Math.min(stripMaxY, Math.max(stripMinY, point.y))
    return point
  }

  /* ---------------------------------------------------------------- */
  /* Pointer                                                           */
  /* ---------------------------------------------------------------- */

  /** @param {PointerEvent} event */
  function onpointerdown(event) {
    if (event.button !== 0) return
    const point = at(event)
    if (!point) return
    surfaceEl?.focus({ preventScroll: true })

    // The clone stamp samples where it will read from. The modifier is the
    // user's (Settings › Shortcuts) because the design's `Alt` is a name no
    // Apple keyboard prints; `S` on this surface is its sticky equivalent and
    // is not a modifier, so it needs no setting of its own.
    if (stamping && modifierHeld(event, session.cloneSourceModifier)) {
      sample(point)
      event.preventDefault()
      return
    }

    // Page edits wait while a run holds the page. Detect draws another area
    // for its own background job, so additional rectangles remain available.
    if (!claimGesture()) {
      event.preventDefault()
      return
    }

    // Deliberately no `preventDefault` here: a polygon is closed by a
    // double-click, and suppressing the compatibility events suppresses that.
    if (polygonal) {
      addVertex(point)
      return
    }

    // The gesture before this one may still be waiting on its last preview
    // frame. It goes now, so this one does not take its place.
    settle?.()

    let points = [point]
    /** @type {ReturnType<typeof resolveClone>} */
    let clone = null
    if (stamping) {
      // `Shift`-click joins the end of the last stroke to this point with a
      // straight line, which is how a panel border is followed.
      const from = event.shiftKey && draft.cloneLast?.pageId === page.id ? draft.cloneLast : null
      if (from) points = [{ x: from.x, y: from.y, p: point.p }, point]
      clone = resolveClone(points[0])
      if (!clone) {
        notify({ key: 'notice.tool.cloneNeedsSource', tone: 'warn' })
        event.preventDefault()
        return
      }
      joined = from !== null
    }
    lock = null

    // Capture on the real pointer, so a drag that leaves the sheet - or leaves
    // the window - still ends here rather than being lost.
    surfaceEl?.setPointerCapture(event.pointerId)
    pointerId = event.pointerId
    downAt = { x: event.clientX, y: event.clientY }
    beginDraft({
      tool,
      kind: draftKind(),
      pageId: page.id,
      points,
      bbox: null,
      mode: draftMode(),
      keyboard: false,
      moved: false,
      ...(clone ? { clone } : {}),
    })
    // A press of the stamp is already a mark: one dab, where it is.
    if (stamping) setDraftBbox(strokeBounds(points))
    event.preventDefault()
  }

  /**
   * The box a stroke's points sweep with the brush that is armed.
   *
   * @param {Array<{x: number, y: number}>} points
   */
  function strokeBounds(points) {
    return strip ? stripBoundsOf(points, feathered()) : boundsOf(points, feathered())
  }

  /**
   * A stamp stroke's next point while `Shift` holds it to one axis, or null
   * while the drag has not yet said which axis that is. Releasing `Shift`
   * frees the stroke again, and pressing it later holds it from there.
   *
   * @param {PointerEvent} event
   * @param {{x: number, y: number, p?: number}} point
   */
  function held(event, point) {
    if (!event.shiftKey || joined || !active) {
      lock = null
      return point
    }
    if (!lock) {
      // Measured from the stroke's own last point, not from this event: the
      // first move with `Shift` down is already part of the drag.
      const from = active.points.at(-1) ?? point
      const rect = sheetRect()
      lock = {
        x: rect ? rect.left + (from.x / 100) * rect.width : event.clientX,
        y: rect ? rect.top + (from.y / 100) * rect.height : event.clientY,
        at: from,
        axis: null,
      }
    }
    if (!lock.axis) {
      const dx = Math.abs(event.clientX - lock.x)
      const dy = Math.abs(event.clientY - lock.y)
      if (Math.max(dx, dy) <= TAP_SLOP) return null
      lock.axis = dx >= dy ? 'x' : 'y'
    }
    return lock.axis === 'x' ? { ...point, y: lock.at.y } : { ...point, x: lock.at.x }
  }

  /** @param {PointerEvent} event */
  function onpointermove(event) {
    const point = at(event)
    if (!point) return
    cursorAt = round ? point : null
    if (stamping) sampling = modifierHeld(event, session.cloneSourceModifier)

    if (pointerId === null) {
      // Not drawing: still light whatever is under the pointer, so the Layers
      // panel keeps answering the canvas even with the surface over it.
      hover(regionAt(point, page.regions)?.id ?? null)
      return
    }
    if (!active) return

    if (downAt && Math.hypot(event.clientX - downAt.x, event.clientY - downAt.y) > TAP_SLOP) {
      markMoved()
    }

    if (active.kind === 'rect' || active.kind === 'ellipse' || active.kind === 'line') {
      // Shift is the constraint every drawing application binds it to: a
      // square, and therefore a circle. Held rather than latched, so it can be
      // pressed and released mid-drag and the box follows.
      setDraftBbox(
        event.shiftKey
          ? squareBetween(active.points[0], point)
          : strip ? stripRectBetween(active.points[0], point) : rectBetween(active.points[0], point),
      )
      if (active.kind === 'line') active.points = [active.points[0], point]
      return
    }
    const next = stamping ? held(event, point) : point
    if (next && shouldStamp(active.points.at(-1) ?? null, next, radius, Number(params.spacing ?? 12))) {
      addPoint(next)
    }
    setDraftBbox(strokeBounds(active.points))
  }

  /** @param {PointerEvent} event */
  function onpointerup(event) {
    if (pointerId === null) return
    const point = at(event)
    if (point && active?.kind === 'line') {
      active.points = [active.points[0], point]
      setDraftBbox(strip ? stripRectBetween(active.points[0], point) : rectBetween(active.points[0], point))
    }
    releasePointer()
    const moved = active?.moved === true
    // A click of the stamp is one dab. A click of any other tool is a click
    // on whatever is under it.
    if (!moved && !stamping) {
      clearDraft()
      if (point) tap(point)
      return
    }
    commit()
  }

  function onpointercancel() {
    releasePointer()
    clearDraft()
  }

  function releasePointer() {
    if (pointerId !== null) surfaceEl?.releasePointerCapture?.(pointerId)
    pointerId = null
    downAt = null
  }

  /**
   * A click rather than a drag: selects the region under it, or clears the
   * selection on empty canvas.
   *
   * @param {{x: number, y: number}} point
   */
  function tap(point) {
    const region = regionAt(point, page.regions)
    if (region) select(region.id)
    else select(null)
  }

  /**
   * The secondary press, routed to the region under it exactly as a tap is.
   *
   * `onpointerdown` already drops every button but the left one, so a
   * right-click never starts a stroke; this is what it does instead. Raised
   * from the keyboard - `Shift`+`F10`, the context-menu key - it carries no
   * position and the surface covers the whole sheet, so the region it is about
   * can only be the selected one, and the menu opens over that region's own
   * middle rather than the middle of the sheet.
   *
   * @param {MouseEvent} event
   */
  function oncontextmenu(event) {
    const rect = sheetRect()
    if (!rect) return
    const keyboard = event.clientX <= 0 && event.clientY <= 0
    const region = keyboard
      ? ((page.regions ?? []).find((candidate) => candidate.id === editor.selectionId) ?? null)
      : regionAt(pointIn(event.clientX, event.clientY, rect), page.regions)
    if (!region) return
    event.preventDefault()
    select(region.id)
    const anchor = keyboard ? clientCentre(region.bbox, rect) : menuPoint(event, rect)
    menu = { ...anchor, region }
  }

  /** @param {{x: number, y: number}} point */
  function sample(point) {
    setCloneSource(page.id, point)
    // A keyboard stamp that is open reads from the new source at once.
    if (active?.keyboard && stamping) active.clone = resolveClone(active.points[0]) ?? undefined
    notify({ key: 'notice.tool.cloneSourceSet' })
  }

  /**
   * Whether the sample modifier is down, kept up to date between pointer
   * moves: the target cursor has to appear when the key goes down, not when
   * the pointer next moves.
   *
   * @param {KeyboardEvent} event
   */
  function trackModifier(event) {
    if (stamping) sampling = modifierHeld(event, session.cloneSourceModifier)
  }

  /**
   * The box two corners describe when `Shift` says it must be **square on the
   * page** - and square is a statement about pixels, not about percentages.
   *
   * A bbox is page percent on both axes and the page is not square, so equal
   * percentages are a rectangle: on a 1600×2400 page, 10% by 10% is 160 by 240
   * pixels and would draw an obvious oblong under a key whose whole promise is
   * that it does not. The longer side in *pixels* wins, and it is converted
   * back per axis.
   *
   * @param {{x: number, y: number}} from
   * @param {{x: number, y: number}} to
   * @returns {import('./gesture.js').Bbox}
   */
  function squareBetween(from, to) {
    const pageW = page.width ?? 1600
    const pageH = page.height ?? 2400
    const side = Math.max(
      (Math.abs(to.x - from.x) / 100) * pageW,
      (Math.abs(to.y - from.y) / 100) * pageH,
    )
    const signX = to.x < from.x ? -1 : 1
    const signY = to.y < from.y ? -1 : 1
    const corner = {
      x: from.x + (signX * side * 100) / pageW,
      y: from.y + (signY * side * 100) / pageH,
    }
    return strip ? stripRectBetween(from, corner) : rectBetween(from, corner)
  }

  /** The `feather` parameter, as a radius in page percent on each axis. */
  function feathered() {
    const feather = tool === 'shapes' ? Number(params.feather ?? 0) : 0
    return {
      rx: radius.rx + (feather / (page.width ?? 1600)) * 100,
      ry: radius.ry + (feather / (page.height ?? 2400)) * 100,
    }
  }

  /* ---------------------------------------------------------------- */
  /* Polygon                                                           */
  /* ---------------------------------------------------------------- */

  /**
   * Whether the draft is finished enough to commit.
   *
   * A polygon needs three vertices before it is a shape, and `hasDraft` cannot
   * say so: the first click already gives the draft a bounding box (a
   * `MIN_SPAN` one), so `Enter` and the double-click would otherwise turn one
   * or two clicks into a mask. Closing on the first vertex asks for three
   * already; this is the same rule on the other two ways to finish.
   *
   * @returns {boolean}
   */
  function committable() {
    if (!active?.bbox) return false
    return polygonal ? (active.points?.length ?? 0) >= 3 : true
  }

  /**
   * Click-click-close. Three ways to finish, because a polygon is the one
   * shape with no natural end: click back on the first vertex, double-click,
   * or press Enter - all three needing three vertices - and `Escape` abandons
   * it, like every other gesture.
   *
   * @param {{x: number, y: number}} point
   */
  function addVertex(point) {
    if (!active) {
      beginDraft({
        tool,
        kind: 'polygon',
        pageId: page.id,
        points: [point],
        bbox: null,
        mode: draftMode(),
        keyboard: false,
        moved: true,
      })
    } else {
      const first = active.points[0]
      if (active.points.length >= 3 && Math.hypot(point.x - first.x, point.y - first.y) <= CLOSE_WITHIN) {
        commit()
        return
      }
      addPoint(point)
    }
    setDraftBbox(
      strip
        ? stripBoundsOf(draft.active?.points ?? [], feathered())
        : boundsOf(draft.active?.points ?? [], feathered()),
    )
  }

  /* ---------------------------------------------------------------- */
  /* Keyboard                                                          */
  /* ---------------------------------------------------------------- */

  /** @param {KeyboardEvent} event */
  function onkeydown(event) {
    const intent = draftKeyIntent(event, {
      hasDraft: !!active?.bbox,
      cloneCapable: stamping,
      step: KEY_STEP,
    })
    if (!intent) return

    // The keyboard follows the same rule as the pointer: page edits wait,
    // while Detect can submit another selected area.
    if (intent.kind === 'open') {
      if (claimGesture()) openKeyboardDraft()
    }
    // A commit the draft is not ready for is still swallowed: the key belongs
    // to this surface either way, and letting it through would page the chapter
    // out from under a half-drawn polygon.
    else if (intent.kind === 'commit') {
      if (stamping && !active?.clone) notify({ key: 'notice.tool.cloneNeedsSource', tone: 'warn' })
      else if (committable() && claimGesture()) commit()
    } else if (intent.kind === 'sample') {
      sample(keyPoint())
    } else if (intent.kind === 'size' || intent.kind === 'hardness') {
      stepBrush(intent.kind, intent.direction)
    } else if (stamping) {
      moveStamp(intent.dx, intent.dy, intent.kind === 'resize')
    } else if (active?.bbox) {
      setDraftBbox(
        intent.kind === 'resize'
          ? resizeBbox(active.bbox, intent.dx, intent.dy)
          : moveBbox(active.bbox, intent.dx, intent.dy),
      )
    }

    event.preventDefault()
    // The editor binds ← / → to paging the chapter and cannot tell an arrow
    // meant for this surface from one meant for the chapter.
    event.stopPropagation()
  }

  /**
   * Where the keyboard's stamp is: the open one, else under the pointer, else
   * where the last was left, else the middle of the page.
   */
  function keyPoint() {
    return active?.points[0] ?? cursorAt ?? keyAt ?? { x: 50, y: 50 }
  }

  /**
   * Move the keyboard's stamp, five steps at a time with `Shift`. A stamp has
   * no box to resize; its size is the brush's, on `[` and `]`.
   *
   * @param {number} dx
   * @param {number} dy
   * @param {boolean} far
   */
  function moveStamp(dx, dy, far) {
    if (!active?.keyboard) return
    const from = active.points[0]
    const reach = far ? 5 : 1
    const point = {
      x: Math.min(100, Math.max(0, from.x + dx * reach)),
      y: Math.min(strip ? stripMaxY : 100, Math.max(strip ? stripMinY : 0, from.y + dy * reach)),
      p: 1,
    }
    active.points = [point]
    active.clone = resolveClone(point) ?? undefined
    setDraftBbox(strokeBounds([point]))
    keyAt = point
  }

  /**
   * One press of `[` or `]`: the brush's size, or with `Shift` its hardness a
   * quarter at a time, inside the range the tool's own row allows.
   *
   * @param {'size'|'hardness'} key
   * @param {number} direction
   */
  function stepBrush(key, direction) {
    const row = toolSpec(tool).params.find((param) => param.key === key)
    if (row?.kind !== 'range') return
    const now = Number(params[key] ?? row.min)
    const next = key === 'size'
      ? steppedSize(now, direction, row.min, row.max)
      : Math.min(row.max, Math.max(row.min, now + Math.sign(direction) * 25))
    setToolParam(tool, key, next)
    // The keyboard's stamp is drawn at the brush's size, so its box follows.
    if (active?.keyboard && stamping) setDraftBbox(strokeBounds(active.points))
  }

  function openKeyboardDraft() {
    if (stamping) {
      // One dab, where the arrows can move it. Before a source is sampled it
      // is only a place: `S` samples there, and it stamps once it has one.
      const point = { ...keyPoint(), p: 1 }
      keyAt = point
      beginDraft({
        tool,
        kind: 'stroke',
        pageId: page.id,
        points: [point],
        bbox: strokeBounds([point]),
        mode: draftMode(),
        keyboard: true,
        moved: true,
        clone: resolveClone(point) ?? undefined,
      })
      return
    }
    beginDraft({
      tool,
      kind: tool === 'shapes' && (shape === 'ellipse' || shape === 'line') ? shape : 'rect',
      pageId: page.id,
      points: [],
      bbox: centredBbox(Math.max(KEY_DRAFT.w, MIN_SPAN), Math.max(KEY_DRAFT.h, MIN_SPAN)),
      mode: draftMode(),
      keyboard: true,
      moved: true,
    })
  }
</script>

<svelte:window onkeydown={trackModifier} onkeyup={trackModifier} onblur={() => (sampling = false)} />

<button
  type="button"
  class="surface"
  class:drawing={!!active}
  class:ringed={round}
  bind:this={surfaceEl}
  tabindex={tabbable ? 0 : -1}
  aria-label={active ? t('canvas.action.drawActive') : t('canvas.action.draw')}
  {onpointerdown}
  {onpointermove}
  {onpointerup}
  {onpointercancel}
  onpointerleave={() => {
    cursorAt = null
    sampling = false
    if (pointerId === null) hover(null)
  }}
  ondblclick={() => committable() && commit()}
  {oncontextmenu}
  {onkeydown}
></button>

<RegionMenu at={menu} onclose={() => (menu = null)} />

<!-- The live pixels, under the cursor ring and over the artwork. Mounted only
     for the two tools it can preview, so no other tool pays for a canvas it
     never draws into. -->
{#if livePixels}
  <PaintLayer {page} busy={inFlight} {dropped} />
{/if}

<!-- What the stamp would copy, inside the ring, and a cross on the place it
     reads from. Under the ring so the ring stays the cursor's edge. -->
{#if stamping}
  <CloneSource at={ringAt} {radius} source={cloneFrom} {sampling} version={pageVersion(page, 'cleaned')} />
{/if}

<!-- The brush's own footprint, under the pointer. An ellipse in page percent
     and therefore a *circle* on screen: the sheet is drawn at the page's
     aspect ratio, so the two axes are scaled by different amounts and a
     per-axis radius is what survives the trip. -->
{#if ringAt && radius.rx > 0 && !(stamping && sampling)}
  <svg class="cursor" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
    <ellipse
      cx={ringAt.x}
      cy={ringAt.y}
      rx={radius.rx}
      ry={radius.ry}
      vector-effect="non-scaling-stroke"
    />
  </svg>
{/if}

<DraftPreview pageId={page.id} pageWidth={page.width ?? 1600} pageHeight={page.height ?? 2400} regions={page.regions ?? []} />

<style>
  .surface {
    position: absolute;
    inset: 0;
    padding: 0;
    border: 0;
    background: transparent;
    /* A tool that drags an area (a shape, the selection's lasso or rectangle)
       keeps the ordinary pointer, before and during the drag: the outline it
       draws is the feedback, and a changed pointer only hides where it is. */
    cursor: default;
    /* A drag must not be turned into a pan or a page zoom by the browser. */
    touch-action: none;
  }

  /* The sheet is --paper in both themes, so the global --accent focus ring
     would disappear on the page in dark. */
  .surface:focus-visible {
    outline: 2px solid var(--page-mark);
    outline-offset: -3px;
  }

  /* A round-brush tool draws its own footprint (`.cursor` below) and that ring
     *is* the pointer: the platform's own glyph inside it added a second,
     differently-shaped mark at the same place, which read as a pen nib sitting
     on the paper and hid the pixels the stroke is being aimed at. The ring
     alone is the cursor for those tools; an area tool keeps the pointer,
     because a drag between two corners has no footprint to show. */
  .surface.ringed { cursor: none }

  .cursor {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
    overflow: visible;
  }

  /* An outline rather than a fill: the point of it is to show what the stamp
     will cover, and a filled disc hides the artwork the user is aiming at.
     Blue rather than ink, because the artwork it is aimed at is black ink and
     a dark ring vanishes into it - and blue is lighter than the ink it
     replaces, so it carries a higher opacity to stay as legible. */
  .cursor ellipse {
    fill: none;
    stroke: var(--page-mark);
    stroke-width: 1;
    /* Less transparent than the near-black ring was: the point of the mark
       colour is that it survives black artwork, and .55 of it did not. At .9
       over --paper the ring still measures 3.49:1 light / 3.26:1 dark; at .8
       the dark sheet drops to 2.82:1 and fails WCAG 1.4.11. */
    opacity: .9;
  }
</style>
