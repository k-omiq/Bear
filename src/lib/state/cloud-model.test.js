import { afterEach, describe, expect, it, vi } from 'vitest'
import { createMockBackend } from '../api/mock.js'
import { cloud, refreshCloudReadiness, stopCloud, retryCloudRecovery, abandonRecovered, dismissRecovered, onAttemptEvent,
  cloudEntries, cloudProfileChoices, parseCloudChoice, pickCloudChoice } from './cloud.svelte.js'
import { setCloudAllowed } from './session.svelte.js'
import { editor } from './editor.svelte.js'

afterEach(() => {
  stopCloud()
  setCloudAllowed(false)
})

describe('cloud model metadata', () => {
  it('loads the selected endpoint model as a read-only readiness followup', async () => {
    const backend = createMockBackend({ timing: { method: 0 } })
    await backend.writeSettings({ cloudEngines: 'allowed' })
    setCloudAllowed(true)
    const endpointUrl = 'https://studio.example.test/mc/v1'
    await backend.writeInferenceConfig({ config: {
      schemaVersion: 1,
      selectedTarget: { type: 'modal', profile_id: 'studio' },
      beamProfiles: {},
      modalProfiles: { studio: {
        id: 'studio', name: 'Studio', endpointUrl,
        canonicalOrigin: new URL(endpointUrl).origin, canonicalOriginFingerprint: '',
        createdAtMs: 1, updatedAtMs: 1,
      } },
    } })
    await backend.storeCloudSecret({ provider: 'modal', profileId: 'studio', role: 'runtime', secret: 'token', tokenId: 'token' })
    const metadata = vi.spyOn(backend, 'getCloudModelInfo')

    await refreshCloudReadiness(backend)
    await vi.waitFor(() => expect(cloud.model?.id).toBe('Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic'))
    expect(metadata).toHaveBeenCalledTimes(1)
    await refreshCloudReadiness(backend)
    expect(metadata).toHaveBeenCalledTimes(1)
  })

  it('ignores metadata from an endpoint deselected while its read was pending', async () => {
    const backend = createMockBackend({ timing: { method: 0 } })
    await backend.writeSettings({ cloudEngines: 'allowed' })
    setCloudAllowed(true)
    const profile = (id) => ({ id, name: id, endpointUrl: `https://${id}.example.test/mc/v1`,
      canonicalOrigin: `https://${id}.example.test`, canonicalOriginFingerprint: '',
      createdAtMs: 1, updatedAtMs: 1 })
    const config = (id) => ({ schemaVersion: 1, selectedTarget: { type: 'modal', profile_id: id },
      beamProfiles: {}, modalProfiles: { first: profile('first'), second: profile('second') } })
    await backend.writeInferenceConfig({ config: config('first') })
    for (const id of ['first', 'second']) {
      await backend.storeCloudSecret({ provider: 'modal', profileId: id, role: 'runtime', secret: 'token', tokenId: id })
    }
    let answerFirst
    const first = new Promise((resolve) => { answerFirst = resolve })
    vi.spyOn(backend, 'getCloudModelInfo').mockImplementation(({ profileId }) =>
      profileId === 'first' ? first : Promise.resolve({ pinnedModelId: 'Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32' }))

    await refreshCloudReadiness(backend)
    await vi.waitFor(() => expect(backend.getCloudModelInfo).toHaveBeenCalledWith({ provider: 'modal', profileId: 'first' }))
    await backend.writeInferenceConfig({ config: config('second') })
    await refreshCloudReadiness(backend)
    await vi.waitFor(() => expect(cloud.model?.id).toBe('Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32'))
    answerFirst({ pinnedModelId: 'Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic' })
    await Promise.resolve()
    expect(cloud.model.id).toBe('Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32')
  })
})


