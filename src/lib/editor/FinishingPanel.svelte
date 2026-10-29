<script>
  import { getBackend } from '../api/backend.js'
  import { finishingKey } from '../model/model-names.js'
  import { isCloudMask } from '../model/masks.js'
  import { offersFinishing, recordedFinishing, setRegionFinishing } from './maskactions.svelte.js'
  import { Empty } from '../ui/index.js'
  import { hasKey, t } from '../i18n/index.js'

  /**
   * The Post-processing window's body: a cleaned layer's post-processing,
   * picked again from previews. One picture per method, each the layer's
   * answer finished that way (`regionFinishingPreviews`, made on this
   * computer, with nothing sent), and one large picture above them of the
   * method the pointer or the keyboard is on, else the current one, so two
   * can be told apart without squinting at tiles. The method the layer's
   * pixels were made by is marked. Picking another finishes the layer by it,
   * one undo step (`maskactions.svelte.js#setRegionFinishing`), and the
   * window stays as it is so the next can be compared.
   *
   * It shows whatever layer is selected (`PostProcessingWindow`), and follows
   * the selection: a layer picked on the page or in Layers is the one shown.
   * The previews are asked for once per result, keyed by the region and what
   * made its pixels, so a pick (which keeps both) does not ask again, and an
   * undo or a new clean under the same region does.
   *
   * Nothing selected, a layer with no post-processing (a detection, a paint
   * stroke, a copy, a Patch or Gradient layer), the previews loading or
   * failed, and a layer the backend cannot finish again (`unavailable`, with
   * why) each get a line in place of the grid. A layer cleaned on this
   * computer may take a model run before its first previews, and its loading
   * line says so.
   *
   * **Nothing here relies on focus.** WebKit, the shipped engine, does not
   * focus a button on click; the large picture follows the pointer over a
   * tile as well as the focus on one, and a pick is a click.
   *
   * @type {{ region: import('../api/backend.js').ApiRegion|null }}
   */
  let { region } = $props()

  const offered = $derived(offersFinishing(region))

  /**
   * What the previews depend on: the region, and the result its pixels came
   * from. A pick rewrites the pixels and the post-processing record and keeps
   * the rest of the provenance, so it is not a new key; a new clean or an
   * undo back past one is.
   */
  const resultKey = $derived.by(() => {
    if (!region || !offered) return null
    const provenance = /** @type {any} */ (region.mask?.provenance) ?? {}
    return [region.id, provenance.engine ?? '', provenance.created ?? '', provenance.cloud?.attempt_id ?? ''].join('|')
  })

  /** @type {import('../api/backend.js').FinishingPreviews|null} */
  let answer = $state(null)
  let failed = $state(false)
  /** The method whose pick is in flight. @type {string|null} */
  let applying = $state(null)
  /** The method the pointer or the keyboard is on, shown large. @type {string|null} */
  let peek = $state(null)

  /** A string, so a new object for the same layer (every pick) is no change. */
  const regionId = $derived(region?.id ?? null)

  $effect(() => {
    const key = resultKey
    const id = regionId
    answer = null
    failed = false
    peek = null
    if (!key || !id) return
    let gone = false
    getBackend().regionFinishingPreviews({ regionId: id }).then(
      (previews) => { if (!gone) answer = previews },
      (error) => {
        console.error('the finishing previews could not be made', error)
        if (!gone) failed = true
      },
    )
    return () => { gone = true }
  })

  const current = $derived(recordedFinishing(region) ?? answer?.current ?? null)

  /** Why a layer cannot be finished again, by the backend's reason. @type {Record<string, string>} */
  const UNAVAILABLE = {
    notCloud: 'masks.finishing.unavailable.notCloud',
    noResult: 'masks.finishing.unavailable.noResult',
    pageChanged: 'masks.finishing.unavailable.pageChanged',
    unsupported: 'masks.finishing.unavailable.unsupported',
    modelChanged: 'masks.finishing.unavailable.modelChanged',
  }

  /** A method's name as the Post-processing settings show it. @param {string} name */
  function label(name) {
    const key = finishingKey('option', name)
    return hasKey(key) ? t(key) : name
  }

  /** A method's one-line description, where it has one. @param {string} name */
  function hint(name) {
    const key = finishingKey('hint', name)
    return hasKey(key) ? t(key) : undefined
  }

  const previews = $derived(answer && !answer.unavailable ? answer.previews : [])
  const shown = $derived(
    previews.find((preview) => preview.finishing === peek) ??
      previews.find((preview) => preview.finishing === current?.finishing) ??
      previews[0] ??
      null,
  )

  /** @param {string} name */
  async function pick(name) {
    if (!region || applying || !answer || current?.finishing === name) return
    applying = name
    try {
      await setRegionFinishing(region, { finishing: name, grayscale: answer.grayscale })
    } finally {
      applying = null
    }
  }

  /** @param {string} name */
  function enter(name) {
    peek = name
  }

  /** @param {string} name */
  function leave(name) {
    if (peek === name) peek = null
  }
</script>

