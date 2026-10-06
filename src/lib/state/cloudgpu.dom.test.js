import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { flushSync } from 'svelte'
import {
  GPU_POLL_MS,
  STOP_SETTLE_MS,
  cloudGpu,
  gpuDeployments,
  isStopping,
  onAnalysisRecord,
  refreshCloudGpu,
  runningAnalyses,
  scaledownLeftMs,
  startCloudGpu,
  trackCloudRun,
  finishCloudRun,
  stopCloudGpu,
  stopCloudGpuWatch,
} from './cloudgpu.svelte.js'
import { cloud } from './cloud.svelte.js'
import { app, closeModal } from './app.svelte.js'
import { session } from './session.svelte.js'

const idle = { role: 'render', gpu: 'L4', state: 'idle', idleSeconds: 120, upForMs: 5000, scaledownInMs: 90_000, listPriceUsdPerHour: 0.8 }
const analysis = { ...idle, role: 'analysis' }
const job = { target: { provider: 'modal', profileId: 'mc-1' }, attemptId: 'att-000000000000000000000001', regionId: null, chapterId: null, pageIndex: null,
  phase: 'running', startedAt: 0, cold: false, cancelling: false, background: false }

/** @param {Partial<Record<string, any>>} [overrides] */
function backend(overrides = {}) {
  return {
    getCloudGpuStatus: vi.fn().mockResolvedValue({ supported: true, unsupported: null, containers: [idle] }),
    stopCloudGpu: vi.fn().mockResolvedValue({ supported: true, stopped: ['render'], cancelledJobs: 0 }),
    ...overrides,
  }
}

function usable() {
  session.cloudAllowed = true
  cloud.readiness = { ...cloud.readiness, allowed: true, configured: true, ready: true, reason: null,
    target: { type: 'modal', profile_id: 'mc-1' } }
}

beforeEach(() => usable())

afterEach(() => {
  stopCloudGpuWatch()
  session.cloudAllowed = false
  cloud.readiness = { ...cloud.readiness, allowed: false, configured: false, ready: false, target: null }
  cloud.jobs = []
  app.modals.length = 0
  app.notices.length = 0
  vi.useRealTimers()
})

it('reads the selected target and keeps what is up', async () => {
  const b = backend()
  await refreshCloudGpu(b)
  expect(b.getCloudGpuStatus).toHaveBeenCalledWith({ provider: 'modal', profileId: 'mc-1' })
  expect(cloudGpu.supported).toBe(true)
  expect(cloudGpu.containers).toEqual([idle])
})

it('asks nothing when cloud is not usable', async () => {
  session.cloudAllowed = false
  const b = backend()
  await refreshCloudGpu(b)
  expect(b.getCloudGpuStatus).not.toHaveBeenCalled()
  expect(cloudGpu.containers).toEqual([])
})

it('treats a deployment that cannot say as showing nothing', async () => {
  await refreshCloudGpu(backend({ getCloudGpuStatus: vi.fn().mockResolvedValue({ supported: false, unsupported: 'outdated', containers: [] }) }))
  expect(cloudGpu.supported).toBe(false)
  expect(cloudGpu.unsupported).toBe('outdated')
  expect(cloudGpu.containers).toEqual([])
})

it('keeps the last rows, marked stale, when a read fails, until an answer clears them', async () => {
  const b = backend()
  await refreshCloudGpu(b)
  b.getCloudGpuStatus.mockRejectedValue(new Error('gateway_unreachable'))
  await refreshCloudGpu(b)
  expect(cloudGpu.containers).toEqual([idle])
  expect(cloudGpu.stale).toBe(true)
  b.getCloudGpuStatus.mockResolvedValue({ supported: true, unsupported: null, containers: [] })
  await refreshCloudGpu(b)
  expect(cloudGpu.containers).toEqual([])
  expect(cloudGpu.stale).toBe(false)
})

