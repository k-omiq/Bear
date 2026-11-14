<script>
  /**
   * A chapter's denoise runs, one page at a time against the raw page it was
   * made from. Opened from Denoise history in the chapter menu
   * (`home/actions.js#openDenoiseHistory`) on the run in use.
   *
   * The run picker is the whole history: every run stays until the user
   * deletes it here. One run is the one in use (the file Replace pages with
   * denoised takes, `Project::current_denoised`); it is the newest unless the
   * user picked another with Use this run. Delete asks first, inline, and then
   * removes the run's rows and files (`denoise_history.rs#delete_run`). The
   * history is read again after either, so the list is what disk now holds.
   *
   * The two pages are stacked in one box, each fitted to it the same way, so
   * a 4x upscale and the scan it came from line up on screen. The denoised
   * page is on top, clipped at the wipe: denoised left of the divider, raw
   * right of it. The slider moves the divider from the keyboard; a press or
   * drag on the pages moves it by pointer.
   *
   * Zoom is Fit, 1:1 or 2:1 of the denoised file's own pixels. A modifier
   * scroll or a pinch zooms about the pointer, and a plain scroll moves the
   * enlarged pages (`editor/CanvasStage.svelte` does the same). The zoom and
   * the place stay from page to page, so one spot can be followed through a
   * chapter; 1:1 and 2:1 are measured again for each file.
   *
   * Both sides come from `denoiseCompareImage` as bytes, never as a path. A
   * page whose file is gone says so and where it was.
   */
  import { onDestroy, untrack } from 'svelte'
  import { Button, IconButton, Modal, Segmented, Select, Slider } from '../ui/index.js'
  import { closeModal } from '../state/app.svelte.js'
  import { getBackend } from '../api/backend.js'
  import { t } from '../i18n/index.js'
  import { imageType, runFacts, runInUse, runLabel } from '../model/denoisehistory.js'
  import { loadLibrary } from '../home/library.svelte.js'
  import { wheelZoom } from '../editor/zoom.js'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()

  const chapter = untrack(() => /** @type {any} */ (spec?.props?.chapter ?? null))
  const pageCount = Array.isArray(chapter?.pages) ? chapter.pages.length : 0

  /** @type {import('../api/backend.js').DenoiseRun[]} */
  let runs = $state(untrack(() => (Array.isArray(spec?.props?.runs) ? spec.props.runs : [])))
  let created = $state(untrack(() => /** @type {number} */ (spec?.props?.run ?? runInUse(runs) ?? 0)))
  const run = $derived(runs.find((entry) => entry.created === created) ?? null)
  const inUse = $derived(runInUse(runs))
  const pages = $derived(run?.pages ?? [])
  let at = $state(0)
  const page = $derived(pages[at] ?? null)
  /** Share of the width, from the left, that shows the denoised page. */
  let wipe = $state(50)

  /** Delete waits on this inline question; `busy` while a call is out. */
  let asking = $state(false)
  let busy = $state(false)
  /** @type {string|null} */
  let failure = $state(null)

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
   * @param {string} folder
   * @returns {Promise<View>}
   */
  async function load(target, which, folder) {
    const number = target.pageIndex + 1
    const gone = { key: 'denoise.compare.missing', params: { page: number, folder } }
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
    const folder = untrack(() => run?.folder ?? '')
    let live = true
    untrack(() => {
      revoke(view)
      view = { loading: true, denoised: null, raw: null, message: null }
    })
    if (!target) {
      view = { loading: false, denoised: null, raw: null, message: { key: 'denoise.compare.empty' } }
      return
    }
    load(target, which, folder).then((shown) => {
      if (live) view = shown
      else revoke(shown)
    })
    return () => { live = false }
  })

  onDestroy(() => revoke(view))

  /** @param {number} to */
  function showRun(to) {
    const pageIndex = page?.pageIndex
    created = to
    asking = false
    failure = null
    // Stay on the same page when the other run has it.
    const same = (runs.find((entry) => entry.created === to)?.pages ?? []).findIndex((entry) => entry.pageIndex === pageIndex)
    at = Math.max(0, same)
  }

  /** The history again, after a change to it. Empty closes the dialog. */
  async function reread() {
    const fresh = await getBackend().denoiseHistory({ chapterId: chapter.id })
    loadLibrary()
    if (!Array.isArray(fresh) || !fresh.length) {
      closeModal(null)
      return false
    }
    runs = fresh
    return true
  }

  async function useRun() {
    if (busy || !run) return
    busy = true
    failure = null
    try {
      await getBackend().useDenoiseRun({ chapterId: chapter.id, run: run.created })
      await reread()
    } catch {
      failure = t('denoise.compare.useFailed')
    } finally {
      busy = false
    }
  }

  async function deleteRun() {
    if (busy || !run) return
    busy = true
    failure = null
    const gone = run.created
    try {
      await getBackend().deleteDenoiseRun({ chapterId: chapter.id, run: gone })
      asking = false
      if (await reread()) showRun(runs.some((entry) => entry.created === gone) ? gone : (runInUse(runs) ?? runs[0].created))
    } catch {
      failure = t('denoise.compare.deleteFailed')
    } finally {
      busy = false
    }
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

  /** @type {HTMLElement|undefined} */
  let stage = $state()
  /** The denoised file's own size, once it has loaded. */
  let natural = $state({ width: 0, height: 0 })
  /** 1 is fitted to the box. */
  let scale = $state(1)
  /** The zoom button that set `scale`, or null after a pinch or scroll zoom. */
  /** @type {'fit'|'actual'|'double'|null} */
  let mode = $state('fit')
  /**
   * Where the pages' middle sits from the box's middle, as a share of the
   * box's width and height, so the place needs no measurement to hold.
   */
  let offset = $state({ x: 0, y: 0 })
  const placed = $derived(
    scale === 1 ? undefined : `translate(${offset.x * 100}%, ${offset.y * 100}%) scale(${scale})`,
  )

  /**
   * The scale at which one pixel of the denoised file is `per` pixels on
   * screen, never below fit. Null before the file and the box are measured.
   *
   * @param {number} per
   */
  function pixelScale(per) {
    const box = stage?.getBoundingClientRect()
    if (!box || box.width <= 0 || box.height <= 0 || natural.width <= 0 || natural.height <= 0) return null
    const fit = Math.min(box.width / natural.width, box.height / natural.height)
    return Math.max(1, per / fit)
  }

  /** The largest a zoom goes: 2:1 at least, and never below 8x fit. */
  function maxScale() {
    return Math.max(8, pixelScale(2) ?? 0)
  }

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
   * Zoom to `next`, keeping the point of the pages at `about` (a share of
   * the box, from its middle) where it is.
   *
   * @param {number} next @param {{x: number, y: number}} [about]
   */
  function zoomTo(next, about = { x: 0, y: 0 }) {
    const to = Math.min(maxScale(), Math.max(1, next))
    const ratio = to / scale
    place(to, about.x - (about.x - offset.x) * ratio, about.y - (about.y - offset.y) * ratio)
  }

  /** @param {string} value */
  function chooseZoom(value) {
    if (value === 'fit') {
      mode = 'fit'
      place(1, 0, 0)
      return
    }
    const to = pixelScale(value === 'double' ? 2 : 1)
    if (to === null) return
    mode = value === 'double' ? 'double' : 'actual'
    zoomTo(to)
  }

  /** @param {Event & {currentTarget: HTMLImageElement}} event */
  function measured(event) {
    natural = { width: event.currentTarget.naturalWidth, height: event.currentTarget.naturalHeight }
    // A run at another scale has other pixels: 1:1 is 1:1 of this file.
    if (mode === 'actual' || mode === 'double') chooseZoom(mode)
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
      const next = wheelZoom({ deltaY: event.deltaY, zoom: scale, min: 1, max: maxScale() })
      if (next === null) return
      mode = next === 1 ? 'fit' : null
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

  const options = $derived(runs.map((entry) => ({
    value: String(entry.created),
    label: entry.created === inUse ? `${runLabel(entry)} · ${t('denoise.compare.inUse')}` : runLabel(entry),
  })))
  const facts = $derived(run ? runFacts(run).join(' · ') : '')
  const files = $derived(run ? run.pages.filter((entry) => entry.exists).length : 0)
  const both = $derived(!!view.denoised && !!view.raw)
  const drawn = $derived(!!view.denoised || !!view.raw)
  const zooms = $derived([
    { value: 'fit', label: t('denoise.compare.fit') },
    { value: 'actual', label: t('denoise.compare.actual'), disabled: !view.denoised },
    { value: 'double', label: t('denoise.compare.double'), disabled: !view.denoised },
  ])
</script>

<Modal
  title={t('denoise.compare.title')}
  meta={t('denoise.compare.meta', { number: chapter?.number ?? '' })}
  width={960}
  onclose={() => closeModal(null)}
>
  <div class="compare" data-run={created} data-page={page?.pageIndex ?? ''}>
    <div class="head">
      <div class="picker">
        <Select label={t('denoise.compare.run')} {options} value={String(created)} onchange={(value) => showRun(Number(value))} />
      </div>
      {#if run && created === inUse}
        <span class="in-use">{t('denoise.compare.inUse')}</span>
      {:else if run}
        <Button variant="soft" disabled={busy} onclick={useRun}>{t('denoise.compare.use')}</Button>
      {/if}
      {#if run && !asking}
        <Button disabled={busy} onclick={() => { asking = true; failure = null }}>{t('denoise.compare.delete')}</Button>
      {/if}
    </div>
    {#if facts}<p class="facts">{facts}</p>{/if}

    {#if asking}
      <div class="ask" role="alertdialog" aria-labelledby="denoise-delete-ask">
        <p id="denoise-delete-ask">{t('denoise.compare.deleteAsk', { count: files })}</p>
        <Button disabled={busy} onclick={() => (asking = false)}>{t('denoise.compare.keep')}</Button>
        <Button variant="primary" disabled={busy} onclick={deleteRun}>{t('denoise.compare.deleteConfirm')}</Button>
      </div>
    {/if}
    {#if failure}<p class="failure" role="alert">{failure}</p>{/if}

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
            onload={measured}
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

    {#if page?.taken}<p class="facts">{t('denoise.compare.taken')}</p>{/if}

    <div class="controls">
      <div class="stepper">
        <IconButton icon="chevron-left" label={t('denoise.compare.previous')} size={26} disabled={at <= 0} onclick={() => step(-1)} />
        <span class="where" aria-live="polite">
          {page ? t('denoise.compare.page', { page: page.pageIndex + 1, count: pageCount }) : ''}
        </span>
        <IconButton icon="chevron-right" label={t('denoise.compare.next')} size={26} disabled={at >= pages.length - 1} onclick={() => step(1)} />
      </div>
      <Segmented
        label={t('denoise.compare.zoom')}
        options={zooms}
        value={mode ?? ''}
        onchange={chooseZoom}
        disabled={!drawn}
        align="start"
      />
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
  .compare { display: flex; flex-direction: column; gap: var(--s-2) }
  .head { display: flex; align-items: flex-end; gap: var(--s-2) }
  .picker { flex: 1; min-width: 0; max-width: 520px }
  .in-use { padding: 0 var(--s-2); line-height: 26px; font-size: 12px; color: var(--t2) }
  .facts,
  .failure { margin: 0; font-size: 11.5px; color: var(--t2) }
  .failure { color: var(--warn) }
  .ask { display: flex; align-items: center; gap: var(--s-2) }
  .ask p { flex: 1; margin: 0; font-size: 12px; color: var(--text) }

  /* One box for both pages. Each is fitted to it the same way, so the two
     line up whatever their resolutions. */
  .stage {
    position: relative;
    height: min(66vh, 720px);
    overflow: hidden;
    border-radius: var(--r-md);
    background: var(--paper);
    box-shadow: inset 0 0 0 1px var(--line);
    touch-action: none;
    cursor: ew-resize;
    user-select: none;
    -webkit-user-select: none;
  }
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
    padding: 1px 6px;
    border-radius: var(--r-sm);
    background: var(--surface);
    color: var(--t2);
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
  .stepper { display: flex; align-items: center; gap: var(--s-2) }
  .where { min-width: 104px; font-size: 12px; text-align: center; color: var(--text); font-variant-numeric: tabular-nums }
  .wipe { flex: 1; max-width: 280px }
</style>
