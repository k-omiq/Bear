import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { cloud } from './cloud.svelte.js'
import { chooseCleanTarget, chooseDetectTarget, cloudTargetKey, syncCloudOffer, targets } from './cloudtargets.svelte.js'
import { backendSettingsPatch, session, setAnalysisTarget, setCleanTarget, setCloudAllowed, setDetectorModels } from './session.svelte.js'
import { writeSettingsSerialized } from './settingswrite.js'

function deferred() {
  let resolve
  let reject
  const promise = new Promise((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

beforeEach(() => {
  setCleanTarget('local')
  setAnalysisTarget('rtFull', 'local')
  setAnalysisTarget('samTs', 'local')
})

afterEach(() => {
  setBackend(null)
  setCleanTarget('local')
  setAnalysisTarget('rtFull', 'local')
  setAnalysisTarget('samTs', 'local')
  setDetectorModels(['ctd', 'rtSmall'])
  setCloudAllowed(false)
  cloud.checked = false
  targets.offer = null
})

it('serializes quick Cloud then Local and keeps the later clean choice after a failed write', async () => {
  const first = deferred()
  const durable = { cleanTarget: 'local' }
  const writeSettings = vi.fn(async (patch) => {
    if (writeSettings.mock.calls.length === 1) await first.promise
    Object.assign(durable, patch)
  })
  const api = { writeSettings }
  setBackend(/** @type {any} */ (api))
  const cloudChoice = chooseCleanTarget('cloud')
  const localChoice = chooseCleanTarget('local')
  first.reject(new Error('disk'))
  expect(await cloudChoice).toBe(false)
  expect(await localChoice).toBe(true)
  expect(session.cleanTarget).toBe('local')
  expect(durable.cleanTarget).toBe('local')
  expect(writeSettings.mock.calls.map(([patch]) => patch.cleanTarget)).toEqual(['cloud', 'local'])
  expect(targets.cleanSaveFailed).toBe(false)
})

it('takes a full snapshot at its turn without overwriting a newer targeted choice', async () => {
  const first = deferred()
  const writes = []
  const api = { writeSettings: vi.fn(async (patch) => {
    writes.push(patch)
    if (writes.length === 1) await first.promise
  }) }
  setBackend(/** @type {any} */ (api))
  const cloudChoice = chooseCleanTarget('cloud')
  const snapshot = writeSettingsSerialized(api, () => backendSettingsPatch())
  const localChoice = chooseCleanTarget('local')
  first.resolve()
  await Promise.all([cloudChoice, snapshot, localChoice])
  expect(writes.map((patch) => patch.cleanTarget)).toEqual(['cloud', 'local', 'local'])
  expect(session.cleanTarget).toBe('local')
})

it('does not roll back a newer detection choice when an earlier save fails', async () => {
  const first = deferred()
  const api = { writeSettings: vi.fn(async () => {
    if (api.writeSettings.mock.calls.length === 1) await first.promise
  }) }
  setBackend(/** @type {any} */ (api))
  const cloudChoice = chooseDetectTarget('cloud')
  const localChoice = chooseDetectTarget('local')
  first.reject(new Error('disk'))
  expect(await cloudChoice).toBe(false)
  expect(await localChoice).toBe(true)
  expect(session.analysisTargets).toEqual({ rtFull: 'local', samTs: 'local' })
  expect(targets.detectSaveFailed).toBe(false)
})

it('asks again when the endpoint URL changes under one profile id', async () => {
  setCloudAllowed(true)
  setDetectorModels(['ctd', 'samTs'])
  cloud.checked = true
  cloud.readiness = { ...cloud.readiness, configured: true, target: { type: 'modal', profile_id: 'p1' },
    profile: { endpointUrl: 'https://first.example/api' } }
  const api = { listRemoteAnalysisCapabilities: vi.fn(async () => ({ capabilities: [] })) }
  setBackend(/** @type {any} */ (api))
  const firstKey = cloudTargetKey()
  syncCloudOffer()
  await vi.waitFor(() => expect(api.listRemoteAnalysisCapabilities).toHaveBeenCalledTimes(1))
  cloud.readiness = { ...cloud.readiness, profile: { endpointUrl: 'https://second.example/api' } }
  expect(cloudTargetKey()).not.toBe(firstKey)
  syncCloudOffer()
  await vi.waitFor(() => expect(api.listRemoteAnalysisCapabilities).toHaveBeenCalledTimes(2))
})
