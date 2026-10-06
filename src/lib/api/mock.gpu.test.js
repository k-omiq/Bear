/**
 * The mock's cloud GPU commands, held to `inference/gpu.rs`: gated on the
 * permission and the configured profile, idempotent stop, and the unsupported
 * answers a Beam or an older deployment gives.
 */

import { expect, it } from 'vitest'

import { readCloudReadiness } from './backend.js'
import { createMockBackend } from './mock.js'

const timing = { method: 0, cloud: 0, provision: 0, region: 0, pageTail: 0 }
const target = { provider: 'modal', profileId: 'mc-mock-gpu' }

it('reports an idle GPU for the ready profile the knob selects, and stops it once', async () => {
  const mock = createMockBackend({ timing, cloudGpuScenario: 'idle' })
  const readiness = await readCloudReadiness(mock)
  expect(readiness.ready).toBe(true)
  expect(readiness.target).toEqual({ type: 'modal', profile_id: 'mc-mock-gpu' })

  const status = await mock.getCloudGpuStatus(target)
  expect(status.supported).toBe(true)
  expect(status.containers).toHaveLength(1)
  expect(status.containers[0]).toMatchObject({ role: 'render', gpu: 'L4', state: 'idle', listPriceUsdPerHour: 0.8 })
  expect(status.containers[0].scaledownInMs).toBeGreaterThan(0)

  expect(await mock.stopCloudGpu(target)).toEqual({ supported: true, stopped: ['render'], cancelledJobs: 0 })
  expect(await mock.stopCloudGpu(target)).toEqual({ supported: true, stopped: [], cancelledJobs: 0 })
  expect((await mock.getCloudGpuStatus(target)).containers).toEqual([])
})

it('stops only the role asked for, and refuses a role it does not know', async () => {
  const mock = createMockBackend({ timing, cloudGpuScenario: 'idle' })
  await readCloudReadiness(mock)
  expect(await mock.stopCloudGpu({ ...target, role: 'analysis' })).toEqual({ supported: true, stopped: [], cancelledJobs: 0 })
  expect((await mock.getCloudGpuStatus(target)).containers).toHaveLength(1)
  await expect(mock.stopCloudGpu({ ...target, role: 'seed' })).rejects.toThrow('invalid_role')
  expect(await mock.stopCloudGpu({ ...target, role: 'render' })).toEqual({ supported: true, stopped: ['render'], cancelledJobs: 0 })
  expect((await mock.getCloudGpuStatus(target)).containers).toEqual([])
})

it('refuses a profile that is not configured', async () => {
  const mock = createMockBackend({ timing, cloudGpuScenario: 'busy' })
  await expect(mock.getCloudGpuStatus({ provider: 'modal', profileId: 'other' })).rejects.toThrow('profile_missing')
  await expect(mock.stopCloudGpu({ provider: 'beam', profileId: 'mc-mock-gpu' })).rejects.toThrow('profile_missing')
})

it('answers unsupported for a deployment that cannot say', async () => {
  for (const scenario of ['unsupported', 'outdated']) {
    const mock = createMockBackend({ timing, cloudGpuScenario: scenario })
    const status = await mock.getCloudGpuStatus(target)
    expect(status).toEqual({ supported: false, unsupported: scenario === 'outdated' ? 'outdated' : 'provider', containers: [] })
    expect(await mock.stopCloudGpu(target)).toEqual({ supported: false, stopped: [], cancelledJobs: 0 })
  }
})

it('is off without the knob, like a desktop with cloud not allowed', async () => {
  const mock = createMockBackend({ timing })
  await expect(mock.getCloudGpuStatus(target)).rejects.toThrow('cloud_disabled')
})

it('can inspect and stop a configured profile after selecting Local', async () => {
  const mock = createMockBackend({ timing, cloudGpuScenario: 'idle' })
  const config = await mock.readInferenceConfig()
  await mock.writeInferenceConfig({ config: { ...config, selectedTarget: { type: 'local' } } })
  expect((await mock.getCloudGpuStatus(target)).containers).toHaveLength(1)
  expect((await mock.stopCloudGpu(target)).stopped).toEqual(['render'])
})
