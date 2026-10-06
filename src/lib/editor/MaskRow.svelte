<script>
  import { editor, isHighlighted, hover, select, undo, undoAvailable, undoLabelKey } from '../state/editor.svelte.js'
  import { MAX_MASK_PADDING, rowEngines, engineChoiceLabel, retryWidens } from '../model/masks.js'
  import { capabilities } from '../state/capabilities.svelte.js'
  import { cloud, cloudEntries, cloudUsable, pickCloudChoice } from '../state/cloud.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { cloudProfileLabel, currentCloudModelId, engineModelLabel } from '../model/model-names.js'
  import { actionHint } from './maskrows.js'
  import { menuPoint } from './gesture.js'
  import { cleanDetected, deleteRow, keepDependencyResult, openLayerEdit, rerunMask, runMaskAction, setDetectedMaskPadding, updateLayer } from './maskactions.svelte.js'
  import { layerCapabilities, layerOf } from '../model/layers.js'
  import { Button, Disclosure, IconButton, Select } from '../ui/index.js'
  import { t } from '../i18n/index.js'
  import MaskFacts from './MaskFacts.svelte'
  import RegionMenu from './RegionMenu.svelte'

  /**
   * One region in the Layers panel. A mask from the automatic pass and a mask
   * drawn by hand are the same row with the same controls and the same
   * provenance; a region with no mask - declined, or
   * skipped by the script gate - is the same row with what happened to it in
   * place of the engine.
   *
   * Collapsed: the status glyph, the engine, the sub-line, and the row's three
   * controls - the engine picker, Try again, and Delete. Expanded: the
   * provenance facts, then whatever actions the row still carries (a
   * gate-skipped region's Clean anyway; nothing, for a mask).
   *
   * **Three controls, not seven.** The row used to offer Stronger, Simpler,
   * Fill mode and Reopen: four buttons that asked the reader to hold an engine
   * ladder in their head to press any of them. The picker says the same thing
   * - *what should this layer be cleaned with* - as a list of names, and Try
   * again covers the case where the answer is "the same thing, again".
   *
   * **Try again replaces; a new stroke refines.** Both Try again and the
   * picker redo this layer from the layers below it and swap its result; a new
   * stroke reads the page as shown. The expanded row says so in one line, and
   * a paint or clone row keeps Try again visible but off, with the reason.
   *
   * **Delete is on every row, warnings included.** For a mask it deletes the
   * mask and the original text comes back; for a region with no mask it is the
   * region itself that goes, which is how a warning the user has read and
   * decided about leaves the list. Both are undoable and both go through the
   * backend, so the panel and the file cannot disagree.
   *
   * Hovering or focusing the row highlights the region on the canvas, and
   * activating it selects: the row writes `hover()` and `select()`, the canvas
   * reads `editor.hoverId` / `editor.selectionId`, and neither knows the other
   * exists (see the highlight contract in `state/editor.svelte.js`).
   *
   * @type {{
   *   row: import('./maskrows.js').MaskRow,
   *   region: import('../api/backend.js').ApiRegion,
   *   open: boolean,
   *   ontoggle: (open: boolean) => void,
   * }}
   */
  let { row, region, open, ontoggle } = $props()

  const pickerId = $props.id()

  const highlighted = $derived(isHighlighted(row.id))

  // A detected row lists Clean on cloud GPU only while a cloud endpoint is
  // ready and the chapter is paginated: left out otherwise, like every other
  // cloud entry, rather than greyed.
  const detected = $derived(row.status === 'detected')
  const actions = $derived(detected && !(cloudUsable() && editor.project?.mode !== 'longstrip')
    ? row.actions.filter((action) => action.id !== 'cleanDetectedCloud')
    : row.actions)

  /** @type {number|null} */
  let paddingDraft = $state(null)
  let savingPadding = $state(false)
  const maskPadding = $derived(paddingDraft ?? region.paddingPx ?? 0)

  /** @param {Event & {currentTarget: HTMLInputElement}} event */
  async function changePadding(event) {
    if (savingPadding || editor.run.active) {
      paddingDraft = null
      return
    }
    const value = Number(event.currentTarget.value)
    paddingDraft = value
    savingPadding = true
    try {
      await setDetectedMaskPadding(region, value)
    } finally {
      paddingDraft = null
      savingPadding = false
    }
  }

  // Try again replaces this layer from the layers below it; a paint or clone
  // stroke has no cleaning to redo, so its row keeps the control in place,
  // dimmed, and says why rather than dropping it without a word. Other masks
  // `reRunnable` refuses (an approved text-shape component) keep Delete alone.
  const retryBlocked = $derived(!row.reRunnable && (row.engine === 'paint' || row.engine === 'clone'))

  // Undo here is the editor's global undo, not an undo of the change that put
  // this layer in review, so its tooltip names what it would reverse.
  const undoKey = $derived(undoLabelKey())
  const undoHint = $derived(undoKey ? t('editor.action.undoCommand', { commandKey: undoKey }) : undefined)
  const title = $derived(engineModelLabel(row.engine, row.modelId, t(row.titleKey)))
  const sub = $derived(row.sub.map((part) => t(part.key, part.params)).join(' · '))
  // What the layer may do is the native side's answer (`mask.capabilities`,
  // not the machine's engine `capabilities`), derived from what produced it:
  // a fill or painted colour moves and takes a lock, a redraw is fixed where
  // it was made, a detection has no output yet. Moving and turning happen on
  // the page (`RegionLayer`); the row offers what is left - opacity, the
  // lock, and putting a moved layer back.
  const layerCaps = $derived(layerCapabilities(region))
  const layer = $derived(layerOf(region))
  const movable = $derived(layerCaps.transform === 'movable')
  const transformed = $derived(layer.offsetX !== 0 || layer.offsetY !== 0 || layer.rotation !== 0)

  /**
   * The opacity slider, mid-edit: the value it shows while its writes are
   * under way, and the one edit they all belong to.
   *
   * A drag across it previews through real, coalesced writes and commits one
   * undo entry on release (`openLayerEdit`). Arrow keys commit the same way
   * once the presses stop, so holding a key down is one step of history, not
   * one per percent.
   */
  /** @type {number|null} */
  let opacityDraft = $state(null)
  /** @type {import('./maskactions.svelte.js').LayerEdit|null} */
  let opacityEdit = null
  /** @type {ReturnType<typeof setTimeout>|null} */
  let opacityTimer = null
  const OPACITY_SETTLE_MS = 400

  /** @param {Event & {currentTarget: HTMLInputElement}} event */
  function onOpacityInput(event) {
    const value = Number(event.currentTarget.value)
    opacityDraft = value
    opacityEdit ??= openLayerEdit(region, 'masks.command.layerOpacity')
    opacityEdit.preview({ opacity: value })
    if (opacityTimer) {
      // A key press that is still arriving keeps its burst open.
      clearTimeout(opacityTimer)
      opacityTimer = setTimeout(() => settleOpacity(true), OPACITY_SETTLE_MS)
    }
  }

  /** @param {boolean} now - on release or blur; otherwise once the presses stop */
  function settleOpacity(now) {
    if (opacityTimer) clearTimeout(opacityTimer)
    opacityTimer = null
    if (!opacityEdit) return
    if (!now) {
      opacityTimer = setTimeout(() => settleOpacity(true), OPACITY_SETTLE_MS)
      return
    }
    const edit = opacityEdit
    const value = opacityDraft
    opacityEdit = null
    void edit.commit(value === null ? undefined : { opacity: value }).finally(() => {
      if (!opacityEdit) opacityDraft = null
    })
  }

  // A row collapsed or removed mid-edit still writes what was chosen.
  $effect(() => () => settleOpacity(true))

  // What this machine can clean with now, and Cloud last while a cloud
  // endpoint is ready and allowed: left out otherwise, like a rung whose
  // weights are missing. The engine the mask actually used leads the list
  // when it is not among them - Cloud while the cloud is not ready, a rung
  // this machine no longer has - because a picker that silently showed the
  // wrong entry as selected would be worse than one that is not shown at
  // all, which is what `row.reRunnable` decides; and a mask cleaned with FLUX
  // on another machine still has to read as FLUX here.
  const offered = $derived(rowEngines(capabilities.engines, { cloud: cloudUsable() }))
  const engineOptions = $derived(
    (row.engine && !offered.includes(row.engine) ? [row.engine, ...offered] : offered).flatMap(
      (rung) => {
        const entry = {
          value: rung,
          label: engineModelLabel(rung,
            rung === 'cloud'
              ? currentCloudModelId(cloud) ?? (!cloudUsable() && row.engine === 'cloud' ? row.modelId : null)
              : rung === 'flux' ? session.fluxModel || (row.engine === 'flux' ? row.modelId : null) || 'flux2-klein-4b' : null,
            t(engineChoiceLabel(rung))),
        }
        // Every cloud profile's model while Cloud is offered: picking another
        // one makes its profile the default before the re-run.
        const profiles = rung === 'cloud' && offered.includes('cloud') ? cloudEntries() : null
        return profiles
          ? profiles.map(({ value, choice }) => ({ value, label: choice ? cloudProfileLabel(choice.modelId, choice.name) : entry.label }))
          : [entry]
      },
    ),
  )

  // The entry last picked, while its re-run is under way. The picker shows it
  // until that re-run has answered, then the engine the mask has: the new
  // one, or the old one again when nothing changed - a cloud consent
  // cancelled, a re-run refused. Without this a native select goes on
  // showing the pick, since the value it is given never moved.
  /** @type {string|null} */
  let picking = $state(null)
  let pickSeq = 0

  // Pointer and focus are two ways into the same highlight, and either can
  // outlast the other: tabbing into an expanded row's buttons with the pointer
  // still over it used to clear the highlight, and so did moving the pointer
  // away from a row whose action button was focused. The highlight goes only
  // when neither is left.
  let pointerIn = false
  let focusIn = false

  /**
   * @param {'pointer'|'focus'} source
   * @param {boolean} inside
   */
  function track(source, inside) {
    if (source === 'pointer') pointerIn = inside
    else focusIn = inside
    hover(pointerIn || focusIn ? row.id : null)
  }

  /** @param {string} next */
  async function onEngineChange(next) {
    if (!next || next === (picking ?? row.engine)) return
    const mine = ++pickSeq
    picking = next
    try {
      // Another cloud profile's model: switch the default, then re-run on Cloud.
      if (next.startsWith('cloud@')) {
        if (await pickCloudChoice(next) && mine === pickSeq) await rerunMask(region, 'engine', 'cloud')
        return
      }
      await rerunMask(region, 'engine', next)
    } finally {
      // A later pick owns the picker until its own answer.
      if (mine === pickSeq) picking = null
    }
  }

  /**
   * The open context menu: `{x, y, region}` in client pixels, or null.
   *
   * @type {{x: number, y: number, region: import('../api/backend.js').ApiRegion} | null}
   */
  let menu = $state(null)

  /**
   * The row's own three controls, again, on the secondary press - the same
   * menu the canvas raises over the same region, so a user who found the
   * gesture on the page finds it on the row and the other way round. The
   * engine picker is behind a disclosure; this is the route to it that does
   * not ask for the row to be expanded first.
   *
   * `Shift`+`F10` and the context-menu key on the row's summary button raise
   * this event too, with no coordinates; `menuPoint` anchors those to the row.
   *
   * @param {MouseEvent} event
   */
  function oncontextmenu(event) {
    if (!region) return
    event.preventDefault()
    select(row.id)
    const rowEl = /** @type {HTMLElement} */ (event.currentTarget)
    menu = { ...menuPoint(event, rowEl.getBoundingClientRect()), region }
  }
