/**
 * The jobs store: runs and denoises followed past the screen that started
 * them. Driven through a small fake backend that owns the three channels the
 * store listens on - the run stream, `denoise://progress` and the quit guard.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { setBackend } from '../api/backend.js'
import { app, goLibrary } from './app.svelte.js'
import {
  MAX_FINISHED,
  answerQuit,
  askQuit,
  clearFinishedJobs,
  dismissJob,
  jobById,
  jobFraction,
  jobs,
  onJobSettled,
  openJobChapter,
  registerJob,
  resetJobs,
  runningCount,
  runningJobFor,
  startJobs,
  stopJob,
  trackDenoise,
  watchJob,
} from './jobs.svelte.js'

const PROJECTS = [
  { id: 'p1', name: 'Tsuki to Hane', chapters: [{ id: 'c1', number: 107, name: 'Feather Weight' }, { id: 'c2', number: 106, name: 'Small Hours' }] },
]

function fakeBackend(extra = {}) {
  const runs = new Set()
  const denoise = new Set()
  const quit = new Set()
  return {
    emit: (/** @type {any} */ event) => [...runs].forEach((handler) => handler(event)),
    progress: (/** @type {any} */ event) => [...denoise].forEach((handler) => handler(event)),
    requestQuit: (/** @type {any} */ request) => [...quit].forEach((handler) => handler(request)),
    listeners: () => runs.size + denoise.size + quit.size,
    subscribe: (/** @type {any} */ handler) => {
      runs.add(handler)
      return () => runs.delete(handler)
    },
    onDenoiseProgress: async (/** @type {any} */ handler) => {
      denoise.add(handler)
      return () => denoise.delete(handler)
    },
    onQuitRequested: async (/** @type {any} */ handler) => {
      quit.add(handler)
      return () => quit.delete(handler)
    },
    listJobs: vi.fn(async () => []),
    listProjects: vi.fn(async () => structuredClone(PROJECTS)),
    cancelRun: vi.fn(async ({ runId }) => runId),
    cancelDenoiseLocal: vi.fn(async () => true),
    confirmQuit: vi.fn(async () => {}),
    hideToTray: vi.fn(async () => true),
    ...extra,
  }
}

/** @type {ReturnType<typeof fakeBackend>} */
let backend

const RUN = { runId: 'run-1', kind: /** @type {const} */ ('clean'), chapterId: 'c1', projectId: 'p1', projectName: 'Tsuki to Hane', chapterName: 'Feather Weight', chapterNumber: 107, total: 4 }

beforeEach(() => {
  resetJobs()
  backend = fakeBackend()
  setBackend(/** @type {any} */ (backend))
  app.modals.length = 0
  app.notices.length = 0
  goLibrary()
})

afterEach(() => {
  resetJobs()
  setBackend(null)
  vi.useRealTimers()
})

