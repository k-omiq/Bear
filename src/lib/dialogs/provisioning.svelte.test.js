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

  it('takes a setup with no cleaning model: nothing of a model to download, its choices kept', async () => {
    const { cleanOptions, existingInstallationsOf, NO_CLEAN_MODEL, reuseDownloadsNothing } = await import('./provisioning.svelte.js')
    expect(cleanOptions({ model_id: NO_CLEAN_MODEL })).toEqual({ model_id: 'none' })
    expect(cleanOptions({ model_id: 'nothing' })).toEqual({})
    const [found] = existingInstallationsOf({ existing_installations: [
      entry({ models_ready: [], options: { gpu: 'L4', idle_seconds: 120, model_id: 'none', analysis_models: ['text_regions_rt@1'] } }),
    ] })
    expect(found.options?.model_id).toBe(NO_CLEAN_MODEL)
    expect(reuseDownloadsNothing(found, { model_id: NO_CLEAN_MODEL, analysis_models: ['text_regions_rt@1'] })).toBe(true)
    expect(reuseDownloadsNothing(found, { model_id: NO_CLEAN_MODEL, analysis_models: ['text_mask_sam_ts@1'] })).toBe(false)
    // Adding a cleaning model to it later does download one.
    expect(reuseDownloadsNothing(found, { model_id: 'owner/model-a', analysis_models: [] })).toBe(false)
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

describe('Update all', () => {
  const targets = [
    { provider: /** @type {const} */ ('modal'), installationId: 'mc-team01', name: 'Studio A' },
    { provider: /** @type {const} */ ('modal'), installationId: 'mc-team02', name: 'Studio B' },
    { provider: /** @type {const} */ ('modal'), installationId: 'mc-team03', name: 'Home' },
  ]

  /**
   * A backend whose keychain holds a setup key for `saved`, and a runner that
   * answers each resume as `answers` says, one helper at a time.
   *
   * @param {{saved?: string[], answers?: Record<string, string|null>}} [spec]
   */
  function fakes({ saved = ['mc-team01', 'mc-team02', 'mc-team03'], answers = {} } = {}) {
    let running = 0
    /** @type {any[]} */
    const runs = []
    /** @type {(() => void)|null} */
    let cancel = null
    const api = {
      getCloudSecretSummary: vi.fn(async ({ profileId, role }) => ({ present: role === 'setup' && saved.includes(profileId) })),
      cancelCloudProvisioner: vi.fn(async () => {
        if (!cancel) return { cancelled: false }
        cancel()
        return { cancelled: true }
      }),
    }
    const runner = vi.fn(async (/** @type {any} */ spec) => {
      running += 1
      expect(running).toBe(1)
      runs.push(spec)
      const code = answers[spec.params.installation_id] ?? null
      const result = await new Promise((resolve) => {
        cancel = () => resolve('ERR_CANCELLED')
        // `wait` answers only when it is stopped.
        if (code !== 'wait') setTimeout(() => resolve(code), 0)
      })
      cancel = null
      running -= 1
      return result
        ? { success: false, error: { code: result } }
        : { success: true, data: { installation_id: spec.params.installation_id } }
    })
    return { api, runner, runs }
  }

  it('updates every setup one at a time from its saved key, keeping the default', async () => {
    const { setup, updateAll } = await import('./provisioning.svelte.js')
    const { api, runner, runs } = fakes()
    const run = await updateAll(targets, runner, /** @type {any} */ (api))
    expect(runs.map((spec) => spec.params.installation_id)).toEqual(['mc-team01', 'mc-team02', 'mc-team03'])
    for (const spec of runs) {
      expect(spec).toEqual({
        op: 'resume',
        provider: 'modal',
        params: {
          saved_setup_credential: spec.params.installation_id,
          installation_id: spec.params.installation_id,
          options: {},
          remember_setup_credential: true,
          keep_selected_target: true,
        },
      })
    }
    expect(run?.status).toBe('done')
    expect(run?.items.map((item) => item.state)).toEqual(['updated', 'updated', 'updated'])
    expect(setup.unfinished).toBeNull()
  })

  it('goes on past a failure, skips a setup with no saved key, and keeps the failed one to resume', async () => {
    const { setup, updateAll } = await import('./provisioning.svelte.js')
    const { api, runner, runs } = fakes({ saved: ['mc-team01', 'mc-team03'], answers: { 'mc-team01': 'ERR_EXECUTION_FAILED' } })
    const run = await updateAll(targets, runner, /** @type {any} */ (api))
    expect(runs.map((spec) => spec.params.installation_id)).toEqual(['mc-team01', 'mc-team03'])
    expect(run?.items).toMatchObject([
      { installationId: 'mc-team01', state: 'failed', reasonKey: 'settings.cloud.setup.error.failed' },
      { installationId: 'mc-team02', state: 'skipped', reasonKey: 'settings.inference.updateAll.noSavedKey' },
      { installationId: 'mc-team03', state: 'updated', reasonKey: null },
    ])
    // The later success does not take away the Resume the failure left.
    expect(setup.unfinished).toMatchObject({ provider: 'modal', installationId: 'mc-team01' })
  })

  it('a refused key is a failure with its reason, and leaves nothing to resume', async () => {
    const { setup, updateAll, forgetUnfinished } = await import('./provisioning.svelte.js')
    forgetUnfinished()
    const { api, runner } = fakes({ answers: { 'mc-team02': 'ERR_UNAPPROVED_PLAN' } })
    const run = await updateAll(targets, runner, /** @type {any} */ (api))
    expect(run?.items[1]).toMatchObject({ state: 'failed', reasonKey: 'settings.cloud.setup.error.wrongAccount' })
    expect(setup.unfinished).toBeNull()
  })

  it('Stop cancels the running setup as a single Stop does and starts no other', async () => {
    const { setup, stopUpdateAll, updateAll, forgetUnfinished } = await import('./provisioning.svelte.js')
    forgetUnfinished()
    const { api, runner, runs } = fakes({ answers: { 'mc-team01': 'wait' } })
    const pending = updateAll(targets, runner, /** @type {any} */ (api))
    await vi.waitFor(() => expect(setup.run?.status).toBe('running'))
    await stopUpdateAll(/** @type {any} */ (api))
    const run = await pending
    expect(api.cancelCloudProvisioner).toHaveBeenCalledTimes(1)
    expect(runs).toHaveLength(1)
    expect(run?.items).toMatchObject([
      { state: 'failed', reasonKey: 'settings.cloud.setup.error.cancelled' },
      { state: 'skipped', reasonKey: 'settings.inference.updateAll.notStarted' },
      { state: 'skipped', reasonKey: 'settings.inference.updateAll.notStarted' },
    ])
    expect(setup.unfinished).toMatchObject({ installationId: 'mc-team01' })
  })

  it('starts nothing while a setup runs, and says how it ended when no Settings watches', async () => {
    const { setup, updateAll, clearUpdateAll } = await import('./provisioning.svelte.js')
    const { app, clearNotices } = await import('../state/app.svelte.js')
    clearNotices()
    const { api, runner } = fakes()
    setup.run = /** @type {any} */ ({ id: -1, status: 'running' })
    expect(await updateAll(targets, runner, /** @type {any} */ (api))).toBeNull()
    expect(runner).not.toHaveBeenCalled()
    setup.run = null
    await updateAll(targets, runner, /** @type {any} */ (api))
    expect(app.notices.filter((notice) => notice.key.startsWith('notice.cloud.setup'))).toEqual([])
    expect(app.notices.at(-1)).toMatchObject({ key: 'notice.cloud.updateAllDone', params: { updated: 3, failed: 0, skipped: 0 } })
    clearUpdateAll()
    expect(setup.updateAll).toBeNull()
  })
})