it('stops an idle GPU without asking, and says stopping until it is gone', async () => {
  const b = backend()
  await refreshCloudGpu(b)
  await stopCloudGpu('render', b)
  expect(app.modals).toHaveLength(0)
  expect(b.stopCloudGpu).toHaveBeenCalledWith({ provider: 'modal', profileId: 'mc-1', role: 'render' })
  // The released container drains before it leaves; the row keeps saying so.
  expect(cloudGpu.containers).toEqual([idle])
  expect(isStopping('render')).toBe(true)
  await stopCloudGpu('render', b)
  expect(b.stopCloudGpu).toHaveBeenCalledTimes(1)
  b.getCloudGpuStatus.mockResolvedValue({ supported: true, unsupported: null, containers: [] })
  await refreshCloudGpu(b)
  expect(isStopping('render')).toBe(false)
  expect(cloudGpu.stopping).toEqual({})
})

it('stops saying stopping after the settle time even if the container is still listed', async () => {
  vi.useFakeTimers()
  const b = backend()
  await refreshCloudGpu(b)
  await stopCloudGpu('render', b)
  expect(isStopping('render')).toBe(true)
  vi.advanceTimersByTime(STOP_SETTLE_MS)
  expect(isStopping('render')).toBe(false)
  await refreshCloudGpu(b)
  expect(cloudGpu.stopping).toEqual({})
})

it('stops one container only, and names what that stop cancels', async () => {
  const b = backend()
  b.getCloudGpuStatus.mockResolvedValue({ supported: true, unsupported: null, containers: [idle, analysis] })
  await refreshCloudGpu(b)
  cloud.jobs = [job]

  // A render running does not make stopping the analysis container ask.
  b.stopCloudGpu.mockResolvedValue({ supported: true, stopped: ['analysis'], cancelledJobs: 0 })
  await stopCloudGpu('analysis', b)
  expect(app.modals).toHaveLength(0)
  expect(b.stopCloudGpu).toHaveBeenLastCalledWith({ provider: 'modal', profileId: 'mc-1', role: 'analysis' })
  expect(isStopping('analysis')).toBe(true)
  expect(isStopping('render')).toBe(false)

  const declined = stopCloudGpu('render', b)
  expect(app.modals.at(-1)).toMatchObject({ titleKey: 'cloud.gpu.confirm.title', props: { bodyKey: 'cloud.gpu.confirm.bodyRender' } })
  closeModal('cancel')
  await declined
  expect(b.stopCloudGpu).toHaveBeenCalledTimes(1)

  b.stopCloudGpu.mockResolvedValue({ supported: true, stopped: ['render'], cancelledJobs: 1 })
  const accepted = stopCloudGpu('render', b)
  closeModal('stop')
  await accepted
  expect(b.stopCloudGpu).toHaveBeenLastCalledWith({ provider: 'modal', profileId: 'mc-1', role: 'render' })

  await stopCloudGpu(/** @type {any} */ ('seed'), b)
  expect(b.stopCloudGpu).toHaveBeenCalledTimes(2)
})

it('asks before stopping analysis that is running, in its own words', async () => {
  cloudGpu.containers = [analysis]
  onAnalysisRecord({ proposal_id: 'p-1', phase: { phase: 'submitted_tile', index: 0 } })
  const b = backend()
  const asked = stopCloudGpu('analysis', b)
  expect(app.modals.at(-1)?.props?.bodyKey).toBe('cloud.gpu.confirm.bodyAnalysis')
  closeModal('cancel')
  await asked
  expect(b.stopCloudGpu).not.toHaveBeenCalled()
})

