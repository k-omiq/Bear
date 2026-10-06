import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import {
  APP_EVENTS,
  CLOUD_EVENTS,
  createTauriBackend,
  EVENT_METHODS,
  implementedMethods,
  isCloudExecutionRegistered,
  isTauri,
  SEAM_METHODS,
  TAURI_PENDING_CLOUD_COMMANDS,
  TAURI_PROVISIONER_COMMANDS,
  TAURI_REGISTERED_CLOUD_COMMANDS,
} from './tauri.js'
import {
  getBackend,
  isCloudExecutionReady,
  isCloudExecutionRegistered as isBackendCloudExecutionRegistered,
  readCloudReadiness,
  setBackend,
} from './backend.js'

it('pins denoising capability reads to the named cloud profile', async () => {
  const invoke = vi.fn().mockResolvedValue(['mangajanai-2x'])
  const backend = createTauriBackend({ invoke, fallback: {} })
  await backend.cloudDenoisePresets({ provider: 'modal', profileId: 'studio' })
  expect(invoke).toHaveBeenCalledWith('cloud_denoise_presets', { provider: 'modal', profileId: 'studio' })
})
import { createMockBackend } from './mock.js'

/** The seam methods that are commands, as opposed to event listeners. */
const COMMAND_METHODS = SEAM_METHODS.filter((method) => !EVENT_METHODS.includes(method))

/**
 * A fallback that records every call and answers with something identifiable,
 * so a test can tell which side of the adapter served a method.
 */
function recordingFallback() {
  const calls = []
  const handlers = new Set()
  const backend = { calls, emit: (event) => handlers.forEach((h) => h(event)) }
  for (const method of SEAM_METHODS) {
    backend[method] = (...args) => {
      calls.push({ method, args })
      return Promise.resolve({ from: 'fallback', method })
    }
  }
  backend.subscribe = (handler) => {
    calls.push({ method: 'subscribe', args: [] })
    handlers.add(handler)
    return () => handlers.delete(handler)
  }
  backend.readSettings = (...args) => {
    calls.push({ method: 'readSettings', args })
    return Promise.resolve({ language: 'en', cloud: false, engineCeiling: 'lama' })
  }
  return backend
}

