import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { FULL_LOCAL_DETECTOR_MODELS } from '../model/pipelines.js'
import { session, setAnalysisTarget, setCleanTarget, setDetectorModels, setModelAccelerator, setOcrRescue, setTextPolicy } from '../state/session.svelte.js'
import SettingsDialog from './SettingsDialog.svelte'
import { resetFirstLaunch } from './firstlaunch.svelte.js'

const SPEC = {
  id: 'local-detect-settings', kind: 'settings', titleKey: 'modal.title.settings', props: { tab: 'models' },
  actions: [{ id: 'close', labelKey: 'shell.action.close' }], blocking: false, dismissable: true, onresolve: null,
}

/** A real installation state shared by the catalogue, capability answer and installer callbacks. */
function open({ missing = [], runtime = true, sam = true, memory = true, saveFails = false, downloadFails = null, samFails = false, accelerators = { providers: [], models: [] }, deferredModel = null } = {}) {
  const absent = new Set(missing)
  let runtimeInstalled = runtime
  let samInstalled = sam
  let subscriber = () => {}
  const catalogue = () => ({
    models: ['textDetector', 'balloonDetector', 'fullRt', 'scriptGate', 'scriptGateLabels', 'hayaiVision', 'hayaiDecoder', 'hayaiTokenizer', 'inpainter'].map((id) => ({
      id, fileName: `${id}.onnx`, bytes: 100, sha256: 'ab'.repeat(32), kindKey: 'models.kind.textDetector',
      requiredBy: [], installed: !absent.has(id), path: absent.has(id) ? null : `/models/${id}.onnx`,
      readOnly: false, sha256Ok: absent.has(id) ? null : true, downloading: false, partialBytes: null,
    })),
    runtime: { installed: runtimeInstalled, path: runtimeInstalled ? '/runtime/libonnxruntime.dylib' : null,
      readOnly: false, downloading: false, available: true, version: '1.28.0', flavour: 'stock', bytes: 100,
      installedFlavour: runtimeInstalled ? 'stock' : null, installedVersion: runtimeInstalled ? '1.28.0' : null,
      flavours: [], platform: 'macos-arm64' },
    modelsDir: '/models', runtimeDir: '/runtime', hasToken: false, tokenStore: 'none', tokenStoreReason: null,
  })
  const caps = () => ({
    runtimeInstalled, runtimeReady: runtimeInstalled, ctdInstalled: !absent.has('textDetector'),
    rtInstalled: !absent.has('balloonDetector'), fullRtInstalled: !absent.has('fullRt'),
    fullRtManaged: !absent.has('fullRt'), fullRtRevision: 'pinned',
    fullRtFile: { name: 'detector.onnx', bytes: 100, sha256: 'ab'.repeat(32) },
    samInstalled, samMemoryReady: memory, samManaged: samInstalled,
    localDetectionReady: runtimeInstalled && !absent.has('fullRt') && samInstalled && memory,
    samFiles: samInstalled ? [{ name: 'encoder.onnx', bytes: 100, sha256: 'ab'.repeat(32) }] : [],
    samRevision: 'pinned', samWriteQualified: false, samWriteNote: 'Preview writes are separate.',
    rtBackends: [{ id: 'ort-cpu', available: runtimeInstalled, qualified: false }],
    samBackends: [{ id: 'ort-cpu', available: runtimeInstalled, qualified: false }],
  })
  const api = {
    listModels: vi.fn(async () => catalogue()),
    listWorkflowCapabilities: vi.fn(async () => caps()),
    listAccelerators: vi.fn(async () => accelerators),
    listSidecarModels: vi.fn(async () => []),
    sidecarAvailable: vi.fn(async () => ({ available: false, reasonKey: null })),
    about: vi.fn(async () => ({ appVersion: 'test', facts: [] })),
    diagnostics: vi.fn(async () => ({ components: [{ name: 'onnxruntime', available: runtimeInstalled }] })),
    subscribe: vi.fn((listen) => { subscriber = listen; return () => {} }),
    writeSettings: vi.fn(async () => {
      if (saveFails) throw new Error('settings unavailable')
      return {}
    }),
    downloadRuntime: vi.fn(async () => { runtimeInstalled = true; return 'started' }),
    downloadModel: vi.fn(async ({ id }) => {
      if (id === downloadFails) throw new Error('download unavailable')
      if (id !== deferredModel) absent.delete(id)
      return 'started'
    }),
    downloadModelGroup: vi.fn(async ({ id }) => {
      for (const file of id === 'scriptGate' ? ['scriptGate', 'scriptGateLabels'] : ['hayaiVision', 'hayaiDecoder', 'hayaiTokenizer']) absent.delete(file)
      return 'started'
    }),
    installSamTs: vi.fn(async () => {
      if (samFails) throw new Error('SAM download unavailable')
      samInstalled = true
      return true
    }),
    readInferenceConfig: async () => ({ schemaVersion: 1, selectedTarget: { type: 'local' }, beamProfiles: {}, modalProfiles: {} }),
  }
  setBackend(/** @type {any} */ (api))
  const rendered = render(SettingsDialog, { props: { spec: SPEC } })
  const tab = rendered.getByRole('tab', { name: t('settings.section.models') })
  const panel = /** @type {HTMLElement} */ (rendered.container.querySelector(`#${tab.getAttribute('aria-controls')}`))
  return { api, rendered, panel, scoped: within(panel), finishModel: (id) => {
    absent.delete(id)
    subscriber({ type: 'model-progress', id, downloaded: 100, total: 100, done: true, error: null })
  } }
}

