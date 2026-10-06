/**
 * The simulated run scheduler.
 *
 * A run walks its queue one page at a time, emitting `region-done` for each
 * region and then `page-done`, so the Pages list ticks over as the progress
 * indicator (there is no progress bar). Nothing is
 * returned synchronously: the caller gets a run id and the queue, and every
 * result arrives on the event channel, exactly as a Tauri channel would
 * deliver it.
 *
 * Cancel keeps completed regions and leaves the job incomplete,
 * which is why the in-flight page falls back to
 * `unclean` rather than being rolled back.
 *
 * Every delay comes from the injected `timers`, so tests drive it with fake
 * timers and a demo can slow it down with a speed factor.
 */

import { isOutsideHeld, isQueueable } from './tools.js'

/**
 * @typedef {Object} QueueEntry
 * @property {string} projectId
 * @property {string} chapterId
 * @property {import('../model/types.js').Page} page
 */

/**
 * @typedef {Object} RunnerDeps
 * @property {(event: Object) => void} emit
 * @property {{ setTimeout: Function, clearTimeout: Function }} timers
 * @property {{ region: number, pageTail: number }} timing - already scaled by the speed factor
 * @property {(region: import('../model/types.js').Region, page: import('../model/types.js').Page, ctx: Object) => void} cleanRegion
 * @property {(summary: Object) => void} onFinished
 * @property {() => string} [nextRunId] - names each run; a pool shares one so ids never repeat
 */

/**
 * @param {RunnerDeps} deps
 * @returns {{ start: Function, cancel: Function, isRunning: () => boolean, activeRunId: () => string|null }}
 */
export function createRunner(deps) {
  let active = null
  let counter = 0

  const schedule = (fn, ms) => {
    active.timer = deps.timers.setTimeout(fn, ms)
  }

  function stepPage() {
    if (!active) return
    if (active.index >= active.queue.length) return finish('completed')
    const entry = active.queue[active.index]
    const { page } = entry
    page.status = 'cleaning'
    // Snapshot the queue of regions once: cleaning one removes it from the
    // pending set, so a filter recomputed per step would skip every other one.
    // With the outside-bubble opt-in on, the regions the gate would have held
    // back as "text outside a speech bubble" are queued too (`isOutsideHeld`).
    const optedIn = (region) => active.outsideBubbles === 'clean' && isOutsideHeld(region)
    // A run of a step other than Detect and clean names its own regions
    // (`pick`): Detect the ones it finds, Clean the stored detections.
    active.pending = active.pick
      ? active.pick(page)
      : page.regions.filter((region) => isQueueable(region) || optedIn(region))
    deps.emit({
      type: 'page-started',
      runId: active.runId,
      chapterId: entry.chapterId,
      pageId: page.id,
      pageIndex: page.index,
    })
    stepRegion(0)
  }

  function stepRegion(i) {
    if (!active) return
    const entry = active.queue[active.index]
    const { page } = entry
    if (i >= active.pending.length) {
      return schedule(() => finishPage(entry), deps.timing.pageTail)
    }
    schedule(() => {
      if (!active) return
      const region = active.pending[i]
      const ctx = {
        engineCeiling: active.engineCeiling,
        bubbleEngine: active.bubbleEngine,
        outsideEngine: active.outsideEngine,
        bubbleColor: active.bubbleColor,
      }
      // `apply` answers false for a region it left as it was (a cloud render
      // that failed): it is still reported, and not counted.
      const done = active.apply ? active.apply(region, page, ctx) !== false : (deps.cleanRegion(region, page, ctx), true)
      if (done) active.regionsCleaned += 1
      // Detect reports its regions with the page, not one by one: a stored
      // detection is no region *done*.
      if (active.reportRegions) {
        deps.emit({
          type: 'region-done',
          runId: active.runId,
          chapterId: entry.chapterId,
          pageId: page.id,
          pageIndex: page.index,
          region,
        })
      }
      stepRegion(i + 1)
    }, deps.timing.region)
  }

  function finishPage(entry) {
    if (!active) return
    const { page } = entry
    page.status = active.pageStatus ? active.pageStatus(page) : 'cleaned'
    active.pagesCleaned += 1
    deps.emit({
      type: 'page-done',
      runId: active.runId,
      chapterId: entry.chapterId,
      pageId: page.id,
      pageIndex: page.index,
      page,
    })
    active.index += 1
    schedule(stepPage, 0)
  }

  /**
   * @param {'completed'|'cancelled'} reason
   */
  function finish(reason) {
    const run = active
    active = null
    deps.timers.clearTimeout(run.timer)
    const summary = {
      type: 'run-finished',
      runId: run.runId,
      chapterId: run.chapterId,
      reason,
      pagesQueued: run.queue.length,
      pagesCleaned: run.pagesCleaned,
      regionsCleaned: run.regionsCleaned,
      nextPageIndex: reason === 'cancelled' ? (run.queue[run.index]?.page.index ?? null) : null,
    }
    deps.emit(summary)
    deps.onFinished({ ...summary, queue: run.queue, stoppedAt: run.index })
  }

  return {
    /**
     * @param {QueueEntry[]} queue
     * @param {{ chapterId: string, engineCeiling?: string, pick?: (page: Object) => Object[], apply?: (region: Object, page: Object, ctx: Object) => boolean|void, pageStatus?: (page: Object) => string, reportRegions?: boolean }} options
     *   `pick`, `apply` and `pageStatus` replace the automatic pass's own
     *   choices for a run of another step: which regions of a page it takes,
     *   what it does to each, and what the page is when it is done.
     *   `reportRegions: false` sends no `region-done`, as Detect does
     * @returns {{ runId: string, pages: Array<{chapterId: string, pageId: string, pageIndex: number}> }}
     */
    start(queue, options) {
      counter += 1
      active = {
        runId: deps.nextRunId ? deps.nextRunId() : `run-${counter}`,
        chapterId: options.chapterId,
        engineCeiling: options.engineCeiling ?? 'lama',
        bubbleEngine: options.bubbleEngine,
        outsideEngine: options.outsideEngine,
        outsideBubbles: options.outsideBubbles ?? 'review',
        bubbleColor: options.bubbleColor ?? '#ffffff',
        pick: options.pick ?? null,
        apply: options.apply ?? null,
        pageStatus: options.pageStatus ?? null,
        reportRegions: options.reportRegions !== false,
        queue,
        index: 0,
        timer: null,
        pagesCleaned: 0,
        regionsCleaned: 0,
      }
      schedule(stepPage, 0)
      return {
        runId: active.runId,
        pages: queue.map((entry) => ({
          chapterId: entry.chapterId,
          pageId: entry.page.id,
          pageIndex: entry.page.index,
        })),
      }
    },

    /** Stops the run, keeping every region completed so far. */
    cancel() {
      if (!active) return null
      const entry = active.queue[active.index]
      if (entry) {
        const { page } = entry
        page.status = active.pageStatus
          ? active.pageStatus(page)
          : page.regions.every((region) => region.outcome !== 'pending')
            ? 'cleaned'
            : 'unclean'
      }
      const runId = active.runId
      finish('cancelled')
      return runId
    },

    isRunning: () => active !== null,
    activeRunId: () => (active ? active.runId : null),
  }
}

