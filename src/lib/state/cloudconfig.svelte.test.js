import { afterEach, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { app } from './app.svelte.js'
import { cloud, cloudCleanAvailable, onAttemptEvent, trackCloudJob, stopCloud } from './cloud.svelte.js'
import { checkCloudDenoise, cloudOffered, offeredPresets, resetDenoiseState } from './denoise.svelte.js'
import { setCloudAllowed } from './session.svelte.js'
import { checkConfiguration, configurationFailed, configurationKey, loadCloudConfiguration,
  rememberedConfiguration, rememberConfiguration, resetCloudConfiguration } from './cloudconfig.svelte.js'

const readiness = (id = 'first', revision = 1) => ({
  allowed: true, configured: true, ready: true,
  target: { type: 'modal', profile_id: id },
  profile: { endpointUrl: `https://${id}.example.test/mc/v1`, updatedAtMs: revision },
  endpoints: ['first', 'second'].map((name) => ({ provider: 'modal', id: name,
    endpointUrl: `https://${name}.example.test/mc/v1`, updatedAtMs: name === id ? revision : 1 })),
})
function backend() {
  const settings = {}
  return { settings, readSettings: vi.fn(async () => settings),
    writeSettings: vi.fn(async (patch) => { Object.assign(settings, patch); return settings }) }
}
afterEach(() => {
  stopCloud()
  resetDenoiseState()
  setCloudAllowed(false)
  setBackend(null)
  app.notices = []
})

it('restores each deployment across restart and uses memory while offline', async () => {
  const api = backend()
  const first = configurationKey(readiness())
  const second = configurationKey(readiness('second'))
  await rememberConfiguration(api, first, 'denoise', { state: 'ready', presets: ['mangajanai-2x'], modelId: null })
  await rememberConfiguration(api, second, 'denoise', { state: 'missing', presets: [], modelId: null })
  await rememberConfiguration(api, first, 'clean', { state: 'ready', presets: [], modelId: 'model-first' })
  resetCloudConfiguration()
  const read = vi.fn().mockRejectedValue(new Error('gateway_unreachable'))
  expect(await checkConfiguration(api, first, 'denoise', read)).toMatchObject({ state: 'ready', presets: ['mangajanai-2x'] })
  expect(read).not.toHaveBeenCalled()
  expect(rememberedConfiguration(second).denoise.state).toBe('missing')
  expect(rememberedConfiguration(first).clean.modelId).toBe('model-first')
  expect(await checkConfiguration(api, first, 'denoise', read, true)).toMatchObject({ state: 'ready' })
  expect(rememberedConfiguration(configurationKey(readiness('first', 2)))).toBeNull()
})

it('updates and notifies for setup failures only, on the original deployment and operation', async () => {
  const api = backend(), first = configurationKey(readiness()), second = configurationKey(readiness('second'))
  await rememberConfiguration(api, first, 'denoise', { state: 'ready', presets: ['mangajanai-2x'] })
  await rememberConfiguration(api, second, 'denoise', { state: 'ready', presets: ['mangajanai-4x'] })
  cloud.readiness = readiness('second')
  for (const code of ['gateway_unreachable', 'cloud_denoise_unsupported_page', 'capability_unavailable: denoise GPU unavailable']) {
    expect(await configurationFailed(api, first, 'denoise', code)).toBe(false)
  }
  expect(await configurationFailed(api, first, 'denoise', 'capability_unavailable: gateway denoise is not configured')).toBe(true)
  expect(rememberedConfiguration(first).denoise.state).toBe('missing')
  expect(rememberedConfiguration(second).denoise.state).toBe('ready')
  expect(app.notices.filter((notice) => notice.key === 'cloud.configuration.denoiseChanged')).toHaveLength(1)
  await configurationFailed(api, first, 'denoise', 'capability_unavailable: gateway denoise is not configured')
  expect(app.notices.filter((notice) => notice.key === 'cloud.configuration.denoiseChanged')).toHaveLength(1)
  await configurationFailed(api, second, 'clean', 'weights_unavailable')
  expect(cloudCleanAvailable()).toBe(false)
  expect(rememberedConfiguration(second).denoise.state).toBe('ready')
  expect(api.settings.cloudConfigurationMemory[second].clean.state).toBe('missing')
})

it('a delayed successful probe cannot overwrite a newer configuration failure', async () => {
  const api = backend(), key = configurationKey(readiness())
  let resolve
  const read = vi.fn(() => new Promise((done) => { resolve = done }))
  const checking = checkConfiguration(api, key, 'clean', read)
  await vi.waitFor(() => expect(read).toHaveBeenCalledOnce())
  await configurationFailed(api, key, 'clean', 'weights_unavailable')
  resolve({ state: 'ready', modelId: 'old-model', presets: [] })
  expect(await checking).toMatchObject({ state: 'missing' })
  // An explicit later connection test may verify repaired setup.
  expect(await checkConfiguration(api, key, 'clean', async () => ({ state: 'ready', modelId: 'repaired' }), true))
    .toMatchObject({ state: 'ready', modelId: 'repaired' })
})

it('switching denoise profiles restores their own presets and ignores a delayed previous answer', async () => {
  const api = backend()
  let finishFirst
  api.cloudDenoisePresets = vi.fn(({ profileId }) => profileId === 'first'
    ? new Promise((resolve) => { finishFirst = resolve }) : Promise.resolve(['mangajanai-4x']))
  setBackend(api)
  setCloudAllowed(true)
  cloud.checked = true
  cloud.readiness = readiness()
  const first = checkCloudDenoise()
  await vi.waitFor(() => expect(api.cloudDenoisePresets).toHaveBeenCalledWith({ provider: 'modal', profileId: 'first' }))
  cloud.readiness = readiness('second')
  await checkCloudDenoise()
  finishFirst(['mangajanai-2x'])
  await first
  expect(offeredPresets('cloud').map((preset) => preset.id)).toEqual(['mangajanai-4x'])
  cloud.readiness = readiness()
  expect(offeredPresets('cloud').map((preset) => preset.id)).toEqual(['mangajanai-2x'])
  await configurationFailed(api, configurationKey(readiness()), 'denoise', 'capability_unavailable: gateway denoise is not configured')
  expect(cloudOffered()).toBe(false)
  cloud.readiness = readiness('second')
  expect(cloudOffered()).toBe(true)
})

it('rejects damaged saved capability records and reports persistence failures', async () => {
  const api = backend(), key = configurationKey(readiness())
  api.settings.cloudConfigurationMemory = { broken: { clean: { state: 'ready' } },
    [key]: { denoise: { state: 'ready', presets: [null, 2, 'mangajanai-2x'] }, clean: { state: 'anything' } } }
  await loadCloudConfiguration(api)
  expect(rememberedConfiguration('broken')).toBeNull()
  expect(rememberedConfiguration(key).denoise.presets).toEqual(['mangajanai-2x'])
  expect(rememberedConfiguration(key).clean).toBeUndefined()
  api.writeSettings.mockRejectedValue(new Error('disk full'))
  await configurationFailed(api, key, 'clean', 'weights_unavailable')
  expect(app.notices.some((notice) => notice.key === 'cloud.configuration.saveFailed')).toBe(true)
})

it('a running cleaner updates its original profile after the user switches', async () => {
  const api = backend(), first = configurationKey(readiness()), second = configurationKey(readiness('second'))
  setBackend(api)
  cloud.readiness = readiness()
  await rememberConfiguration(api, first, 'clean', { state: 'ready', modelId: 'first-model', presets: [] })
  const attemptId = `att-${'b'.repeat(24)}`
  trackCloudJob({ attemptId })
  cloud.readiness = readiness('second')
  onAttemptEvent({ attemptId, phase: 'failed', errorCode: 'weights_unavailable' })
  await vi.waitFor(() => expect(api.settings.cloudConfigurationMemory[first].clean.state).toBe('missing'))
  expect(rememberedConfiguration(first).clean.modelId).toBe('first-model')
  expect(rememberedConfiguration(second)).toBeNull()
  expect(cloudCleanAvailable()).toBe(true)
})

it('remembers invalid access and allows a repaired deployment to be rechecked', async () => {
  const api = backend(), key = configurationKey(readiness())
  await rememberConfiguration(api, key, 'clean', { state: 'ready', modelId: 'model', presets: [] })
  expect(await configurationFailed(api, key, 'clean', 'gateway_unauthorized')).toBe(true)
  expect(rememberedConfiguration(key).clean.state).toBe('missing')
  await checkConfiguration(api, key, 'clean', async () => ({ state: 'ready', modelId: 'model', presets: [] }), true)
  expect(rememberedConfiguration(key).clean.state).toBe('ready')
})

it('does not restore an old backend from a probe that finishes after reset', async () => {
  const old = backend(), next = backend(), key = configurationKey(readiness())
  let resolve
  const read = vi.fn(() => new Promise((done) => { resolve = done }))
  const pending = checkConfiguration(old, key, 'clean', read)
  await vi.waitFor(() => expect(read).toHaveBeenCalledOnce())
  resetCloudConfiguration()
  await rememberConfiguration(next, key, 'clean', { state: 'missing', presets: [] })
  resolve({ state: 'ready', modelId: 'old', presets: [] })
  expect(await pending).toBeNull()
  expect(rememberedConfiguration(key).clean.state).toBe('missing')
  expect(old.writeSettings).not.toHaveBeenCalled()
})

it('keeps unseen saved profiles intact when the initial settings read fails', async () => {
  const api = backend(), key = configurationKey(readiness())
  api.readSettings.mockRejectedValue(new Error('temporarily unreadable'))
  await rememberConfiguration(api, key, 'clean', { state: 'ready', modelId: 'model', presets: [] })
  expect(rememberedConfiguration(key).clean.state).toBe('ready')
  expect(api.writeSettings).not.toHaveBeenCalled()
  expect(app.notices.some((notice) => notice.key === 'cloud.configuration.saveFailed')).toBe(true)
})
