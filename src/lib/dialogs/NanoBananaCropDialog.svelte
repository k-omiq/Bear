<script>
  /**
   * One crop, right before it goes to Google (`editor/nanobananacrop.svelte.js`):
   * the picture with the region marked blue, the model, size and ratio, and a
   * note when Google refused an earlier crop of the same region. The person
   * picks Send, Skip (this region is not cleaned, a batch goes on) or Stop
   * (the request and the rest of its window end).
   *
   * With Restrictive crop on, only the kept areas are sent and the rest of
   * the crop goes as plain white. The window paints that white over the
   * picture so the person sees what Google will see. The kept areas are the
   * app's suggestions (each region's bounds plus some page around it), or
   * the ones the person draws by dragging on the picture. The marked regions
   * (`holes`) are always kept: native adds them to whatever is sent.
   *
   * Drawing is SVG over an <img>, with pointer events and pointer capture,
   * since the app ships on WebKit and reads no canvas pixels. Coordinates go
   * from the screen to crop pixels through the picture's own box.
   *
   * Pushed with `blocking: true`: Escape is Skip, the backdrop is inert.
   * Answers `{ answer, keep }`, `keep` the drawn areas or null for the
   * suggested ones.
   */
  import { untrack } from 'svelte'
  import { Button, Modal } from '../ui/index.js'
  import { closeModal } from '../state/app.svelte.js'
  import { nanoBananaModel } from '../model/nanobanana.js'
  import { t } from '../i18n/index.js'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()

  const uid = $props.id()
  const crop = untrack(() => /** @type {any} */ (spec?.props?.crop ?? {}))
  const [, , cropW = 1, cropH = 1] = Array.isArray(crop.crop) ? crop.crop.map(Number) : []
  const width = Math.max(1, cropW)
  const height = Math.max(1, cropH)
  const model = nanoBananaModel(crop.model)
  const restrictive = crop.restrictive === true
  const attempt = Number(crop.attempt) || 0

  /** The widest the picture is drawn, in screen pixels. */
  const MAX_SHOWN = 520
  /** A drag shorter than this on either side, in screen pixels, draws nothing. */
  const MIN_DRAG = 4
  const scale = Math.min(1, MAX_SHOWN / width)
  const shownW = Math.round(width * scale)
  const shownH = Math.round(height * scale)

  /**
   * Rects as `[x, y, w, h]`, clipped to the crop; anything empty is dropped.
   *
   * @param {unknown} list
   * @returns {number[][]}
   */
  function clipped(list) {
    if (!Array.isArray(list)) return []
    return list.flatMap((rect) => {
      if (!Array.isArray(rect) || rect.length !== 4) return []
      const [x, y, w, h] = rect.map(Number)
      if (![x, y, w, h].every(Number.isFinite)) return []
      const left = Math.max(0, x)
      const top = Math.max(0, y)
      const right = Math.min(width, x + w)
      const bottom = Math.min(height, y + h)
      return right > left && bottom > top ? [[left, top, right - left, bottom - top]] : []
    })
  }

  const holes = clipped(crop.holes)
  const suggested = clipped(crop.suggested)

  /** The areas the person drew, in crop pixels. @type {number[][]} */
  let drawn = $state([])
  /** The drag under way, in crop pixels. @type {{ x: number, y: number, x2: number, y2: number, pointer: number }|null} */
  let drag = $state(null)
  let showHidden = $state(false)
  /** @type {HTMLElement|undefined} */
  let stage = $state()

  const kept = $derived(drawn.length ? drawn : suggested)
  const draft = $derived(drag ? rectOf(drag) : null)

  /** @param {{ x: number, y: number, x2: number, y2: number }} span */
  function rectOf(span) {
    const left = Math.floor(Math.min(span.x, span.x2))
    const top = Math.floor(Math.min(span.y, span.y2))
    const right = Math.ceil(Math.max(span.x, span.x2))
    const bottom = Math.ceil(Math.max(span.y, span.y2))
    return [left, top, right - left, bottom - top]
  }

  /**
   * A pointer's place on the picture, in crop pixels, held inside the crop.
   *
   * @param {PointerEvent} event
   */
  function cropPoint(event) {
    const box = stage?.getBoundingClientRect()
    const boxW = box?.width || shownW
    const boxH = box?.height || shownH
    const x = ((event.clientX - (box?.left ?? 0)) * width) / boxW
    const y = ((event.clientY - (box?.top ?? 0)) * height) / boxH
    return { x: Math.min(width, Math.max(0, x)), y: Math.min(height, Math.max(0, y)) }
  }

  /** @param {PointerEvent} event */
  function down(event) {
    if (!restrictive || event.button !== 0 || drag) return
    event.preventDefault()
    const at = cropPoint(event)
    drag = { x: at.x, y: at.y, x2: at.x, y2: at.y, pointer: event.pointerId }
    try { stage?.setPointerCapture?.(event.pointerId) } catch { /* the drag still follows the stage */ }
  }

  /** @param {PointerEvent} event */
  function move(event) {
    if (!drag || event.pointerId !== drag.pointer) return
    const at = cropPoint(event)
    drag = { ...drag, x2: at.x, y2: at.y }
  }

  /** @param {PointerEvent} event */
  function up(event) {
    if (!drag || event.pointerId !== drag.pointer) return
    const at = cropPoint(event)
    const span = { ...drag, x2: at.x, y2: at.y }
    drag = null
    try { stage?.releasePointerCapture?.(event.pointerId) } catch { /* already released */ }
    // Measured on the screen, so a small slip of the hand draws nothing at
    // any zoom.
    const screenW = Math.abs(span.x2 - span.x) * scale
    const screenH = Math.abs(span.y2 - span.y) * scale
    if (screenW < MIN_DRAG || screenH < MIN_DRAG) return
    const rect = clipped([rectOf(span)])
    if (rect.length) drawn = [...drawn, rect[0]]
  }

  /** @param {PointerEvent} event */
  function cancel(event) {
    if (drag && event.pointerId === drag.pointer) drag = null
  }

  /** @param {'send'|'skip'|'stop'} pick */
  function answer(pick) {
    const keep = pick === 'send' && restrictive && drawn.length ? drawn.map((rect) => [...rect]) : null
    closeModal(/** @type {any} */ ({ answer: pick, keep }))
  }

  const areaText = $derived(drawn.length
    ? t('nanoBanana.crop.drawnCount', { count: drawn.length })
    : t('nanoBanana.crop.suggestedCount', { count: suggested.length }))
