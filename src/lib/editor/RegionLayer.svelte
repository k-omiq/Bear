<script>
  import { tick } from 'svelte'
  import {
    editor,
    hover,
    isHighlighted,
    readingDirection,
    select,
  } from '../state/editor.svelte.js'
  import { readingOrder, regionMarker } from './regions.js'
  import { menuPoint } from './gesture.js'
  import { updateLayer } from './maskactions.svelte.js'
  import {
    canTransform,
    layerCapabilities,
    layerFrame,
    layerOf,
    normalizeAngle,
    sourceBboxOf,
  } from '../model/layers.js'
  import {
    DRAG_SLOP,
    clampOffset,
    clientPointToPage,
    nudgeFor,
    turnFor,
    turnedRotation,
    TURN,
    TURN_LARGE,
  } from './layertransform.js'
  import { t } from '../i18n/index.js'
  import RegionMenu from './RegionMenu.svelte'

  /**
   * Everything the *application* draws on a page: the region outlines, the two
   * region markers, and the hit target for each region. Not the artwork -
   * `PageArtwork` is the image, and this layer sits over it.
   *
   * **A cleaned region is an outline, never a fill.** The overlay's job is to
   * say "a layer exists here"; a filled box says it by hiding the very artwork
   * the user is judging. So a region is drawn as a dashed, sky-blue outline
   * and nothing else - blue rather than ink because the artwork under it is
   * black ink, and a dark dash on a dark panel is one the user cannot find -
   * and it is **off by default**:
   *
   *   - pointer over a region, or keyboard focus on it - its outline appears.
   *     Hover shows, it does not select.
   *   - a click selects the region, and a selected region keeps its outline
   *     until something else is selected or `Esc` clears it.
   *   - the Layers panel selecting a row lights the same outline, through the
   *     shared highlight contract and not through any reference between them.
   *   - the mask overlay (`M`) shows **every** region's outline at once. It is
   *     off when a chapter opens: an editor that starts covered in boxes is one
   *     the user has to switch off before they can look at the page.
   *
   * **A detection draws no box here.** It is drawn as its mask, the area Clean
   * will erase, by `DetectionMasks` under this layer; its button stays, and
   * draws only the keyboard focus ring.
   *
   * **Every region is a real `<button>`**, so it has a role, an accessible
   * name and native activation. The prototype is pointer-only; that is not
   * shippable. The set carries a **roving
   * tabindex** the way `PageList` does - a flat list of dozens of buttons in
   * the tab order is worse than no keyboard route - and the arrows move
   * between regions in *reading order*, right-to-left by default, which is the
   * order the page is actually read in.
   *
   * Arrow keys `stopPropagation()`. The global shortcut layer binds ← and → in
   * editor scope to page the chapter, and it cannot tell an arrow meant for a
   * local widget from one meant for the chapter; `FloatingWindow` already
   * settles that the same way.
   *
   * **Nothing here is clipped by the wipe.** An outline is a statement about
   * the region, not about the cleaned pixels, so it holds wherever the wipe
   * stands. A declined region's marker is the strongest case of the same rule:
   * it is on the page whatever the overlay, the filter or the wipe says,
   * because it is the one outcome where nothing visibly happened.
   *
   * Selection and hover go through the contract in `state/editor.svelte.js`
   * and nowhere near the Layers panel: `hover()` on pointer and focus,
   * `select()` on activation, `isHighlighted()` to read.
   *
   * **Moving and turning a layer happens here, on the page.** A layer whose
   * native capabilities say `movable` (a fill or a painted colour; never a
   * redraw, never a detection - `model/layers.js`) gets a frame while it is
   * selected: its own box, moved and turned exactly as the compositor moves
   * and turns its pixels, with a turn handle on a stem from its top edge.
   * Dragging the layer moves it; dragging the handle turns it about the
   * frame's centre, which is the pivot the native side uses. Both gestures
   * capture the pointer, measure against the sheet's drawn box so every zoom
   * maps to the same page pixels, stop the layer's centre at the page's edge,
   * and end in **one** undoable write on release. `Escape`, a cancelled
   * pointer or a lost capture puts the preview back and writes nothing.
   * From the keyboard, `Alt` with an arrow nudges the focused layer and
   * `Alt` with `[` or `]` turns it; the handle is a slider and answers the
   * slider keys. A burst of presses is one write and one undo entry.
   *
   * A redraw is fixed where it was made, so it has none of this: no frame,
   * no handle, no drag. A locked layer keeps its frame, without the handle.
   */

  /**
   * `tabbable` is false for every page of a longstrip column except the
   * current position: the pointer reaches all of them, but only one page at a
   * time may put a stop in the tab order.
   *
   * `interactive` is false while a drag tool has its drawing surface over the
   * sheet (`DrawLayer`). The layer yields the **pointer** only: the buttons
   * keep their tab stops, their names and their roving index, so every
   * keyboard route into a region survives, and the surface routes a tap back
   * to the region under it. Without this the buttons would swallow the
   * pointerdown that starts a stroke - and starting a stroke over an existing
   * region is exactly what erasing means.
   *
   * `stripMinY` / `stripMaxY` are how far a longstrip page lets a layer's
   * centre travel past its own top and bottom, in page percent - into the
   * neighbouring page and no further, the reach a stroke has too.
   *
   * @type {{
   *   page: import('../api/backend.js').ApiPage,
   *   tabbable?: boolean,
   *   interactive?: boolean,
   *   stripMinY?: number,
   *   stripMaxY?: number,
   * }}
   */
  let { page, tabbable = true, interactive = true, stripMinY = 0, stripMaxY = 100 } = $props()

  /** @type {HTMLElement|undefined} */
  let layerEl = $state()
  let focusIndex = $state(0)
  /**
   * The open context menu: where it is, and which region it is about.
   *
   * @type {{x: number, y: number, region: import('../api/backend.js').ApiRegion} | null}
   */
  let menu = $state(null)
  let suppressClick = false

  /**
   * The move or turn in progress, and its preview: the offset and rotation
   * the frame and the outline are drawn at until the one write lands.
   *
   * `kind` is `move` (a drag on the layer), `turn` (a drag on the handle) or
   * `keys` (a burst of arrow presses, written when the presses stop).
   *
   * A pointer gesture lives in **page pixels**: where it started, the pivot
   * and the point it turned from are held in the page's own coordinates, and
   * every pointer position is read through the sheet as it is drawn at that
   * moment. A scroll or a zoom mid-drag therefore moves nothing under the
   * pointer. `client` is the last pointer position, replayed when the sheet
   * scrolls under a pointer that has not moved.
   *
   * @typedef {{x: number, y: number}} Point
   * @type {null | {
   *   id: string,
   *   kind: 'move'|'turn'|'keys',
   *   base: import('../model/layers.js').LayerStyle,
   *   offsetX: number,
   *   offsetY: number,
   *   rotation: number,
   *   active: boolean,
   *   committing: boolean,
   *   pointerId?: number,
   *   captured?: Element|null,
   *   start?: Point,
   *   pressed?: Point,
   *   pivot?: Point,
   *   from?: Point,
   *   client?: Point & {shift: boolean},
   * }}
   */
  let transform = $state(null)
  /** @type {ReturnType<typeof setTimeout>|null} */
  let keyTimer = null
  /** How long a burst of arrow presses waits for the next one before it is written. */
  const KEY_SETTLE_MS = 500

  /** @param {string|null|undefined} id */
  function regionById(id) {
    return id ? (page.regions ?? []).find((region) => region.id === id) ?? null : null
  }

  const selectedRegion = $derived(regionById(editor.selectionId))
  const frameRegion = $derived(
    selectedRegion && layerCapabilities(selectedRegion).transform === 'movable' ? selectedRegion : null,
  )
  const frameLocked = $derived(!!frameRegion && layerOf(frameRegion).locked)

  /**
   * The saved style of a region, or the gesture's preview of it.
   *
   * @param {import('../api/backend.js').ApiRegion} region
   */
  function liveLayer(region) {
    const layer = layerOf(region)
    if (transform?.id !== region.id) return layer
    return { ...layer, offsetX: transform.offsetX, offsetY: transform.offsetY, rotation: transform.rotation }
  }

  const frame = $derived(frameRegion ? layerFrame(sourceBboxOf(frameRegion), liveLayer(frameRegion), page) : null)

  /**
   * How far the outline of a region under a move preview is shifted, in page
   * percent; the frame carries the turn, so a turning outline is hidden.
   *
   * Measured from the **saved** style, which is what the region's box is
   * drawn from - not from where the gesture began, which may be a burst
   * that has not been written yet.
   *
   * @param {string} id
   */
  function outlineShift(id) {
    const region = transform?.id === id ? regionById(id) : null
    if (!transform || !region) return null
    const saved = layerOf(region)
    const width = Number(page.width) || 100
    const height = Number(page.height) || 100
    return {
      x: ((transform.offsetX - saved.offsetX) / width) * 100,
      y: ((transform.offsetY - saved.offsetY) / height) * 100,
      turning: transform.rotation !== saved.rotation,
    }
  }

  /** @param {KeyboardEvent} event */
  function onEscape(event) {
    if (event.key !== 'Escape' || !transform || transform.committing) return
    // Capture phase, ahead of the editor's own Escape: this one cancels the
    // gesture and must not also clear the selection it was acting on.
    event.preventDefault()
    event.stopPropagation()
    cancelTransform()
  }

  /** The sheet scrolled under a pointer gesture: read the pointer again. */
  function onScroll() {
    const current = transform
    if (!current?.client || current.committing) return
    if (current.kind === 'move') moveTo(current, current.client)
    else if (current.kind === 'turn') turnTo(current, current.client)
  }

  let listening = false
  function listen(on) {
    if (on === listening) return
    listening = on
    if (on) {
      globalThis.addEventListener?.('keydown', onEscape, true)
      globalThis.addEventListener?.('scroll', onScroll, { capture: true, passive: true })
    } else {
      globalThis.removeEventListener?.('keydown', onEscape, true)
      globalThis.removeEventListener?.('scroll', onScroll, { capture: true })
    }
  }

  // Leaving the page - a page turn, a chapter switch, a longstrip page
  // scrolled out of the column - writes a keyboard burst still waiting for
  // its last press rather than dropping it. A pointer gesture has no release
  // to wait for once its layer is gone, so it is put back.
  $effect(() => () => {
    listen(false)
    if (transform?.kind === 'keys' && !transform.committing) void commitTransform()
    else if (transform && !transform.committing) cancelTransform()
    if (keyTimer) clearTimeout(keyTimer)
    keyTimer = null
  })

  /**
   * The page point under a client point, through the sheet as drawn now.
   *
   * @param {Point} point
   */
  function pagePoint(point) {
    const box = layerEl?.getBoundingClientRect()
    return box ? clientPointToPage(point, box, page) : null
  }

  /**
   * Start a gesture from where the layer is **drawn**: a keyboard burst still
   * waiting to be written is where the layer is, and the new gesture goes on
   * from it rather than from the saved style under it. The burst's own write
   * is already queued ahead of this gesture's, so the two land in order.
   *
   * @param {import('../api/backend.js').ApiRegion} region
   * @param {'move'|'turn'|'keys'} kind
   * @param {object} [extra]
   */
  function begin(region, kind, extra = {}) {
    const layer = liveLayer(region)
    transform = {
      id: region.id,
      kind,
      base: layer,
      offsetX: layer.offsetX,
      offsetY: layer.offsetY,
      rotation: layer.rotation,
      active: false,
      committing: false,
      ...extra,
    }
    listen(true)
  }

  /** Put the preview back and write nothing. */
  function cancelTransform() {
    const current = transform
    if (keyTimer) clearTimeout(keyTimer)
    keyTimer = null
    transform = null
    listen(false)
    if (current?.captured && current.pointerId !== undefined && current.captured.hasPointerCapture?.(current.pointerId)) {
      current.captured.releasePointerCapture(current.pointerId)
    }
  }

  /**
   * The gesture's one write, and its one undo entry. The preview stays drawn
   * until the answer lands, so the layer does not flick back to where it was
   * for the length of a round trip.
   */
  async function commitTransform() {
    const current = transform
    if (!current || current.committing) return
    const chapterId = page.chapterId ?? null
    if (keyTimer) clearTimeout(keyTimer)
    keyTimer = null
    listen(false)
    const changes = { offsetX: current.offsetX, offsetY: current.offsetY, rotation: current.rotation }
    const unchanged = changes.offsetX === current.base.offsetX
      && changes.offsetY === current.base.offsetY
      && changes.rotation === current.base.rotation
    const region = regionById(current.id)
    if (unchanged || !region) {
      transform = null
      return
    }
    transform = { ...current, committing: true }
    const label = changes.rotation !== current.base.rotation ? 'masks.command.layerRotate' : 'masks.command.layerMove'
    try {
      await updateLayer(region, changes, label, chapterId)
    } finally {
      if (transform?.id === current.id && transform.committing) transform = null
    }
  }

  /**
   * @param {PointerEvent} event
   * @param {string} id
   */
  function onRegionPointerDown(event, id) {
    suppressClick = false
    const region = regionById(id)
    if (event.button !== 0 || !interactive || !region || !canTransform(region)) return
    if (transform?.committing) return
    const client = { x: event.clientX, y: event.clientY, shift: event.shiftKey }
    const start = pagePoint(client)
    if (!start) return
    // A burst still waiting for its last press is written first; the drag
    // goes on from where the burst left the layer.
    if (transform) void commitTransform()
    select(id)
    const target = /** @type {Element} */ (event.currentTarget)
    begin(region, 'move', { pointerId: event.pointerId, captured: target, start, pressed: client, client })
    target.setPointerCapture?.(event.pointerId)
  }

  /**
   * Move the preview to where a client point now falls on the page.
   *
   * @param {NonNullable<typeof transform>} current
   * @param {Point & {shift: boolean}} client
   */
  function moveTo(current, client) {
    const region = regionById(current.id)
    const now = pagePoint(client)
    if (!region || !now || !current.start) return
    const offset = clampOffset(
      sourceBboxOf(region),
      { x: current.base.offsetX + now.x - current.start.x, y: current.base.offsetY + now.y - current.start.y },
      page,
      { minY: stripMinY, maxY: stripMaxY },
    )
    transform = { ...current, client, active: true, offsetX: offset.x, offsetY: offset.y }
  }

  /** @param {PointerEvent} event */
  function onRegionPointerMove(event) {
    const current = transform
    if (current?.kind !== 'move' || current.pointerId !== event.pointerId || current.committing) return
    const client = { x: event.clientX, y: event.clientY, shift: event.shiftKey }
    // The slop is the pointer's own travel, in client pixels: whether a press
    // is a click is a question about the hand, not about the page.
    const pressed = current.pressed ?? client
    if (!current.active && Math.hypot(client.x - pressed.x, client.y - pressed.y) <= DRAG_SLOP) return
    moveTo(current, client)
  }

  /** @param {PointerEvent} event */
  function onRegionPointerUp(event) {
    const current = transform
    if (current?.kind !== 'move' || current.pointerId !== event.pointerId || current.committing) return
    if (!current.active) {
      // A press that never became a drag is a click, and the click selects.
      cancelTransform()
      return
    }
    suppressClick = true
    void commitTransform()
  }

  /**
   * A cancelled pointer, or a capture the platform took away (WebKit does,
   * for a system gesture or a window switch mid-drag): the gesture is
   * abandoned, not committed. After a normal release the capture is lost too,
   * and by then the gesture is already committing or gone.
   *
   * @param {PointerEvent} event
   */
  function onLostPointer(event) {
    const current = transform
    if (!current || current.committing || current.kind === 'keys' || current.pointerId !== event.pointerId) return
    cancelTransform()
  }

  /** @param {PointerEvent} event */
  function onTurnPointerDown(event) {
    if (event.button !== 0 || !interactive || !frameRegion || !canTransform(frameRegion) || !frame) return
    if (transform?.committing) return
    event.preventDefault()
    event.stopPropagation()
    const client = { x: event.clientX, y: event.clientY, shift: event.shiftKey }
    const from = pagePoint(client)
    if (!from) return
    const width = Number(page.width) || 0
    const height = Number(page.height) || 0
    if (transform) void commitTransform()
    const target = /** @type {Element} */ (event.currentTarget)
    begin(frameRegion, 'turn', {
      active: true,
      pointerId: event.pointerId,
      captured: target,
      // The frame's centre, in page pixels: the pivot the native side turns
      // the pixels about. Read off the frame as drawn, which is where a
      // pending burst left it.
      pivot: { x: ((frame.x + frame.w / 2) / 100) * width, y: ((frame.y + frame.h / 2) / 100) * height },
      from,
      client,
    })
    target.setPointerCapture?.(event.pointerId)
  }

  /**
   * Turn the preview to where a client point now falls about the pivot.
   *
   * @param {NonNullable<typeof transform>} current
   * @param {Point & {shift: boolean}} client
   */
  function turnTo(current, client) {
    const to = pagePoint(client)
    if (!current.pivot || !current.from || !to) return
    const rotation = turnedRotation({
      start: current.base.rotation,
      pivot: current.pivot,
      from: current.from,
      to,
      snap: client.shift,
    })
    transform = { ...current, client, rotation }
  }

  /** @param {PointerEvent} event */
  function onTurnPointerMove(event) {
    const current = transform
    if (current?.kind !== 'turn' || current.pointerId !== event.pointerId || current.committing) return
    turnTo(current, { x: event.clientX, y: event.clientY, shift: event.shiftKey })
  }

  /** @param {PointerEvent} event */
  function onTurnPointerUp(event) {
    const current = transform
    if (current?.kind !== 'turn' || current.pointerId !== event.pointerId || current.committing) return
    void commitTransform()
  }

  /**
   * One arrow press of a keyboard burst: the preview moves now, the write
   * waits until the presses stop, and the whole burst is one undo entry.
   *
   * @param {import('../api/backend.js').ApiRegion} region
   * @param {{offsetX?: number, offsetY?: number, rotation?: number}} step - deltas
   */
  function keyStep(region, step) {
    if (transform?.committing) return
    if (transform && (transform.id !== region.id || transform.kind !== 'keys')) {
      // Another gesture is written first, and the burst starts from where it
      // left the layer.
      void commitTransform()
      begin(region, 'keys', { active: true })
    }
    if (!transform) begin(region, 'keys', { active: true })
    const current = /** @type {NonNullable<typeof transform>} */ (transform)
    const offset = clampOffset(
      sourceBboxOf(region),
      { x: current.offsetX + (step.offsetX ?? 0), y: current.offsetY + (step.offsetY ?? 0) },
      page,
      { minY: stripMinY, maxY: stripMaxY },
    )
    transform = {
      ...current,
      offsetX: offset.x,
      offsetY: offset.y,
      rotation: normalizeAngle(current.rotation + (step.rotation ?? 0)),
    }
    if (keyTimer) clearTimeout(keyTimer)
    keyTimer = setTimeout(() => void commitTransform(), KEY_SETTLE_MS)
  }

  /** @param {KeyboardEvent} event */
  function onTurnKey(event) {
    if (!frameRegion || !canTransform(frameRegion)) return
    const current = transform?.id === frameRegion.id ? transform.rotation : layerOf(frameRegion).rotation
    const next = turnFor(event.key, event.shiftKey, current)
    if (next === null) return
    event.preventDefault()
    // The editor's global arrows page the chapter; this one turned a layer.
    event.stopPropagation()
    keyStep(frameRegion, { rotation: next - current })
  }

  /** A burst is written when focus leaves the control it was typed into. */
  function settleKeys() {
    if (transform?.kind === 'keys' && !transform.committing) void commitTransform()
  }

  const marksVisible = $derived(editor.maskOverlay || editor.reviewFilter)
  const ordered = $derived(readingOrder(page.regions ?? [], readingDirection()))
  const markers = $derived(ordered.map((region) => ({
    ...regionMarker(region, { marksVisible }),
    movable: canTransform(region),
  })))

  // The tab stop follows the selection while the selection is on this page, so
  // tabbing to the canvas lands on the region being worked on rather than back
  // at the top of the page.
  //
  // `focusIndex` is clamped on the way out, not only on the way in: a region
  // set that shrinks under a stale index (a mask deleted, a re-run, a filter)
  // would otherwise match no marker at all, and a set where no marker carries
  // `tabindex="0"` is a set the Tab key cannot reach.
  const selectedIndex = $derived(markers.findIndex((marker) => marker.id === editor.selectionId))
  const rovingIndex = $derived(
    selectedIndex >= 0 ? selectedIndex : Math.min(focusIndex, markers.length - 1),
  )

  /**
   * Arrowing selects as it goes. Unlike the Pages list - where selection means
   * loading a page - selecting a region only highlights it, and the whole
   * point of moving between regions by keyboard is to inspect them.
   *
   * @param {number} index
   */
  async function focusRegion(index) {
    const count = markers.length
    if (count === 0) return
    const next = Math.min(count - 1, Math.max(0, index))
    focusIndex = next
    select(markers[next].id)
    await tick()
    const element = layerEl?.querySelector(`[data-region="${markers[next].id}"]`)
    if (element instanceof HTMLElement) {
      element.focus({ preventScroll: true })
      element.scrollIntoView({ block: 'nearest', inline: 'nearest' })
    }
  }

  /**
   * @param {KeyboardEvent} event
   * @param {number} index
   */
  function onkeydown(event, index) {
    if (event.altKey && !event.metaKey && !event.ctrlKey) {
      onTransformKey(event, markers[index]?.id)
      return
    }
    if (event.metaKey || event.ctrlKey || event.altKey) return
    switch (event.key) {
      case 'ArrowDown':
      case 'ArrowRight':
        focusRegion(index + 1)
        break
      case 'ArrowUp':
      case 'ArrowLeft':
        focusRegion(index - 1)
        break
      case 'Home':
        focusRegion(0)
        break
      case 'End':
        focusRegion(markers.length - 1)
        break
      default:
        return
    }
    event.preventDefault()
    // The editor's global ← / → page the chapter. An arrow this layer has
    // already used must not also turn the page.
    event.stopPropagation()
  }

  /**
   * `Alt` with an arrow nudges the focused layer a page pixel (ten with
   * `Shift`); `Alt` with `[` or `]` turns it a degree (fifteen with
   * `Shift`). Read by `code` for the brackets: on a Mac, `Alt` turns the
   * bracket keys into typographic quotes. Only for a layer that may move -
   * a redraw, a detection or a locked layer ignores them.
   *
   * @param {KeyboardEvent} event
   * @param {string|undefined} id
   */
  function onTransformKey(event, id) {
    const region = regionById(id)
    if (!region || !canTransform(region)) return
    const nudge = nudgeFor(event.key, event.shiftKey)
    const turn = event.code === 'BracketRight' ? 1 : event.code === 'BracketLeft' ? -1 : 0
    if (!nudge && !turn) return
    event.preventDefault()
    event.stopPropagation()
    if (editor.selectionId !== region.id) select(region.id)
    keyStep(region, nudge
      ? { offsetX: nudge.x, offsetY: nudge.y }
      : { rotation: turn * (event.shiftKey ? TURN_LARGE : TURN) })
  }

  /** Select a region without applying a cleaning tool. */
  function onRegionClick(regionId) {
    if (suppressClick) { suppressClick = false; return }
    select(regionId)
  }

  /**
   * The secondary press: **selects, and then offers what to do about it**.
   *
   * Selecting first is not decoration. The menu's entries are the Layers row's
   * entries, and a right-click that opened a menu for one region while the
   * canvas and the panel still lit another would be two answers to "which
   * region is this about". It never applies the armed tool - that is what the
   * left button is for, and a menu is a question, not a command.
   *
   * The same handler serves `Shift`+`F10` and the context-menu key, which the
   * browser reports as this event with no coordinates; `menuPoint` anchors
   * those to the region's own box.
   *
   * @param {MouseEvent} event
   * @param {string} regionId
   */
  function onRegionMenu(event, regionId) {
    const region = (page.regions ?? []).find((candidate) => candidate.id === regionId)
    if (!region) return
    event.preventDefault()
    select(regionId)
    const target = /** @type {HTMLElement} */ (event.currentTarget)
    menu = { ...menuPoint(event, target.getBoundingClientRect()), region }
  }