describe('the Tauri adapter', () => {
  /**
   * The adapter is only useful if it *is* a backend. A method missing from both
   * the command table and the delegation loop would be `undefined` at a call
   * site, which is a runtime crash in a component rather than a test failure.
   */
  it('answers every method the seam fixes', () => {
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke: vi.fn() })
    for (const method of SEAM_METHODS) {
      expect(typeof backend[method], `${method} is not a function`).toBe('function')
    }
  })

  it('does not expose the unqualified text-shaped IPC route', () => {
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke: vi.fn() })
    for (const method of ['prepareTextShape', 'textShapePreviewTile', 'applyPreparedTextShape']) {
      expect(SEAM_METHODS).not.toContain(method)
      expect(implementedMethods()).not.toContain(method)
      expect(backend[method]).toBeUndefined()
    }
  })

  it('binds a new stroke to the source slot and hash shown at gesture start', async () => {
    const invoke = vi.fn().mockResolvedValue(null)
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
    await backend.createRegion({ chapterId: 'c1', pageIndex: 2,
      sourceIndex: 7, sourceSha: 'scan-digest',
      bbox: { x: 1, y: 2, w: 3, h: 4 }, tool: 'aiMaskBrush', params: {} })
    expect(invoke).toHaveBeenCalledWith('create_region', expect.objectContaining({
      pageIndex: 2, expectedSourceIdx: 7, expectedSourceSha: 'scan-digest',
    }))
  })

  it('sends a mask edit bound to the source it was drawn over, with one gesture and the other null', async () => {
    const invoke = vi.fn().mockResolvedValue({ pageStatus: 'detected', changed: ['r1'], created: [], removed: [] })
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
    const stroke = { points: [{ x: 10, y: 20 }, { x: 12, y: 22 }], radius: 16 }
    const answer = await backend.editDetectionMask({ chapterId: 'c1', pageIndex: 2,
      sourceIndex: 7, sourceSha: 'scan-digest', mode: 'add', stroke })
    expect(invoke).toHaveBeenCalledWith('edit_detection_mask', {
      chapterId: 'c1', pageIndex: 2, expectedSourceIdx: 7, expectedSourceSha: 'scan-digest',
      mode: 'add', stroke, painted: null,
    })
    expect(answer).toEqual({ pageStatus: 'detected', changed: ['r1'], created: [], removed: [] })

    const painted = { kind: 'polygon', points: [{ x: 1, y: 1 }, { x: 9, y: 1 }, { x: 5, y: 8 }], feather: 0 }
    await backend.editDetectionMask({ chapterId: 'c1', pageIndex: 0, mode: 'remove', painted })
    expect(invoke).toHaveBeenLastCalledWith('edit_detection_mask', {
      chapterId: 'c1', pageIndex: 0, expectedSourceIdx: null, expectedSourceSha: null,
      mode: 'remove', stroke: null, painted,
    })
  })

  it('sends a text type change by region id and the one flag', async () => {
    const answer = { id: 'c1-p001-d1', outcome: 'detected', insideBubble: false }
    const invoke = vi.fn().mockResolvedValue(answer)
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
    expect(await backend.setDetectionType({ regionId: 'c1-p001-d1', inside: false })).toEqual(answer)
    expect(invoke).toHaveBeenCalledWith('set_detection_type', { regionId: 'c1-p001-d1', inside: false })
  })

  it('sends a mask padding for one page, or for the chapter with the page null', async () => {
    const invoke = vi.fn().mockResolvedValue({ changed: ['r1'], pages: [2] })
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
    expect(await backend.setDetectionPadding({ chapterId: 'c1', pageIndex: 2, paddingPx: 6 }))
      .toEqual({ changed: ['r1'], pages: [2] })
    expect(invoke).toHaveBeenCalledWith('set_detection_padding', { chapterId: 'c1', pageIndex: 2, regionId: null, paddingPx: 6 })
    await backend.setDetectionPadding({ chapterId: 'c1', paddingPx: 0 })
    expect(invoke).toHaveBeenLastCalledWith('set_detection_padding', { chapterId: 'c1', pageIndex: null, regionId: null, paddingPx: 0 })
    await backend.setDetectionPadding({ chapterId: 'c1', pageIndex: 2, regionId: 'r1', paddingPx: 3 })
    expect(invoke).toHaveBeenLastCalledWith('set_detection_padding', { chapterId: 'c1', pageIndex: 2, regionId: 'r1', paddingPx: 3 })
  })

  it('maps chapter analysis, saved corrections, group installs, and corrected exact-W plans', async () => {
    const invoke = vi.fn().mockResolvedValue(null)
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
    const request = { analysisId: 'analysis-1', chapterId: 'chapter-1', pageIndex: 3, componentId: 'sam-1' }
    const additions = { bounds: { x: 4, y: 5, w: 1, h: 1 }, bits: [255] }
    const removals = { bounds: { x: 0, y: 0, w: 0, h: 0 }, bits: [] }

    await backend.analyzeChapterPage({ chapterId: 'chapter-1', pageIndex: 3, workflow: 'text_shape',
      rtProfile: 'full-halves', rtBackend: 'ort-cpu', samBackend: 'ort-webgpu' })
    await backend.loadComponentCorrection(request)
    await backend.prepareComponentWrite({ ...request, allowOutsideBubbles: false, paddingPx: 5,
      additions, removals, correctionRevision: 8 })
    await backend.downloadModelGroup({ id: 'scriptGate' })

    expect(invoke).toHaveBeenNthCalledWith(1, 'analyze_chapter_page', {
      chapterId: 'chapter-1', pageIndex: 3, workflow: 'text_shape', rtProfile: 'full-halves',
      rtBackend: 'ort-cpu', samBackend: 'ort-webgpu', requestId: expect.any(String),
    })
    expect(invoke).toHaveBeenNthCalledWith(2, 'load_component_correction', request)
    expect(invoke).toHaveBeenNthCalledWith(3, 'prepare_component_write', {
      ...request, allowOutsideBubbles: false, paddingPx: 5, additions, removals, correctionRevision: 8,
    })
    expect(invoke).toHaveBeenNthCalledWith(4, 'download_model_group', { id: 'scriptGate' })
  })

  it('generates distinct ids for callers that omit an analysis id', async () => {
    const invoke = vi.fn().mockResolvedValue(null)
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
    const spec = { sourcePath: '/tmp/page.png', workflow: 'mask', rtProfile: 'full-halves',
      rtBackend: 'ort-cpu', samBackend: 'ort-cpu' }
    await backend.analyzeCapabilities(spec)
    await backend.analyzeCapabilities(spec)
    const first = invoke.mock.calls[0][1].requestId
    const second = invoke.mock.calls[1][1].requestId
    expect(first).toEqual(expect.any(String))
    expect(second).not.toBe(first)
  })

  it('passes explicit analysis ids to both commands and cancels that id', async () => {
    const invoke = vi.fn().mockResolvedValue(true)
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
    const spec = { sourcePath: '/tmp/page.png', workflow: 'mask', rtProfile: 'full-halves',
      rtBackend: 'ort-cpu', samBackend: 'ort-cpu', requestId: 'request-1' }
    await backend.analyzeCapabilities(spec)
    await backend.analyzeChapterPage({ ...spec, chapterId: 'c1', pageIndex: 0 })
    expect(await backend.cancelCapabilityAnalysis('request-1')).toBe(true)
    expect(invoke).toHaveBeenNthCalledWith(1, 'analyze_capabilities', spec)
    expect(invoke).toHaveBeenNthCalledWith(2, 'analyze_chapter_page', {
      chapterId: 'c1', pageIndex: 0, workflow: 'mask', rtProfile: 'full-halves',
      rtBackend: 'ort-cpu', samBackend: 'ort-cpu', requestId: 'request-1',
    })
    expect(invoke).toHaveBeenNthCalledWith(3, 'cancel_capability_analysis', { requestId: 'request-1' })
  })

  it('maps remote analysis consent and status to registered commands', async () => {
    const invoke = vi.fn().mockResolvedValue({})
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
    await backend.listRemoteAnalysisCapabilities({ provider: 'modal', profileId: 'm1' })
    await backend.proposeRemoteAnalysis({ chapterId: 'c1', pageIndex: 0,
      provider: 'modal', profileId: 'm1', capability: 'text_mask_sam_ts@1' })
    await backend.confirmRemoteAnalysis({ proposalId: 'p1', rightsAttested: true, retentionAcknowledged: true })
    await backend.cancelRemoteAnalysis({ proposalId: 'p1' })
    await backend.getRemoteAnalysisStatus({ proposalId: 'p1' })
    expect(invoke.mock.calls).toEqual([
      ['list_remote_analysis_capabilities', { provider: 'modal', profileId: 'm1' }],
      ['propose_remote_analysis', { regions: [], companion: null, chapterId: 'c1', pageIndex: 0,
        provider: 'modal', profileId: 'm1', capability: 'text_mask_sam_ts@1' }],
      ['confirm_remote_analysis', { proposalId: 'p1', rightsAttested: true, retentionAcknowledged: true }],
      ['cancel_remote_analysis', { proposalId: 'p1' }],
      ['get_remote_analysis_status', { proposalId: 'p1' }],
    ])
  })

  it('lists exactly the methods it does not delegate', async () => {
    const fallback = recordingFallback()
    const invoke = vi.fn().mockResolvedValue({})
    const backend = createTauriBackend({ fallback, invoke })

    for (const method of COMMAND_METHODS) {
      await backend[method]({})
    }
    backend.subscribe(() => {})

    const delegated = new Set(fallback.calls.map((call) => call.method))
    // `readSettings` reaches the fallback too - for the defaults, not for the
    // value - so it is excluded from the delegation check by name rather than
    // by accident.
    const served = COMMAND_METHODS.filter((m) => !delegated.has(m) || m === 'readSettings')
    expect(served.sort()).toEqual(implementedMethods())
  })

  it('sends about straight to the command and returns what it says', async () => {
    const invoke = vi
      .fn()
      .mockResolvedValue({ appVersion: '0.1.0', facts: [{ labelKey: 'about.fact.licence', value: 'GPL-3.0-only' }] })
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })

    await expect(backend.about()).resolves.toEqual({
      appVersion: '0.1.0',
      facts: [{ labelKey: 'about.fact.licence', value: 'GPL-3.0-only' }],
    })
    expect(invoke).toHaveBeenCalledWith('about')
  })

  // The command's `source_files` is a required bool; the seam makes it optional.
  it('sends deleteChapter without source files unless the caller asks for them', async () => {
    const invoke = vi.fn().mockResolvedValue(true)
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })

    await expect(backend.deleteChapter({ projectId: 'p1', chapterId: 'c1' })).resolves.toBe(true)
    expect(invoke).toHaveBeenLastCalledWith('delete_chapter', { projectId: 'p1', chapterId: 'c1', sourceFiles: false })
    await backend.deleteChapter({ projectId: 'p1', chapterId: 'c1', sourceFiles: true })
    expect(invoke).toHaveBeenLastCalledWith('delete_chapter', { projectId: 'p1', chapterId: 'c1', sourceFiles: true })
  })

  // `diagnostics.rs` serialises its fields as written, so the command answers
  // `reason_key` where every other command answers camelCase.
  it('brings the diagnostics answer onto the seam in camelCase', async () => {
    const invoke = vi.fn().mockResolvedValue({
      app_version: '1.0.0',
      components: [{ name: 'onnxruntime', available: false, detail: 'dlopen failed', reason_key: 'diagnostics.runtime.quarantined' }],
    })
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke })

    await expect(backend.diagnostics()).resolves.toEqual({
      appVersion: '1.0.0',
      components: [{ name: 'onnxruntime', available: false, detail: 'dlopen failed', reasonKey: 'diagnostics.runtime.quarantined' }],
    })
    expect(invoke).toHaveBeenCalledWith('diagnostics')

    invoke.mockResolvedValue({
      appVersion: '1.0.0',
      components: [{ name: 'onnxruntime', available: true, detail: '1.28.0', reasonKey: null }],
    })
    await expect(backend.diagnostics()).resolves.toEqual({
      appVersion: '1.0.0',
      components: [{ name: 'onnxruntime', available: true, detail: '1.28.0', reasonKey: null }],
    })
  })

  /**
   * The core stores settings without knowing what one means, so on a first
   * launch it returns `{}`. An adapter that passed that through would hand the
   * interface a settings object with no settings in it.
   */
  it('merges the stored settings over the interface defaults', async () => {
    const fallback = recordingFallback()
    const invoke = vi.fn().mockResolvedValue({ cloud: true })
    const backend = createTauriBackend({ fallback, invoke })

    await expect(backend.readSettings()).resolves.toEqual({
      language: 'en',
      cloud: true,
      engineCeiling: 'lama',
    })
  })

  it('returns the whole snapshot after a write, not the patch', async () => {
    const fallback = recordingFallback()
    const invoke = vi.fn().mockResolvedValue({ cloud: true })
    const backend = createTauriBackend({ fallback, invoke })

    await expect(backend.writeSettings({ cloud: true })).resolves.toEqual({
      language: 'en',
      cloud: true,
      engineCeiling: 'lama',
    })
    expect(invoke).toHaveBeenCalledWith('write_settings', { patch: { cloud: true } })
  })

  it('empty stored settings leave the defaults alone', async () => {
    const backend = createTauriBackend({ fallback: recordingFallback(), invoke: vi.fn().mockResolvedValue({}) })
    await expect(backend.readSettings()).resolves.toEqual({
      language: 'en',
      cloud: false,
      engineCeiling: 'lama',
    })
  })

  /**
   * Events come from whichever implementation is running the job, and since
   * `run.rs` landed that is both of them: the run's four are the backend's and
   * the six region-level edits' notices are still the fallback's. A handler
   * that received only one side would leave the Pages list frozen with a run
   * apparently in progress, which is why `subscribe` is a merge and not a
   * replacement.
   *
   * This test is the fallback half. The backend half is next, and the merged
   * stream under a real run is `tauri-events.test.js`.
   */
  it('passes the fallback’s events through', () => {
    const fallback = recordingFallback()
    const backend = createTauriBackend({ fallback, invoke: vi.fn() })
    const seen = []
    const unsubscribe = backend.subscribe((event) => seen.push(event))

    fallback.emit({ type: 'page-started', pageIndex: 0 })
    unsubscribe()
    fallback.emit({ type: 'page-done', pageIndex: 0 })

    expect(seen).toEqual([{ type: 'page-started', pageIndex: 0 }])
  })

  /**
   * And the backend half, through the adapter's own `subscribe` rather than
   * through `createEventStream` directly - the channel is taken from
   * `globalThis.__TAURI__.core.Channel`, which is where `withGlobalTauri` puts
   * it and therefore the only place the shipped adapter looks.
   *
   * The channel is registered at construction and stays registered: a run
   * emits for minutes with no call outstanding, so an event that arrives while
   * no component is subscribed must find the sink still there.
   */
  it('passes the backend’s events through, on a channel it keeps open', async () => {
    const opened = []
    globalThis.__TAURI__ = {
      core: {
        Channel: class {
          constructor() {
            this.onmessage = null
            opened.push(this)
          }
        },
      },
    }
    try {
      const invoke = vi.fn().mockResolvedValue(7)
      const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
      expect(opened).toHaveLength(1)
      expect(invoke).toHaveBeenCalledWith('subscribe_events', { channel: opened[0] })

      const seen = []
      const unsubscribe = backend.subscribe((event) => seen.push(event))
      opened[0].onmessage({ type: 'page-started', runId: 'run-1', pageIndex: 0 })
      unsubscribe()
      await Promise.resolve()
      await Promise.resolve()

      // Detaching the only handler stops delivery and nothing else: no second
      // channel, and the backend is never told to drop the sink.
      opened[0].onmessage({ type: 'run-finished', runId: 'run-1', reason: 'completed' })
      expect(seen).toEqual([{ type: 'page-started', runId: 'run-1', pageIndex: 0 }])
      expect(opened).toHaveLength(1)
      expect(invoke).not.toHaveBeenCalledWith('unsubscribe_events', expect.anything())

      // And a handler attached afterwards is on the same channel the run holds.
      backend.subscribe((event) => seen.push(event))
      opened[0].onmessage({ type: 'run-finished', runId: 'run-1', reason: 'completed' })
      expect(seen.at(-1)).toEqual({ type: 'run-finished', runId: 'run-1', reason: 'completed' })
    } finally {
      delete globalThis.__TAURI__
    }
  })

  /**
   * The delegation loop is still there and has nothing left to serve.
   *
   * Every method the seam fixes is a command now - the four region edits were
   * the last of them (`src-tauri/src/region.rs`) - so the assertion this test can make is
   * the one that is true: the fallback is reached for exactly two things, and
   * neither is a method it *serves*. `readSettings` reaches it for the
   * interface's defaults, which the command's stored snapshot is merged over,
   * and `subscribe` reaches it because the stream is a merge.
   */
  it('serves every seam method itself, and reaches the fallback only for the defaults', async () => {
    const fallback = recordingFallback()
    const backend = createTauriBackend({ fallback, invoke: vi.fn().mockResolvedValue({}) })

    for (const method of COMMAND_METHODS) {
      await backend[method]({})
    }
    backend.subscribe(() => {})

    expect([...new Set(fallback.calls.map((call) => call.method))].sort()).toEqual([
      'readSettings',
      'subscribe',
    ])
    expect(implementedMethods()).toEqual([...COMMAND_METHODS].sort())
  })

  it('constructed outside a Tauri window, a command rejects rather than throwing', async () => {
    const fallback = recordingFallback()
    const backend = createTauriBackend({ fallback })
    // The seam declares every method async, so being built outside a window
    // has to surface as a rejected promise like any other backend failure -
    // never as a synchronous throw out of a call site that is awaiting one.
    await expect(backend.about()).rejects.toThrow(/outside a Tauri window/)
    await expect(backend.applyTool({ tool: 'brush', regionId: 'r1' })).rejects.toThrow(
      /outside a Tauri window/,
    )
  })

  describe('inference and cloud secrets command mapping', () => {
    it('maps readInferenceConfig to read_inference_config command', async () => {
      const invoke = vi.fn().mockResolvedValue({
        schemaVersion: 1,
        selectedTarget: { type: 'local' },
        beamProfiles: {},
        modalProfiles: {},
      })
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.readInferenceConfig()
      expect(invoke).toHaveBeenCalledWith('read_inference_config')
      expect(result).toEqual({
        schemaVersion: 1,
        selectedTarget: { type: 'local' },
        beamProfiles: {},
        modalProfiles: {},
      })
      expect(fallback.calls.some((c) => c.method === 'readInferenceConfig')).toBe(false)
    })

    it('maps writeInferenceConfig to write_inference_config command with { config } argument', async () => {
      const config = {
        schemaVersion: 1,
        selectedTarget: { type: 'beam', profile_id: 'beam-prod' },
        beamProfiles: {
          'beam-prod': {
            id: 'beam-prod',
            name: 'Beam Prod',
            endpointUrl: 'https://api.beam.cloud/ep',
            canonicalOrigin: 'https://api.beam.cloud',
            canonicalOriginFingerprint: 'fp123',
            createdAtMs: 100,
            updatedAtMs: 200,
          },
        },
        modalProfiles: {},
      }
      const invoke = vi.fn().mockResolvedValue(config)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.writeInferenceConfig({ config })
      expect(invoke).toHaveBeenCalledWith('write_inference_config', { config })
      expect(result).toEqual(config)
      expect(JSON.stringify(result)).not.toContain('secret')
      expect(fallback.calls.some((c) => c.method === 'writeInferenceConfig')).toBe(false)
    })

    it('maps storeCloudSecret with exact parameter structure', async () => {
      const summary = {
        provider: 'beam',
        profileId: 'beam-prod',
        role: 'runtime',
        present: true,
        backend: 'keyring',
      }
      const invoke = vi.fn().mockResolvedValue(summary)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.storeCloudSecret({
        provider: 'beam',
        profileId: 'beam-prod',
        role: 'runtime',
        secret: 'raw-secret-token-value',
        sessionOnly: false,
      })

      expect(invoke).toHaveBeenCalledWith('store_cloud_secret', {
        provider: 'beam',
        profileId: 'beam-prod',
        role: 'runtime',
        secret: 'raw-secret-token-value',
        sessionOnly: false,
      })
      expect(result).toEqual(summary)
      expect(result).not.toHaveProperty('secret')
      expect(fallback.calls.some((c) => c.method === 'storeCloudSecret')).toBe(false)
    })

    it('maps storeCloudSecret with tokenId for modal runtime', async () => {
      const summary = {
        provider: 'modal',
        profileId: 'modal-prod',
        role: 'runtime',
        present: true,
        backend: 'keyring',
      }
      const invoke = vi.fn().mockResolvedValue(summary)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.storeCloudSecret({
        provider: 'modal',
        profileId: 'modal-prod',
        role: 'runtime',
        secret: 'raw-secret-token-value',
        tokenId: 'ak-genuine-token-id',
        sessionOnly: false,
      })

      expect(invoke).toHaveBeenCalledWith('store_cloud_secret', {
        provider: 'modal',
        profileId: 'modal-prod',
        role: 'runtime',
        secret: 'raw-secret-token-value',
        tokenId: 'ak-genuine-token-id',
        sessionOnly: false,
      })
      expect(result).toEqual(summary)
      expect(result).not.toHaveProperty('secret')
      expect(result).not.toHaveProperty('tokenId')
      expect(fallback.calls.some((c) => c.method === 'storeCloudSecret')).toBe(false)
    })

    it('maps deleteCloudSecret with exact parameter structure', async () => {
      const summary = {
        provider: 'modal',
        profileId: 'modal-dev',
        role: 'setup',
        present: false,
        backend: 'keyring',
      }
      const invoke = vi.fn().mockResolvedValue(summary)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.deleteCloudSecret({
        provider: 'modal',
        profileId: 'modal-dev',
        role: 'setup',
      })

      expect(invoke).toHaveBeenCalledWith('delete_cloud_secret', {
        provider: 'modal',
        profileId: 'modal-dev',
        role: 'setup',
      })
      expect(result).toEqual(summary)
      expect(fallback.calls.some((c) => c.method === 'deleteCloudSecret')).toBe(false)
    })

    it('maps getCloudSecretSummary with exact parameter structure', async () => {
      const summary = {
        provider: 'modal',
        profileId: 'modal-dev',
        role: 'model_download',
        present: true,
        backend: 'session',
      }
      const invoke = vi.fn().mockResolvedValue(summary)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.getCloudSecretSummary({
        provider: 'modal',
        profileId: 'modal-dev',
        role: 'model_download',
      })

      expect(invoke).toHaveBeenCalledWith('get_cloud_secret_summary', {
        provider: 'modal',
        profileId: 'modal-dev',
        role: 'model_download',
      })
      expect(result).toEqual(summary)
      expect(fallback.calls.some((c) => c.method === 'getCloudSecretSummary')).toBe(false)
    })

    it('propagates rejected invoke errors without falling back to mock for all 5 methods', async () => {
      const invoke = vi.fn().mockRejectedValue(new Error('Tauri command failure'))
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      await expect(backend.readInferenceConfig()).rejects.toThrow('Tauri command failure')
      await expect(
        backend.writeInferenceConfig({
          schemaVersion: 1,
          selectedTarget: { type: 'local' },
          beamProfiles: {},
          modalProfiles: {},
        }),
      ).rejects.toThrow('Tauri command failure')
      await expect(
        backend.storeCloudSecret({
          provider: 'beam',
          profileId: 'p1',
          role: 'runtime',
          secret: 's1',
        }),
      ).rejects.toThrow('Tauri command failure')
      await expect(
        backend.deleteCloudSecret({
          provider: 'beam',
          profileId: 'p1',
          role: 'runtime',
        }),
      ).rejects.toThrow('Tauri command failure')
      await expect(
        backend.getCloudSecretSummary({
          provider: 'beam',
          profileId: 'p1',
          role: 'runtime',
        }),
      ).rejects.toThrow('Tauri command failure')

      // Assert that none of the 5 methods fell back to fallback mock implementation
      expect(fallback.calls.filter((c) =>
        [
          'readInferenceConfig',
          'writeInferenceConfig',
          'storeCloudSecret',
          'deleteCloudSecret',
          'getCloudSecretSummary',
        ].includes(c.method),
      )).toHaveLength(0)
    })
  })

  describe('cloud lifecycle command mapping and registration', () => {
    it('maps prepareCloudConsent with exact parameters', async () => {
      const proposal = {
        proposalId: 'prop-123',
        profileId: 'beam-prod',
        provider: 'beam',
        endpointUrl: 'https://api.beam.cloud/ep',
        canonicalOriginFingerprint: 'fp123',
        profileEpoch: 1,
        cropSha256: 'crop123',
        hintSha256: 'hint123',
        sourceHash: 'src123',
        maskHash: 'mask123',
        regionRevision: 1,
        rect: { x: 0, y: 0, w: 100, h: 100 },
        recipe: { recipeId: 'sdnq-v1', preprocessingVersion: '1.0.0', modelId: 'flux', modelRevision: 'rev', nativeMaskConditioning: false },
        intent: { action: 'cleanAnyway' },
        createdAtMs: 1000,
        expiresAtMs: 2000,
      }
      const invoke = vi.fn().mockResolvedValue(proposal)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const req = {
        target: { type: 'beam', profile_id: 'beam-prod' },
        intent: { action: 'cleanAnyway' },
        chapterId: 'ch-1',
        pageIndex: 0,
        regionId: 'reg-1',
      }
      const res = await backend.prepareCloudConsent(req)
      expect(invoke).toHaveBeenCalledWith('prepare_cloud_consent', req)
      expect(res).toEqual(proposal)
      expect(fallback.calls.some((c) => c.method === 'prepareCloudConsent')).toBe(false)
    })

    it('maps confirmCloudConsent with exact parameters', async () => {
      const grant = {
        nonce: 'grant-nonce-xyz',
        scope: {
          provider: 'beam',
          profileId: 'beam-prod',
          endpointFingerprint: 'fp123',
          cropSha256: 'crop123',
          maskHash: 'mask123',
          revision: 1,
          recipe: { recipeId: 'sdnq-v1', preprocessingVersion: '1.0.0', modelId: 'flux', modelRevision: 'rev', nativeMaskConditioning: false },
          operationDigest: 'opdig123',
        },
        issuedAtMs: 1000,
        expiresAtMs: 2000,
        allowedAttempts: 1,
        usedAttempts: 0,
      }
      const invoke = vi.fn().mockResolvedValue(grant)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const req = {
        proposalId: 'prop-123',
        intent: { action: 'cleanAnyway' },
        rightsAttested: true,
        retentionAcknowledged: true,
      }
      const res = await backend.confirmCloudConsent(req)
      expect(invoke).toHaveBeenCalledWith('confirm_cloud_consent', req)
      expect(res).toEqual(grant)
      expect(fallback.calls.some((c) => c.method === 'confirmCloudConsent')).toBe(false)
    })

    it('maps submitCloudAttempt with exact parameters', async () => {
      const submission = {
        attemptId: 'att-123',
        handle: 'h-456',
        status: 'accepted',
        requestDigest: 'reqdig123',
        autoRetryable: false,
      }
      const invoke = vi.fn().mockResolvedValue(submission)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const req = {
        attemptId: 'att-123',
        grantNonce: 'grant-nonce-xyz',
        proposalId: 'prop-123',
      }
      const res = await backend.submitCloudAttempt(req)
      expect(invoke).toHaveBeenCalledWith('submit_cloud_attempt', req)
      expect(res).toEqual(submission)
      expect(fallback.calls.some((c) => c.method === 'submitCloudAttempt')).toBe(false)
    })

    it('maps getCloudAttemptStatus with exact parameters', async () => {
      const status = {
        attemptId: 'att-123',
        handle: 'h-456',
        status: 'completed',
        reportedCostUsd: 0.002,
        acknowledged: true,
      }
      const invoke = vi.fn().mockResolvedValue(status)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const req = { attemptId: 'att-123', handle: 'h-456' }
      const res = await backend.getCloudAttemptStatus(req)
      expect(invoke).toHaveBeenCalledWith('get_cloud_attempt_status', req)
      expect(res).toEqual(status)
      expect(fallback.calls.some((c) => c.method === 'getCloudAttemptStatus')).toBe(false)
    })

    it('maps getCloudAttemptResult with exact parameters', async () => {
      const result = {
        attemptId: 'att-123',
        handle: 'h-456',
        resultDigest: 'resdig123',
        reportedCostUsd: 0.002,
        width: 100,
        height: 100,
        cached: true,
      }
      const invoke = vi.fn().mockResolvedValue(result)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const req = { attemptId: 'att-123', handle: 'h-456' }
      const res = await backend.getCloudAttemptResult(req)
      expect(invoke).toHaveBeenCalledWith('get_cloud_attempt_result', req)
      expect(res).toEqual(result)
      expect(fallback.calls.some((c) => c.method === 'getCloudAttemptResult')).toBe(false)
    })

    it('maps cancelCloudAttempt with exact parameters', async () => {
      const cancelRes = {
        handle: 'h-456',
        status: 'cancel_requested',
        acknowledged: true,
      }
      const invoke = vi.fn().mockResolvedValue(cancelRes)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const req = { attemptId: 'att-123', handle: 'h-456' }
      const res = await backend.cancelCloudAttempt(req)
      expect(invoke).toHaveBeenCalledWith('cancel_cloud_attempt', req)
      expect(res).toEqual(cancelRes)
      expect(fallback.calls.some((c) => c.method === 'cancelCloudAttempt')).toBe(false)
    })

    it('maps reconcileCloudRecovery with exact parameters and defaults', async () => {
      const recovery = {
        decision: 'resume_polling',
        attemptId: 'att-123',
        handle: 'h-456',
        autoRetryable: false,
      }
      const invoke = vi.fn().mockResolvedValue(recovery)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const res1 = await backend.reconcileCloudRecovery({ attemptId: 'att-123' })
      expect(invoke).toHaveBeenCalledWith('reconcile_cloud_recovery', { attemptId: 'att-123' })
      expect(res1).toEqual(recovery)

      const res2 = await backend.reconcileCloudRecovery()
      expect(invoke).toHaveBeenCalledWith('reconcile_cloud_recovery', {})
      expect(res2).toEqual(recovery)
      expect(fallback.calls.some((c) => c.method === 'reconcileCloudRecovery')).toBe(false)
    })

    it('maintains semantic split between registration truth and execution readiness', () => {
      // Registration truth: all 14 remote execution lifecycle commands are registered in Tauri
      expect(isCloudExecutionRegistered()).toBe(true)
      expect(isBackendCloudExecutionRegistered()).toBe(true)
      expect(TAURI_PENDING_CLOUD_COMMANDS).toEqual([])
      expect(TAURI_REGISTERED_CLOUD_COMMANDS).toEqual([
        'read_inference_config',
        'write_inference_config',
        'select_cloud_profile',
        'store_cloud_secret',
        'delete_cloud_secret',
        'get_cloud_secret_summary',
        'forget_cloud_secret_denials',
        'check_cloud_connection',
        'get_cloud_model_info',
        'get_cloud_gpu_status',
        'stop_cloud_gpu',
        'list_remote_analysis_capabilities',
        'propose_remote_analysis',
        'confirm_remote_analysis',
        'cancel_remote_analysis',
        'get_remote_analysis_status',
        'propose_run_analysis',
        'confirm_run_analysis',
        'cancel_run_analysis',
        'prepare_cloud_clean',
        'confirm_cloud_clean',
        'start_cloud_clean',
        'cancel_cloud_clean',
        'prepare_cloud_consent',
        'confirm_cloud_consent',
        'submit_cloud_attempt',
        'get_cloud_attempt_status',
        'get_cloud_attempt_result',
        'resolve_qwen_review',
        'cancel_cloud_attempt',
        'reconcile_cloud_recovery',
      ])
    })

    it('computes readiness from the permission, the default target and its runtime secret', async () => {
      const mock = createMockBackend({ timing: { method: 0 } })
      expect(await isCloudExecutionReady(mock)).toBe(false)
      expect((await readCloudReadiness(mock)).reason).toBe('off')

      await mock.writeSettings({ cloudEngines: 'allowed' })
      expect((await readCloudReadiness(mock)).reason).toBe('noTarget')

      await mock.writeInferenceConfig({
        config: {
          schemaVersion: 1,
          selectedTarget: { type: 'modal', profile_id: 'm1' },
          beamProfiles: {},
          modalProfiles: {
            m1: {
              id: 'm1',
              name: 'Modal Worker',
              endpointUrl: 'https://worker.modal.run/mc/v1',
              canonicalOrigin: 'https://worker.modal.run',
              canonicalOriginFingerprint: 'fp-1',
              createdAtMs: 1,
              updatedAtMs: 1,
            },
          },
        },
      })
      expect(await readCloudReadiness(mock)).toMatchObject({ reason: 'noSecret', configured: false })

      await mock.storeCloudSecret({ provider: 'modal', profileId: 'm1', role: 'runtime', secret: 's', tokenId: 't' })
      const verdict = await readCloudReadiness(mock)
      expect(verdict).toMatchObject({
        allowed: true,
        configured: true,
        ready: true,
        reason: null,
        target: { type: 'modal', profile_id: 'm1' },
      })
      expect(verdict.profile.name).toBe('Modal Worker')
      expect(await isCloudExecutionReady(mock)).toBe(true)

      await mock.writeSettings({ cloudEngines: 'blocked' })
      const off = await readCloudReadiness(mock)
      expect(off).toMatchObject({ allowed: false, configured: true, ready: false, reason: 'off' })
      // The endpoint that would be used is still named while the switch is off,
      // and so is the fact that switching cloud on is all that is left.
      expect(off.target).toEqual({ type: 'modal', profile_id: 'm1' })
    })

    it('counts anything it cannot read as not ready, and cannot be talked into ready', async () => {
      expect(await isCloudExecutionReady({})).toBe(false)
      expect(
        await isCloudExecutionReady({ readSettings: () => Promise.reject(new Error('store locked')) }),
      ).toBe(false)
      const forged = {
        readSettings: async () => ({ cloudEngines: 'allowed' }),
        readInferenceConfig: async () => ({ selectedTarget: { type: 'local' }, modalProfiles: {}, beamProfiles: {} }),
        getCloudSecretSummary: async () => ({ present: true }),
        ready: true,
        livePrerequisitesMet: true,
      }
      expect(await readCloudReadiness(forged)).toMatchObject({ ready: false, reason: 'noTarget' })
    })

    it('says the password store is locked, not that the token is gone, when the store will not answer', async () => {
      const profile = { id: 'm1', name: 'Modal Worker', endpointUrl: 'https://worker.modal.run/mc/v1' }
      const locked = {
        readSettings: async () => ({ cloudEngines: 'allowed' }),
        readInferenceConfig: async () => ({
          selectedTarget: { type: 'modal', profile_id: 'm1' },
          modalProfiles: { m1: profile },
          beamProfiles: {},
        }),
        getCloudSecretSummary: async () => ({ present: false, backend: 'unavailable' }),
      }
      expect(await readCloudReadiness(locked)).toMatchObject({ ready: false, configured: false, reason: 'secretLocked' })
    })
  })

  describe('cloud events and cancel', () => {
    it('listens to cloud events and answers with their unlisten', async () => {
      const unlisten = vi.fn()
      const listen = vi.fn(() => Promise.resolve(unlisten))
      const backend = createTauriBackend({ fallback: recordingFallback(), invoke: vi.fn(), listen })
      const onProgress = () => {}
      const onAttempt = () => {}
      const onAnalysis = () => {}
      const onDenoise = () => {}

      expect(await backend.onProvisionProgress(onProgress)).toBe(unlisten)
      expect(await backend.onCloudAttempt(onAttempt)).toBe(unlisten)
      expect(await backend.onRemoteAnalysis(onAnalysis)).toBe(unlisten)
      expect(await backend.onDenoiseProgress(onDenoise)).toBe(unlisten)
      expect(listen).toHaveBeenNthCalledWith(1, CLOUD_EVENTS.provisionProgress, onProgress)
      expect(listen).toHaveBeenNthCalledWith(2, CLOUD_EVENTS.cloudAttempt, onAttempt)
      expect(listen).toHaveBeenNthCalledWith(3, CLOUD_EVENTS.remoteAnalysis, onAnalysis)
      expect(listen).toHaveBeenNthCalledWith(4, CLOUD_EVENTS.denoiseProgress, onDenoise)
      expect(CLOUD_EVENTS).toEqual({
        provisionProgress: 'provision://progress',
        qwenReview: 'qwen://review',
        cloudAttempt: 'cloud://attempt',
        remoteAnalysis: 'cloud://analysis',
        denoiseProgress: 'denoise://progress',
        denoiseRecorded: 'denoise://recorded',
      })
    })

    it('listens for a held quit, and answers it with confirm_quit or hide_to_tray', async () => {
      const unlisten = vi.fn()
      const listen = vi.fn(() => Promise.resolve(unlisten))
      const invoke = vi.fn().mockResolvedValue(null)
      const backend = createTauriBackend({ fallback: recordingFallback(), invoke, listen })
      const onQuit = () => {}

      expect(await backend.onQuitRequested(onQuit)).toBe(unlisten)
      expect(listen).toHaveBeenCalledWith(APP_EVENTS.quitRequested, onQuit)
      expect(APP_EVENTS).toEqual({ quitRequested: 'app://quit-requested' })

      await backend.confirmQuit()
      await backend.hideToTray()
      await backend.listJobs()
      expect(invoke.mock.calls).toEqual([['confirm_quit'], ['hide_to_tray'], ['list_jobs']])
    })

    it('names a cloud denoise with the caller\'s run id, and leaves it off for an older caller', async () => {
      const invoke = vi.fn().mockResolvedValue({ written: [], failed: [] })
      const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
      await backend.startCloudDenoise({ grantId: 'g1', outDir: '/out', runId: 'den-1' })
      await backend.startCloudDenoise({ grantId: 'g2', outDir: '/out' })
      expect(invoke.mock.calls).toEqual([
        ['start_cloud_denoise', { grantId: 'g1', outDir: '/out', runId: 'den-1' }],
        ['start_cloud_denoise', { grantId: 'g2', outDir: '/out' }],
      ])
    })

    it('hands the handler only the payload of a Tauri event', async () => {
      const listeners = new Map()
      globalThis.__TAURI__ = {
        event: {
          listen: (event, callback) => {
            listeners.set(event, callback)
            return Promise.resolve(() => listeners.delete(event))
          },
        },
      }
      try {
        const backend = createTauriBackend({ fallback: recordingFallback(), invoke: vi.fn() })
        const seen = []
        const unlisten = await backend.onCloudAttempt((payload) => seen.push(payload))
        listeners.get('cloud://attempt')({ event: 'cloud://attempt', id: 7, payload: { phase: 'queued' } })
        expect(seen).toEqual([{ phase: 'queued' }])
        unlisten()
        expect(listeners.has('cloud://attempt')).toBe(false)
      } finally {
        delete globalThis.__TAURI__
      }
    })

    it('outside a window, a listener resolves to an unlisten that does nothing', async () => {
      const backend = createTauriBackend({ fallback: recordingFallback(), invoke: vi.fn() })
      const unlisten = await backend.onProvisionProgress(() => {})
      expect(typeof unlisten).toBe('function')
      expect(() => unlisten()).not.toThrow()
    })

    it('stops a running helper with cancel_cloud_provisioner', async () => {
      const invoke = vi.fn().mockResolvedValue({ cancelled: true })
      const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
      await backend.cancelCloudProvisioner()
      expect(invoke).toHaveBeenCalledWith('cancel_cloud_provisioner')
    })

    it('passes the cloud grant to rerun_mask and clean_anyway inside params', async () => {
      const invoke = vi.fn().mockResolvedValue({})
      const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
      const params = {
        grantNonce: 'grant-1',
        executionTarget: { type: 'modal', profile_id: 'm1' },
        recipe: { recipe_id: 'mc-flux2-klein-inpaint-v1' },
        intent: { action: 'cleanAnyway' },
      }
      await backend.rerunMask({ maskId: 'r1-m2', kind: 'fill', engine: 'cloud', params })
      await backend.cleanAnyway({ regionId: 'r1', engine: 'cloud', params })
      await backend.cleanAnyway({ regionId: 'r2', engine: 'lama' })
      expect(invoke).toHaveBeenCalledWith('rerun_mask', { maskId: 'r1-m2', kind: 'fill', engine: 'cloud', params })
      expect(invoke).toHaveBeenCalledWith('clean_anyway', { regionId: 'r1', engine: 'cloud', params })
      expect(invoke).toHaveBeenCalledWith('clean_anyway', { regionId: 'r2', engine: 'lama' })
    })

    it('asks for recovery with apply, as the start-up pass does', async () => {
      const invoke = vi.fn().mockResolvedValue({ attached: [], stillRunning: [], needsAttention: [] })
      const backend = createTauriBackend({ fallback: recordingFallback(), invoke })
      await backend.reconcileCloudRecovery({ apply: true })
      expect(invoke).toHaveBeenCalledWith('reconcile_cloud_recovery', { apply: true })
    })
  })

  describe('cloud provisioner helper desktop bridge', () => {
    it('maps runCloudProvisioner to run_cloud_provisioner command with exact parameters', async () => {
      const responseEnvelope = {
        protocol_version: '1.0.0',
        request_id: 'req-prov-1',
        success: true,
        data: { op: 'inspect', provider: 'modal', status: 'ready' },
        error: null,
      }
      const invoke = vi.fn().mockResolvedValue(responseEnvelope)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const spec = {
        op: 'inspect',
        provider: 'modal',
        params: {
          credentials: { token_id: 'test-id', token_secret: 'test-secret' },
        },
      }
      const result = await backend.runCloudProvisioner(spec)
      expect(invoke).toHaveBeenCalledTimes(1)
      expect(invoke).toHaveBeenCalledWith('run_cloud_provisioner', spec)
      expect(result).toEqual(responseEnvelope)
      expect(fallback.calls.some((c) => c.method === 'runCloudProvisioner')).toBe(false)
    })

    it('propagates missing-helper and execution errors from runCloudProvisioner without fallback', async () => {
      const errorEnvelope = {
        protocol_version: '1.0.0',
        request_id: 'req-missing-1',
        success: false,
        data: null,
        error: {
          code: 'ERR_PROVIDER_UNAVAILABLE',
          message: 'Cloud provisioner helper binary not found',
          actionable_guidance: 'Install provisioner helper',
          remedy_steps: ['Set MANGA_CLEANER_PROVISIONER_BIN'],
        },
      }
      const invoke = vi.fn().mockResolvedValue(errorEnvelope)
      const fallback = recordingFallback()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.runCloudProvisioner({ op: 'probe_compatibility', provider: 'modal' })
      expect(result.success).toBe(false)
      expect(result.error.code).toBe('ERR_PROVIDER_UNAVAILABLE')
      expect(fallback.calls.some((c) => c.method === 'runCloudProvisioner')).toBe(false)
    })

    it('exports all 8 allowlisted provisioner IPC commands in TAURI_PROVISIONER_COMMANDS', () => {
      expect(TAURI_PROVISIONER_COMMANDS).toEqual([
        'run_cloud_provisioner',
        'cancel_cloud_provisioner',
        'provision_inspect',
        'provision_plan',
        'provision_apply',
        'provision_resume',
        'provision_cleanup',
        'provision_probe',
      ])
    })

    it('supports runCloudProvisioner in mock backend with simulated missing helper', async () => {
      const mock = createMockBackend({ timing: { method: 0 } })
      const successRes = await mock.runCloudProvisioner({
        op: 'inspect',
        provider: 'beam',
        params: { credentials: { beam_token: 'fake-token' } },
      })
      expect(successRes.success).toBe(true)
      expect(successRes.protocol_version).toBe('1.0.0')

      const noKey = await mock.runCloudProvisioner({ op: 'inspect', provider: 'beam' })
      expect(noKey.success).toBe(false)
      expect(noKey.error.code).toBe('ERR_VALIDATION_ERROR')

      const missingRes = await mock.runCloudProvisioner({
        op: 'inspect',
        provider: 'beam',
        params: { simulateMissingHelper: true },
      })
      expect(missingRes.success).toBe(false)
      expect(missingRes.error.code).toBe('ERR_PROVIDER_UNAVAILABLE')
    })
  })
})