it('writes nothing when the target changes while a stop is in flight', async () => {
  vi.useFakeTimers()
  /** @type {(value: any) => void} */
  let answer = () => {}
  const b = backend({ stopCloudGpu: vi.fn(() => new Promise((resolve) => { answer = resolve })) })
  startCloudGpu(b)
  flushSync()
  await vi.advanceTimersByTimeAsync(0)
  expect(cloudGpu.containers).toEqual([idle])

  const stopping = stopCloudGpu('render', b)
  expect(isStopping('render')).toBe(true)
  cloud.readiness = { ...cloud.readiness, target: { type: 'modal', profile_id: 'mc-2' } }
  flushSync()
  expect(cloudGpu.stopping).toEqual({})
  await vi.advanceTimersByTimeAsync(0)
  const reads = b.getCloudGpuStatus.mock.calls.length
  const shown = cloudGpu.containers

  answer({ supported: false, stopped: [], cancelledJobs: 0 })
  await stopping
  expect(cloudGpu.supported).toBe(true)
  expect(cloudGpu.containers).toBe(shown)
  expect(cloudGpu.stopping).toEqual({})
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(reads)
})

it('says so when the stop fails, and lets it be tried again', async () => {
  const b = backend({ stopCloudGpu: vi.fn().mockRejectedValue(new Error('gateway_error')) })
  cloudGpu.containers = [idle]
  await stopCloudGpu('render', b)
  expect(app.notices.at(-1)).toMatchObject({ key: 'cloud.gpu.notice.stopFailed', tone: 'warn' })
  expect(isStopping('render')).toBe(false)
  await stopCloudGpu('render', b)
  expect(b.stopCloudGpu).toHaveBeenCalledTimes(2)
})

it('counts a remote analysis as running from confirmation to its ending', () => {
  onAnalysisRecord({ proposal_id: 'p-1', phase: { phase: 'proposed' } })
  expect(runningAnalyses()).toBe(0)
  onAnalysisRecord({ proposal_id: 'p-1', phase: { phase: 'submitted_tile', index: 0 } })
  expect(runningAnalyses()).toBe(1)
  onAnalysisRecord({ proposal_id: 'p-1', phase: { phase: 'attached_evidence' } })
  expect(runningAnalyses()).toBe(0)
  onAnalysisRecord({ proposal_id: 42, phase: { phase: 'confirmed' } })
  onAnalysisRecord(null)
  expect(cloudGpu.analyses).toEqual({})
})

it('shows batch cloud clean and cloud detection runs until their native end', async () => {
  let emit
  const off = vi.fn()
  const b = backend({
    getCloudGpuStatus: vi.fn().mockResolvedValue({ supported: true, unsupported: null, containers: [] }),
    subscribe: vi.fn((handler) => { emit = handler; return off }),
  })
  trackCloudRun('clean-1', 'render', b)
  trackCloudRun('detect-1', 'analysis', b)
  expect(cloudGpu.runs).toEqual({
    'clean-1': { role: 'render', target: { provider: 'modal', profileId: 'mc-1' } },
    'detect-1': { role: 'analysis', target: { provider: 'modal', profileId: 'mc-1' } },
  })
  await vi.waitFor(() => expect(b.getCloudGpuStatus).toHaveBeenCalled())
  emit({ type: 'run-finished', runId: 'detect-1' })
  expect(cloudGpu.runs).toEqual({ 'clean-1': { role: 'render', target: { provider: 'modal', profileId: 'mc-1' } } })
  finishCloudRun('clean-1', b)
  expect(cloudGpu.runs).toEqual({})
  expect(off).toHaveBeenCalledTimes(2)
})

it('counts an idle GPU down from when its status arrived', () => {
  cloudGpu.receivedAt = 1_000
  cloudGpu.now = 31_000
  expect(scaledownLeftMs(idle)).toBe(60_000)
  cloudGpu.now = 500_000
  expect(scaledownLeftMs(idle)).toBe(0)
  expect(scaledownLeftMs({ ...idle, state: 'busy', scaledownInMs: null })).toBeNull()
})