/**
 * Several runs at once, as the native side keeps them (`run.rs`): one run per
 * chapter, at most `max` in all. Each run is its own `createRunner`, so each
 * walks its own queue on its own timers and the events of two runs
 * interleave, which is exactly what a consumer has to cope with.
 *
 * `kind` and `mode` ride along with a run so its ending can say what it was
 * (`onFinished` gets both) and `list` can answer `list_jobs`.
 *
 * @param {RunnerDeps} deps
 * @param {{max?: number}} [options]
 */
export function createRunnerPool(deps, { max = 3 } = {}) {
  /** @type {Map<string, {runner: ReturnType<typeof createRunner>, chapterId: string, kind: string, total: number, done: number}>} */
  const runs = new Map()
  let counter = 0

  return {
    max,

    /**
     * @param {QueueEntry[]} queue
     * @param {Object & {chapterId: string, kind?: string, mode?: string}} options - `createRunner#start`'s, plus the run's kind and mode
     */
    start(queue, options) {
      const run = { runner: /** @type {any} */ (null), chapterId: options.chapterId, kind: options.kind ?? 'clean', total: queue.length, done: 0 }
      const runner = createRunner({
        ...deps,
        nextRunId: () => `run-${(counter += 1)}`,
        emit: (event) => {
          if (event.type === 'page-done') run.done += 1
          deps.emit(event)
        },
        onFinished: (summary) => {
          runs.delete(summary.runId)
          deps.onFinished({ ...summary, kind: run.kind, mode: options.mode ?? 'auto' })
        },
      })
      run.runner = runner
      const handle = runner.start(queue, options)
      runs.set(handle.runId, run)
      return handle
    },

    /** The run on this chapter, if one is going. @param {string} chapterId */
    runFor(chapterId) {
      for (const [runId, run] of runs) if (run.chapterId === chapterId) return runId
      return null
    },

    atCapacity: () => runs.size >= max,

    /**
     * Stop one run. With no id, only when exactly one is going: an older
     * caller that names none cannot mean one of several.
     *
     * @param {string} [runId]
     */
    cancel(runId) {
      if (runId) return runs.get(runId)?.runner.cancel() ?? null
      if (runs.size !== 1) return null
      return [...runs.values()][0].runner.cancel()
    },

    isRunning: () => runs.size > 0,

    /** @returns {Array<{runId: string, kind: string, chapterId: string, done: number, total: number}>} */
    list: () => [...runs].map(([runId, run]) => ({ runId, kind: run.kind, chapterId: run.chapterId, done: run.done, total: run.total })),
  }
}
