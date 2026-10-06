/**
 * Cloud renders in flight and what the last session left behind: the status
 * element (phase, elapsed time, Cancel, the first-run hint), the one notice
 * each render ends in, and recovery when the app starts.
 */
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte'
import { tick } from 'svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { t } from '../i18n/index.js'
import { app, clearNotices } from '../state/app.svelte.js'
import {
  cloud,
  onAttemptEvent,
  settleCloudJob,
  SETTLE_GRACE_MS,
  startCloud,
  stopCloud,
  trackCloudJob,
} from '../state/cloud.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { setCloudAllowed } from '../state/session.svelte.js'
import CloudJobStatus from './CloudJobStatus.svelte'
import { runCloudJob } from './cloudflow.svelte.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
/** @param {string} digit */
const attempt = (digit) => `att-${digit.repeat(24)}`
const A = attempt('a')
const B = attempt('b')
const C = attempt('c')
const D = attempt('d')

/** Local storage for the test: the node running the suite has none. */
function memoryStorage() {
  /** @type {Map<string, string>} */
  const values = new Map()
  return {
    get length() {
      return values.size
    },
    key: (/** @type {number} */ index) => [...values.keys()][index] ?? null,
    getItem: (/** @type {string} */ key) => values.get(key) ?? null,
    setItem: (/** @type {string} */ key, /** @type {string} */ value) => void values.set(key, String(value)),
    removeItem: (/** @type {string} */ key) => void values.delete(key),
    clear: () => values.clear(),
  }
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
})

