<script>
  /**
   * One denoise run of a chapter against the raw pages it was made from, a
   * page at a time. Opened from Denoise history in the chapter menu
   * (`home/actions.js#openDenoiseHistory`) on the newest run; the run picker
   * is the rest of the history.
   *
   * The two pages are stacked in one box, each fitted to it the same way, so
   * a 4x upscale and the scan it came from line up pixel for pixel on screen.
   * The denoised page is on top and clipped at the wipe, the way the editor's
   * sheet clips its cleaned layer over the original (`editor/PageSheet.svelte`):
   * denoised left of the divider, raw right of it, each side labelled. The
   * slider moves the divider from the keyboard; a press or drag on the pages
   * moves it by pointer.
   *
   * Both sides come from `denoiseCompareImage` as bytes, never as a path: the
   * backend serves only files the manifest recorded for that page and run, and
   * after a take the raw side is the scan the file replaced. A page whose file
   * is gone says so and where it was, instead of drawing half a comparison.
   *
   * Full window lays the dialog over the whole window (`Modal`'s `fill`) and
   * gives the box every pixel the controls leave, the pages still fitted to
   * it. Escape leaves full window first and closes on the second press, the
   * one route out of the dialog doing one thing at a time.
   *
   * Zoom enlarges both pages together under the wipe, which stays where it is
   * on screen. A pinch or a modifier-scroll zooms about the pointer and a
   * plain scroll moves the enlarged pages, as on the editor's canvas
   * (`editor/CanvasStage.svelte`); the two buttons zoom about the middle and
   * the readout between them goes back to fit. The zoom and the place are
   * kept from page to page and run to run, so one spot can be followed
   * through a chapter.
   */
  import { onDestroy, untrack } from 'svelte'
  import { Button, IconButton, Modal, Readout, Select, Slider } from '../ui/index.js'
  import { closeModal } from '../state/app.svelte.js'
  import { getBackend } from '../api/backend.js'
  import { t } from '../i18n/index.js'
  import { imageType, runFacts, runLabel } from '../model/denoisehistory.js'
  import { wheelZoom } from '../editor/zoom.js'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()

  const chapter = untrack(() => /** @type {any} */ (spec?.props?.chapter ?? null))
  /** @type {import('../api/backend.js').DenoiseRun[]} */
  const runs = untrack(() => (Array.isArray(spec?.props?.runs) ? spec.props.runs : []))
  const pageCount = Array.isArray(chapter?.pages) ? chapter.pages.length : 0

  let created = $state(untrack(() => /** @type {number} */ (spec?.props?.run ?? runs[0]?.created ?? 0)))
  const run = $derived(runs.find((entry) => entry.created === created) ?? null)
  const pages = $derived(run?.pages ?? [])
  let at = $state(0)
  const page = $derived(pages[at] ?? null)
  /** Share of the width, from the left, that shows the denoised page. */
  let wipe = $state(50)
  let fullscreen = $state(false)

  /** Escape, the backdrop: out of full window first, then out of the dialog. */
  function dismiss() {
    if (fullscreen) fullscreen = false
    else closeModal(null)
  }

  /**
   * What the box shows for the page: its two object URLs once loaded, and
   * the message when a side could not be read.
   *
   * @typedef {{loading: boolean, denoised: string|null, raw: string|null,
   *   message: {key: string, params?: Record<string, unknown>}|null}} View
   * @type {View}
   */
  let view = $state({ loading: true, denoised: null, raw: null, message: null })

  /** @param {View} shown */
  function revoke(shown) {
    for (const url of [shown.denoised, shown.raw]) if (url) URL.revokeObjectURL(url)
  }

  /**
   * One side as an object URL, or the error code it was refused with.
   *
   * @param {number} pageIndex @param {number} which @param {'raw'|'denoised'} side
   * @returns {Promise<{url: string}|{code: string}>}
   */
  async function side(pageIndex, which, side) {
    try {
      const bytes = await getBackend().denoiseCompareImage({ chapterId: chapter.id, pageIndex, run: which, side })
      return { url: URL.createObjectURL(new Blob([bytes], { type: imageType(bytes) })) }
    } catch (error) {
      return { code: String(/** @type {any} */ (error)?.message ?? error) }
    }
  }

  /**
   * @param {{pageIndex: number, exists: boolean}} target @param {number} which
   * @returns {Promise<View>}
   */
  async function load(target, which) {
    const number = target.pageIndex + 1
    const gone = { key: 'denoise.compare.missing', params: { page: number, folder: run?.folder ?? '' } }
    if (!target.exists) return { loading: false, denoised: null, raw: null, message: gone }
    const [denoised, raw] = await Promise.all([side(target.pageIndex, which, 'denoised'), side(target.pageIndex, which, 'raw')])
    const url = (/** @type {{url: string}|{code: string}} */ answer) => ('url' in answer ? answer.url : null)
    const shown = { loading: false, denoised: url(denoised), raw: url(raw), message: null }
    if ('code' in denoised) {
      shown.message = denoised.code.startsWith('denoise_file_missing') ? gone : { key: 'denoise.compare.failed', params: { page: number } }
    } else if ('code' in raw) {
      shown.message = { key: 'denoise.compare.rawMissing' }
    }
    return shown
  }

  $effect(() => {
    const target = page
    const which = created
    let live = true
    untrack(() => {
      revoke(view)
      view = { loading: true, denoised: null, raw: null, message: null }
    })
    if (!target) {
      view = { loading: false, denoised: null, raw: null, message: { key: 'denoise.compare.empty' } }
      return
    }
    load(target, which).then((shown) => {
      if (live) view = shown
      else revoke(shown)
    })
    return () => { live = false }
  })

  onDestroy(() => revoke(view))

  /** @param {string} value */
  function chooseRun(value) {
    const pageIndex = page?.pageIndex
    created = Number(value)
    // Stay on the same page when the other run has it.
    const same = (runs.find((entry) => entry.created === created)?.pages ?? []).findIndex((entry) => entry.pageIndex === pageIndex)
    at = Math.max(0, same)
  }

  /** @param {number} delta */
  function step(delta) {
    at = Math.min(Math.max(0, at + delta), Math.max(0, pages.length - 1))
  }

  /** @param {PointerEvent & {currentTarget: HTMLElement}} event */
  function wipeTo(event) {
    if (event.type === 'pointermove' && !(event.buttons & 1)) return
    const box = event.currentTarget.getBoundingClientRect()
    if (box.width > 0) wipe = Math.round(Math.min(100, Math.max(0, ((event.clientX - box.left) / box.width) * 100)))
  }

  /** The largest the pages are drawn, as a multiple of their fitted size. */
  const MAX_SCALE = 8
  /** What one press of a zoom button multiplies the scale by. */
  const SCALE_STEP = 1.5

  /** @type {HTMLElement|undefined} */
  let stage = $state()
  /** 1 is fitted to the box. */
  let scale = $state(1)
  /**
   * Where the pages' middle sits from the box's middle, as a share of the
   * box's width and height. A share and not pixels, so the place holds when
   * full window resizes the box, and its limit needs no measurement.
   */
  let offset = $state({ x: 0, y: 0 })
  const placed = $derived(
    scale === 1 ? undefined : `translate(${offset.x * 100}%, ${offset.y * 100}%) scale(${scale})`,
  )

  /**
   * Draw the pages at `to`, their middle at `x`, `y`, held so that an
   * enlarged page still covers the box.
   *
   * @param {number} to @param {number} x @param {number} y
   */
  function place(to, x, y) {
    const reach = (to - 1) / 2
    scale = to
    offset = { x: Math.min(reach, Math.max(-reach, x)), y: Math.min(reach, Math.max(-reach, y)) }
  }

  /**
   * Zoom to `next`, keeping the point of the pages at `at` (a share of the
   * box, from its middle) where it is.
   *
   * @param {number} next @param {{x: number, y: number}} [at]
   */
  function zoomTo(next, at = { x: 0, y: 0 }) {
    const to = Math.min(MAX_SCALE, Math.max(1, next))
    const ratio = to / scale
    place(to, at.x - (at.x - offset.x) * ratio, at.y - (at.y - offset.y) * ratio)
  }

  /**
   * A pinch or a modifier-scroll zooms about the pointer; a plain scroll
   * moves the enlarged pages. Fitted pages have nowhere to move, so a plain
   * scroll over them is left to the dialog.
   *
   * @param {WheelEvent} event
   */
  function onwheel(event) {
    const box = stage?.getBoundingClientRect()
    if (!box || box.width <= 0 || box.height <= 0) return
    if (event.ctrlKey || event.metaKey) {
      event.preventDefault()
      const next = wheelZoom({ deltaY: event.deltaY, zoom: scale, min: 1, max: MAX_SCALE })
      if (next === null) return
      zoomTo(next, {
        x: (event.clientX - box.left) / box.width - 0.5,
        y: (event.clientY - box.top) / box.height - 0.5,
      })
      return
    }
    if (scale === 1) return
    event.preventDefault()
    // A mouse wheel has one axis: Shift turns it sideways where the system
    // has not already.
    const sideways = event.shiftKey && event.deltaX === 0
    const dx = sideways ? event.deltaY : event.deltaX
    const dy = sideways ? 0 : event.deltaY
    place(scale, offset.x - dx / box.width, offset.y - dy / box.height)
  }

  // Registered by hand: the zooming path prevents the default, which a
  // passive listener may not.
  $effect(() => {
    const box = stage
    if (!box) return
    box.addEventListener('wheel', onwheel, { passive: false })
    return () => box.removeEventListener('wheel', onwheel)
  })

  const options = runs.map((entry) => ({ value: String(entry.created), label: runLabel(entry) }))
  const facts = $derived(run ? runFacts(run).join(' · ') : '')
  const both = $derived(!!view.denoised && !!view.raw)
  const drawn = $derived(!!view.denoised || !!view.raw)
