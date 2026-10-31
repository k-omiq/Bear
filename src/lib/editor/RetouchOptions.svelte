<script>
  import { editor, setToolParam } from '../state/editor.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { Button, ColorPicker, IconButton, Popover, RangeField } from '../ui/index.js'
  import { t } from '../i18n/index.js'
  import { isGradientLayer } from '../model/layers.js'
  import { draft } from './draft.svelte.js'
  import { DEFAULT_STOPS, addStop, colorBetween, opacityBetween, presetStops } from './gradient.js'
  import { applicable, applySelection, armEyedrop, matchSurroundings, reopenGradient, retouch } from './retouch.svelte.js'

  /**
   * The Patch tool's and the Gradient tool's controls beyond their rows: what
   * makes a held selection an edit (`retouch.svelte.js`).
   *
   * Both end in **Apply**, which `Enter` on the page also does, and a line of
   * help that says what the page is waiting for. On the Patch tool's pill
   * that is all, after a hairline. The Gradient is a panel (`tools.js`), and
   * this draws its body: the tool shell's parameter rows (`params`), then,
   * with no dialog (the plan's "easier"), its stops behind a swatch of the
   * gradient itself, with the two presets and an eyedropper per colour stop;
   * Reverse, Dither and As screentone as toggles; Match surroundings; and
   * **Edit gradient** while a gradient layer is selected, which holds that
   * layer's gradient again (an Auto layer's too, and under the Auto type). Apply and the help sit under them, where the
   * Text cleanup panel has its run button. The **Auto** type has none of
   * these: a selection fills as soon as it is drawn, from all round it.
   *
   * `rows` and `action` are the shell's own class names for the two parts:
   * it reads `.rows` to decide when they must scroll (`ToolBar.svelte`).
   *
   * @type {{
   *   tool: 'patch'|'gradient',
   *   panel?: boolean,
   *   params?: import('svelte').Snippet,
   * }}
   */
  let { tool, panel = false, params: paramRows } = $props()

  const params = $derived(editor.toolParams[tool] ?? {})
  const held = $derived(draft.selection?.tool === tool ? draft.selection : null)
  const ready = $derived(applicable(held, params))
  const colors = $derived(params.colors?.length ? params.colors : DEFAULT_STOPS.colors)
  const opacities = $derived(params.opacities?.length ? params.opacities : DEFAULT_STOPS.opacities)
  const auto = $derived(tool === 'gradient' && params.kind === 'auto')
  const selectedGradient = $derived(tool === 'gradient' && !held
    ? (editor.chapter?.pages ?? []).flatMap((page) => page.regions ?? [])
      .find((region) => region.id === editor.selectionId && isGradientLayer(region)) ?? null
    : null)

  /** The help line: what the page is waiting for now. */
  const help = $derived.by(() => {
    if (tool === 'patch') {
      if (!held) return 'retouch.help.patchDraw'
      if (params.mode !== 'normal') return 'retouch.help.patchApply'
      return held.proposing ? 'retouch.help.patchFinding' : 'retouch.help.patchGhost'
    }
    if (auto) return 'retouch.help.gradientAuto'
    if (retouch.eyedrop !== null) return 'retouch.help.eyedrop'
    if (!held) return 'retouch.help.gradientDraw'
    return held.start && held.end ? 'retouch.help.gradientHandles' : 'retouch.help.gradientLine'
  })

  /** A CSS picture of the stops, for the swatch that opens them. */
  const swatch = $derived(`linear-gradient(to right, ${[...colors]
    .sort((a, b) => a.at - b.at)
    .map((stop) => `${stop.color} ${Math.round(stop.at * 100)}%`)
    .join(', ')})`)

  /** @param {string} key @param {unknown} value */
  function set(key, value) {
    setToolParam(tool, key, value)
  }

  /** @param {number} index @param {Record<string, unknown>} change */
  function editColor(index, change) {
    set('colors', colors.map((stop, at) => (at === index ? { ...stop, ...change } : stop)))
  }

  /** @param {number} index @param {Record<string, unknown>} change */
  function editOpacity(index, change) {
    set('opacities', opacities.map((stop, at) => (at === index ? { ...stop, ...change } : stop)))
  }

  /** @param {'foregroundToBackground'|'foregroundToTransparent'} preset */
  function usePreset(preset) {
    const stops = presetStops(preset, session.colorPair.active, session.colorPair.other)
    set('colors', stops.colors)
    set('opacities', stops.opacities)
  }