afterEach(() => {
  cleanup()
  vi.useRealTimers()
  stopCloud()
  setCloudAllowed(false)
  clearNotices()
  editor.chapter = null
  setBackend(null)
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

function mockBackend() {
  const backend = createMockBackend({ timing: ZERO })
  setBackend(backend)
  return backend
}

/** The cloud notices raised so far, as `key` or `key params`. */
function cloudNotices() {
  return app.notices
    .filter((notice) => notice.key.startsWith('notice.cloud.'))
    .map((notice) => (notice.params && Object.keys(notice.params).length ? [notice.key, notice.params] : notice.key))
}

const statusRegion = () => screen.queryByRole('region', { name: t('notice.cloud.job.title') })
const findStatus = () => screen.findByRole('region', { name: t('notice.cloud.job.title') })

describe('the cloud job status', () => {
  it('shows a render before its first event, follows its phases, and cancels it', async () => {
    const backend = mockBackend()
    const cancel = vi
      .spyOn(backend, 'cancelCloudAttempt')
      .mockResolvedValue({ handle: '', status: 'cancel_requested', acknowledged: false })
    render(CloudJobStatus)
    expect(statusRegion()).toBeNull()

    trackCloudJob({ attemptId: A, chapterId: 'chapter-1', pageIndex: 2, regionId: 'region-1' })
    const status = await findStatus()
    expect(within(status).getByText(t('notice.cloud.phase.preparing'))).toBeTruthy()
    expect(within(status).getByText('0:00')).toBeTruthy()
    expect(within(status).getByText(t('notice.cloud.job.page', { page: 3 }))).toBeTruthy()
    // No render has finished yet, so the GPU may be starting.
    expect(within(status).getByText(t('notice.cloud.job.firstRun'))).toBeTruthy()

    onAttemptEvent({ attemptId: A, phase: 'running', elapsedMs: 65_500 })
    await tick()
    expect(within(status).getByText(t('notice.cloud.phase.running'))).toBeTruthy()
    expect(within(status).getByText('1:05')).toBeTruthy()
    expect(within(status).queryByText(t('notice.cloud.job.firstRun'))).toBeNull()

    await fireEvent.click(within(status).getByRole('button', { name: t('notice.cloud.job.cancel') }))
    expect(cancel).toHaveBeenCalledWith({ attemptId: A })
    const cancelling = within(status).getByRole('button', { name: t('notice.cloud.job.cancelling') })
    expect(cancelling.getAttribute('aria-disabled')).toBe('true')
    // A second press sends nothing more.
    await fireEvent.click(cancelling)
    expect(cancel).toHaveBeenCalledTimes(1)

    // The render ends with its own event, and a late answer says nothing more.
    onAttemptEvent({ attemptId: A, phase: 'cancelled', errorCode: 'cancelled', elapsedMs: 66_000 })
    await tick()
    expect(statusRegion()).toBeNull()
    settleCloudJob(A, { phase: 'cancelled', errorCode: 'cancelled' })
    expect(cloudNotices()).toEqual(['notice.cloud.cancelled'])
  })

  it('shows the result when the cancel came too late to stop the render', async () => {
    const backend = mockBackend()
    vi.spyOn(backend, 'cancelCloudAttempt').mockResolvedValue({
      handle: '',
      status: 'cancel_requested',
      acknowledged: false,
    })
    render(CloudJobStatus)
    trackCloudJob({ attemptId: A, chapterId: 'chapter-1', pageIndex: 0, regionId: 'region-1' })
    const status = await findStatus()
    await fireEvent.click(within(status).getByRole('button', { name: t('notice.cloud.job.cancel') }))
    onAttemptEvent({ attemptId: A, phase: 'committed', elapsedMs: 12_000 })
    await tick()
    expect(statusRegion()).toBeNull()
    settleCloudJob(A, { phase: 'committed' })
    expect(cloudNotices()).toEqual([['notice.cloud.finished', { seconds: 12 }]])
  })

  it('says nothing about a first run once a render has finished recently', async () => {
    mockBackend()
    cloud.lastCommitAt = Date.now()
    render(CloudJobStatus)
    trackCloudJob({ attemptId: A, chapterId: 'chapter-1', pageIndex: null, regionId: 'region-1' })
    const status = await findStatus()
    expect(within(status).getByText(t('notice.cloud.job.region'))).toBeTruthy()
    expect(within(status).queryByText(t('notice.cloud.job.firstRun'))).toBeNull()
  })

  it('keeps Cancel when the cancel request fails, and says so', async () => {
    const backend = mockBackend()
    vi.spyOn(backend, 'cancelCloudAttempt').mockRejectedValueOnce(new Error('gateway_unreachable'))
    render(CloudJobStatus)
    trackCloudJob({ attemptId: A, chapterId: 'chapter-1', pageIndex: 0, regionId: 'region-1' })
    const status = await findStatus()
    await fireEvent.click(within(status).getByRole('button', { name: t('notice.cloud.job.cancel') }))
    await waitFor(() => expect(cloudNotices()).toEqual(['notice.cloud.cancelFailed']))
    const again = within(status).getByRole('button', { name: t('notice.cloud.job.cancel') })
    expect(again.getAttribute('aria-disabled')).toBe('false')
  })
})

describe('how a render ends', () => {
  const grant = (/** @type {string} */ attemptId) => ({
    attemptId,
    params: {
      grantNonce: 'grant-test',
      executionTarget: { type: /** @type {const} */ ('modal'), profile_id: 'mc-abc123' },
      recipe: /** @type {any} */ ({}),
      intent: /** @type {any} */ ({ action: 'applyTool', tool: 'contentAwareFill', params: {} }),
    },
  })
  const where = { chapterId: 'chapter-1', pageIndex: 0, regionId: 'region-1' }

  it('is tracked from before the call and ends in one notice', async () => {
    mockBackend()
    /** @type {(value: {status: string}) => void} */
    let answer = () => {}
    const call = vi.fn(() => new Promise((resolve) => (answer = resolve)))
    const done = runCloudJob(grant(A), where, call, (result) => (result.status === 'applied' ? { phase: 'committed' } : null))
    expect(cloud.jobs.map((job) => job.attemptId)).toEqual([A])
    expect(call).toHaveBeenCalledWith(grant(A).params)

    answer({ status: 'applied' })
    await done
    expect(cloud.jobs).toEqual([])
    expect(cloudNotices()).toEqual([['notice.cloud.finished', { seconds: 1 }]])
    // The event that follows the answer does not end it twice.
    onAttemptEvent({ attemptId: A, phase: 'committed', elapsedMs: 900 })
    expect(cloudNotices()).toHaveLength(1)
  })

  it('names the reason a failed call carries', async () => {
    mockBackend()
    await runCloudJob(grant(A), where, () => Promise.reject(new Error('gateway_unreachable')), () => null)
    expect(cloudNotices()).toEqual([['notice.cloud.failed', { reasonKey: 'notice.cloud.error.unreachable' }]])
  })

  it('reads an unknown code as the generic reason', () => {
    mockBackend()
    trackCloudJob({ attemptId: A, ...where })
    settleCloudJob(A, { phase: 'failed', errorCode: 'something_new' })
    expect(cloudNotices()).toEqual([['notice.cloud.failed', { reasonKey: 'notice.cloud.error.generic' }]])
  })

  it('treats attempt_busy as a render already running, not as a failure', () => {
    mockBackend()
    trackCloudJob({ attemptId: A, ...where })
    settleCloudJob(A, { phase: 'failed', errorCode: 'attempt_busy' })
    expect(cloud.jobs.map((job) => job.attemptId)).toEqual([A])
    expect(cloudNotices()).toEqual([])
    onAttemptEvent({ attemptId: A, phase: 'committed', elapsedMs: 4_000 })
    expect(cloud.jobs).toEqual([])
    expect(cloudNotices()).toEqual([['notice.cloud.finished', { seconds: 4 }]])
  })

  it('waits briefly for the event when the answer does not say how it ended', () => {
    vi.useFakeTimers()
    mockBackend()
    trackCloudJob({ attemptId: A, ...where })
    settleCloudJob(A, null)
    onAttemptEvent({ attemptId: A, phase: 'failed', errorCode: 'remote_failed', elapsedMs: 2_000 })
    vi.advanceTimersByTime(SETTLE_GRACE_MS * 2)
    expect(cloudNotices()).toEqual([['notice.cloud.failed', { reasonKey: 'notice.cloud.error.remote' }]])

    trackCloudJob({ attemptId: B, ...where })
    settleCloudJob(B, null)
    vi.advanceTimersByTime(SETTLE_GRACE_MS - 1)
    expect(cloud.jobs.map((job) => job.attemptId)).toEqual([B])
    vi.advanceTimersByTime(1)
    expect(cloud.jobs).toEqual([])
    expect(cloudNotices()).toHaveLength(2)
    expect(cloudNotices()[1]).toEqual(['notice.cloud.failed', { reasonKey: 'notice.cloud.error.generic' }])
  })

  it('says the outcome is unknown rather than guessing', () => {
    mockBackend()
    trackCloudJob({ attemptId: A, ...where })
    onAttemptEvent({ attemptId: A, phase: 'unknown', errorCode: 'submission_unknown' })
    expect(cloudNotices()).toEqual(['notice.cloud.unknown'])
  })
})

describe('recovery when the app starts', () => {
  /**
   * @param {string} id
   * @param {string} note
   */
  const region = (id, note) => ({ id, pageId: 'page-0', note, mask: null })

  it('waits for the permission, settles once, reports it, and reads committed regions back', async () => {
    const backend = mockBackend()
    const reconcile = vi.spyOn(backend, 'reconcileCloudRecovery').mockResolvedValue(
      /** @type {any} */ ({
        attached: [{ attemptId: A, chapterId: 'chapter-open', pageIndex: 0, regionId: 'page-0-r1' }],
        stillRunning: [{ attemptId: B, chapterId: 'chapter-open', pageIndex: 0, regionId: 'page-0-r2' }],
        needsAttention: [
          { attemptId: C, chapterId: 'chapter-open', pageIndex: 0, regionId: 'page-0-r3', reason: 'ambiguous' },
          { attemptId: D, chapterId: null, pageIndex: null, regionId: null, reason: 'failed' },
        ],
      }),
    )
    const loadPages = vi.spyOn(backend, 'loadPages').mockImplementation(async () => [
      /** @type {any} */ ({
        index: 0,
        id: 'page-0',
        status: 'cleaned',
        regions: [region('page-0-r1', 'cloud'), region('page-0-r2', 'cloud')],
      }),
    ])
    editor.chapter = /** @type {any} */ ({
      id: 'chapter-open',
      review: [],
      pages: [
        {
          index: 0,
          id: 'page-0',
          status: 'unclean',
          regions: [region('page-0-r1', 'local'), region('page-0-r2', 'local')],
        },
      ],
    })
    render(CloudJobStatus)

    startCloud(backend)
    await tick()
    // Settling can wait on the provider, so it waits for the permission.
    expect(reconcile).not.toHaveBeenCalled()

    setCloudAllowed(true)
    await waitFor(() => expect(reconcile).toHaveBeenCalledTimes(1))
    expect(reconcile).toHaveBeenCalledWith({ apply: true })
    await waitFor(() =>
      expect(cloudNotices()).toEqual([
        ['notice.cloud.recovered', { count: 1 }],
        ['notice.cloud.needsAttention', { count: 2 }],
      ]),
    )
    expect(app.notices.find((notice) => notice.key === 'notice.cloud.needsAttention')?.tone).toBe('warn')
    expect(cloud.recovery?.needsAttention.map((entry) => entry.attemptId)).toEqual([C, D])

    // What finished while the app was closed is read back into the open chapter.
    await waitFor(() => expect(editor.chapter?.pages[0].regions[0].note).toBe('cloud'))
    expect(loadPages).toHaveBeenCalledWith({ chapterId: 'chapter-open', indices: [0] })
    expect(editor.chapter?.pages[0].regions[1].note).toBe('local')

    // What is still running shows in the status element, and a commit with no
    // command waiting on it is read back too.
    const status = await findStatus()
    expect(within(status).getByText(t('notice.cloud.job.page', { page: 1 }))).toBeTruthy()
    expect(cloud.jobs.map((job) => [job.attemptId, job.background])).toEqual([[B, true]])
    onAttemptEvent({ attemptId: B, phase: 'committed', elapsedMs: 3_000 })
    await waitFor(() => expect(editor.chapter?.pages[0].regions[1].note).toBe('cloud'))
    expect(cloudNotices()).toContainEqual(['notice.cloud.finished', { seconds: 3 }])
    await tick()
    expect(statusRegion()).toBeNull()

    // Once per app run.
    setCloudAllowed(false)
    await tick()
    setCloudAllowed(true)
    await tick()
    expect(reconcile).toHaveBeenCalledTimes(1)
  })

  it('says nothing when there was nothing to settle', async () => {
    const backend = mockBackend()
    setCloudAllowed(true)
    startCloud(backend)
    await waitFor(() => expect(cloud.recovery).not.toBeNull())
    expect(cloudNotices()).toEqual([])
    expect(cloud.jobs).toEqual([])
  })
})
