import { afterEach, describe, expect, it } from 'vitest'
import { loadedModels, refreshLoadedModels } from './loadedmodels.svelte.js'

const ROW = { id: 1, kindKey: 'models.kind.sidecar', bytes: 7_000_000_000, basis: 'reported', deviceKey: 'accel.coreml', gpu: true, idleMs: 0, unloading: false }

describe('the loaded-models poll', () => {
  afterEach(() => {
    loadedModels.models = []
    loadedModels.totalBytes = null
  })

  it('holds the rows and what the operating system counts for all of it', async () => {
    await refreshLoadedModels(/** @type {any} */ ({ listLoadedModels: async () => [ROW], memoryInUse: async () => 8_300_000_000 }))
    expect(loadedModels.models).toEqual([ROW])
    expect(loadedModels.totalBytes).toBe(8_300_000_000)
  })

  it('says nothing about the total when the system cannot be read', async () => {
    await refreshLoadedModels(/** @type {any} */ ({ listLoadedModels: async () => [ROW], memoryInUse: async () => null }))
    expect(loadedModels.models).toEqual([ROW])
    expect(loadedModels.totalBytes).toBeNull()
  })

  it('empties the panel when the backend cannot answer', async () => {
    loadedModels.totalBytes = 5
    await refreshLoadedModels(/** @type {any} */ ({ listLoadedModels: async () => { throw new Error('no') }, memoryInUse: async () => 5 }))
    expect(loadedModels.models).toEqual([])
    expect(loadedModels.totalBytes).toBeNull()
  })
})
