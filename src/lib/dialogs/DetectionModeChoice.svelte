<script>
  /**
   * Normal or Advanced, as two cards: what each cleans, the computer it
   * needs, and what it still costs to download. Setup's Detection step and
   * Settings > Models draw the same cards, so the choice reads the same in
   * both places (`pipelines.js#DETECTION_MODES`).
   *
   * The memory figures are the measured ones: a Normal run peaks at about
   * 2.5 GB with LaMa, and the SAM-TS-L lettering mask Advanced adds needs
   * about 5 GB free (`cleaner-core/src/sam_ts.rs#MIN_ROOM_BYTES`). Neither
   * needs a graphics card. When the backend says what this computer has, one
   * line under the cards says so; Advanced with less free memory still runs,
   * slower, and the run says so when it starts.
   */
  import { t } from '../i18n/index.js'
  import { DETECTION_MODE_IDS } from '../model/pipelines.js'

  /**
   * @type {{
   *   value: 'normal'|'advanced',
   *   onchange: (mode: 'normal'|'advanced') => void,
   *   sizes?: {normal?: number|null, advanced?: number|null},
   *   memory?: {totalBytes?: number|null, roomBytes?: number|null}|null,
   *   label: string,
   * }}
   */
  let { value, onchange, sizes = {}, memory = null, label } = $props()

  const uid = $props.id()

  /** Whole keys chosen between, never built, so the catalogue test sees each one. */
  const TEXT = {
    normal: { name: 'detectionMode.normal.name', about: 'detectionMode.normal.about', needs: 'detectionMode.normal.needs' },
    advanced: { name: 'detectionMode.advanced.name', about: 'detectionMode.advanced.about', needs: 'detectionMode.advanced.needs' },
  }

  /** @param {'normal'|'advanced'} mode */
  function costOf(mode) {
    const bytes = sizes?.[mode]
    if (bytes === null || bytes === undefined) return ''
    return bytes > 0 ? t('detectionMode.download', { bytes }) : t('detectionMode.ready')
  }

  const machine = $derived(
    typeof memory?.totalBytes === 'number' && typeof memory?.roomBytes === 'number'
      ? t('detectionMode.machine', { total: memory.totalBytes, free: memory.roomBytes })
      : '',
  )

  /**
   * The card keyboard focus is on, while it is inside the group: it holds the
   * one tab stop, and leaving the group hands it back to the chosen card.
   *
   * @type {'normal'|'advanced'|null}
   */
  let focused = $state(null)

  /**
   * Arrow keys move focus between the cards, never the choice: in Settings a
   * choice starts that mode's download, so passing over a card on the way to
   * another must not fetch it. Space, Enter or a click chooses.
   *
   * @param {KeyboardEvent & {currentTarget: HTMLElement}} event
   * @param {number} index
   */
  function onkeydown(event, index) {
    const count = DETECTION_MODE_IDS.length
    let next = null
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (index + 1) % count
    else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') next = (index - 1 + count) % count
    else if (event.key === 'Home') next = 0
    else if (event.key === 'End') next = count - 1
    if (next === null) return
    event.preventDefault()
    event.stopPropagation()
    const card = event.currentTarget.parentElement?.children[next]
    if (card instanceof HTMLElement) card.focus()
  }

  /** @param {FocusEvent & {currentTarget: HTMLElement}} event */
  function onfocusout(event) {
    if (!(event.relatedTarget instanceof Node && event.currentTarget.contains(event.relatedTarget))) focused = null
  }
</script>

<div class="modes" role="radiogroup" aria-label={label} aria-describedby="{uid}-hardware" data-detection-mode {onfocusout}>
  {#each DETECTION_MODE_IDS as mode, index (mode)}
    <button
      type="button"
      role="radio"
      class="mode"
      class:on={value === mode}
      aria-checked={value === mode}
      tabindex={(focused ?? value) === mode ? 0 : -1}
      data-mode={mode}
      onfocus={() => (focused = mode)}
      onclick={() => onchange(mode)}
      onkeydown={(event) => onkeydown(event, index)}
    >
      <span class="name">{t(TEXT[mode].name)}</span>
      <span class="about">{t(TEXT[mode].about)}</span>
      <span class="needs">{t(TEXT[mode].needs)}</span>
      {#if costOf(mode)}<span class="cost">{costOf(mode)}</span>{/if}
    </button>
  {/each}
</div>
<p class="hardware" id="{uid}-hardware">
  {t('detectionMode.noGpu')}{#if machine}{' '}{machine}{/if}
</p>

<style>
  .modes { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--s-4) }
  @media (max-width: 620px) { .modes { grid-template-columns: 1fr } }
  .mode {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    padding: var(--s-5) var(--s-5) var(--s-6);
    border: none;
    border-radius: var(--r-xl);
    background: var(--panel);
    box-shadow: 0 0 0 1px var(--line);
    color: var(--text);
    text-align: left;
    cursor: pointer;
    transition: box-shadow var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease);
  }
  .mode:hover { box-shadow: 0 0 0 1px var(--line2) }
  .mode.on { background: var(--accent-soft); box-shadow: 0 0 0 1.5px var(--accent) }
  .mode:active { transform: scale(.99) }
  .name { font-size: 13px; font-weight: 600 }
  .about { color: var(--t2); font-size: 12px; line-height: 1.5 }
  .needs, .cost { color: var(--t2); font-size: 11.5px; line-height: 1.45; font-variant-numeric: tabular-nums }
  .needs { margin-top: var(--s-2) }
  .hardware { margin: var(--s-3) 0 0; color: var(--t2); font-size: 11.5px; line-height: 1.45 }
</style>