describe('every profile at once', () => {
  async function twoProfiles() {
    const backend = createMockBackend({ timing: { method: 0 } })
    await backend.writeSettings({ cloudEngines: 'allowed' })
    setCloudAllowed(true)
    const profile = (id, name) => ({ id, name, endpointUrl: `https://${id}.example.test/mc/v1`,
      canonicalOrigin: `https://${id}.example.test`, canonicalOriginFingerprint: '',
      createdAtMs: 1, updatedAtMs: 1 })
    await backend.writeInferenceConfig({ config: { schemaVersion: 1, selectedTarget: { type: 'modal', profile_id: 'klein' },
      beamProfiles: {}, modalProfiles: { klein: profile('klein', 'Studio'), qwen: profile('qwen', 'Edits') } } })
    for (const id of ['klein', 'qwen']) {
      await backend.storeCloudSecret({ provider: 'modal', profileId: id, role: 'runtime', secret: 'token', tokenId: id })
    }
    vi.spyOn(backend, 'getCloudModelInfo').mockImplementation(async ({ profileId }) => ({
      pinnedModelId: profileId === 'qwen' ? 'Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32' : 'Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32',
    }))
    return backend
  }

  it('lists each profile with its model, the default as Cloud and the rest by profile', async () => {
    const backend = await twoProfiles()
    await refreshCloudReadiness(backend)
    await vi.waitFor(() => expect(cloudProfileChoices().every((choice) => choice.modelId)).toBe(true))
    expect(cloudProfileChoices().map((choice) => [choice.name, choice.selected])).toEqual([['Edits', false], ['Studio', true]])
    expect(cloudEntries()?.map((entry) => entry.value)).toEqual(['cloud@modal:qwen', 'cloud'])
    expect(backend.getCloudModelInfo).toHaveBeenCalledTimes(2)
    expect(parseCloudChoice('cloud@modal:qwen')).toEqual({ provider: 'modal', profileId: 'qwen' })
    expect(parseCloudChoice('cloud')).toBeNull()
    expect(parseCloudChoice('cloud@modal:')).toBeNull()
  })

  it('switches the default on a pick, and keeps it when the switch is refused', async () => {
    const backend = await twoProfiles()
    await refreshCloudReadiness(backend)
    await expect(pickCloudChoice('cloud@modal:qwen', backend)).resolves.toBe(true)
    expect(cloud.readiness.target).toEqual({ type: 'modal', profile_id: 'qwen' })
    expect(cloudEntries()?.find((entry) => entry.value === 'cloud')?.choice?.profileId).toBe('qwen')

    vi.spyOn(backend, 'selectCloudProfile').mockRejectedValueOnce(new Error('failed to write'))
    await expect(pickCloudChoice('cloud@modal:klein', backend)).resolves.toBe(false)
    expect(cloud.readiness.target).toEqual({ type: 'modal', profile_id: 'qwen' })
    await expect(pickCloudChoice('cloud@beam:old', backend)).resolves.toBe(false)
  })

  it('offers no list for a single profile', async () => {
    const backend = await twoProfiles()
    const config = await backend.readInferenceConfig()
    delete config.modalProfiles.qwen
    await backend.writeInferenceConfig({ config })
    await refreshCloudReadiness(backend)
    expect(cloudEntries()).toBeNull()
  })
})

