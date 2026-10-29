<script>
  import { regionMenuSections } from '../model/masks.js'
  import { capabilities } from '../state/capabilities.svelte.js'
  import { cloud, cloudEntries, cloudUsable, pickCloudChoice } from '../state/cloud.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { cloudProfileLabel, currentCloudModelId, engineModelLabel } from '../model/model-names.js'
  import { offersFinishing, regionPageBusy, requestDetectedMaskPadding, runRegionMenuItem, shownMaskPadding } from './maskactions.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { shortcutHint } from '../shortcuts.js'
  import { ContextMenu } from '../ui/index.js'
  import { t } from '../i18n/index.js'

  /**
   * The region context menu - Try again, Clean with…, Delete - wherever a
   * region can be right-clicked: on the canvas (`RegionLayer`), on the canvas
   * under a drawing tool's surface (`DrawLayer`), and on a Layers row
   * (`MaskRow`).
   *
   * One component for the three of them, so a menu raised over the page and a
   * menu raised over the row it belongs to cannot end up offering different
   * things. What it offers is `model/masks.js#regionMenuSections`, which is
   * pure and tested; what an entry *does* is
   * `maskactions.svelte.js#runRegionMenuItem`, which is the row's own controls
   * and no second implementation of them.
   *
   * A detection's Mask padding is a slider in the menu, with a box to type it
   * and `;` / `'` to step it. The value it settles on is the Layers row's own
   * padding change (`requestDetectedMaskPadding`), one undo step, and the menu
   * stays open. It shows the region as it is now, not as it was when the menu
   * opened, so a change saved behind it is never shown stale.
   *
   * A cleaned layer a model made gets Post-processing…, which selects it and
   * opens the Post-processing window on it, with the chord that does the
   * same printed beside it as the user has it bound. The Screentone fill is
   * among the engines only while Settings > Beta offers it.
   *
   * Cloud is among the engines while a cloud endpoint is ready
   * (`state/cloud.svelte.js#cloudUsable`), and left out otherwise. Picking it,
   * or Try again on a cloud mask, asks for consent before anything is sent.
   *
   * Controlled: the host owns `at` - `{x, y, region}` in client pixels, or
   * `null` - and closes the menu by clearing it.
   *
   * @type {{
   *   at: {x: number, y: number, region: import('../api/backend.js').ApiRegion} | null,
   *   onclose: () => void,
   * }}
   */
  let { at, onclose } = $props()

  /** The menu's region as the editor holds it now, if it is still on a page. */
  const live = $derived(at
    ? editor.chapter?.pages.flatMap((page) => page.regions ?? []).find((region) => region.id === at.region.id) ?? null
    : null)

  const sections = $derived(
    at
      ? regionMenuSections(at.region, {
        engines: capabilities.engines,
        cloud: cloudUsable(),
        beta: session.betaTools,
        finishing: offersFinishing(at.region),
      }).map((section) => ({
          id: section.id,
          label: section.labelKey ? t(section.labelKey) : null,
          items: section.items.flatMap((item) => {
            const entry = {
              id: item.id,
              label: item.id.startsWith('engine:') || item.id.startsWith('approve:')
                ? engineModelLabel(item.id.split(':')[1],
                  item.id.endsWith(':cloud') ? currentCloudModelId(cloud) ??
                    (!cloudUsable() ? at?.region?.mask?.provenance?.cloud?.model : null) :
                    item.id.endsWith(':flux') ? session.fluxModel || 'flux2-klein-4b' : null,
                  t(item.labelKey))
                : t(item.labelKey),
              icon: item.icon,
              selected: item.selected,
              // Read through `session.shortcuts` so a rebinding redraws it.
              hint: item.shortcut && session.shortcuts ? shortcutHint([item.shortcut]) || undefined : undefined,
              ...(item.range ? {
                range: { ...item.range, value: live ? shownMaskPadding(live) : item.range.value, hint: shortcutHint(['mask.paddingDown', 'mask.paddingUp']),
                  boxLabel: t('tools.param.exactValue', { name: t(item.labelKey) }) },
                disabled: regionPageBusy(at.region),
              } : {}),
            }
            // Every cloud profile's model: picking another one makes its
            // profile the default before the clean (`pick`).
            const profiles = item.id === 'engine:cloud' ? cloudEntries() : null
            return profiles
              ? profiles.map(({ value, choice }) => ({
                ...entry,
                id: `engine:${value}`,
                label: choice ? cloudProfileLabel(choice.modelId, choice.name) : entry.label,
                selected: value === 'cloud' && entry.selected,
              }))
              : [entry]
          }),
        }))
      : [],
  )

  /**
   * The region the menu was last opened on. A slider's last value can arrive
   * as the menu closes (`RangeField` settles a pending run of steps when it
   * goes), after the host has already cleared `at`.
   *
   * @type {import('../api/backend.js').ApiRegion|null}
   */
  let opened = null
  $effect(() => {
    if (at) opened = at.region
  })

  /**
   * @param {string} id
   * @param {number} value
   */
  function range(id, value) {
    const region = live ?? at?.region ?? (opened
      ? editor.chapter?.pages.flatMap((page) => page.regions ?? []).find((entry) => entry.id === opened?.id) ?? opened
      : null)
    if (id === 'padding' && region) paddingSave = requestDetectedMaskPadding(region, value)
  }

  /** The menu's last padding save, which an entry picked after it waits for. */
  /** @type {Promise<void>|null} */
  let paddingSave = null

  /** @param {string} id */
  async function pick(id) {
    // Read the region off the open menu before the host clears it, as the
    // editor holds it now: the menu's own padding row may have moved it.
    let region = live ?? at?.region
    if (!region) return
    // A padding the row just set is saved before the entry runs on the mask.
    if (paddingSave) {
      const regionId = region.id
      await paddingSave.catch(() => {})
      region = editor.chapter?.pages.flatMap((page) => page.regions ?? []).find((entry) => entry.id === regionId) ?? region
    }
    if (id.startsWith('engine:cloud@')) {
      void pickCloudChoice(id.slice('engine:'.length)).then((switched) => {
        if (switched) runRegionMenuItem('engine:cloud', region)
      })
      return
    }
    runRegionMenuItem(id, region)
  }
</script>

{#if at}
  <ContextMenu
    x={at.x}
    y={at.y}
    label={t('masks.menu.label')}
    {sections}
    onselect={pick}
    onrange={range}
    {onclose}
  />
{/if}