<div class="finishing" data-finishing-panel>
  {#if !region}
    <Empty size="sm" title={t('masks.finishing.window.noLayer')} body={t('masks.finishing.window.noLayerBody')} />
  {:else if !offered}
    <Empty size="sm" title={t('masks.finishing.window.notOffered')} body={t('masks.finishing.window.notOfferedBody')} />
  {:else if failed}
    <p class="note" role="status">{t('masks.finishing.failed')}</p>
  {:else if !answer}
    <p class="note" role="status" data-finishing-loading>
      {t(isCloudMask(region.mask ?? null) ? 'masks.finishing.loading' : 'masks.finishing.loadingLocal')}
    </p>
  {:else if answer.unavailable}
    <p class="note" data-finishing-unavailable={answer.unavailable}>{t(UNAVAILABLE[answer.unavailable] ?? 'masks.finishing.unavailable.notCloud')}</p>
  {:else}
    {#if shown}
      {@const shownCurrent = current?.finishing === shown.finishing}
      <figure class="stage" data-finishing-stage={shown.finishing}>
        <span class="frame large">
          {#if shown.url}
            <img src={shown.url} alt={label(shown.finishing)} draggable="false" />
          {:else}
            <span class="missing">{t('masks.finishing.previewFailed')}</span>
          {/if}
        </span>
        <figcaption>
          <span class="caption-line">
            <span class="caption-name">{label(shown.finishing)}</span>
            {#if shownCurrent}<span class="pill">{t('masks.finishing.current')}</span>{/if}
          </span>
          {#if hint(shown.finishing)}<span class="caption-hint">{hint(shown.finishing)}</span>{/if}
        </figcaption>
      </figure>
    {/if}

    <p class="note">{t(answer.grayscale ? 'masks.finishing.introGray' : 'masks.finishing.intro')}</p>
    <div class="grid" role="group" aria-label={t('masks.finishing.title')}>
      {#each previews as preview (preview.finishing)}
        {@const isCurrent = current?.finishing === preview.finishing}
        <button
          type="button"
          class="tile"
          class:current={isCurrent}
          class:peeked={shown?.finishing === preview.finishing && !isCurrent}
          aria-pressed={isCurrent}
          disabled={applying !== null && applying !== preview.finishing}
          aria-busy={applying === preview.finishing}
          title={hint(preview.finishing)}
          data-finishing={preview.finishing}
          onclick={() => pick(preview.finishing)}
          onpointerenter={() => enter(preview.finishing)}
          onpointerleave={() => leave(preview.finishing)}
          onfocus={() => enter(preview.finishing)}
          onblur={() => leave(preview.finishing)}
        >
          <span class="frame">
            {#if preview.url}
              <img src={preview.url} alt="" draggable="false" />
            {:else}
              <span class="missing">{t('masks.finishing.previewFailed')}</span>
            {/if}
            {#if applying === preview.finishing}<span class="busy" aria-hidden="true"></span>{/if}
          </span>
          <span class="name">{label(preview.finishing)}</span>
          {#if isCurrent}
            <span class="badge">{t('masks.finishing.current')}</span>
          {:else if applying === preview.finishing}
            <span class="badge">{t('masks.finishing.applying')}</span>
          {/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .finishing {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    min-height: 0;
    cursor: default;
  }

  .note {
    margin: 0;
    color: var(--t2);
    font-size: 11.5px;
    line-height: 1.5;
  }

  .stage {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    margin: 0;
  }

  .frame {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    aspect-ratio: 4 / 3;
    overflow: hidden;
    border-radius: var(--r-sm);
    background: var(--panel2);
  }
  /* As large as the window is wide, but never so tall that the grid under it
     is pushed out of a window of the default height. */
  .frame.large {
    aspect-ratio: auto;
    height: clamp(140px, 32vh, 340px);
    border-radius: var(--r-md);
  }

  img {
    display: block;
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    image-rendering: auto;
  }
  .large img {
    width: 100%;
    height: 100%;
  }

  figcaption {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .caption-line {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    min-width: 0;
  }

  .caption-name {
    overflow: hidden;
    color: var(--text);
    font-size: 12px;
    font-weight: 500;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .caption-hint {
    color: var(--t3);
    font-size: 11px;
    line-height: 1.45;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: var(--s-2);
  }

  .tile {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
    padding: 4px;
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: transparent;
    color: var(--t2);
    font: inherit;
    text-align: start;
    cursor: pointer;
    transition:
      border-color var(--dur-fast) var(--ease),
      background var(--dur-fast) var(--ease);
  }
  .tile:hover:not(:disabled),
  .tile.peeked,
  .tile:focus-visible { background: var(--accent-soft); color: var(--text) }
  .tile:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px }
  .tile:disabled { opacity: .45; cursor: default }
  .tile.current {
    border-color: var(--accent);
    box-shadow: inset 0 0 0 1px var(--accent);
    color: var(--text);
  }

  .missing {
    padding: var(--s-2);
    color: var(--t3);
    font-size: 10.5px;
    text-align: center;
  }

  /* A pick in flight: the tile's picture dims and a thin bar runs along its
     foot, so the wait is seen on the method that was asked for. */
  .busy {
    position: absolute;
    inset: 0;
    background: color-mix(in srgb, var(--panel) 45%, transparent);
  }
  .busy::after {
    content: '';
    position: absolute;
    left: 0;
    bottom: 0;
    width: 40%;
    height: 2px;
    background: var(--accent);
    animation: mcBusy 1s var(--ease) infinite alternate;
  }
  @keyframes mcBusy {
    from { transform: translateX(0) }
    to { transform: translateX(150%) }
  }

  .name {
    overflow: hidden;
    font-size: 11px;
    line-height: 1.3;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .badge,
  .pill {
    padding: 1px 6px;
    border-radius: var(--r-pill);
    background: var(--accent);
    color: var(--accent-fg);
    font-size: 9.5px;
    line-height: 1.5;
    white-space: nowrap;
  }
  .badge {
    position: absolute;
    top: 7px;
    right: 7px;
  }

  @media (prefers-reduced-motion: reduce) {
    .busy::after { animation: none; width: 100% }
  }
</style>
