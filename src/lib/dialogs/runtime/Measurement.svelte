<script>
  /**
   * The Runtime screen's Measurement: time every installed model on every path
   * this machine can run it on, and let Automatic use the fastest.
   *
   * **Automatic follows it; a choice still wins.** A model set to a backend by
   * hand keeps that backend, and the row says so beside the timing, so a
   * fastest that is not in use is never a mystery.
   *
   * **A measurement is of one setup** - the runtime build, the graphics
   * devices, the NVIDIA driver (`measurement.rs`). Changed since, the result
   * stays on screen with a line saying it is no longer followed, because the
   * numbers still say something useful about the paths and throwing them away
   * would make the user measure to find out what they were.
   *
   * The timings are the median of at least three runs after a first run that
   * is shown apart: it pays for compiling the graph, and a run of a held
   * session never pays it again (`cleaner_core::measure`).
   *
   * @type {{
   *   accelerators: import('../../api/backend.js').Accelerators|null,
   *   ready: boolean,
   *   onmeasured?: () => (void|Promise<void>),
   * }}
   */
  import { onMount } from 'svelte'
  import { hasKey, t } from '../../i18n/index.js'
  import { Button } from '../../ui/index.js'
  import { getBackend } from '../../api/backend.js'
  import { backendName, duration, errorText, modelName } from './runtime.js'

  let { accelerators, ready, onmeasured } = $props()

  /** @type {import('../../api/backend.js').MeasurementView|null} */
  let view = $state(null)
  /** @type {{model: string, backend: string, step: number, total: number}|null} */
  let progress = $state(null)
  let running = $state(false)
  let stopping = $state(false)
  /** Whether the first read has answered: until then a run may already be going. */
  let read = $state(false)
  let error = $state('')
  /** A stop the user asked for, said plainly rather than as a failure. */
  let stopped = $state(false)
  /** Whether this mount's own `measure()` is waiting on the run, which then settles it. */
  let mine = false

  /** Read the saved result and whether a run is going, as the backend has them now. */
  async function refresh() {
    try {
      const answer = await getBackend().readMeasurement()
      view = answer
      running = answer.running
      if (!answer.running) stopping = false
    } catch {
      // The screen keeps what it had; Measure still works.
    } finally {
      read = true
    }
  }

  onMount(() => {
    refresh()
    return getBackend().subscribe((event) => {
      if (event.type !== 'measure-progress') return
      if (!event.done) {
        progress = { model: event.model, backend: event.backend, step: event.step, total: event.total }
        running = true
        return
      }
      progress = null
      // A run this mount did not start (Settings closed and opened again while
      // it ran) has no promise here to settle it: the done event is the sign.
      if (!mine) refresh().then(() => onmeasured?.())
    })
  })

  async function measure() {
    if (running || !read) return
    mine = true
    running = true
    stopping = false
    stopped = false
    error = ''
    try {
      view = await getBackend().measureBackends()
      await onmeasured?.()
    } catch (failure) {
      if (errorText(failure, () => false) === 'notice.measure.cancelled') stopped = true
      else error = errorText(failure, hasKey)
    } finally {
      mine = false
      running = false
      stopping = false
      progress = null
    }
  }

  async function stop() {
    stopping = true
    await getBackend().cancelMeasurement().catch(() => false)
  }

  async function forget() {
    error = ''
    stopped = false
    try {
      view = await getBackend().forgetMeasurement()
      await onmeasured?.()
    } catch (failure) {
      error = errorText(failure, hasKey)
    }
  }

  const rows = $derived(
    (view?.saved?.models ?? []).map((measured) => {
      const placed = accelerators?.models.find((model) => model.id === measured.id) ?? null
      const timed = measured.results.filter((result) => result.medianMs !== null)
      const slowest = Math.max(...timed.map((result) => /** @type {number} */ (result.medianMs)), 1)
      return {
        id: measured.id,
        name: placed ? modelName(placed) : measured.id,
        winner: measured.winner,
        placed,
        slowest,
        results: [...measured.results].sort((a, b) => (a.medianMs ?? Infinity) - (b.medianMs ?? Infinity)),
      }
    }),
  )

  const when = $derived(view?.saved ? new Date(view.saved.measuredAt).toLocaleString() : '')
  const progressModel = $derived.by(() => {
    if (!progress) return ''
    const placed = accelerators?.models.find((model) => model.id === progress?.model)
    return placed ? modelName(placed) : progress.model
  })

  /**
   * Where the model runs now, said only where it is not the measured
   * winner: a hand-picked backend, or an Automatic the setup change sent
   * back to the table.
   *
   * @param {typeof rows[number]} row
   */
  function inUse(row) {
    if (!row.placed || !row.winner || row.placed.acceleratorId === row.winner) return null
    const key = row.placed.preference === 'auto' ? 'settings.runtime.measure.autoUses' : 'settings.runtime.measure.chosenUses'
    return t(key, { backend: backendName(row.placed.acceleratorId) })
  }

  /** @param {string|null} reason */
  function failure(reason) {
    if (reason && hasKey(reason)) return t(reason)
    return t('settings.runtime.measure.failed')
  }
</script>

