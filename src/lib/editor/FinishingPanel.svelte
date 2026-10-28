<script>
  import { getBackend } from '../api/backend.js'
  import { editor } from '../state/editor.svelte.js'
  import { finishingKey } from '../model/model-names.js'
  import { recordedFinishing, setRegionFinishing } from './maskactions.svelte.js'
  import { captureFocus } from '../ui/focus.js'
  import { hasKey, t } from '../i18n/index.js'

  /**
   * A cloud result's post-processing, picked again from previews: one small
   * picture per method, side by side, each the saved answer finished that way
   * (`regionFinishingPreviews`, composited natively from the answer the
   * attempt kept, with nothing sent). The method the layer's pixels were made
   * by is marked. Picking another finishes the layer by it, one undo step
   * (`maskactions.svelte.js#setRegionFinishing`), and the panel stays open so
   * the next can be compared.
   *
   * A layer that cannot be finished again - cleaned on this computer, its
   * saved answer gone, the page under it changed - gets a line saying so and
   * that a new clean is the way, and no grid.
   *
   * Opened from the Layers row (`MaskRow`), whose window clips what overflows
   * it, so the panel is fixed to the viewport under its trigger, as a context
   * menu is. Controlled: the host mounts it and clears it on `onclose`.
   *
   * **Dismissal does not rely on focus.** WebKit, the shipped engine, does
   * not focus a button on click, so after a pick the focus may be on `<body>`.
   * Escape is heard on the window before the editor's own keys hear it, and
   * an outside press is heard on the document; a press on the trigger is the
   * trigger's own toggle.
   *
   * @type {{
   *   region: import('../api/backend.js').ApiRegion,
   *   anchor: HTMLElement|null|undefined,
   *   onclose: () => void,
   * }}
   */
  let { region, anchor, onclose } = $props()

  /** How close to the window edge the panel may come. */
  const MARGIN = 6

  const titleId = $props.id()
  const regionId = $derived(region.id)

  /** The layer as the editor holds it now: a pick or an undo moves it. */
  const live = $derived(
    editor.chapter?.pages.flatMap((page) => page.regions ?? []).find((entry) => entry.id === regionId) ?? null,
  )

  /** @type {import('../api/backend.js').FinishingPreviews|null} */
  let answer = $state(null)
  let failed = $state(false)
  /** The method whose pick is in flight. @type {string|null} */
  let applying = $state(null)

  // The previews are made once per region: they come from the saved answer,
  // which no pick changes. What a pick or an undo does change is which one
  // is current, and that is read off the live layer.
  $effect(() => {
    const id = regionId
    let gone = false
    answer = null
    failed = false
    getBackend().regionFinishingPreviews({ regionId: id }).then(
      (previews) => { if (!gone) answer = previews },
      (error) => {
        console.error('the finishing previews could not be made', error)
        if (!gone) failed = true
      },
    )
    return () => { gone = true }
  })

  const current = $derived(recordedFinishing(live) ?? answer?.current ?? null)

  /** @type {Record<string, string>} */
  const UNAVAILABLE = {
    notCloud: 'masks.finishing.unavailable.notCloud',
    noResult: 'masks.finishing.unavailable.noResult',
    pageChanged: 'masks.finishing.unavailable.pageChanged',
    unsupported: 'masks.finishing.unavailable.unsupported',
  }

  /** A method's name as the Post-processing settings show it. @param {string} name */
  function label(name) {
    const key = finishingKey('option', name)
    return hasKey(key) ? t(key) : name
  }

  /** @param {string} name */
  async function pick(name) {
    const target = live ?? region
    if (applying || !answer || current?.finishing === name) return
    applying = name
    try {
      await setRegionFinishing(target, { finishing: name, grayscale: answer.grayscale })
    } finally {
      applying = null
    }
  }

  /** @type {HTMLElement|undefined} */
  let root = $state()
  /** @type {{left: number, top: number}|null} */
  let placed = $state(null)

  // Under the trigger, kept inside the window once the panel has a size: the
  // grid's height is not known until the previews are in.
  $effect(() => {
    void answer
    void failed
    if (!root) return
    const box = root.getBoundingClientRect()
    const at = anchor?.getBoundingClientRect()
    const width = globalThis.innerWidth ?? box.width
    const height = globalThis.innerHeight ?? box.height
    const left = at ? at.left : box.left
    const below = at ? at.bottom + MARGIN : box.top
    const top = below + box.height > height - MARGIN && at && at.top - box.height - MARGIN > MARGIN
      ? at.top - box.height - MARGIN
      : below
    placed = {
      left: Math.max(MARGIN, Math.min(left, width - box.width - MARGIN)),
      top: Math.max(MARGIN, Math.min(top, height - box.height - MARGIN)),
    }
  })

  const restore = captureFocus()

  // The panel takes focus as it appears, without scrolling the Layers list
  // to it, so Tab walks its tiles.
  $effect(() => {
    root?.focus({ preventScroll: true })
  })

  $effect(() => {
    /** @param {Node|null} node */
    const inside = (node) => Boolean(node && (root?.contains(node) || anchor?.contains(node)))
    /** @param {Event} event */
    const outside = (event) => {
      if (!inside(/** @type {Node} */ (event.target))) onclose()
    }
    /** @param {KeyboardEvent} event */
    const key = (event) => {
      if (event.key !== 'Escape') return
      event.preventDefault()
      event.stopPropagation()
      event.stopImmediatePropagation()
      restore()
      onclose()
    }
    // The list it hangs from scrolling leaves it pointing at nothing.
    /** @param {Event} event */
    const scrolled = (event) => {
      if (event.target instanceof Node && root?.contains(event.target)) return
      onclose()
    }
    const away = () => onclose()
    document.addEventListener('pointerdown', outside, true)
    globalThis.addEventListener('keydown', key, true)
    globalThis.addEventListener('scroll', scrolled, true)
    globalThis.addEventListener('resize', away)
    return () => {
      document.removeEventListener('pointerdown', outside, true)
      globalThis.removeEventListener('keydown', key, true)
      globalThis.removeEventListener('scroll', scrolled, true)
      globalThis.removeEventListener('resize', away)
    }
  })