</script>

<Modal title={t('nanoBanana.crop.title')} width={Math.max(400, shownW + 48)} blocking onclose={() => answer('skip')}
  describedby="{uid}-what">
  <p id="{uid}-what">{t('nanoBanana.crop.model', { model: t(model.labelKey), size: String(crop.size ?? ''), ratio: String(crop.ratio ?? '') })}</p>
  {#if attempt > 0}
    <p class="note" data-attempt={attempt}>{t('nanoBanana.crop.attempt', { attempt: attempt + 1 })}</p>
  {/if}

  <!-- The picture is a drawing surface only with Restrictive crop on; the
       suggested areas are the keyboard's way to send it. -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="stage"
    class:drawing={restrictive}
    bind:this={stage}
    style:width="{shownW}px"
    style:height="{shownH}px"
    data-stage
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={cancel}
  >
    <img
      src="data:image/png;base64,{crop.png ?? ''}"
      alt={t(restrictive ? 'nanoBanana.crop.altRestrictive' : 'nanoBanana.crop.alt')}
      width={shownW}
      height={shownH}
      draggable="false"
    />
    <svg viewBox="0 0 {width} {height}" preserveAspectRatio="none" aria-hidden="true">
      {#if restrictive}
        <defs>
          <mask id="{uid}-hidden" maskUnits="userSpaceOnUse" x="0" y="0" {width} {height}>
            <rect x="0" y="0" {width} {height} fill="white" />
            {#each [...kept, ...holes] as [x, y, w, h], at (at)}
              <rect {x} {y} width={w} height={h} fill="black" />
            {/each}
          </mask>
        </defs>
        <rect class="white" data-hidden x="0" y="0" {width} {height} fill="white"
          fill-opacity={showHidden ? 0.8 : 1} mask="url(#{uid}-hidden)" />
        {#each kept as [x, y, w, h], at (at)}
          <rect class="kept" data-kept x={x} y={y} width={w} height={h} vector-effect="non-scaling-stroke" />
        {/each}
        {#if draft}
          <rect class="draft" x={draft[0]} y={draft[1]} width={draft[2]} height={draft[3]} vector-effect="non-scaling-stroke" />
        {/if}
      {/if}
      {#each holes as [x, y, w, h], at (at)}
        <rect class="hole" x={x} y={y} width={w} height={h} vector-effect="non-scaling-stroke" />
      {/each}
    </svg>
  </div>

  {#if restrictive}
    <p class="hint">{t('nanoBanana.crop.restrictiveHint')}</p>
    <div class="tools">
      <label class="show">
        <input type="checkbox" class="check" bind:checked={showHidden} />
        {t('nanoBanana.crop.showHidden')}
      </label>
      <Button size="sm" variant="soft" disabled={drawn.length === 0} onclick={() => { drawn = [] }}>
        {t('nanoBanana.crop.useSuggested')}
      </Button>
    </div>
    <p class="hint" aria-live="polite" data-areas>{areaText}</p>
  {/if}
  <p class="hint">{t('nanoBanana.crop.sent')}</p>

  {#snippet buttons()}
    <Button variant="ghost" onclick={() => answer('skip')}>{t('nanoBanana.crop.skip')}</Button>
    <Button variant="ghost" onclick={() => answer('stop')}>{t('nanoBanana.crop.stop')}</Button>
    <Button variant="primary" onclick={() => answer('send')}>{t('nanoBanana.crop.send')}</Button>
  {/snippet}
</Modal>

<style>
  .note {
    color: var(--warn);
  }
  .stage {
    position: relative;
    max-width: 100%;
    margin: 8px auto;
    border: 1px solid var(--line, rgba(0, 0, 0, 0.15));
    background: #fff;
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
  }
  .stage.drawing {
    cursor: crosshair;
  }
  .stage img,
  .stage svg {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    display: block;
    pointer-events: none;
  }
  .kept {
    fill: none;
    stroke: var(--accent, #2f6fde);
    stroke-width: 2;
  }
  .draft {
    fill: none;
    stroke: var(--accent, #2f6fde);
    stroke-width: 1.5;
    stroke-dasharray: 4 3;
  }
  .hole {
    fill: none;
    stroke: #1f4fff;
    stroke-width: 1;
    stroke-dasharray: 3 2;
  }
  .tools {
    display: flex;
    gap: 10px;
    align-items: center;
    justify-content: space-between;
    margin-top: 6px;
  }
  .show {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .check {
    width: 15px;
    height: 15px;
    margin: 0;
    accent-color: var(--accent);
  }
  .hint {
    margin: 4px 0;
    opacity: 0.75;
    font-size: 0.9em;
  }
</style>