describe('a run on the jobs list', () => {
  it('shows from its registration, counts its pages, and ends on run-finished', async () => {
    const settled = vi.fn()
    onJobSettled(settled)
    const job = registerJob(RUN)
    expect(job).toMatchObject({ status: 'running', done: 0, total: 4 })
    expect(runningCount()).toBe(1)
    expect(runningJobFor('c1')?.runId).toBe('run-1')
    // Registering started the store's one subscription.
    expect(backend.listeners()).toBeGreaterThan(0)

    backend.emit({ type: 'page-started', runId: 'run-1', chapterId: 'c1', pageIndex: 0 })
    backend.emit({ type: 'page-done', runId: 'run-1', chapterId: 'c1', pageIndex: 0, page: {} })
    backend.emit({ type: 'page-done', runId: 'run-1', chapterId: 'c1', pageIndex: 1, page: {} })
    expect(jobById('run-1')?.done).toBe(2)
    expect(jobFraction(/** @type {any} */ (jobById('run-1')))).toBe(0.5)

    backend.emit({ type: 'run-finished', runId: 'run-1', chapterId: 'c1', reason: 'completed', pagesQueued: 4, pagesCleaned: 4, regionsCleaned: 9, nextPageIndex: null })
    expect(jobById('run-1')).toMatchObject({ status: 'completed', done: 4 })
    expect(jobById('run-1')?.finishedAt).toEqual(expect.any(Number))
    expect(runningCount()).toBe(0)
    expect(settled).toHaveBeenCalledTimes(1)
  })

  it('applies an end that overtook the registration', () => {
    startJobs(/** @type {any} */ (backend))
    backend.emit({ type: 'run-finished', runId: 'run-1', chapterId: 'c1', reason: 'cancelled', pagesQueued: 4, pagesCleaned: 1, regionsCleaned: 2, nextPageIndex: 1 })
    expect(jobById('run-1')).toBeNull()
    registerJob(RUN)
    expect(jobById('run-1')).toMatchObject({ status: 'cancelled', done: 1 })
  })

  it('stops a run with cancelRun, and says it is stopping until the end arrives', async () => {
    registerJob(RUN)
    await stopJob('run-1')
    expect(backend.cancelRun).toHaveBeenCalledWith({ runId: 'run-1' })
    expect(jobById('run-1')?.stopping).toBe(true)
    await stopJob('run-1')
    expect(backend.cancelRun).toHaveBeenCalledTimes(1)
    backend.emit({ type: 'run-finished', runId: 'run-1', chapterId: 'c1', reason: 'cancelled', pagesQueued: 4, pagesCleaned: 1, regionsCleaned: 1, nextPageIndex: 1 })
    expect(jobById('run-1')).toMatchObject({ status: 'cancelled', stopping: false })
  })

  it('keeps every running job and the newest finished ones, until dismissed', () => {
    registerJob({ ...RUN, runId: 'still-going', chapterId: 'c2' })
    for (let i = 0; i < MAX_FINISHED + 3; i += 1) {
      registerJob({ ...RUN, runId: `r${i}` })
      backend.emit({ type: 'run-finished', runId: `r${i}`, chapterId: 'c1', reason: 'completed', pagesQueued: 4, pagesCleaned: 4, regionsCleaned: 1, nextPageIndex: null })
    }
    expect(jobs.list.filter((job) => job.status !== 'running')).toHaveLength(MAX_FINISHED)
    expect(jobById('r0')).toBeNull()
    expect(jobById(`r${MAX_FINISHED + 2}`)).not.toBeNull()
    expect(jobById('still-going')?.status).toBe('running')

    dismissJob('still-going')
    expect(jobById('still-going')).not.toBeNull()
    dismissJob(`r${MAX_FINISHED + 2}`)
    expect(jobById(`r${MAX_FINISHED + 2}`)).toBeNull()
    clearFinishedJobs()
    expect(jobs.list.map((job) => job.runId)).toEqual(['still-going'])
  })
})

describe('a denoise on the jobs list', () => {
  const DENOISE = { ...RUN, runId: 'den-1', kind: /** @type {const} */ ('denoise'), total: 4 }

  it('follows its progress and its answer, and says the ending when nothing watches', async () => {
    /** @type {(report: any) => void} */
    let answer = () => {}
    const tracked = trackDenoise(DENOISE, () => new Promise((resolve) => { answer = resolve }))
    await Promise.resolve()
    backend.progress({ runId: 'den-1', done: 1, total: 4, page: 0.5 })
    expect(jobFraction(/** @type {any} */ (jobById('den-1')))).toBeCloseTo(0.375)

    answer({ written: [{ pageIndex: 0 }, { pageIndex: 1 }, { pageIndex: 2 }], failed: [{ pageIndex: 3, code: 'x' }] })
    const job = await tracked
    expect(job).toMatchObject({ status: 'completed', done: 4 })
    expect(job?.report?.written).toHaveLength(3)
    expect(app.notices.at(-1)).toMatchObject({ key: 'denoise.notice.finished', params: { count: 3, failed: 1, number: 107 }, tone: 'warn' })
  })

  it('leaves the ending to a dialog that watches it', async () => {
    const unwatch = watchJob('den-1')
    await trackDenoise(DENOISE, async () => ({ written: [], failed: [], cancelled: true }))
    expect(jobById('den-1')?.status).toBe('cancelled')
    expect(app.notices).toHaveLength(0)
    unwatch()
  })

  it('records a failure with its code, and stops with cancelDenoiseLocal', async () => {
    /** @type {(error: Error) => void} */
    let fail = () => {}
    const tracked = trackDenoise({ ...DENOISE, kind: 'cloudDenoise' }, () => new Promise((_, reject) => { fail = reject }))
    await stopJob('den-1')
    expect(backend.cancelDenoiseLocal).toHaveBeenCalledWith({ runId: 'den-1' })
    expect(backend.cancelRun).not.toHaveBeenCalled()
    fail(new Error('Error: cloud_disabled'))
    expect(await tracked).toMatchObject({ status: 'failed', errorCode: 'cloud_disabled' })
    expect(app.notices.at(-1)).toMatchObject({ key: 'denoise.notice.stopped', tone: 'warn' })
  })
})