it('polls only while something is up or running, and stops after', async () => {
  vi.useFakeTimers()
  const b = backend()
  startCloudGpu(b)
  flushSync()
  await vi.advanceTimersByTimeAsync(0)
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(1)
  flushSync()

  await vi.advanceTimersByTimeAsync(GPU_POLL_MS)
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(2)

  // The GPU scaled down: the next answer is empty, and with nothing running the asking stops.
  b.getCloudGpuStatus.mockResolvedValue({ supported: true, unsupported: null, containers: [] })
  await vi.advanceTimersByTimeAsync(GPU_POLL_MS)
  flushSync()
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(3)
  await vi.advanceTimersByTimeAsync(GPU_POLL_MS * 4)
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(3)

  // A render starting is asked about at once, and polled while it runs.
  cloud.jobs = [job]
  flushSync()
  await vi.advanceTimersByTimeAsync(0)
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(4)
  await vi.advanceTimersByTimeAsync(GPU_POLL_MS)
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(5)
})

it('keeps polling through failed reads while a GPU was last seen up', async () => {
  vi.useFakeTimers()
  const b = backend()
  startCloudGpu(b)
  flushSync()
  await vi.advanceTimersByTimeAsync(0)
  flushSync()
  b.getCloudGpuStatus.mockRejectedValue(new Error('gateway_unreachable'))
  await vi.advanceTimersByTimeAsync(GPU_POLL_MS * 3)
  flushSync()
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(4)
  expect(cloudGpu.stale).toBe(true)
  expect(cloudGpu.containers).toEqual([idle])

  b.getCloudGpuStatus.mockResolvedValue({ supported: true, unsupported: null, containers: [] })
  await vi.advanceTimersByTimeAsync(GPU_POLL_MS)
  flushSync()
  expect(cloudGpu.stale).toBe(false)
  await vi.advanceTimersByTimeAsync(GPU_POLL_MS * 3)
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(5)
})

it('stops asking a deployment that cannot say', async () => {
  vi.useFakeTimers()
  const b = backend({ getCloudGpuStatus: vi.fn().mockResolvedValue({ supported: false, unsupported: 'provider', containers: [] }) })
  cloud.jobs = [job]
  startCloudGpu(b)
  flushSync()
  await vi.advanceTimersByTimeAsync(0)
  flushSync()
  await vi.advanceTimersByTimeAsync(GPU_POLL_MS * 3)
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(1)
})

it('keeps and polls detection on A while denoise on B runs in another project', async () => {
  vi.useFakeTimers()
  const a = { provider: 'modal', profileId: 'mc-1' }
  const bTarget = { provider: 'modal', profileId: 'mc-2' }
  const b = backend({ getCloudGpuStatus: vi.fn(async () => ({ supported: true, containers: [analysis] })) })
  startCloudGpu(b); flushSync(); await vi.advanceTimersByTimeAsync(0)
  trackCloudRun('detect-project-A', 'analysis', b, a)
  cloud.readiness = { ...cloud.readiness, target: { type: 'modal', profile_id: 'mc-2' } }
  flushSync()
  trackCloudRun('denoise-project-B', 'analysis', b, bTarget)
  await vi.advanceTimersByTimeAsync(0)
  expect(gpuDeployments().map((d) => d.target.profileId)).toEqual(['mc-1', 'mc-2'])
  expect(cloudGpu.runs['detect-project-A'].target).toEqual(a)
  expect(cloudGpu.runs['denoise-project-B'].target).toEqual(bTarget)
  b.getCloudGpuStatus.mockClear()
  await vi.advanceTimersByTimeAsync(GPU_POLL_MS)
  expect(b.getCloudGpuStatus).toHaveBeenCalledWith(a)
  expect(b.getCloudGpuStatus).toHaveBeenCalledWith(bTarget)
  finishCloudRun('detect-project-A', b)
  expect(cloudGpu.runs['denoise-project-B'].target).toEqual(bTarget)
})

