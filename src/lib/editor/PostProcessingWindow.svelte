<script>
  /**
   * The Post-processing window: the selected layer's post-processing, picked
   * again from previews. The shell is `FloatingWindow`, so it moves, sizes,
   * folds and closes like Pages and Layers, and keeps where the user left it;
   * the body is `FinishingPanel`.
   *
   * It follows the selection: whatever layer is selected on the page or in
   * Layers is the one shown, and with none selected it says how to pick one.
   * Three ways open it, all on the same window: the shortcut
   * (`layers.finishingSelected`, `⌘P` unless rebound), the region menu's
   * Post-processing…, and the Post-processing button on a Layers row. The
   * last two select their layer first (`maskactions.svelte.js#openFinishingWindow`).
   *
   * The header names the layer and the method its pixels were made by. The
   * footer sends the reader to Settings > Post-processing, where the method
   * a new clean starts with is chosen: a pick here changes this layer only.
   */
  import { editor, selectedRegion } from '../state/editor.svelte.js'
  import { engineModelLabel, finishingKey } from '../model/model-names.js'
  import { offersFinishing, recordedFinishing } from './maskactions.svelte.js'
  import { maskRow } from './maskrows.js'
  import { openSettings } from '../dialogs/settingslinks.js'
  import { Button } from '../ui/index.js'
  import { hasKey, t } from '../i18n/index.js'
  import FloatingWindow from './FloatingWindow.svelte'
  import FinishingPanel from './FinishingPanel.svelte'

  const region = $derived(editor.chapter ? selectedRegion() : null)

  /** The layer as its Layers row names it. */
  const layerName = $derived.by(() => {
    if (!region) return ''
    const row = maskRow(region, false)
    return engineModelLabel(row.engine, row.modelId, t(row.titleKey))
  })

  /** The page the layer is on, counted from one. */
  const pageNumber = $derived.by(() => {
    if (!region) return null
    const page = editor.chapter?.pages.find((entry) => entry.regions.some((candidate) => candidate.id === region.id))
    return page ? page.index + 1 : null
  })

  /** The method the layer's pixels were made by, as Settings names it. */
  const method = $derived.by(() => {
    const name = offersFinishing(region) ? recordedFinishing(region)?.finishing : null
    const key = name ? finishingKey('option', name) : null
    return key && hasKey(key) ? t(key) : null
  })
</script>

<FloatingWindow id="postprocessing" title={t('editor.panel.postprocessing')} meta={method ?? ''} pad="9px 10px 12px">
  {#if region}
    <div class="layer" data-finishing-layer>
      <span class="layer-name">{layerName}</span>
      {#if pageNumber !== null}<span class="layer-page">{t('masks.finishing.window.page', { page: pageNumber })}</span>{/if}
    </div>
  {/if}
  <FinishingPanel {region} />

  {#snippet footer()}
    <div class="foot">
      <span class="foot-note">{t('masks.finishing.window.defaultsNote')}</span>
      <Button
        size="sm"
        variant="ghost"
        title={t('masks.finishing.window.defaultsHint')}
        data-finishing-settings
        onclick={() => openSettings('postprocessing')}
      >
        {t('masks.finishing.window.defaults')}
      </Button>
    </div>
  {/snippet}
</FloatingWindow>

<style>
  .layer {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
    min-width: 0;
    margin-bottom: var(--s-3);
  }

  .layer-name {
    overflow: hidden;
    color: var(--text);
    font-size: 12.5px;
    font-weight: 500;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .layer-page {
    flex: none;
    color: var(--t3);
    font-size: 11px;
  }

  .foot {
    display: flex;
    align-items: center;
    flex: none;
    gap: var(--s-2);
    /* Clear of the window's resize corner, which sits over this line. */
    padding: 5px 20px 6px 10px;
    border-top: 1px solid var(--line);
  }

  .foot-note {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    color: var(--t3);
    font-size: 10.5px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