describe('selecting a backend', () => {
  beforeEach(() => {
    setBackend(null)
    delete globalThis.__TAURI_INTERNALS__
    delete globalThis.__TAURI__
    delete globalThis.__MANGA_CLEANER_FORCE_MOCK__
  })

  it('is the mock outside a Tauri window', async () => {
    expect(isTauri()).toBe(false)
    // The mock's own projects. Outside a Tauri window there is no `invoke` to
    // reach `list_projects` with, so this is the mock answering, not the
    // adapter falling through.
    await expect(getBackend().listProjects()).resolves.toEqual(expect.any(Array))
  })

  it('is the adapter inside one', async () => {
    globalThis.__TAURI_INTERNALS__ = { invoke: vi.fn().mockResolvedValue({ appVersion: '9.9.9', facts: [] }) }
    expect(isTauri()).toBe(true)
    await expect(getBackend().about()).resolves.toEqual({ appVersion: '9.9.9', facts: [] })
  })

  it('the force-mock flag wins inside a Tauri window', async () => {
    globalThis.__TAURI_INTERNALS__ = { invoke: vi.fn() }
    globalThis.__MANGA_CLEANER_FORCE_MOCK__ = true
    const about = await getBackend().about()
    expect(about.appVersion).not.toBe('9.9.9')
    expect(globalThis.__TAURI_INTERNALS__.invoke).not.toHaveBeenCalled()
  })
})

it('passes Qwen guidance to batch preparation and review choices to their command', async () => {
  const invoke = vi.fn().mockResolvedValue(null)
  const backend = createTauriBackend({ invoke, fallback: {} })
  const qwenEdit = { target: 'sound_effect', description: 'Black letters beside the hand' }
  await backend.prepareCloudClean({ chapterId: 'c1', scope: 'page', qwenEdit })
  expect(invoke).toHaveBeenLastCalledWith('prepare_cloud_clean', expect.objectContaining({ qwenEdit }))
  await backend.resolveQwenReview({ attemptId: 'att-' + 'a'.repeat(24), choice: 'discard' })
  expect(invoke).toHaveBeenLastCalledWith('resolve_qwen_review', { attemptId: 'att-' + 'a'.repeat(24), choice: 'discard' })
})
