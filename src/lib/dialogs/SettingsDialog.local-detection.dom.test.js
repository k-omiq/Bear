import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { session, setAnalysisTarget, setCleanTarget, setDetectionMode, setModelAccelerator, setOcrRescue } from '../state/session.svelte.js'
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
    readMeasurement: vi.fn(async () => ({ saved: null, current: null, running: false })),
  }
  setBackend(/** @type {any} */ (api))
  const rendered = render(SettingsDialog, { props: { spec: SPEC } })
  const tab = rendered.getByRole('tab', { name: t('settings.section.models') })
  const panel = /** @type {HTMLElement} */ (rendered.container.querySelector(`#${tab.getAttribute('aria-controls')}`))
  return { api, rendered, panel, scoped: within(panel), progressModel: (id) => {
    subscriber({ type: 'model-progress', id, downloaded: 50, total: 100, done: false, error: null })
  }, finishModel: (id) => {
    absent.delete(id)
    subscriber({ type: 'model-progress', id, downloaded: 100, total: 100, done: true, error: null })
  } }
}

/** The Normal and Advanced cards, by their mode. */
const card = (view, mode) => /** @type {HTMLElement} */ (view.panel.querySelector(`[role="radiogroup"] [data-mode="${mode}"]`))
const readinessBox = (view) => /** @type {HTMLElement} */ (view.panel.querySelector('.readiness'))
const more = (view) => /** @type {HTMLElement} */ (
  view.panel.querySelector('[data-settings-anchor="detection-more"] button[aria-expanded]'))

afterEach(() => {
  cleanup()
  resetFirstLaunch()
  setBackend(null)
  setAnalysisTarget('rtFull', 'local')
  setAnalysisTarget('samTs', 'local')
  setDetectionMode('normal')
  setOcrRescue(false)
  setCleanTarget('local')
  setModelAccelerator('samTs', 'inherit')
})

it('offers detection as two mode cards, with More options shut', async () => {
  const view = open()
  const group = view.scoped.getByRole('radiogroup', { name: t('pipelines.detection') })
  const radios = within(group).getAllByRole('radio')
  expect(radios.map((radio) => radio.dataset.mode)).toEqual(['normal', 'advanced'])
  expect(radios[0].textContent).toContain(t('detectionMode.normal.name'))
  expect(radios[1].textContent).toContain(t('detectionMode.advanced.name'))
  expect(card(view, 'normal').getAttribute('aria-checked')).toBe('true')
  expect(card(view, 'advanced').getAttribute('aria-checked')).toBe('false')
  // The model rows, the reader and the filtering wait behind More options.
  expect(more(view).textContent).toContain(t('settings.detection.more'))
  expect(more(view).getAttribute('aria-expanded')).toBe('false')
  expect(view.panel.querySelector('[data-model="rtFull"]')).toBeNull()
  // No model checkboxes and no full-pipeline press any more.
  expect(view.panel.querySelector('input[type="checkbox"][data-detector]')).toBeNull()
  await fireEvent.click(more(view))
  expect(more(view).getAttribute('aria-expanded')).toBe('true')
  await waitFor(() => expect(view.panel.querySelector('[data-model="rtFull"]')).not.toBeNull())
})

it('fetches everything Advanced is missing as soon as it is chosen, the reader included', async () => {
  const view = open({ missing: ['fullRt', 'hayaiVision', 'hayaiDecoder', 'hayaiTokenizer'], sam: false })
  await waitFor(() => expect(readinessBox(view)).toBeTruthy())
  await fireEvent.click(card(view, 'advanced'))
  await waitFor(() => expect(view.api.installSamTs).toHaveBeenCalledOnce())
  expect(session.detectionMode).toBe('advanced')
  expect(session.detectorModels).toEqual(['ctd', 'rtFull', 'samTs'])
  expect(session.textPolicy).toBe('all_text')
  expect(session.ocrRescue).toBe(true)
  expect(view.api.downloadModel.mock.calls.map(([request]) => request.id)).toEqual(['fullRt'])
  expect(view.api.downloadModelGroup).toHaveBeenCalledOnce()
  expect(card(view, 'advanced').getAttribute('aria-checked')).toBe('true')
  await waitFor(() => expect(readinessBox(view).classList.contains('complete')).toBe(true))
  expect(readinessBox(view).textContent).toContain(t('settings.detection.ready.allText'))
})

it('counts the text reader Advanced switched on among what is missing, here as on the cloud', async () => {
  setDetectionMode('advanced')
  const view = open({ missing: ['hayaiVision', 'hayaiDecoder', 'hayaiTokenizer'] })
  await waitFor(() => expect(readinessBox(view).textContent).toContain(t('settings.detection.ready.allTextMissing', { bytes: 300 })))
  expect(readinessBox(view).classList.contains('complete')).toBe(false)
  const press = within(readinessBox(view)).getByRole('button', { name: t('settings.detection.download', { bytes: 300 }) })
  await fireEvent.click(press)
  await waitFor(() => expect(view.api.downloadModelGroup).toHaveBeenCalledOnce())
  expect(view.api.downloadModel).not.toHaveBeenCalled()
  await waitFor(() => expect(readinessBox(view).classList.contains('complete')).toBe(true))
})