describe('what the backend lists', () => {
  it('reads the running jobs at start and names them from the library', async () => {
    backend.listJobs.mockResolvedValueOnce([{ runId: 'run-9', kind: 'detect', chapterId: 'c2', done: 3, total: 18 }])
    startJobs(/** @type {any} */ (backend))
    await vi.waitFor(() => expect(jobById('run-9')?.chapterName).toBe('Small Hours'))
    expect(jobById('run-9')).toMatchObject({ kind: 'detect', status: 'running', done: 3, total: 18, projectId: 'p1', projectName: 'Tsuki to Hane', chapterNumber: 106 })
  })

  it('asks again when an event names a run it has never heard of', async () => {
    vi.useFakeTimers()
    startJobs(/** @type {any} */ (backend))
    await vi.advanceTimersByTimeAsync(0)
    expect(backend.listJobs).toHaveBeenCalledTimes(1)
    backend.listJobs.mockResolvedValueOnce([{ runId: 'resumed', kind: 'clean', chapterId: 'c1', done: 0, total: 5 }])
    backend.emit({ type: 'page-started', runId: 'resumed', chapterId: 'c1', pageIndex: 0 })
    backend.emit({ type: 'page-done', runId: 'resumed', chapterId: 'c1', pageIndex: 0, page: {} })
    await vi.advanceTimersByTimeAsync(300)
    expect(backend.listJobs).toHaveBeenCalledTimes(2)
    expect(jobById('resumed')).toMatchObject({ status: 'running', total: 5 })
  })

  it('opens a listed job\'s chapter once its project is known', async () => {
    backend.listJobs.mockResolvedValueOnce([{ runId: 'run-9', kind: 'clean', chapterId: 'c2', done: 0, total: 4 }])
    startJobs(/** @type {any} */ (backend))
    await vi.waitFor(() => expect(jobById('run-9')).not.toBeNull())
    expect(await openJobChapter('run-9')).toBe(true)
    expect(app.route).toMatchObject({ name: 'editor', projectId: 'p1', chapterId: 'c2' })
  })
})

describe('the quit guard', () => {
  it('asks once, beside the modal stack, and answers with confirmQuit or hideToTray', async () => {
    registerJob(RUN)
    startJobs(/** @type {any} */ (backend))
    await Promise.resolve()
    await Promise.resolve()
    app.modals.push(/** @type {any} */ ({ id: 'under', kind: 'workflowReview' }))
    backend.requestQuit({ jobs: 1, canHide: true })
    backend.requestQuit({ jobs: 3, canHide: false })
    expect(jobs.quit).toEqual({ count: 1, canHide: true })
    // The dialog underneath is left where it was.
    expect(app.modals.map((modal) => modal.id)).toEqual(['under'])

    answerQuit('hide')
    expect(jobs.quit).toBeNull()
    await vi.waitFor(() => expect(backend.hideToTray).toHaveBeenCalledTimes(1))
    expect(backend.confirmQuit).not.toHaveBeenCalled()

    // Keep running is only an answer while a tray was offered.
    askQuit({ jobs: 1, canHide: false })
    answerQuit('hide')
    askQuit({ jobs: 1, canHide: false })
    answerQuit('quit')
    await vi.waitFor(() => expect(backend.confirmQuit).toHaveBeenCalledTimes(1))

    askQuit({ jobs: 1, canHide: true })
    answerQuit(null)
    answerQuit('quit')
    await Promise.resolve()
    expect(backend.confirmQuit).toHaveBeenCalledTimes(1)
    expect(backend.hideToTray).toHaveBeenCalledTimes(1)
  })
})

it('tracks cloud denoise on its granted endpoint until its background promise ends', async () => {
  const { cloudGpu, stopCloudGpuWatch } = await import('./cloudgpu.svelte.js')
  const target = { provider: 'modal', profileId: 'gpu-A' }
  let complete
  const pending = trackDenoise({ ...RUN, runId: 'denoise-A', kind: 'cloudDenoise', target },
    () => new Promise((resolve) => { complete = resolve }))
  expect(cloudGpu.runs['denoise-A']).toEqual({ role: 'analysis', target })
  goLibrary()
  expect(jobById('denoise-A').status).toBe('running')
  complete({ written: [], failed: [], cancelled: false })
  await pending
  expect(cloudGpu.runs['denoise-A']).toBeUndefined()
  expect(jobById('denoise-A').status).toBe('completed')
  stopCloudGpuWatch()
})

it('releases cloud denoise tracking after a rejected background request', async () => {
  const { cloudGpu, stopCloudGpuWatch } = await import('./cloudgpu.svelte.js')
  const target = { provider: 'modal', profileId: 'gpu-A' }
  await trackDenoise({ ...RUN, runId: 'denoise-A', kind: 'cloudDenoise', target }, async () => { throw new Error('gateway_unreachable') })
  expect(cloudGpu.runs['denoise-A']).toBeUndefined()
  expect(jobById('denoise-A').status).toBe('failed')
  stopCloudGpuWatch()
})