const use = (view) => view.scoped.getByRole('button', { name: t('settings.detection.localFull.use') })
const install = (view) => view.scoped.getByRole('button', { name: t('settings.detection.localFull.install') })
const status = (view) => view.panel.querySelector('[data-local-detection-status]')?.textContent

afterEach(() => {
  cleanup()
  resetFirstLaunch()
  setBackend(null)
  setAnalysisTarget('rtFull', 'local')
  setAnalysisTarget('samTs', 'local')
  setDetectorModels(['ctd', 'rtSmall'])
  setOcrRescue(false)
  setTextPolicy('legacy_gate')
  setCleanTarget('local')
  setModelAccelerator('samTs', 'inherit')
})

it.each([false, true])('selects the full local pipeline from cloud detection, preserving reader=%s and the cleaning target', async (reader) => {
  setAnalysisTarget('rtFull', 'cloud')
  setAnalysisTarget('samTs', 'cloud')
  setCleanTarget('cloud')
  setOcrRescue(reader)
  const view = open()
  await fireEvent.click(use(view))
  await waitFor(() => expect(session.detectorModels).toEqual([...FULL_LOCAL_DETECTOR_MODELS]))
  expect(view.api.writeSettings).toHaveBeenCalledWith({ analysisTargets: { rtFull: 'local', samTs: 'local' } })
  expect(session.analysisTargets).toEqual({ rtFull: 'local', samTs: 'local' })
  expect(session.ocrRescue).toBe(reader)
  expect(session.cleanTarget).toBe('cloud')
  await waitFor(() => expect(status(view)).toContain(t('settings.detection.localFull.ready')))
})

it('keeps detection choices when saving the local routing fails', async () => {
  setDetectorModels(['samTs'])
  setOcrRescue(true)
  setAnalysisTarget('rtFull', 'cloud')
  setAnalysisTarget('samTs', 'cloud')
  const view = open({ saveFails: true })
  await fireEvent.click(use(view))
  await waitFor(() => expect(view.scoped.getByRole('alert').textContent).toContain(t('settings.detection.localFull.saveFailed')))
  expect(session.analysisTargets).toEqual({ rtFull: 'cloud', samTs: 'cloud' })
  expect(session.detectorModels).toEqual(['samTs'])
  expect(session.ocrRescue).toBe(true)
})