it('work on A does not make an idle GPU on B ask before stopping', async () => {
  const a = { provider: 'modal', profileId: 'mc-1' }
  const bTarget = { provider: 'modal', profileId: 'mc-2' }
  const b = backend({ stopCloudGpu: vi.fn().mockResolvedValue({ supported: true, stopped: ['analysis'] }) })
  trackCloudRun('detect-A', 'analysis', b, a)
  cloud.readiness = { ...cloud.readiness, target: { type: 'modal', profile_id: 'mc-2' } }
  await refreshCloudGpu(b, bTarget)
  await stopCloudGpu('analysis', b, bTarget)
  expect(app.modals).toHaveLength(0)
  expect(b.stopCloudGpu).toHaveBeenCalledWith({ ...bTarget, role: 'analysis' })
  expect(cloudGpu.runs['detect-A']).toBeDefined()
})

it('an explicit stop remains on A when B becomes the selected default', async () => {
  const a = { provider: 'modal', profileId: 'mc-1' }
  let answer
  const b = backend({ stopCloudGpu: vi.fn(() => new Promise((resolve) => { answer = resolve })) })
  await refreshCloudGpu(b, a)
  const stopping = stopCloudGpu('render', b, a)
  cloud.readiness = { ...cloud.readiness, target: { type: 'modal', profile_id: 'mc-2' } }
  await refreshCloudGpu(b)
  answer({ supported: true, stopped: ['render'] })
  await stopping
  expect(isStopping('render', a)).toBe(true)
  expect(isStopping('render', { provider: 'modal', profileId: 'mc-2' })).toBe(false)
  expect(b.stopCloudGpu).toHaveBeenCalledWith({ ...a, role: 'render' })
})

it('a late status for A updates A without replacing B or losing A', async () => {
  let answer
  const a = { provider: 'modal', profileId: 'mc-1' }
  const b = backend({ getCloudGpuStatus: vi.fn((target) => target.profileId === 'mc-1'
    ? new Promise((resolve) => { answer = resolve })
    : Promise.resolve({ supported: true, containers: [idle] })) })
  const first = refreshCloudGpu(b, a)
  cloud.readiness = { ...cloud.readiness, target: { type: 'modal', profile_id: 'mc-2' } }
  await refreshCloudGpu(b)
  answer({ supported: true, containers: [analysis] }); await first
  expect(cloudGpu.containers).toEqual([idle])
  expect(gpuDeployments().find((d) => d.target.profileId === 'mc-1').containers).toEqual([analysis])
})

it('analysis events retain their own profile after selection changes', () => {
  onAnalysisRecord({ proposal_id: 'p-A', provider: 'modal', profile_id: 'mc-1', phase: { phase: 'submitted_tile' } })
  cloud.readiness = { ...cloud.readiness, target: { type: 'modal', profile_id: 'mc-2' } }
  expect(runningAnalyses({ provider: 'modal', profileId: 'mc-1' })).toBe(1)
  expect(runningAnalyses({ provider: 'modal', profileId: 'mc-2' })).toBe(0)
  onAnalysisRecord({ proposal_id: 'p-A', provider: 'modal', profile_id: 'mc-1', phase: { phase: 'run_page_complete' } })
  expect(runningAnalyses()).toBe(0)
})

it('coalesces slow status reads independently for each endpoint', async () => {
  let answer
  const a = { provider: 'modal', profileId: 'mc-1' }
  const bTarget = { provider: 'modal', profileId: 'mc-2' }
  const b = backend({ getCloudGpuStatus: vi.fn((target) => target.profileId === 'mc-1'
    ? new Promise((resolve) => { answer = resolve }) : Promise.resolve({ supported: true, containers: [idle] })) })
  const first = refreshCloudGpu(b, a)
  const duplicate = refreshCloudGpu(b, a)
  await refreshCloudGpu(b, bTarget)
  expect(b.getCloudGpuStatus).toHaveBeenCalledTimes(2)
  answer({ supported: true, containers: [analysis] })
  await Promise.all([first, duplicate])
  expect(gpuDeployments().find((d) => d.target.profileId === 'mc-1').containers).toEqual([analysis])
})