<div class="measure">
  <p class="lead">{t('settings.runtime.measure.body')}</p>

  <div class="bar">
    <!-- One button that turns into Stop and back, so a keyboard user's focus
         stays where it was pressed. -->
    <Button
      size="sm"
      variant={running || view?.saved ? 'ghost' : 'primary'}
      disabled={running ? stopping : !ready || !read}
      onclick={running ? stop : measure}
    >
      {running ? t('settings.runtime.measure.stop') : view?.saved ? t('settings.runtime.measure.again') : t('settings.runtime.measure.start')}
    </Button>
    {#if view?.saved && !running}
      <Button size="sm" variant="plain" onclick={forget}>{t('settings.runtime.measure.forget')}</Button>
    {/if}
    <span class="status" role="status" aria-live="polite">
      {#if running && progress}
        {t('settings.runtime.measure.progress', { model: progressModel, backend: backendName(progress.backend), step: progress.step, total: progress.total })}
      {:else if running}
        {t('settings.runtime.measure.preparing')}
      {:else if stopped}
        {t('notice.measure.cancelled')}
      {:else if !ready}
        {t('settings.runtime.measure.needsRuntime')}
      {:else if view?.saved}
        {t('settings.runtime.measure.when', { when })}
      {/if}
    </span>
  </div>

  {#if running && progress}
    <div class="track" aria-hidden="true"><span class="fill" style:transform="scaleX({progress.step / Math.max(progress.total, 1)})"></span></div>
  {/if}

  {#if error}<p class="alert" role="alert">{error}</p>{/if}

  {#if view?.saved && view.current === false}
    <p class="stale">{t('settings.runtime.measure.stale')}</p>
  {/if}

  {#if rows.length}
    <ul class="models" aria-label={t('settings.runtime.heading.measure')}>
      {#each rows as row (row.id)}
        <li class="model">
          <div class="model-head">
            <span class="model-name">{row.name}</span>
            {#if inUse(row)}<span class="in-use">{inUse(row)}</span>{/if}
          </div>
          <table class="results">
            <thead class="sr">
              <tr>
                <th scope="col">{t('settings.runtime.measure.col.backend')}</th>
                <th scope="col">{t('settings.runtime.measure.col.run')}</th>
                <th scope="col">{t('settings.runtime.measure.col.load')}</th>
              </tr>
            </thead>
            <tbody>
              {#each row.results as result (result.backend)}
                {@const fastest = result.backend === row.winner}
                <tr class:fastest class:failed={result.medianMs === null}>
                  <th scope="row" class="backend">{backendName(result.backend)}</th>
                  {#if result.medianMs !== null}
                    <td>
                      <div class="run">
                        <span class="meter" aria-hidden="true"><span style:width="{Math.max(4, (result.medianMs / row.slowest) * 100)}%"></span></span>
                        <span class="figure">{duration(result.medianMs)}</span>
                        {#if fastest}<span class="badge">{t('settings.runtime.measure.fastest')}</span>{/if}
                      </div>
                    </td>
                    <td class="load">{t('settings.runtime.measure.load', { time: duration(result.loadMs ?? 0) })}</td>
                  {:else}
                    <td class="reason" colspan="2" title={result.error ?? undefined}>{failure(result.error)}</td>
                  {/if}
                </tr>
              {/each}
            </tbody>
          </table>
        </li>
      {/each}
    </ul>
  {:else if view && !running}
    <p class="empty">{t('settings.runtime.measure.none')}</p>
  {/if}
</div>

<style>
  .measure { display: grid; gap: var(--s-5) }
  .lead { margin: 0; color: var(--t2); font-size: 12.5px; line-height: 1.5; max-width: 68ch }
  .bar { display: flex; align-items: center; flex-wrap: wrap; gap: var(--s-3) }
  .status { color: var(--t2); font-size: 12px; font-variant-numeric: tabular-nums; margin-left: var(--s-2) }
  .track { height: 3px; border-radius: var(--r-pill); background: var(--line2); overflow: hidden }
  .fill { display: block; height: 100%; background: var(--accent); transform-origin: left; transition: transform var(--dur-fast, .18s) var(--ease, ease-out) }
  .alert, .stale { margin: 0; font-size: 12.5px; line-height: 1.5; max-width: 72ch }
  .alert { color: var(--warn) }
  .stale { color: var(--t2); padding: var(--s-4) var(--s-5); border-radius: var(--r-md); background: var(--panel2) }
  .empty { margin: 0; color: var(--t3); font-size: 12px }

  .models { list-style: none; margin: 0; padding: 0; display: grid }
  .model { padding: var(--s-5) 0; border-top: 1px solid var(--line) }
  .model:first-child { border-top: none; padding-top: 0 }
  .model-head { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: var(--s-5); margin-bottom: var(--s-3) }
  .model-name { font-weight: 600; font-size: 12.5px }
  .in-use { color: var(--t2); font-size: 11.5px; text-align: right }

  .results { width: 100%; border-collapse: collapse; font-size: 12px; font-variant-numeric: tabular-nums }
  .results th, .results td { padding: 3px 0; text-align: left; font-weight: 400; vertical-align: middle }
  .backend { width: 96px; color: var(--t2) }
  .fastest .backend { color: var(--text); font-weight: 600 }
  .run { display: flex; align-items: center; gap: var(--s-3) }
  .meter { flex: 0 1 140px; height: 4px; border-radius: var(--r-pill); background: var(--accent-soft); overflow: hidden }
  .meter span { display: block; height: 100%; border-radius: inherit; background: var(--tintline) }
  .fastest .meter span { background: var(--accent) }
  .figure { min-width: 56px }
  .fastest .figure { font-weight: 600 }
  .badge {
    padding: 1px var(--s-2);
    border-radius: var(--r-pill);
    background: var(--accent-soft);
    color: var(--text);
    font-size: 10.5px;
    font-weight: 600;
  }
  .load { width: 96px; color: var(--t3); text-align: right !important }
  .reason { color: var(--t3) }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