</script>

<div
  bind:this={root}
  class="panel"
  role="dialog"
  aria-labelledby={titleId}
  tabindex="-1"
  data-finishing-panel
  style:left={placed ? `${placed.left}px` : null}
  style:top={placed ? `${placed.top}px` : null}
  style:visibility={placed ? null : 'hidden'}
>
  <h3 class="title" id={titleId}>{t('masks.finishing.title')}</h3>
  {#if failed}
    <p class="note" role="status">{t('masks.finishing.failed')}</p>
  {:else if !answer}
    <p class="note" role="status">{t('masks.finishing.loading')}</p>
  {:else if answer.unavailable}
    <p class="note" data-finishing-unavailable={answer.unavailable}>{t(UNAVAILABLE[answer.unavailable] ?? 'masks.finishing.unavailable.notCloud')}</p>
  {:else}
    <p class="note">{t(answer.grayscale ? 'masks.finishing.introGray' : 'masks.finishing.intro')}</p>
    <div class="grid" role="group" aria-label={t('masks.finishing.title')}>
      {#each answer.previews as preview (preview.finishing)}
        {@const isCurrent = current?.finishing === preview.finishing}
        <button
          type="button"
          class="tile"
          class:current={isCurrent}
          aria-pressed={isCurrent}
          disabled={applying !== null && applying !== preview.finishing}
          aria-busy={applying === preview.finishing}
          title={hasKey(finishingKey('hint', preview.finishing)) ? t(finishingKey('hint', preview.finishing)) : undefined}
          data-finishing={preview.finishing}
          onclick={() => pick(preview.finishing)}
        >
          <span class="frame">
            {#if preview.url}
              <img src={preview.url} alt="" draggable="false" />
            {:else}
              <span class="missing">{t('masks.finishing.previewFailed')}</span>
            {/if}
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
  .panel {
    position: fixed;
    z-index: 55;
    width: min(420px, calc(100vw - 12px));
    max-height: min(560px, calc(100vh - 12px));
    overflow-y: auto;
    padding: var(--s-3);
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--edge);
    animation: mcIn var(--dur) var(--ease);
    cursor: default;
    outline: none;
  }

  .title {
    margin: 0 0 var(--s-1);
    color: var(--t3);
    font-size: 10px;
    font-weight: 500;
    letter-spacing: .04em;
    text-transform: uppercase;
  }

  .note {
    margin: 0 0 var(--s-3);
    color: var(--t2);
    font-size: 11.5px;
    line-height: 1.5;
  }
  .note:last-child { margin-bottom: 0 }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(118px, 1fr));
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
  .tile:focus-visible { background: var(--accent-soft); color: var(--text) }
  .tile:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px }
  .tile:disabled { opacity: .45; cursor: default }
  .tile.current { border-color: var(--accent); color: var(--text) }

  .frame {
    display: flex;
    align-items: center;
    justify-content: center;
    aspect-ratio: 4 / 3;
    overflow: hidden;
    border-radius: var(--r-sm);
    background: var(--panel2);
  }

  img {
    display: block;
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    image-rendering: auto;
  }

  .missing {
    padding: var(--s-2);
    color: var(--t3);
    font-size: 10.5px;
    text-align: center;
  }

  .name {
    overflow: hidden;
    font-size: 11px;
    line-height: 1.3;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .badge {
    position: absolute;
    top: 7px;
    right: 7px;
    padding: 1px 5px;
    border-radius: var(--r-pill);
    background: var(--accent);
    color: var(--accent-fg);
    font-size: 9.5px;
    line-height: 1.5;
  }
</style>
