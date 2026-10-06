import { describe, expect, it, vi } from 'vitest'
import { cleanupErrorKey, removeEndpoint, setupErrorKey } from './provisioning.svelte.js'

vi.mock('../state/cloud.svelte.js', () => ({ refreshCloudReadiness: vi.fn() }))

function backend() {
  const calls = []
  let config = { modalProfiles: { studio: { id: 'studio', endpointUrl: 'https://example.modal.run/mc/v1' } }, beamProfiles: {}, selectedTarget: { type: 'modal', profile_id: 'studio' } }
  return {
    calls,
    readInferenceConfig: async () => config,
    deleteCloudSecret: vi.fn(async ({ role }) => {
      expect(config.modalProfiles.studio).toBeTruthy()
      calls.push(role)
    }),
    writeInferenceConfig: vi.fn(async ({ config: next }) => { calls.push('config'); config = next }),
  }
}

it('forgets runtime and billing credentials before removing their origin-bound profile', async () => {
  const api = backend()
  expect(await removeEndpoint({ provider: 'modal', profileId: 'studio' }, api)).toBe(true)
  expect(api.calls).toEqual(['runtime', 'setup', 'config'])
  expect((await api.readInferenceConfig()).selectedTarget).toEqual({ type: 'local' })
})

it('keeps the profile accessible when credential removal fails', async () => {
  const api = backend()
  api.deleteCloudSecret.mockRejectedValue(new Error('keychain unavailable'))
  await expect(removeEndpoint({ provider: 'modal', profileId: 'studio' }, api)).rejects.toThrow('keychain unavailable')
  expect(api.writeInferenceConfig).not.toHaveBeenCalled()
  expect((await api.readInferenceConfig()).modalProfiles.studio).toBeTruthy()
})

it('says a refused plan only after Review, and a key for another account after Resume or Clean up', () => {
  expect(setupErrorKey('ERR_UNAPPROVED_PLAN', 'apply')).toBe('settings.cloud.setup.error.planChanged')
  expect(setupErrorKey('ERR_UNAPPROVED_PLAN', 'resume')).toBe('settings.cloud.setup.error.wrongAccount')
  expect(setupErrorKey('ERR_VALIDATION_ERROR', 'resume')).toBe('settings.cloud.setup.error.validation')
  expect(cleanupErrorKey('ERR_UNAPPROVED_PLAN')).toBe('settings.cloud.setup.error.cleanupWrongAccount')
  expect(cleanupErrorKey('ERR_EXECUTION_FAILED')).toBe('settings.cloud.setup.error.cleanup')
})