describe('explicit recovery actions', () => {
  it('requires duplicate-risk confirmation and preserves attention until backend success', async () => {
    const backend = createMockBackend({ timing: { method: 0 } })
    const reconcile = vi.spyOn(backend, 'reconcileCloudRecovery').mockResolvedValue({ decision: 'abandon' })
    const entry = { attemptId: 'attempt-stuck', reason: 'ambiguous' }
    cloud.recovery = { attached: [], stillRunning: [], needsAttention: [entry] }
    await expect(abandonRecovered(entry.attemptId, false, backend)).rejects.toThrow('confirmation')
    expect(reconcile).not.toHaveBeenCalled()
    reconcile.mockRejectedValueOnce(new Error('disk failed'))
    await expect(abandonRecovered(entry.attemptId, true, backend)).rejects.toThrow('disk failed')
    expect(cloud.recovery.needsAttention).toHaveLength(1)
    await abandonRecovered(entry.attemptId, true, backend)
    expect(reconcile).toHaveBeenLastCalledWith({ action: 'abandon', attemptId: entry.attemptId, acceptDuplicateRisk: true })
    expect(cloud.recovery.needsAttention).toEqual([])
  })

  it('accepts a recovered commit after uncertainty and ignores a late ambiguous report', async () => {
    const backend = createMockBackend({ timing: { method: 0 } })
    const attemptId = `att-${'a'.repeat(24)}`
    onAttemptEvent({ attemptId, phase: 'unknown', errorCode: 'submission_unknown' })
    vi.spyOn(backend, 'reconcileCloudRecovery').mockImplementation(async () => {
      onAttemptEvent({ attemptId, phase: 'committed' })
      return { attached: [], stillRunning: [], needsAttention: [{ attemptId, regionId: 'region', reason: 'ambiguous' }] }
    })
    await retryCloudRecovery(backend)
    expect(cloud.lastCommitAt).toBeGreaterThan(0)
    expect(cloud.recovery.needsAttention).toEqual([])
    expect(cloud.jobs).toEqual([])
  })

  it('reruns recovery on demand and persists repair acknowledgement', async () => {
    const backend = createMockBackend({ timing: { method: 0 } })
    const entry = { attemptId: 'attempt-repair', regionId: 'region', reason: 'repair_needed' }
    const reconcile = vi.spyOn(backend, 'reconcileCloudRecovery').mockResolvedValue({ attached: [], stillRunning: [], needsAttention: [entry] })
    await retryCloudRecovery(backend)
    expect(reconcile).toHaveBeenLastCalledWith({ apply: true })
    expect(cloud.recovery.needsAttention).toHaveLength(1)
    await dismissRecovered(entry.attemptId, backend)
    expect(reconcile).toHaveBeenLastCalledWith({ action: 'acknowledge', attemptId: entry.attemptId })
    expect(cloud.recovery.needsAttention).toEqual([])
    reconcile.mockRejectedValueOnce(new Error('recovery_library_load_failed'))
    await expect(retryCloudRecovery(backend)).rejects.toThrow('recovery_library_load_failed')
    expect(cloud.recovery.needsAttention).toEqual([])
  })

  it('reads every region recovery names back when it flags them and when one is acknowledged', async () => {
    const backend = createMockBackend({ timing: { method: 0 } })
    const entry = { attemptId: 'attempt-missing', chapterId: 'ch-open', pageIndex: 2, regionId: 'region', reason: 'repair_needed' }
    const ambiguous = { ...entry, attemptId: 'attempt-ambiguous', pageIndex: 5, regionId: 'other', reason: 'ambiguous' }
    vi.spyOn(backend, 'reconcileCloudRecovery').mockResolvedValue({ attached: [], stillRunning: [], needsAttention: [entry, ambiguous] })
    const load = vi.spyOn(backend, 'loadPages').mockResolvedValue([])
    const open = editor.chapter
    editor.chapter = /** @type {any} */ ({ id: 'ch-open' })
    try {
      await retryCloudRecovery(backend)
      expect(load).toHaveBeenCalledTimes(2)
      expect(load).toHaveBeenCalledWith({ chapterId: 'ch-open', indices: [2] })
      expect(load).toHaveBeenCalledWith({ chapterId: 'ch-open', indices: [5] })
      await dismissRecovered(entry.attemptId, backend)
      expect(load).toHaveBeenCalledTimes(3)
    } finally {
      editor.chapter = open
    }
  })

  it('reads a region back when its recovery is abandoned, whatever the reason was', async () => {
    const backend = createMockBackend({ timing: { method: 0 } })
    const entry = { attemptId: 'attempt-abandoned', chapterId: 'ch-open', pageIndex: 3, regionId: 'region', reason: 'ambiguous' }
    vi.spyOn(backend, 'reconcileCloudRecovery').mockResolvedValue({ decision: 'abandon' })
    const load = vi.spyOn(backend, 'loadPages').mockResolvedValue([])
    cloud.recovery = { attached: [], stillRunning: [], needsAttention: [entry] }
    const open = editor.chapter
    editor.chapter = /** @type {any} */ ({ id: 'ch-open' })
    try {
      await abandonRecovered(entry.attemptId, true, backend)
      expect(cloud.recovery.needsAttention).toEqual([])
      expect(load).toHaveBeenCalledTimes(1)
      expect(load).toHaveBeenCalledWith({ chapterId: 'ch-open', indices: [3] })
    } finally {
      editor.chapter = open
    }
  })
})
