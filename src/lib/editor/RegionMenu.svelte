<script>
  import { regionMenuSections } from '../model/masks.js'
  import { capabilities } from '../state/capabilities.svelte.js'
  import { cloud, cloudEntries, cloudUsable, pickCloudChoice } from '../state/cloud.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { cloudProfileLabel, currentCloudModelId, engineModelLabel } from '../model/model-names.js'
  import { runRegionMenuItem } from './maskactions.svelte.js'
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

  const sections = $derived(
    at
      ? regionMenuSections(at.region, { engines: capabilities.engines, cloud: cloudUsable() }).map((section) => ({
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

  /** @param {string} id */
  function pick(id) {
    // Read the region off the open menu before the host clears it.
    const region = at?.region
    if (!region) return
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
    {onclose}
  />
{/if}