describe('installations that already exist', () => {
  const entry = (/** @type {Record<string, unknown>} */ over = {}) => ({
    installation_id: 'mc-old123', app_name: 'mc-old123', created_at: 1_760_000_000, deployed_at: null,
    weights_checked: true, models_ready: ['owner/model-a'], analysis_ready: ['text_regions_rt@1'],
    options: { gpu: 'A10', idle_seconds: 300, model_id: 'owner/model-a', analysis_models: [] },
    on_this_computer: false, ...over,
  })

  it('keeps well formed entries and leaves out the rest', async () => {
    const { existingInstallationsOf } = await import('./provisioning.svelte.js')
    const list = existingInstallationsOf({
      existing_installations: [
        entry(),
        entry({ installation_id: 'MC-BAD', app_name: 'MC-BAD' }),
        entry({ installation_id: 'mc-renamed', app_name: 'mc-other' }),
        entry(),
        entry({ installation_id: 'mc-half01', app_name: 'mc-half01', options: { gpu: 'L4' }, created_at: -1,
          models_ready: ['../x', 'owner/model-a'], analysis_ready: ['sam3'], on_this_computer: 'yes' }),
      ],
    })
    expect(list.map((item) => item.installationId)).toEqual(['mc-old123', 'mc-half01'])
    expect(list[0]).toEqual({
      installationId: 'mc-old123', createdAt: 1_760_000_000, deployedAt: null, weightsChecked: true,
      modelsReady: ['owner/model-a'], analysisReady: ['text_regions_rt@1'],
      options: { gpu: 'A10', idle_seconds: 300, model_id: 'owner/model-a', analysis_models: [] }, onThisComputer: false,
    })
    expect(list[1]).toMatchObject({ createdAt: null, options: null, modelsReady: ['owner/model-a'], analysisReady: [], onThisComputer: false })
    expect(existingInstallationsOf({})).toEqual([])
    expect(existingInstallationsOf(null)).toEqual([])
  })

  it('says a reuse downloads nothing only when the model and every graph are there', async () => {
    const { existingInstallationsOf, reuseDownloadsNothing } = await import('./provisioning.svelte.js')
    const [found] = existingInstallationsOf({ existing_installations: [entry()] })
    expect(reuseDownloadsNothing(found, { model_id: 'owner/model-a', analysis_models: ['text_regions_rt@1'] })).toBe(true)
    expect(reuseDownloadsNothing(found, { model_id: 'owner/model-a', analysis_models: ['text_mask_sam_ts@1'] })).toBe(false)
    expect(reuseDownloadsNothing(found, { model_id: 'owner/model-b' })).toBe(false)
    expect(reuseDownloadsNothing({ ...found, weightsChecked: false }, { model_id: 'owner/model-a' })).toBe(false)
  })

  it('keeps a recorded Page denoise choice only as a boolean', async () => {
    const { cleanOptions, existingInstallationsOf } = await import('./provisioning.svelte.js')
    expect(cleanOptions({ denoise: true })).toEqual({ denoise: true })
    expect(cleanOptions({ denoise: false })).toEqual({ denoise: false })
    expect(cleanOptions({ denoise: 'yes' })).toEqual({})
    expect(cleanOptions({ denoise: 1 })).toEqual({})
    // Only a routing region the helper knows is sent to it.
    expect(cleanOptions({ routing_region: 'ap-south' })).toEqual({ routing_region: 'ap-south' })
    expect(cleanOptions({ routing_region: 'mars' })).toEqual({})
    expect(cleanOptions({ routing_region: 7 })).toEqual({})
    const [on, older] = existingInstallationsOf({ existing_installations: [
      entry({ options: { gpu: 'A10', idle_seconds: 300, model_id: 'owner/model-a', analysis_models: [], denoise: true } }),
      entry({ installation_id: 'mc-old456', app_name: 'mc-old456' }),
    ] })
    expect(on.options?.denoise).toBe(true)
    // An older helper recorded no denoise: the other choices still count.
    expect(older.options).toEqual({ gpu: 'A10', idle_seconds: 300, model_id: 'owner/model-a', analysis_models: [] })
  })

  it('says a reuse with Page denoise on downloads something unless it recorded denoise on', async () => {
    const { existingInstallationsOf, reuseDownloadsNothing } = await import('./provisioning.svelte.js')
    const withDenoise = { gpu: 'A10', idle_seconds: 300, model_id: 'owner/model-a', analysis_models: [], denoise: true }
    const [off, on, unknown] = existingInstallationsOf({ existing_installations: [
      entry({ options: { ...withDenoise, denoise: false } }),
      entry({ installation_id: 'mc-den123', app_name: 'mc-den123', options: withDenoise }),
      entry({ installation_id: 'mc-none12', app_name: 'mc-none12', options: null }),
    ] })
    const wanted = { model_id: 'owner/model-a', denoise: true }
    expect(reuseDownloadsNothing(off, wanted)).toBe(false)
    expect(reuseDownloadsNothing(unknown, wanted)).toBe(false)
    expect(reuseDownloadsNothing(on, wanted)).toBe(true)
    expect(reuseDownloadsNothing(off, { model_id: 'owner/model-a', denoise: false })).toBe(true)
    expect(reuseDownloadsNothing(off, { model_id: 'owner/model-a' })).toBe(true)
  })

  it('finds the endpoints setup made on this computer, not ones added by hand', async () => {
    const { setupsOnThisComputer } = await import('./provisioning.svelte.js')
    expect(setupsOnThisComputer({
      modalProfiles: {
        'mc-here01': { name: 'Modal (mc-here01)', endpointUrl: 'https://studio--mc-here01-gateway.modal.run/mc/v1' },
        ep_abc123: { name: 'Manual' },
      },
      beamProfiles: { 'mc-beam01': { name: '  ' } },
    })).toEqual([
      { provider: 'modal', installationId: 'mc-here01', name: 'Modal (mc-here01)', account: 'studio' },
      { provider: 'beam', installationId: 'mc-beam01', name: 'mc-beam01', account: null },
    ])
    expect(setupsOnThisComputer(null)).toEqual([])
  })

  it('reads the Modal account from the endpoint host, and nothing else', async () => {
    const { endpointAccount } = await import('./provisioning.svelte.js')
    expect(endpointAccount('modal', 'https://studio--mc-def456-gateway.modal.run/mc/v1')).toBe('studio')
    expect(endpointAccount('modal', 'https://Team-Dev--mc-ab12cd-gateway.modal.run./mc/v1')).toBe('team-dev')
    expect(endpointAccount('modal', 'https://example.com/mc/v1')).toBeNull()
    expect(endpointAccount('modal', 'https://gateway.modal.run/mc/v1')).toBeNull()
    expect(endpointAccount('modal', 'not a url')).toBeNull()
    expect(endpointAccount('beam', 'https://ws--mc-ab12cd-gateway.modal.run/mc/v1')).toBeNull()
  })
})
