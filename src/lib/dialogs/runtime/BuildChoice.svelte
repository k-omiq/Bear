<script>
  /**
   * Which runtime build to download, as one radio group: name, size, and
   * what it needs installed by hand. The build the machine check found this
   * computer ready for is marked, and the platform's default says it is one.
   *
   * Setup draws this before anything is downloaded, which is the point: a
   * build is one archive, the process loads exactly one, and choosing after
   * the download costs a second one.
   *
   * @type {{
   *   flavours: Array<{id: string, bytes: number, isDefault: boolean, userInstalled: string[]}>,
   *   value: string|null,
   *   recommended?: string|null,
   *   disabled?: boolean,
   *   onchoose: (id: string) => void,
   * }}
   */
  import { t } from '../../i18n/index.js'
  import { flavourName } from './runtime.js'

  let { flavours, value, recommended = null, disabled = false, onchoose } = $props()
  const uid = $props.id()

  /** @param {string} id */
  function about(id) {
    return {
      directml: 'settings.runtime.build.aboutDirectml',
      cuda12: 'settings.runtime.build.aboutCuda',
      cuda13: 'settings.runtime.build.aboutCuda',
    }[id] ?? 'settings.runtime.build.aboutStandard'
  }
</script>

<div class="builds" role="radiogroup" aria-label={t('settings.runtime.heading.build')}>
  {#each flavours as build (build.id)}
    <label class="build" class:chosen={build.id === value} for="{uid}-{build.id}">
      <input
        type="radio"
        id="{uid}-{build.id}"
        name="{uid}-build"
        value={build.id}
        checked={build.id === value}
        {disabled}
        onchange={() => onchoose(build.id)}
      />
      <span class="text">
        <span class="name">
          {flavourName(build.id)}
          {#if build.id === recommended}<span class="mark">{t('settings.runtime.build.recommended')}</span>
          {:else if build.isDefault}<span class="mark quiet">{t('settings.runtime.build.default')}</span>{/if}
        </span>
        <span class="about">{t(about(build.id))}</span>
        {#if build.userInstalled.length}
          <span class="needs" class:met={build.id === recommended}>{t('settings.models.runtime.flavourNeeds', { items: build.userInstalled.join(', ') })}</span>
        {/if}
      </span>
      <span class="size">{t('models.value.size', { bytes: build.bytes })}</span>
    </label>
  {/each}
</div>

<style>
  .builds { display: grid }
  .build {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: start;
    gap: var(--s-4);
    padding: var(--s-4) 0;
    border-top: 1px solid var(--line);
    cursor: pointer;
  }
  .build:first-child { border-top: none; padding-top: 0 }
  input { margin: 2px 0 0; accent-color: var(--accent) }
  .text { display: flex; flex-direction: column; gap: 2px; min-width: 0 }
  .name { display: flex; align-items: baseline; gap: var(--s-3); font-weight: 600 }
  .mark {
    padding: 1px var(--s-2);
    border-radius: var(--r-pill);
    background: var(--accent-soft);
    font-size: 10.5px;
    font-weight: 600;
  }
  .mark.quiet { background: none; padding: 0; color: var(--t3); font-weight: 400; font-size: 11px }
  .about { color: var(--t2); font-size: 11.5px; line-height: 1.45; max-width: 64ch }
  .needs { color: var(--warn); font-size: 11.5px }
  /* The machine check found what this build needs: still worth saying, no longer a warning. */
  .needs.met { color: var(--t3) }
  .size { color: var(--t2); font-size: 11.5px; font-variant-numeric: tabular-nums; text-align: right }
</style>