</script>

<!-- Pointer-down rather than click: the row is a container, and the selection
     has to follow a press anywhere in it - the summary, the picker, either
     icon - not only the one button that happens to carry the toggle. -->
<div
  class="row"
  class:highlighted
  role="listitem"
  data-mask-row={row.id}
  onpointerdown={() => select(row.id)}
  onpointerenter={() => track('pointer', true)}
  onpointerleave={() => track('pointer', false)}
  onfocusin={() => track('focus', true)}
  onfocusout={() => track('focus', false)}
  {oncontextmenu}
>
  <Disclosure {open} {ontoggle} variant="plain">
    {#snippet summary()}
      <span class="line">
        <span class="dot {row.status}" aria-hidden="true">{row.glyph}</span>
        <span class="text">
          <span class="title">{title}</span>
          <span class="sub">{sub}</span>
        </span>
        <span class="sr">{t(row.statusKey)}</span>
      </span>
    {/snippet}

    <!-- Two icons on the collapsed line and no more. The panel is a narrow
         column, and a third control here took its width out of the layer's own
         name - which is the one thing on the line the reader is scanning for.
         The engine picker is a decision, not a reflex, so it goes below with
         the facts that inform it. -->
    {#snippet trailing()}
      {#if detected}
        <!-- Clean is the one thing a detection is waiting for, so it is on
             the collapsed line where Try again is on a layer's. -->
        <IconButton
          icon="sparkle"
          label={t('masks.action.cleanDetected')}
          title={t('masks.hint.cleanDetected')}
          size={21}
          iconSize={13}
          onclick={() => cleanDetected(region)}
        />
      {:else if row.reRunnable}
        <IconButton
          icon="refresh"
          label={t('masks.action.retry')}
          title={t('masks.action.retryHint')}
          size={21}
          iconSize={13}
          onclick={() => rerunMask(region, 'retry')}
        />
        {#if retryWidens(region.mask)}
          <!-- Plain Try again repeats the last result exactly; this one grows
               the area the model redraws on each press. -->
          <IconButton
            icon="mask-add"
            label={t('masks.action.retryWider')}
            title={t('masks.action.retryWiderHint')}
            size={21}
            iconSize={13}
            onclick={() => rerunMask(region, 'retryWider')}
          />
        {/if}
      {:else if retryBlocked}
        <!-- aria-disabled rather than disabled: a disabled button leaves the
             tab order and WebKit shows no tooltip over it, and then nothing
             says why Try again is off. -->
        <IconButton
          icon="refresh"
          label={t('masks.action.retry')}
          title={t('masks.action.retryBlocked')}
          aria-disabled="true"
          data-retry-blocked
          size={21}
          iconSize={13}
        />
      {/if}
      <IconButton
        icon="trash"
        label={t(row.engine ? 'masks.action.delete' : 'masks.action.deleteRegion')}
        title={t(row.engine ? 'masks.action.deleteHint' : 'masks.action.deleteRegionHint')}
        size={21}
        iconSize={13}
        onclick={() => deleteRow(region)}
      />
    {/snippet}

    <MaskFacts facts={row.facts} />

    {#if detected}
      <div class="mask-padding" data-detection-padding aria-busy={savingPadding}>
        <label>{t('tools.param.maskPadding')} <output>{maskPadding} px</output>
          <input type="range" min="0" max={MAX_MASK_PADDING} step="1" value={maskPadding}
            aria-label={t('tools.param.maskPadding')}
            aria-valuetext={`${maskPadding} px`}
            disabled={savingPadding || editor.run.active}
            oninput={(event) => (paddingDraft = Number(event.currentTarget.value))}
            onchange={changePadding} />
        </label>
      </div>
    {/if}

    {#if region.mask && (layerCaps.opacity || layerCaps.lock)}
      <div class="layer-style" data-layer-style={layerCaps.transform}>
        {#if layerCaps.opacity}
          <label>{t('masks.action.opacity')} <output>{opacityDraft ?? layer.opacity}%</output>
            <input type="range" min="0" max="100" step="1" value={opacityDraft ?? layer.opacity}
              aria-valuetext={t('masks.layer.opacityValue', { percent: opacityDraft ?? layer.opacity })}
              oninput={onOpacityInput}
              onchange={() => settleOpacity(false)}
              onpointerup={() => settleOpacity(true)}
              onpointercancel={() => settleOpacity(true)}
              onblur={() => settleOpacity(true)} />
          </label>
        {/if}
        {#if layerCaps.lock}
          <label class="lock"><input type="checkbox" checked={layer.locked}
            onchange={(event) => updateLayer(region, { locked: event.currentTarget.checked }, 'masks.command.layerLock')} />
            {t('masks.action.locked')}
          </label>
        {/if}
        {#if movable}
          <p class="note">{t(layer.locked ? 'masks.layer.lockedNote' : 'masks.layer.moveHint')}</p>
          {#if transformed && !layer.locked}
            <div class="acts">
              <Button size="sm" title={t('masks.layer.resetPositionHint')}
                onclick={() => updateLayer(region, { offsetX: 0, offsetY: 0, rotation: 0 }, 'masks.command.layerMove')}>
                {t('masks.layer.resetPosition')}
              </Button>
            </div>
          {/if}
        {:else if layerCaps.transform === 'fixed'}
          <p class="note">{t('masks.layer.fixedNote')}</p>
        {/if}
      </div>
    {/if}

    {#if region.mask?.dependencyReview}
      <!-- The facts above end with why ("Flagged"); these are the three
           answers to it. Rebuild is Try again, so it says what Try again says. -->
      <div class="acts" role="group" aria-label={t('masks.status.needsReview')} data-dependency-review>
        <Button size="sm" title={t('masks.dependency.keepHint')} onclick={() => keepDependencyResult(region)}>
          {t('masks.dependency.keep')}
        </Button>
        {#if row.reRunnable}
          <Button size="sm" title={t('masks.action.retryHint')} onclick={() => rerunMask(region, 'retry')}>
            {t('masks.dependency.rebuild')}
          </Button>
        {/if}
        {#if undoAvailable()}
          <Button size="sm" title={undoHint} onclick={() => undo()}>{t('masks.dependency.undoLast')}</Button>
        {/if}
      </div>
    {/if}

    {#if row.reRunnable}
      <!-- The kit's picker at the row's own size, not a `<select>` of this
           file's own: the tool window asks the same question with the same
           control, and two appearances of it was not worth keeping. -->
      <div class="pick">
        <label class="pick-label" for={pickerId}>{t('masks.action.engine')}</label>
        <div class="pick-control">
          <Select
            id={pickerId}
            size="row"
            options={engineOptions}
            value={picking ?? row.engine}
            title={t('masks.action.engineHint')}
            onchange={onEngineChange}
          />
        </div>
      </div>
      <p class="note">{t('masks.action.rerunNote')}</p>
    {:else if retryBlocked}
      <p class="note">{t('masks.action.retryBlocked')}</p>
    {/if}

    {#if actions.length > 0}
      <div class="acts" data-acts>
        {#each actions as action (action.id)}
          {@const hint = action.hintKey ? { key: action.hintKey } : actionHint(action.id)}
          <Button
            size="sm"
            title={t(hint.key, hint.params)}
            onclick={() => runMaskAction(action.id, region)}
          >
            {t(action.labelKey)}
          </Button>
        {/each}
      </div>
    {/if}
  </Disclosure>

  <RegionMenu at={menu} onclose={() => (menu = null)} />
</div>

<style>
  .row {
    border-radius: var(--r-chip);
    padding: 2px var(--s-2);
    margin-bottom: 2px;
    transition: background var(--dur-fast) var(--ease);
  }
  /* Hover *or* selection: one rule, so the canvas lighting a region and the
     pointer lighting a row look the same. */
  .highlighted { background: var(--panel2) }

  .line {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    min-height: 26px;
  }

  .dot {
    flex: none;
    width: 14px;
    text-align: center;
    font-size: 11px;
    color: var(--t3);
  }
  .dot.review { color: var(--t2) }
  .dot.declined { color: var(--warn) }
  .dot.detected { color: var(--accent) }

  .text { flex: 1; min-width: 0 }

  .title,
  .sub {
    display: block;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .title { font-size: 11.5px }
  .sub { font-size: 10.5px; color: var(--t3) }

  /* The picker sits under the facts, in the same 70px key column they use, so
     "Clean with" reads as one more line of the same table - which is what it
     is: the only line of it the reader can change. */
  .pick {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    margin-top: 7px;
    font-size: 10.5px;
  }

  .layer-style, .mask-padding { display: grid; gap: 7px; margin-top: 8px; font-size: 10.5px; color: var(--t3) }
  .layer-style label, .mask-padding label { display: grid; gap: 3px }
  .layer-style output, .mask-padding output { color: var(--t2) }
  .layer-style input[type='range'], .mask-padding input[type='range'] { width: 100% }
  .layer-style .lock { display: flex; align-items: center; gap: 6px }
  .layer-style .note,
  .layer-style .acts { margin-top: 0 }

  .pick-label {
    flex: none;
    width: 70px;
    color: var(--t3);
  }

  /* The picker draws its own metrics; this is only the cell it fills. */
  .pick-control {
    flex: 1;
    min-width: 0;
  }

  /* What Try again means next to a new stroke, or why it is off. Muted and
     small: it is a reminder under the controls, not another fact. */
  .note {
    margin: 6px 0 0;
    font-size: 10.5px;
    line-height: 1.45;
    color: var(--t3);
  }

  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    margin-top: 9px;
  }

  /* The kit's IconButton dims only a native `disabled`; this one stays
     focusable so its reason can be read. The glyph is dimmed rather than the
     button, so the focus ring keeps its full contrast. */
  .row :global(.ibtn[aria-disabled='true']) { cursor: default }
  .row :global(.ibtn[aria-disabled='true'] > *) { opacity: .35 }
  .row :global(.ibtn[aria-disabled='true']:hover) {
    background: transparent;
    color: var(--t2);
  }

  /* Visually hidden, still read aloud: the glyph's meaning in words. */
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    margin: -1px;
    padding: 0;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