it('brings detection back to this computer when Normal is chosen, and keeps the cleaning target', async () => {
  setDetectionMode('advanced')
  setAnalysisTarget('rtFull', 'cloud')
  setAnalysisTarget('samTs', 'cloud')
  setCleanTarget('cloud')
  const view = open()
  await fireEvent.click(card(view, 'normal'))
  await waitFor(() => expect(session.analysisTargets).toEqual({ rtFull: 'local', samTs: 'local' }))
  expect(view.api.writeSettings).toHaveBeenCalledWith({ analysisTargets: { rtFull: 'local', samTs: 'local' } })
  expect(session.detectionMode).toBe('normal')
  expect(session.detectorModels).toEqual(['ctd', 'rtSmall'])
  expect(session.ocrRescue).toBe(false)
  expect(session.cleanTarget).toBe('cloud')
  // Normal is all here already: nothing is fetched.
  expect(view.api.downloadModel).not.toHaveBeenCalled()
  expect(view.api.installSamTs).not.toHaveBeenCalled()
})

it('does nothing when the mode already chosen is pressed again', async () => {
  const view = open({ missing: ['fullRt'] })
  await waitFor(() => expect(readinessBox(view)).toBeTruthy())
  await fireEvent.click(card(view, 'normal'))
  expect(view.api.writeSettings).not.toHaveBeenCalled()
  expect(view.api.downloadModel).not.toHaveBeenCalled()
})

it('stops on a failed download and keeps Advanced incomplete', async () => {
  const view = open({ missing: ['textDetector', 'fullRt'], sam: false, downloadFails: 'fullRt' })
  await waitFor(() => expect(readinessBox(view)).toBeTruthy())
  await fireEvent.click(card(view, 'advanced'))
  await waitFor(() => expect(view.api.downloadModel.mock.calls.map(([request]) => request.id)).toEqual(['textDetector', 'fullRt']))
  expect(session.detectionMode).toBe('advanced')
  await waitFor(() => expect(readinessBox(view).classList.contains('complete')).toBe(false))
  expect(readinessBox(view).textContent).toContain(t('settings.detection.ready.allTextMissing', { bytes: 100 }))
})

it('says short memory for the lettering mask as a line, and does not hold readiness back for it', async () => {
  setDetectionMode('advanced')
  const short = open({ memory: false })
  await waitFor(() => expect(readinessBox(short).textContent).toContain(t('settings.detection.ready.allTextMemory')))
  expect(readinessBox(short).classList.contains('complete')).toBe(true)
})

it('says Downloading while a started model is still on its way, and is ready once it lands', async () => {
  const view = open({ missing: ['fullRt'], deferredModel: 'fullRt' })
  await waitFor(() => expect(readinessBox(view)).toBeTruthy())
  await fireEvent.click(card(view, 'advanced'))
  await waitFor(() => expect(view.api.downloadModel).toHaveBeenCalledWith({ id: 'fullRt' }))
  view.progressModel('fullRt')
  await waitFor(() => expect(readinessBox(view).textContent).toContain(t('settings.detection.ready.fetching')))
  expect(readinessBox(view).textContent).not.toContain(t('settings.detection.ready.allText'))
  expect(readinessBox(view).classList.contains('complete')).toBe(false)
  view.finishModel('fullRt')
  await waitFor(() => expect(readinessBox(view).classList.contains('complete')).toBe(true))
  expect(readinessBox(view).textContent).not.toContain(t('settings.detection.ready.fetching'))
})

it('holds readiness to CTD and to a supported forced local backend', async () => {
  setDetectionMode('advanced')
  setOcrRescue(false)
  const missingCtd = open({ missing: ['textDetector'] })
  await waitFor(() => expect(readinessBox(missingCtd).textContent).toContain(t('settings.detection.ready.allTextMissing', { bytes: 100 })))
  expect(readinessBox(missingCtd).classList.contains('complete')).toBe(false)
  cleanup()
  setModelAccelerator('samTs', 'cuda')
  const blocked = open({ accelerators: { providers: [], models: [{
    id: 'samTs', modelName: 'SAM-TS-L lettering mask', preference: 'cuda',
    backendStatus: [{ id: 'cuda', available: false, supported: false, installed: false, verified: false, reasonKey: 'accel.declined.unavailable' }],
  }] } })
  const held = t('settings.accel.unavailableForModel', {
    model: 'SAM-TS-L lettering mask', backend: t('accel.cuda'), reason: t('accel.declined.unavailable'),
  })
  await waitFor(() => expect(readinessBox(blocked).textContent).toContain(held))
  expect(readinessBox(blocked).classList.contains('complete')).toBe(false)
})