</script>

<div class="regions" class:inert={!interactive} bind:this={layerEl}>
  <!-- The page is not a click target; only its regions are. This is the empty
       space, and clearing the selection is all it does. It is out of the tab
       order on purpose: Escape already clears the selection from the keyboard
       (`cancelInteraction`), and a tab stop that means "nothing here" is
       noise.

       `aria-hidden` for the same reason, and it is safe *because* of the
       `tabindex="-1"`: nothing focusable is being hidden. Without it a screen
       reader meets a full-sheet button ahead of every region on the page - on
       every page of a longstrip - announcing an action the Escape key already
       performs. It keeps its name for the pointer's tooltip-free hit target
       and for anything reading the DOM. -->
  <button
    type="button"
    class="clear"
    tabindex="-1"
    aria-hidden="true"
    aria-label={t('canvas.action.clearSelection')}
    onclick={() => select(null)}
  ></button>

  {#each markers as marker, index (marker.id)}
    {@const shift = outlineShift(marker.id)}
    <button
      type="button"
      class="region {marker.status}"
      class:marked={!!marker.badge}
      class:lit={isHighlighted(marker.id)}
      class:outlined={editor.maskOverlay}
      data-region={marker.id}
      tabindex={tabbable && index === rovingIndex ? 0 : -1}
      aria-current={editor.selectionId === marker.id ? 'true' : undefined}
      aria-label={t(marker.nameKey, marker.nameParams)}
      title={t(marker.nameKey, marker.nameParams)}
      class:movable={marker.movable}
      class:turning={shift?.turning}
      aria-keyshortcuts={marker.movable ? 'Alt+ArrowLeft Alt+ArrowRight Alt+ArrowUp Alt+ArrowDown' : undefined}
      style:left="{marker.bbox.x + (shift?.x ?? 0)}%"
      style:top="{marker.bbox.y + (shift?.y ?? 0)}%"
      style:width="{marker.bbox.w}%"
      style:height="{marker.bbox.h}%"
      onkeydown={(event) => onkeydown(event, index)}
      onpointerdown={(event) => onRegionPointerDown(event, marker.id)}
      onpointermove={onRegionPointerMove}
      onpointerup={onRegionPointerUp}
      onpointercancel={onLostPointer}
      onlostpointercapture={onLostPointer}
      onpointerenter={() => hover(marker.id)}
      onpointerleave={() => hover(null)}
      onfocus={() => hover(marker.id)}
      onblur={() => { hover(null); settleKeys() }}
      onclick={() => onRegionClick(marker.id)}
      oncontextmenu={(event) => onRegionMenu(event, marker.id)}
    >
      {#if marker.badge}
        <span class="badge" aria-hidden="true">{marker.badge}</span>
      {/if}
    </button>
  {/each}

  {#if frame}
    <!-- The selected layer's own box, moved and turned the way the compositor
         moves and turns its pixels. Drawn over the outline and transparent
         to the pointer, so a drag on the layer still reaches its button; only
         the handle takes the pointer. -->
    <div
      class="frame"
      class:locked={frameLocked}
      class:live={transform?.id === frameRegion?.id}
      data-layer-frame={frameRegion?.id}
      style:left="{frame.x}%"
      style:top="{frame.y}%"
      style:width="{frame.w}%"
      style:height="{frame.h}%"
      style:transform="rotate({frame.rotation}deg)"
    >
      {#if !frameLocked}
        <span class="stem" aria-hidden="true"></span>
        <div
          class="turn"
          role="slider"
          tabindex={tabbable ? 0 : -1}
          aria-label={t('masks.transform.rotate')}
          aria-valuemin="-180"
          aria-valuemax="180"
          aria-valuenow={Math.round(frame.rotation)}
          aria-valuetext={t('masks.transform.degrees', { degrees: Math.round(frame.rotation * 10) / 10 })}
          aria-keyshortcuts="0"
          title={t('masks.transform.rotateHint')}
          data-turn-handle
          onpointerdown={onTurnPointerDown}
          onpointermove={onTurnPointerMove}
          onpointerup={onTurnPointerUp}
          onpointercancel={onLostPointer}
          onlostpointercapture={onLostPointer}
          onkeydown={onTurnKey}
          onblur={settleKeys}
        ></div>
      {/if}
    </div>
  {/if}

  <RegionMenu at={menu} onclose={() => (menu = null)} />
</div>

<style>
  .regions {
    position: absolute;
    inset: 0;
  }

  /* The pointer only. `pointer-events` does not affect focus, so every region
     keeps its tab stop, its name and its activation. */
  .regions.inert { pointer-events: none }

  .clear {
    position: absolute;
    inset: 0;
    padding: 0;
    border: 0;
    background: transparent;
    cursor: default;
  }

  .region {
    position: absolute;
    padding: 0;
    border: 0;
    background: transparent;
    border-radius: 2px;
    /* The design file's armed-tool cursor. A click here applies the active
       tool to the region - the pointer should say that before the click, not
       after. A tool is always armed in this app (`editor.tool` is never null,
       unlike the prototype's rail), so the crosshair is unconditional. */
    cursor: crosshair;
    transition:
      outline-color var(--dur) var(--ease),
      box-shadow var(--dur) var(--ease);
    /* Declared unconditionally and transparent, so the rules below change only
       the colour: an outline that appears from nothing jumps the layout of the
       artwork's perceived edge, and one that fades in does not. */
    outline: 1.5px dashed transparent;
    outline-offset: 2px;
  }

  /* The layer indicator: a dashed mark, no fill, ever. On when the pointer or
     the keyboard is on the region, when it is selected, or when the mask
     overlay asks for all of them at once. In --page-mark rather than the
     page's ink: most manga is black, and a near-black dash over it is the one
     place the outline is needed and cannot be seen. */
  .region.outlined,
  .region.lit {
    outline-color: var(--page-mark);
  }

  /* Selected or hovered, among a page full of outlines: heavier, and haloed so
     the dashes survive the artwork underneath. The halo is dark now, not the
     white --page-halo: the blue carries itself over black ink, and it is pale
     panels and white gutters that need the rim. */
  .region.lit {
    outline-width: 2px;
    box-shadow: 0 0 0 1px var(--page-mark-halo);
  }

  /* Declined: always, whatever the overlay and the filter say. Ordered after
     the two rules above, which carry the same specificity, so the warn colour
     wins on a region that is both declined and lit. */
  .region.declined {
    outline-color: var(--page-warn);
  }

  /* Detected, waiting to be cleaned: no box at all. After a Detect the
     result is the mask Clean will erase, and `DetectionMasks` draws that
     under this layer, lit and all, in the selection colour. A box over it
     would say "here is where it looked" on top of "here is what goes", so
     the button stays - hover, selection, the menu and the keyboard all keep
     working - and draws nothing, whatever the overlay says. */
  .region.detected,
  .region.detected.outlined,
  .region.detected.lit {
    outline-color: transparent;
    box-shadow: none;
    background: transparent;
  }

  /* A candidate, held for the user's choice: always drawn too, or it could
     only be found from the Layers list. Dotted, as its ◌ row mark is,
     because nothing has been decided about it yet - where a detection's
     mask is a plan and a layer's dash is a result. Thin and in
     --page-mark, which does not change with the theme (the sheet is paper in
     both), and no badge: a held area is not a problem to count. */
  .region.candidate {
    outline-style: dotted;
    outline-width: 1.5px;
    outline-color: var(--page-mark);
  }
  .region.candidate.lit {
    outline-width: 2px;
    background: var(--page-mark-tint);
  }

  /* Needs review: only when the overlay or the review filter put its badge on. */
  .region.review.marked {
    outline-style: dotted;
    outline-color: var(--page-line);
  }

  /* The sheet is --paper in both themes, so the global --accent focus ring
     would disappear on the page in dark. A detection keeps it too: it draws
     no box, and the ring is how a keyboard user sees where focus is. */
  .region:focus-visible,
  .region.detected:focus-visible {
    outline: 2px solid var(--page-mark);
    outline-offset: 2px;
  }

  /* A layer that moves says so before the press: the armed tool's crosshair
     would promise a stroke, and a press here starts a drag. */
  .region.movable { cursor: move; touch-action: none }

  /* Mid-turn the axis-aligned outline no longer describes the layer; the
     frame does, so the outline steps aside until the answer lands. */
  .region.turning { outline-color: transparent }

  /* The selected movable layer's own box. Dashed like every layer mark, in
     --page-mark over a dark halo so it reads over ink and over paper, and
     transparent to the pointer: the layer's button under it is what a drag
     grabs. */
  .frame {
    position: absolute;
    box-sizing: border-box;
    border: 1.5px dashed var(--page-mark);
    box-shadow: 0 0 0 1px var(--page-mark-halo);
    border-radius: 1px;
    transform-origin: 50% 50%;
    pointer-events: none;
  }
  /* A locked layer keeps its box and loses its handle: it is still the
     selection, it just does not move. */
  .frame.locked {
    border-style: dotted;
    border-color: var(--page-line);
  }

  /* The stem joins the handle to the frame's top edge, so the handle reads
     as part of the layer and turns with it. */
  .stem {
    position: absolute;
    left: 50%;
    bottom: 100%;
    width: 0;
    height: 18px;
    border-left: 1.5px solid var(--page-mark);
    transform: translateX(-50%);
  }

  /* The turn handle. 16px drawn inside a 28px hit area: a knob the pointer
     can find at any zoom without covering the artwork it is turning. */
  .turn {
    position: absolute;
    left: 50%;
    bottom: calc(100% + 18px);
    width: 28px;
    height: 28px;
    margin-left: -14px;
    margin-bottom: -14px;
    border-radius: 50%;
    cursor: grab;
    touch-action: none;
  }
  .regions:not(.inert) .turn { pointer-events: auto }
  .turn::after {
    content: '';
    position: absolute;
    inset: 6px;
    border-radius: 50%;
    background: var(--paper);
    border: 2px solid var(--page-mark);
    box-shadow: 0 0 0 1px var(--page-mark-halo);
  }
  .frame.live .turn { cursor: grabbing }
  .turn:focus-visible {
    outline: 2px solid var(--page-mark);
    outline-offset: 1px;
  }

  @media (prefers-reduced-motion: no-preference) {
    .turn::after { transition: transform var(--dur-fast) var(--ease) }
    .turn:hover::after { transform: scale(1.12) }
  }

  .badge {
    position: absolute;
    top: -9px;
    right: -9px;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 15px;
    height: 15px;
    border-radius: var(--r-xs);
    background: var(--paper);
    border: 1px solid var(--page-line);
    color: var(--page-ink);
    font-size: 10px;
    font-weight: 700;
    line-height: 1;
  }
  .region.declined .badge {
    border-color: var(--page-warn);
    color: var(--page-warn);
  }
</style>