</script>

{#snippet toggle(key, labelKey, fallback = false)}
  {@const on = params[key] === undefined ? fallback : params[key] === true}
  <Button size="sm" variant={on ? 'soft' : 'ghost'} aria-pressed={on} onclick={() => set(key, !on)}>
    {t(labelKey)}
  </Button>
{/snippet}

{#snippet edit()}
  {#if selectedGradient}
    <Button size="sm" variant="soft" onclick={() => reopenGradient(selectedGradient)}>
      {t('retouch.gradient.edit')}
    </Button>
  {/if}
{/snippet}

{#snippet apply()}
  <Button size="sm" variant="primary" disabled={!ready} onclick={() => void applySelection()}>
    {t('retouch.apply')}
  </Button>
{/snippet}

{#snippet stopsEditor()}
  <div class="stops">
    <div class="presets">
      <Button size="sm" variant="soft" onclick={() => usePreset('foregroundToBackground')}>{t('retouch.gradient.toBackground')}</Button>
      <Button size="sm" variant="soft" onclick={() => usePreset('foregroundToTransparent')}>{t('retouch.gradient.toTransparent')}</Button>
    </div>
    <p class="heading">{t('retouch.gradient.colorStops')}</p>
    {#each colors as stop, index (index)}
      <div class="stop">
        <ColorPicker unclipped value={stop.color} label={t('retouch.gradient.stopColor', { n: index + 1 })} onchange={(hex) => editColor(index, { color: hex })} />
        <IconButton
          icon="eyedropper"
          size={24}
          iconSize={13}
          label={t('retouch.gradient.eyedrop', { n: index + 1 })}
          active={retouch.eyedrop === index}
          pressed={retouch.eyedrop === index}
          onclick={() => armEyedrop(index)}
        />
        <RangeField layout="row" label={t('retouch.gradient.at')} value={Math.round(stop.at * 100)} min={0} max={100} step={1} unit="%"
          boxLabel={t('tools.param.exactValue', { name: t('retouch.gradient.at') })}
          onchange={(value) => editColor(index, { at: value / 100 })} />
        <IconButton icon="trash" size={24} iconSize={13} label={t('retouch.gradient.removeStop')} disabled={colors.length <= 1}
          onclick={() => set('colors', colors.filter((_, at) => at !== index))} />
      </div>
    {/each}
    <Button size="sm" variant="ghost" onclick={() => set('colors', addStop(colors, (at) => ({ at, color: colorBetween(colors, at) })))}>
      {t('retouch.gradient.addColor')}
    </Button>
    <p class="heading">{t('retouch.gradient.opacityStops')}</p>
    {#each opacities as stop, index (index)}
      <div class="stop">
        <RangeField layout="row" label={t('tools.param.opacity')} value={Number(stop.opacity)} min={0} max={100} step={5} unit="%"
          boxLabel={t('tools.param.exactValue', { name: t('tools.param.opacity') })}
          onchange={(value) => editOpacity(index, { opacity: value })} />
        <RangeField layout="row" label={t('retouch.gradient.at')} value={Math.round(stop.at * 100)} min={0} max={100} step={1} unit="%"
          boxLabel={t('tools.param.exactValue', { name: t('retouch.gradient.at') })}
          onchange={(value) => editOpacity(index, { at: value / 100 })} />
        <IconButton icon="trash" size={24} iconSize={13} label={t('retouch.gradient.removeStop')} disabled={opacities.length <= 1}
          onclick={() => set('opacities', opacities.filter((_, at) => at !== index))} />
      </div>
    {/each}
    <Button size="sm" variant="ghost" onclick={() => set('opacities', addStop(opacities, (at) => ({ at, opacity: opacityBetween(opacities, at) })))}>
      {t('retouch.gradient.addOpacity')}
    </Button>
  </div>
{/snippet}

{#if panel}
  <div class="rows">
    {@render paramRows?.()}
    {#if tool === 'gradient' && !auto}
      <div class="field">
        <span class="label">{t('retouch.gradient.stops')}</span>
        <Popover label={t('retouch.gradient.stops')} align="start" width="420px">
          {#snippet trigger({ toggle: open, triggerProps })}
            <button type="button" class="swatch" style:background={swatch} aria-label={t('retouch.gradient.stops')} onclick={open} {...triggerProps}></button>
          {/snippet}
          {@render stopsEditor()}
        </Popover>
      </div>
      <div class="choices">
        {@render toggle('reverse', 'retouch.gradient.reverse')}
        {@render toggle('dither', 'retouch.gradient.dither', true)}
        {@render toggle('asScreentone', 'retouch.gradient.asScreentone')}
      </div>
      <div class="choices">
        <Button size="sm" variant="soft" disabled={!held} onclick={() => void matchSurroundings()}>
          {t('retouch.gradient.match')}
        </Button>
        {@render edit()}
      </div>
    {:else if selectedGradient}
      <div class="choices">{@render edit()}</div>
    {/if}
  </div>
  <div class="action">
    <span class="help" aria-live="polite">{t(help)}</span>
    {@render apply()}
  </div>
{:else}
  <span class="rule" role="separator" aria-orientation="vertical"></span>
  {@render apply()}
  <span class="help" aria-live="polite">{t(help)}</span>
{/if}

<style>
  /* The bar's own hairline (`ToolBar.svelte .rule`). */
  .rule {
    flex: none;
    width: 1px;
    height: 22px;
    background: var(--line);
  }

  /* The Gradient panel's body, on the label column its sliders share, and
     its action under a hairline: the Text cleanup panel's two parts. */
  .rows {
    display: grid;
    gap: var(--s-3);
    padding: var(--s-4) var(--s-5);
    --field-label: 96px;
    --field-readout: 40px;
  }

  .field {
    display: grid;
    grid-template-columns: var(--field-label) minmax(0, 1fr);
    align-items: center;
    column-gap: var(--s-3);
    min-height: 28px;
  }

  .label {
    font-size: 10.5px;
    line-height: 1.3;
    color: var(--t2);
  }

  /* Toggles and actions with no label of their own: in the control column. */
  .choices {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-1);
    padding-left: calc(var(--field-label) + var(--s-3));
  }

  .action {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-3) var(--s-5) var(--s-4);
    border-top: 1px solid var(--line);
  }

  .action .help {
    flex: 1;
    max-width: none;
  }

  .help {
    max-width: 185px;
    white-space: normal;
    font-size: 10px;
    line-height: 1.3;
    color: var(--t2);
  }

  .swatch {
    width: 44px;
    height: 20px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 5px;
    cursor: pointer;
  }

  /* The panel's swatch: the whole control column, so the gradient reads. */
  .field .swatch {
    flex: 1;
    width: auto;
    height: 24px;
  }

  .swatch:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .stops {
    display: grid;
    gap: var(--s-2);
    --field-label: 52px;
    --field-readout: 36px;
  }

  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-1);
  }

  .heading {
    margin: var(--s-1) 0 0;
    font-size: 10px;
    color: var(--t2);
  }

  .stop {
    display: grid;
    grid-template-columns: auto auto 1fr auto;
    align-items: center;
    gap: var(--s-1);
  }

  .stop:has(> :global(:nth-child(3):last-child)) {
    grid-template-columns: 1fr 1fr auto;
  }
</style>
