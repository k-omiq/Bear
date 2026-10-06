<script>
  /**
   * The local denoise model: whether it is here, and the download that puts
   * it here.
   *
   * The same download as Settings > Models (`downloadModel`,
   * `cancelDownload`, progress on `model-progress`), for the catalogue rows
   * required by `pageDenoise`. The rows are read on mount and again when a
   * transfer ends, so a download started in another window is found too.
   *
   * `installed` is bound out, so the caller can let Measure and Run wait for it.
   *
   * Settings passes its own catalogue (`view`) and refresh, so the screen
   * asks `listModels` once however many panels read it; the chapter dialog
   * passes neither, and the row reads the catalogue itself.
   *
   * @type {{
   *   installed?: boolean,
   *   view?: import('../../api/backend.js').ModelsView|null,
   *   refresh?: () => Promise<unknown>,
   * }}
   */
  import { onMount } from 'svelte'
  import { Button } from '../../ui/index.js'
  import { hasKey, t } from '../../i18n/index.js'
  import { getBackend } from '../../api/backend.js'
  import { loadCapabilities } from '../../state/capabilities.svelte.js'
  import { denoiseRows } from '../../model/denoise.js'

  let { installed = $bindable(false), view = undefined, refresh: refreshOwner = undefined } = $props()

  /** @type {Array<{id: string, kindKey: string, fileName: string, bytes: number, installed: boolean, downloading?: boolean}>|null} */
  let own = $state(null)
  const owned = $derived(view === undefined)
  const rows = $derived(owned ? own : view ? denoiseRows(view.models) : null)
  /** @type {Record<string, {downloaded: number, total: number|null}>} */
  let progress = $state({})
  /** @type {Record<string, string>} */
  let failures = $state({})

  async function refresh() {
    if (!owned) {
      await refreshOwner?.()
      return
    }
    try {
      const answer = await getBackend().listModels()
      own = denoiseRows(answer?.models)
    } catch {
      own = []
    }
  }

  $effect(() => {
    installed = Boolean(rows?.length) && /** @type {any[]} */ (rows).every((row) => row.installed)
  })

  onMount(() => {
    if (owned) refresh()
    return getBackend().subscribe((event) => {
      if (event.type !== 'model-progress' || !rows?.some((row) => row.id === event.id)) return
      if (event.done) {
        const { [event.id]: _gone, ...rest } = progress
        progress = rest
        failures = event.error && event.error !== 'cancelled'
          ? { ...failures, [event.id]: String(event.error) }
          : Object.fromEntries(Object.entries(failures).filter(([id]) => id !== event.id))
        // Settings refreshes its own catalogue and capabilities on every
        // ending; only a row reading for itself has to.
        if (owned) {
          refresh()
          loadCapabilities()
        }
        return
      }
      progress = { ...progress, [event.id]: { downloaded: event.downloaded, total: event.total } }
    })
  })

  /** @param {string} id */
  async function download(id) {
    failures = Object.fromEntries(Object.entries(failures).filter(([key]) => key !== id))
    try {
      await getBackend().downloadModel({ id })
    } catch (error) {
      failures = { ...failures, [id]: String(error) }
    }
    await refresh()
  }

  /** @param {string} id */
  async function cancel(id) {
    try {
      await getBackend().cancelDownload({ id })
    } finally {
      await refresh()
    }
  }

  /** @param {{kindKey: string, fileName: string}} row */
  function nameOf(row) {
    return row.kindKey && hasKey(row.kindKey) ? t(row.kindKey) : row.fileName
  }

  /** @param {{id: string, installed: boolean, bytes: number}} row */
  function statusOf(row) {
    const live = progress[row.id]
    if (live) {
      return live.total
        ? t('settings.models.status.downloadingPercent', { percent: Math.floor((live.downloaded / live.total) * 100) })
        : t('settings.models.status.downloading')
    }
    if (failures[row.id]) return t('settings.models.status.failed')
    if (row.installed) return t('settings.models.status.installed')
    return t('denoise.model.notInstalled', { bytes: row.bytes })
  }

  /** @param {string} id */
  function percentOf(id) {
    const live = progress[id]
    return live?.total ? Math.min(100, Math.floor((live.downloaded / live.total) * 100)) : 0
  }
</script>

{#if rows === null}
  <p class="line quiet">{t('settings.models.status.checking')}</p>
{:else if rows.length === 0}
  <p class="line quiet" data-denoise-model="none">{t('denoise.model.unavailable')}</p>
{:else}
  <ul class="rows">
    {#each rows as row (row.id)}
      {@const busy = Boolean(progress[row.id] || row.downloading)}
      <li class="row" data-denoise-model={row.id} data-installed={row.installed}>
        <span class="text">
          <span class="name">{nameOf(row)}</span>
          <span class="status" class:failed={Boolean(failures[row.id])}>{statusOf(row)}</span>
          {#if busy}
            <span class="bar" aria-hidden="true"><span style:transform="scaleX({percentOf(row.id) / 100})"></span></span>
          {/if}
        </span>
        {#if busy}
          <Button size="sm" onclick={() => cancel(row.id)}>{t('settings.models.action.cancel')}</Button>
        {:else if !row.installed}
          <Button size="sm" onclick={() => download(row.id)}>{t('settings.models.action.download')}</Button>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .rows { display: grid; gap: var(--s-3); margin: 0; padding: 0; list-style: none }
  .row { display: flex; align-items: center; justify-content: space-between; gap: var(--s-5) }
  .text { display: flex; flex-direction: column; gap: 3px; min-width: 0; flex: 1 }
  .name { font-size: 12.5px; font-weight: 600; color: var(--text) }
  .status { font-size: 11.5px; color: var(--t2); font-variant-numeric: tabular-nums }
  .status.failed { color: var(--warn) }
  .bar { display: block; height: 3px; max-width: 240px; border-radius: var(--r-pill); background: var(--line2); overflow: hidden }
  .bar > span { display: block; width: 100%; height: 100%; background: var(--accent); transform-origin: left; transition: transform var(--dur-fast) var(--ease) }
  .line { margin: 0; font-size: 12px; line-height: 1.5 }
  .quiet { color: var(--t2) }
</style>
