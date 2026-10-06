<script>
  import { editor, hover, select } from '../state/editor.svelte.js'
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
    detectAreaAround,
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
  } from './gesture.js'
  import { commitDraft } from './drawing.svelte.js'
  import { detectArea } from './cloudrun.js'
  import { draftShape } from './tools.js'
  import { t } from '../i18n/index.js'
  import DraftPreview from './DraftPreview.svelte'
  import PaintLayer from './PaintLayer.svelte'
  import { flushPaintPreview } from './paintpreview.js'
  import RegionMenu from './RegionMenu.svelte'
  import { ContextMenu } from '../ui/index.js'

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
   * cut rather than as more mask.
   *
   * **A tap is not a drag.** Below `TAP_SLOP` of movement the gesture is
   * treated as a click on whatever region is under it, which is exactly what
   * `RegionLayer`'s own buttons do and what keeps Task 9's region-click seam
   * alive underneath a surface that covers them.
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
   * **Clone / heal's modifier-click has a sticky equivalent**: `S` on the
   * focused surface samples at the draft's centre, so the source can be set
   * without holding anything down. Which modifier that click carries is
   * `session.cloneSourceModifier`, set in Settings › Shortcuts.
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
  /**
   * The selection tool's menu over bare paper: where it opened, and the area
   * a Detect from it is held to (page percent).
   *
   * @type {{x: number, y: number, area: import('./gesture.js').Bbox} | null}
   */
  let detectMenu = $state(null)

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
  const livePixels = $derived(tool === 'cloneHeal' || tool === 'brush')

  /**
   * Whether the gesture that just ended was **committed** rather than
   * abandoned.
   *
   * `clearDraft` cannot tell the two apart - `commitDraft` and the Escape
   * ladder both leave `draft.active` null - and `PaintLayer` has to, because a
   * committed stroke's pixels are on their way and an abandoned one's are not.
   * This is set immediately before every route into `commitDraft` and cleared
   * when the next draft opens.
   */
  let committing = $state(false)

  /** Commit, and let the paint preview know its pixels are coming. */
  async function commit() {
    const pendingDraft = draft.active
    committing = true
    await flushPaintPreview(page.id)
    if (draft.active !== pendingDraft) { committing = false; return }
    const applied = await commitDraft()
    if (!applied) committing = false
  }

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

  /** @returns {'rect'|'ellipse'|'lasso'|'polygon'|'line'|'stroke'} */
  function draftKind() {
    return shape ?? 'stroke'
  }

  /**
   * What the draft does: the Brush paints, the selection tool's remove mode
   * erases from the detected masks, and everything else adds.
   *
   * @returns {'add'|'erase'|'paint'}
   */
  function draftMode() {
    if (tool === 'brush') return 'paint'
    if (tool === 'maskSelect' && params.mode === 'remove') return 'erase'
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

    // Clone / heal samples where it will read from. The modifier is the user's
    // (Settings › Shortcuts) because the design's `Alt` is a name no Apple
    // keyboard prints; `S` on this surface is its sticky equivalent and is not
    // a modifier, so it needs no setting of its own.
    if (tool === 'cloneHeal' && modifierHeld(event, session.cloneSourceModifier)) {
      sample(point)
      event.preventDefault()
      return
    }

    // Deliberately no `preventDefault` here: a polygon is closed by a
    // double-click, and suppressing the compatibility events suppresses that.
    if (polygonal) {
      addVertex(point)
      return
    }

    // Capture on the real pointer, so a drag that leaves the sheet - or leaves
    // the window - still ends here rather than being lost.
    surfaceEl?.setPointerCapture(event.pointerId)
    pointerId = event.pointerId
    downAt = { x: event.clientX, y: event.clientY }
    committing = false
    beginDraft({
      tool,
      kind: draftKind(),
      pageId: page.id,
      points: [point],
      bbox: null,
      mode: draftMode(),
      keyboard: false,
      moved: false,
    })
    event.preventDefault()
  }

  /** @param {PointerEvent} event */
  function onpointermove(event) {
    const point = at(event)
    if (!point) return
    cursorAt = round ? point : null

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
    if (shouldStamp(active.points.at(-1) ?? null, point, radius, Number(params.spacing ?? 12))) {
      addPoint(point)
    }
    setDraftBbox(strip ? stripBoundsOf(active.points, feathered()) : boundsOf(active.points, feathered()))
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
    if (!moved) {
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
   * **Over bare paper the selection tool offers Detect text here.** The tool
   * edits detected masks, and the one edit a brush cannot make is the mask
   * that is not there: text Detect missed, or a mask deleted by mistake. The
   * detector looks again around the pointer and adds what it finds
   * (`cloudrun.js#detectArea`). It needs a pointer, because the keyboard's
   * event names no spot on the page.
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
    if (!region) {
      if (tool !== 'maskSelect' || keyboard) return
      event.preventDefault()
      const point = pointIn(event.clientX, event.clientY, rect)
      detectMenu = {
        ...menuPoint(event, rect),
        area: detectAreaAround(point, page.width ?? 1600, page.height ?? 2400),
      }
      return
    }
    event.preventDefault()
    select(region.id)
    const anchor = keyboard ? clientCentre(region.bbox, rect) : menuPoint(event, rect)
    menu = { ...anchor, region }
  }

  /** @param {{x: number, y: number}} point */
  function sample(point) {
    setCloneSource(page.id, point)
    notify({ key: 'notice.tool.cloneSourceSet' })
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
      committing = false
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
      cloneCapable: tool === 'cloneHeal',
      step: KEY_STEP,
    })
    if (!intent) return

    if (intent.kind === 'open') openKeyboardDraft()
    // A commit the draft is not ready for is still swallowed: the key belongs
    // to this surface either way, and letting it through would page the chapter
    // out from under a half-drawn polygon.
    else if (intent.kind === 'commit') {
      if (committable()) commit()
    } else if (intent.kind === 'sample') {
      const box = active?.bbox ?? centredBbox(KEY_DRAFT.w, KEY_DRAFT.h)
      sample({ x: box.x + box.w / 2, y: box.y + box.h / 2 })
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

  function openKeyboardDraft() {
    committing = false
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
    if (pointerId === null) hover(null)
  }}
  ondblclick={() => committable() && commit()}
  {oncontextmenu}
  {onkeydown}
></button>

<RegionMenu at={menu} onclose={() => (menu = null)} />

{#if detectMenu}
  <ContextMenu
    x={detectMenu.x}
    y={detectMenu.y}
    label={t('masks.menu.page')}
    sections={[{
      id: 'detect',
      items: [{ id: 'detectHere', label: t('masks.menu.detectHere'), icon: 'sparkle', disabled: editor.run.active }],
    }]}
    onselect={() => {
      // Read the area off the open menu before closing clears it.
      const area = detectMenu?.area
      if (area) void detectArea(page.index, area)
    }}
    onclose={() => (detectMenu = null)}
  />
{/if}

<!-- The live pixels, under the cursor ring and over the artwork. Mounted only
     for the two tools it can preview, so no other tool pays for a canvas it
     never draws into. -->
{#if livePixels}
  <PaintLayer {page} {committing} />
{/if}

<!-- The brush's own footprint, under the pointer. An ellipse in page percent
     and therefore a *circle* on screen: the sheet is drawn at the page's
     aspect ratio, so the two axes are scaled by different amounts and a
     per-axis radius is what survives the trip. -->
{#if cursorAt && radius.rx > 0}
  <svg class="cursor" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
    <ellipse
      cx={cursorAt.x}
      cy={cursorAt.y}
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