</script>

<Modal
  title={t('denoise.compare.title')}
  meta={t('denoise.compare.meta', { number: chapter?.number ?? '' })}
  width={760}
  fill={fullscreen}
  onclose={dismiss}
>
  <div class="compare" class:full={fullscreen} data-run={created} data-page={page?.pageIndex ?? ''}>
    <div class="head">
      <div class="top">
        <div class="picker">
          <Select label={t('denoise.compare.run')} {options} value={String(created)} onchange={chooseRun} />
        </div>
        <IconButton
          icon="zoom-fit"
          label={t('denoise.compare.fullscreen')}
          pressed={fullscreen}
          size={26}
          onclick={() => (fullscreen = !fullscreen)}
        />
      </div>
      {#if facts}<p class="facts">{facts}</p>{/if}
      {#if run?.folder}<p class="folder" title={run.folder}>{t('denoise.compare.folder', { folder: run.folder })}</p>{/if}
    </div>

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="stage" bind:this={stage} onpointerdown={wipeTo} onpointermove={wipeTo}>
      {#if view.raw}
        <img class="layer" src={view.raw} alt={t('denoise.compare.raw')} draggable="false" style:transform={placed} />
      {/if}
      {#if view.denoised}
        <!-- The wipe cuts the box, not the page: a clip on the image itself
             would be scaled and moved with it. -->
        <div class="cut" style:clip-path={both ? `inset(0 ${100 - wipe}% 0 0)` : undefined}>
          <img
            class="layer"
            src={view.denoised}
            alt={t('denoise.compare.denoised')}
            draggable="false"
            style:transform={placed}
          />
        </div>
      {/if}
      {#if both}
        <div class="divider" style:left="{wipe}%" aria-hidden="true"></div>
        <span class="tag start" aria-hidden="true">{t('denoise.compare.denoised')}</span>
        <span class="tag end" aria-hidden="true">{t('denoise.compare.raw')}</span>
      {/if}
      {#if view.loading && page}
        <p class="status">{t('denoise.compare.loading', { page: page.pageIndex + 1 })}</p>
      {:else if view.message}
        <p class="status" role="status">{t(view.message.key, view.message.params)}</p>
      {/if}
    </div>

    {#if page?.taken}<p class="note">{t('denoise.compare.taken')}</p>{/if}

    <div class="controls">
      <div class="stepper">
        <IconButton icon="chevron-left" label={t('denoise.compare.previous')} size={26} disabled={at <= 0} onclick={() => step(-1)} />
        <span class="where" aria-live="polite">
          {page ? t('denoise.compare.page', { page: page.pageIndex + 1, count: pageCount }) : ''}
        </span>
        <IconButton icon="chevron-right" label={t('denoise.compare.next')} size={26} disabled={at >= pages.length - 1} onclick={() => step(1)} />
      </div>
      <div class="zoom">
        <IconButton icon="zoom-out" label={t('editor.action.zoomOut')} size={26} disabled={!drawn || scale <= 1} onclick={() => zoomTo(scale / SCALE_STEP)} />
        <Readout text="{Math.round(scale * 100)}%" label={t('editor.action.zoomFit')} minWidth={48} onclick={() => place(1, 0, 0)} />
        <IconButton icon="zoom-in" label={t('editor.action.zoomIn')} size={26} disabled={!drawn || scale >= MAX_SCALE} onclick={() => zoomTo(scale * SCALE_STEP)} />
      </div>
      <div class="wipe">
        <Slider
          label={t('denoise.compare.wipe')}
          value={wipe}
          min={0}
          max={100}
          format={(value) => t('denoise.compare.wipeValue', { value })}
          onchange={(value) => (wipe = value)}
          compact
          disabled={!both}
        />
      </div>
    </div>
  </div>

  {#snippet buttons()}
    <Button variant="primary" onclick={() => closeModal(null)}>{t('shell.action.close')}</Button>
  {/snippet}
</Modal>

<style>
  .compare { display: flex; flex-direction: column; gap: var(--s-3) }
  .head { display: flex; flex-direction: column; gap: var(--s-1) }
  .top { display: flex; align-items: center; justify-content: space-between; gap: var(--s-3) }
  .picker { flex: 1; max-width: 420px }
  .facts,
  .folder,
  .note { margin: 0; font-size: 11.5px; color: var(--t2) }
  .folder { overflow: hidden; color: var(--t3); white-space: nowrap; text-overflow: ellipsis }

  /* One box for both pages. Each is fitted to it the same way, so the two
     line up whatever their resolutions. */
  .stage {
    position: relative;
    height: min(60vh, 620px);
    overflow: hidden;
    border-radius: var(--r-md);
    background: var(--paper);
    box-shadow: inset 0 0 0 1px var(--line);
    touch-action: none;
    cursor: ew-resize;
    user-select: none;
  }
  /* Full window: the box takes whatever height the controls leave. */
  .compare.full { flex: 1; min-height: 0 }
  .compare.full .stage { flex: 1; height: auto; min-height: 240px }
  .cut { position: absolute; inset: 0; pointer-events: none }
  .layer {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
    pointer-events: none;
  }
  .divider {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--page-divider);
    box-shadow: 0 0 0 1px var(--page-halo);
    pointer-events: none;
  }
  .tag {
    position: absolute;
    top: var(--s-2);
    padding: 2px 7px;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: var(--edge);
    color: var(--text);
    font-size: 11px;
    pointer-events: none;
  }
  .tag.start { left: var(--s-2) }
  .tag.end { right: var(--s-2) }
  .status {
    position: absolute;
    inset: auto var(--s-4) 50% var(--s-4);
    margin: 0;
    transform: translateY(50%);
    font-size: 12px;
    text-align: center;
    color: var(--t2);
  }

  .controls { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--s-3) }
  .stepper,
  .zoom { display: flex; align-items: center; gap: var(--s-2) }
  .where { min-width: 104px; font-size: 12px; text-align: center; color: var(--text); font-variant-numeric: tabular-nums }
  .wipe { flex: 1; max-width: 320px }
</style>