it('installs the runtime and missing full pipeline, then refreshes readiness without enabling the reader', async () => {
  setDetectorModels([...FULL_LOCAL_DETECTOR_MODELS])
  setTextPolicy('all_text')
  const view = open({ missing: ['textDetector', 'fullRt'], runtime: false, sam: false })
  await waitFor(() => expect(install(view)).toBeTruthy())
  expect(status(view)).toContain(t('settings.detection.localFull.pending'))
  await fireEvent.click(install(view))
  await waitFor(() => expect(status(view)).toContain(t('settings.detection.localFull.ready')))
  expect(view.api.downloadRuntime).toHaveBeenCalledOnce()
  expect(view.api.downloadModel.mock.calls.map(([request]) => request.id)).toEqual(['textDetector', 'fullRt'])
  expect(view.api.installSamTs).toHaveBeenCalledOnce()
  expect(view.api.downloadModelGroup).not.toHaveBeenCalled()
  expect(session.ocrRescue).toBe(false)
})

it('stops on a failed middle model and keeps the partial installation incomplete', async () => {
  setDetectorModels([...FULL_LOCAL_DETECTOR_MODELS])
  setTextPolicy('all_text')
  const view = open({ missing: ['textDetector', 'fullRt'], sam: false, downloadFails: 'fullRt' })
  await waitFor(() => expect(install(view)).toBeTruthy())
  await fireEvent.click(install(view))
  await waitFor(() => expect(view.scoped.getByRole('alert').textContent).toContain(t('settings.detection.localFull.installFailed')))
  expect(status(view)).toContain(t('settings.detection.localFull.pending'))
  expect(view.panel.querySelector('[data-model="rtFull"]').textContent).toContain('download unavailable')
  expect(view.api.installSamTs).not.toHaveBeenCalled()
  expect(view.api.downloadModel.mock.calls.map(([request]) => request.id)).toEqual(['textDetector', 'fullRt'])
})

it('does not claim readiness when SAM lacks memory, and does not require qualified preview writes for CPU detection', async () => {
  setDetectorModels([...FULL_LOCAL_DETECTOR_MODELS])
  const short = open({ memory: false })
  await waitFor(() => expect(status(short)).toContain(t('settings.detection.localFull.pending')))
  await waitFor(() => expect(short.panel.querySelector('.readiness').classList.contains('complete')).toBe(false))
  cleanup()
  const cpu = open()
  await waitFor(() => expect(status(cpu)).toContain(t('settings.detection.localFull.ready')))
})

it('opens Performance to choose the available CPU or GPU backend', async () => {
  const view = open()
  await fireEvent.click(view.scoped.getByRole('button', { name: t('settings.detection.localFull.backends') }))
  expect(view.rendered.getByRole('tab', { name: t('settings.section.performance') }).getAttribute('aria-selected')).toBe('true')
})

it('refreshes verified readiness when a started model download completes later', async () => {
  setDetectorModels([...FULL_LOCAL_DETECTOR_MODELS])
  const view = open({ missing: ['fullRt'], deferredModel: 'fullRt' })
  await waitFor(() => expect(install(view)).toBeTruthy())
  await fireEvent.click(install(view))
  await waitFor(() => expect(view.api.downloadModel).toHaveBeenCalledWith({ id: 'fullRt' }))
  expect(status(view)).toContain(t('settings.detection.localFull.pending'))
  view.finishModel('fullRt')
  await waitFor(() => expect(status(view)).toContain(t('settings.detection.localFull.ready')))
})

it('holds readiness to CTD and to a supported forced local backend', async () => {
  setDetectorModels([...FULL_LOCAL_DETECTOR_MODELS])
  const missingCtd = open({ missing: ['textDetector'] })
  await waitFor(() => expect(status(missingCtd)).toContain(t('settings.detection.localFull.pending')))
  cleanup()
  setModelAccelerator('samTs', 'cuda')
  const blocked = open({ accelerators: { providers: [], models: [{
    id: 'samTs', modelName: 'SAM-TS-L lettering mask', preference: 'cuda',
    backendStatus: [{ id: 'cuda', available: false, supported: false, installed: false, verified: false, reasonKey: 'accel.declined.unavailable' }],
  }] } })
  await waitFor(() => expect(blocked.panel.querySelector('.readiness').textContent).toContain('CUDA'))
  expect(status(blocked)).toContain(t('settings.detection.localFull.pending'))
})
