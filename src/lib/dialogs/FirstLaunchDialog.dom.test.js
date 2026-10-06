/**
 * The setup a first launch opens, mounted: the eleven steps over the real
 * session, the real stores and a hand-written seam stub.
 *
 * The session and capabilities are the real modules, so a setting the setup
 * changes is checked where the rest of the app reads it. The backend is a stub
 * (`setBackend`) whose `subscribe` the tests drive, because the download run
 * is a sequence of calls and `model-progress` events and the interesting part
 * is the order they come in. The cloud provisioner is replaced by
 * `onboarding/ProvisionerStub.svelte`: the cloud step is tested against the
 * provisioner's props (IC-5), not against its flow, which is tested beside it.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'

vi.mock('./CloudProvisioner.svelte', async () => ({
  default: (await import('./onboarding/ProvisionerStub.svelte')).default,
}))

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { app, closeAllModals, pushModal } from '../state/app.svelte.js'
import { capabilities } from '../state/capabilities.svelte.js'
import {
  session,
  setAnalysisTarget,
  setCloseToTray,
  setCloudAllowed,
  setDetection,
  setDenoiseTarget,
  setDetectorModels,
  setFluxModel,
  setOcrRescue,
  setSidecarPath,
  setTextPolicy,
  setTheme,
} from '../state/session.svelte.js'
import { resetDenoiseState } from '../state/denoise.svelte.js'
import FirstLaunchDialog from './FirstLaunchDialog.svelte'
import SettingsDialog from './SettingsDialog.svelte'
import { DEFAULT_FLUX_MODEL, FIRST_LAUNCH_STEPS, RUNTIME_ID } from './firstlaunch.js'
import {
  configureFirstLaunchCloud,
  dismissFirstLaunch,
  firstLaunch,
  offerFirstLaunch,
  pauseFile,
  resetFirstLaunch,
  resumeFile,
  setFirstLaunchStep,
  startFirstLaunchDownloads,
} from './firstlaunch.svelte.js'

/** Each step's heading, in the order the steps come. */
const HEADINGS = FIRST_LAUNCH_STEPS.map((step) => `onboarding.${step}.heading`)

/** Sizes small enough to add up by eye. */
const DETECTION_BYTES = 95 + 4 + 1 + 11
const RUNTIME_BYTES = 32
const REDRAW_BYTES = 207
/** What Download costs with the default choices. */
const PRICE = DETECTION_BYTES + RUNTIME_BYTES + REDRAW_BYTES
const HAYAI_BYTES = 344 + 256 + 2

/** The spec `pushModal({kind: 'settings'})` would have handed Settings. */
const SPEC = {
  id: 'modal-1',
  kind: 'settings',
  titleKey: 'modal.title.settings',
  props: {},
  actions: [{ id: 'close', labelKey: 'shell.action.close' }],
  blocking: false,
  dismissable: true,
  onresolve: null,
}

/**
 * The catalogue with nothing installed: the Auto clean set, the redraw
 * engine, the Japanese reader's three files, and the runtime. Whole
 * `ModelsView` rows, because Settings draws every field of them when the
 * replay test mounts it.
 *
 * @param {{runtimeInstalled?: boolean}} [options]
 */
function view({ runtimeInstalled = false } = {}) {
  const row = (id, kindKey, bytes, requiredBy) => ({
    id,
    fileName: `${id}.onnx`,
    kindKey,
    bytes,
    requiredBy,
    installed: false,
    path: null,
    readOnly: false,
    sha256Ok: null,
    downloading: false,
    partialBytes: null,
  })
  return {
    modelsDir: '/app-data/models',
    runtimeDir: '/app-data/runtimes',
    hasToken: false,
    tokenStore: 'keychain',
    tokenStoreReason: null,
    models: [
      row('textDetector', 'models.kind.textDetector', 95, ['autoClean']),
      row('inpainter', 'models.kind.inpainter', REDRAW_BYTES, ['lama']),
      row('scriptGate', 'models.kind.scriptGate', 4, ['autoClean']),
      row('scriptGateLabels', 'models.kind.scriptGateLabels', 1, ['autoClean']),
      row('balloonDetector', 'models.kind.balloonDetector', 11, ['autoClean']),
      row('ocrEncoder', 'models.kind.ocr', 343, []),
      row('ocrDecoder', 'models.kind.ocrDecoder', 117, []),
      row('ocrVocab', 'models.kind.ocrVocab', 1, []),
      row('hayaiVision', 'models.kind.hayaiVision', 344, []),
      row('hayaiDecoder', 'models.kind.hayaiDecoder', 256, []),
      row('hayaiTokenizer', 'models.kind.hayaiTokenizer', 2, []),
    ],
    runtime: {
      installed: runtimeInstalled,
      bytes: RUNTIME_BYTES,
      available: true,
      path: null,
      readOnly: false,
      downloading: false,
      version: '1.28.0',
      flavour: 'stock',
      flavours: [],
      platform: 'macos-arm64',
      installedFlavour: null,
      installedVersion: null,
      partialBytes: null,
    },
  }
}

/**
 * What the runtime answers about its processors: one it can use, one it
 * cannot, and the stored preference marked the way the backend marks it.
 *
 * @param {string} preference
 */
function accelerators(preference) {
  const provider = (id, available, reasonKey) => ({
    id,
    labelKey: `accel.${id}`,
    available,
    reasonKey,
    measured: false,
    active: available,
    selected: id === preference,
  })
  return {
    preference,
    providers: [provider('cpu', true, null), provider('coreml', false, 'accel.declined.unavailable')],
    models: [],
  }
}

function makeBackend() {
  const handlers = new Set()
  return {
    /** Deliver one event to everything subscribed, as the process-wide channel does. */
    emit(event) {
      for (const handler of [...handlers]) handler(event)
    },
    subscribe: vi.fn((handler) => {
      handlers.add(handler)
      return () => handlers.delete(handler)
    }),
    listModels: vi.fn(async () => view()),
    listWorkflowCapabilities: vi.fn(async () => ({ samInstalled: false })),
    installSamTs: vi.fn(async () => true),
    writeSettings: vi.fn(async () => ({})),
    downloadRuntime: vi.fn(async () => 'started'),
    downloadModel: vi.fn(async () => 'started'),
    downloadModelGroup: vi.fn(async () => 'started'),
    cancelDownload: vi.fn(async () => true),
    listAccelerators: vi.fn(async () => accelerators('auto')),
    listSidecarModels: vi.fn(async () => []),
    sidecarAvailable: vi.fn(async () => ({ available: false, reasonKey: null })),
    about: vi.fn(async () => ({ appVersion: '0.0.0-test', facts: [] })),
    discardPartial: vi.fn(async () => true),
  }
}

/** @type {ReturnType<typeof makeBackend>} */
let backend

beforeEach(() => {
  backend = makeBackend()
  setBackend(/** @type {any} */ (backend))
  setCloseToTray(false)
  setTheme('system')
  setCloudAllowed(false)
  setDenoiseTarget('off')
  resetDenoiseState()
  setFluxModel('')
  setSidecarPath('')
  for (const language of ['ja', 'zh', 'ko']) setDetection(language, 'ctd-rtdetr')
  setOcrRescue(false)
  setTextPolicy('legacy_gate')
  setDetectorModels(['ctd', 'rtSmall'])
  setAnalysisTarget('rtFull', 'local')
  setAnalysisTarget('samTs', 'local')
  session.firstLaunchOffered = false
  capabilities.sidecar = false
})

afterEach(() => {
  cleanup()
  resetFirstLaunch()
  closeAllModals()
  setBackend(null)
  vi.clearAllMocks()
})

/** Open the setup the way a first launch does, on one of its steps. */
function open(step = 'welcome', answer = view()) {
  expect(offerFirstLaunch(answer)).toBe(true)
  setFirstLaunchStep(step)
  return render(FirstLaunchDialog)
}

/** @param {ReturnType<typeof render>} rendered */
function heading(rendered) {
  return rendered.getByRole('heading', { level: 1 })
}

/**
 * @param {ReturnType<typeof render>} rendered
 * @param {string} name
 */
async function press(rendered, name) {
  await fireEvent.click(rendered.getByRole('button', { name }))
}

/**
 * A download ending, as the backend reports it: well, or with its error.
 *
 * @param {string} id
 * @param {string|null} [error]
 */
function finish(id, error = null) {
  backend.emit({ type: 'model-progress', id, downloaded: 1, total: 1, done: true, error })
}

/** @param {string} id @param {string|null} [error] */
function finishGroup(id, error = null) {
  backend.emit({ type: 'model-progress', id, downloaded: 0, total: null, done: true, error })
}

describe('the ten steps', () => {
  it('offers the Discord invite after cloud, then continues to dependencies', async () => {
    const rendered = open('community')
    const invite = rendered.getByRole('link', { name: t('onboarding.community.join') })
    expect(invite.getAttribute('href')).toBe('https://discord.gg/3ueKc9PaX7')
    expect(invite.getAttribute('target')).toBe('_blank')
    await press(rendered, t('onboarding.action.next'))
    expect(firstLaunch.step).toBe('dependencies')
  })

  it('come in order, forward and back, and walking through changes nothing', async () => {
    const rendered = open()
    const seen = [heading(rendered).textContent?.trim()]
    // The cover has one way in and no footer.
    expect(rendered.queryByRole('button', { name: t('onboarding.action.back') })).toBeNull()
    expect(rendered.getByRole('link', { name: t('onboarding.welcome.source') }).getAttribute('href')).toBe(
      'https://github.com/k-omiq/manga-cleaner',
    )

    await press(rendered, t('onboarding.action.start'))
    expect(firstLaunch.step).toBe('theme')
    await press(rendered, t('onboarding.action.back'))
    expect(firstLaunch.step).toBe('welcome')
    await press(rendered, t('onboarding.action.start'))

    // theme, token (skipped), background, detection, cleaning, denoise, cloud (not now), community, dependencies
    for (const action of [
      'onboarding.action.next',
      'onboarding.action.skipStep',
      'onboarding.action.next',
      'onboarding.action.next',
      'onboarding.action.next',
      'onboarding.action.next',
      'onboarding.action.notNow',
      'onboarding.action.next',
    ]) {
      seen.push(heading(rendered).textContent?.trim())
      await press(rendered, t(action))
    }
    seen.push(heading(rendered).textContent?.trim())
    expect(firstLaunch.step).toBe('dependencies')
    setFirstLaunchStep('downloads')
    await waitFor(() => expect(heading(rendered).textContent?.trim()).toBe(t('onboarding.downloads.heading')))
    seen.push(heading(rendered).textContent?.trim())

    expect(seen).toEqual(HEADINGS.map((key) => t(key)))
    expect(backend.downloadRuntime).not.toHaveBeenCalled()
    expect(backend.downloadModel).not.toHaveBeenCalled()
    expect(backend.writeSettings).not.toHaveBeenCalled()
  })

  it('put focus on each heading, where Enter presses the primary action but never Download', async () => {
    const rendered = open()
    await waitFor(() => expect(document.activeElement).toBe(heading(rendered)))
    await fireEvent.keyDown(heading(rendered), { key: 'Enter' })
    expect(firstLaunch.step).toBe('theme')
    await waitFor(() => expect(document.activeElement).toBe(heading(rendered)))
    // A held key is not a press, and Enter on a control belongs to it.
    await fireEvent.keyDown(heading(rendered), { key: 'Enter', repeat: true })
    await fireEvent.keyDown(rendered.getByRole('button', { name: t('onboarding.action.back') }), { key: 'Enter' })
    expect(firstLaunch.step).toBe('theme')

    setFirstLaunchStep('dependencies')
    await waitFor(() => expect(document.activeElement).toBe(heading(rendered)))
    await fireEvent.keyDown(heading(rendered), { key: 'Enter' })
    expect(firstLaunch.step).toBe('dependencies')
    expect(backend.downloadRuntime).not.toHaveBeenCalled()
  })
})

describe('leaving early', () => {
  it('Skip setup closes it from any step and records that it was offered', async () => {
    const rendered = open('detection')
    await press(rendered, t('onboarding.action.skip'))
    expect(firstLaunch.open).toBe(false)
    expect(session.firstLaunchOffered).toBe(true)

    // The next launch does not ask again; Settings still can.
    expect(offerFirstLaunch(view())).toBe(false)
    expect(firstLaunch.open).toBe(false)
    expect(offerFirstLaunch(view(), { force: true })).toBe(true)
    expect(firstLaunch.step).toBe('welcome')
  })

  it('Escape closes it the same way', async () => {
    open('background')
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(firstLaunch.open).toBe(false)
    expect(session.firstLaunchOffered).toBe(true)
  })
})

describe('the setting steps', () => {
  it('apply a theme the moment it is picked, and send it to the backend', async () => {
    const rendered = open('theme')
    await fireEvent.click(rendered.getByRole('radio', { name: t('settings.theme.jade') }))
    expect(session.theme).toBe('jade')
    await waitFor(() => expect(backend.writeSettings).toHaveBeenCalledWith(expect.objectContaining({ theme: 'jade' })))
  })

  it('save a Hugging Face key to the backend alone, or let it be skipped', async () => {
    const rendered = open('token')
    expect(rendered.getByRole('button', { name: t('onboarding.action.skipStep') })).toBeTruthy()
    await fireEvent.input(rendered.getByLabelText(t('onboarding.token.label')), { target: { value: 'hf_test' } })
    await press(rendered, t('onboarding.token.save'))
    await waitFor(() => expect(backend.writeSettings).toHaveBeenCalledWith({ hfToken: 'hf_test' }))
    expect(firstLaunch.step).toBe('background')
    expect(JSON.stringify(session)).not.toContain('hf_test')
  })

  it('say when a key could not be saved, and stay on the step', async () => {
    backend.writeSettings.mockRejectedValueOnce(new Error('keychain locked'))
    const rendered = open('token')
    await fireEvent.input(rendered.getByLabelText(t('onboarding.token.label')), { target: { value: 'hf_test' } })
    await press(rendered, t('onboarding.token.save'))
    expect((await rendered.findByRole('alert')).textContent).toBe(t('onboarding.token.failed'))
    expect(firstLaunch.step).toBe('token')
  })

  it('say what quitting on close costs, and store the choice in both places', async () => {
    const rendered = open('background')
    expect(rendered.getByText(/Downloads and cleaning stop/)).toBeTruthy()
    await fireEvent.click(rendered.getByRole('radio', { name: new RegExp(t('onboarding.background.keep')) }))
    await waitFor(() => expect(backend.writeSettings).toHaveBeenCalledWith(expect.objectContaining({ closeToTray: true })))
    expect(session.closeToTray).toBe(true)
  })

  it('take a choice back, and say so, when the backend refuses it', async () => {
    backend.writeSettings.mockRejectedValueOnce(new Error('disk full'))
    const rendered = open('background')
    await fireEvent.click(rendered.getByRole('radio', { name: new RegExp(t('onboarding.background.keep')) }))
    expect((await rendered.findByRole('alert')).textContent).toBe(t('onboarding.saveFailed'))
    expect(session.closeToTray).toBe(false)
  })
})

describe('the pipelines', () => {
  it('drop a skipped language, and every detection file once all three are skipped', async () => {
    const rendered = open('detection')
    expect(rendered.queryByText('RT-DETR v2 + COO + SAM-TS')).toBeNull()
    for (const language of ['Japanese', 'Chinese', 'Korean']) {
      await fireEvent.change(rendered.getByLabelText(t('pipelines.detectorFor', { language })), { target: { value: '' } })
    }
    setFirstLaunchStep('dependencies')
    // The runtime and the cleaner; no detection file.
    await waitFor(() =>
      expect(rendered.getByText(t('onboarding.dependencies.total', { count: 2, bytes: RUNTIME_BYTES + REDRAW_BYTES }))).toBeTruthy(),
    )
  })

  it('offer the text reader as an opt-in switch, not a detector, and fetch it only when ticked', async () => {
    const rendered = open('detection')
    for (const language of ['Japanese', 'Chinese', 'Korean']) {
      const select = /** @type {HTMLSelectElement} */ (rendered.getByLabelText(t('pipelines.detectorFor', { language })))
      expect([...select.options].map((option) => option.value)).toEqual(['ctd-rtdetr', ''])
    }
    const rescue = /** @type {HTMLInputElement} */ (rendered.getByRole('checkbox', { name: t('pipelines.workflow.ocrRescue') }))
    expect(rescue.checked).toBe(false)
    // The cost is said before the box is ticked, and read with it.
    const cost = t('settings.detection.rescue.size', { bytes: HAYAI_BYTES })
    expect(rendered.getByText(cost)).toBeTruthy()
    expect(rescue.getAttribute('aria-describedby')?.split(' ').map((id) => document.getElementById(id)?.textContent))
      .toEqual([t('pipelines.workflow.ocrRescueDescription'), cost])

    await fireEvent.click(rescue)
    expect(session.ocrRescue).toBe(true)
    setFirstLaunchStep('dependencies')
    await waitFor(() =>
      expect(rendered.getByText(t('onboarding.dependencies.total', { count: 9, bytes: PRICE + HAYAI_BYTES }))).toBeTruthy(),
    )
  })

  it('keep the text reader once Japanese is skipped, since it reads Chinese and Korean too', async () => {
    setOcrRescue(true)
    const rendered = open('detection')
    await fireEvent.change(rendered.getByLabelText(t('pipelines.detectorFor', { language: 'Japanese' })), { target: { value: '' } })
    setFirstLaunchStep('dependencies')
    // Chinese and Korean still need the detector pair, the script gate and the reader.
    await waitFor(() => expect(rendered.getByText(t('onboarding.dependencies.total', { count: 9, bytes: PRICE + HAYAI_BYTES }))).toBeTruthy())
  })

  it('fetch the text reader but not the script gate when a replay finds all-text review chosen', async () => {
    setTextPolicy('all_text')
    setOcrRescue(true)
    const rendered = open('detection')
    expect(rendered.getByText(t('settings.detection.setupAllText'))).toBeTruthy()
    setFirstLaunchStep('dependencies')
    // The stored CTD + small RT choice remains selected; all-text omits the gate, not the reader.
    await waitFor(() =>
      expect(rendered.getByText(t('onboarding.dependencies.total', { count: 7, bytes: RUNTIME_BYTES + 95 + 11 + REDRAW_BYTES + HAYAI_BYTES }))).toBeTruthy(),
    )
  })

  // The boxes and the reader are this computer's choice; a cloud GPU always
  // uses all four, and one line says so.
  it('say that the cloud GPU uses all four models, beside this computer’s own choice', async () => {
    const rendered = open('detection')
    expect(rendered.getByText(t('pipelines.cloudCombo'))).toBeTruthy()
    expect(rendered.queryByText(t('onboarding.detection.cloudNow'))).toBeNull()
    expect(rendered.getByRole('checkbox', { name: 'Comic Text Detector (CTD)' }).checked).toBe(true)
    expect(rendered.getByRole('checkbox', { name: 'SAM-TS-L lettering mask' }).checked).toBe(false)
    expect(rendered.getByRole('checkbox', { name: t('pipelines.workflow.ocrRescue') }).checked).toBe(false)
  })

  // A replay with Detect on already Cloud GPU shows that choice, the fixed
  // four in place of the picker, and downloads what that run needs here: CTD,
  // the script gate under legacy, and the text reader, never Small or a cloud
  // stage, and installs no SAM-TS-L. The stored local choice is kept.
  it('fetch only CTD and the text reader when a replay finds detection on the cloud GPU', async () => {
    setDetectorModels(['rtSmall', 'samTs'])
    setAnalysisTarget('rtFull', 'cloud')
    setAnalysisTarget('samTs', 'cloud')
    const rendered = open('detection')
    expect(/** @type {HTMLInputElement} */ (rendered.getByRole('radio', { name: t('tools.option.onCloud') })).checked).toBe(true)
    expect(rendered.getByText(t('onboarding.detection.cloudNow'))).toBeTruthy()
    for (const name of ['Comic Text Detector (CTD)', 'Ogkalu comic text & bubble detector (Full)', 'SAM-TS-L lettering mask', t('pipelines.workflow.ocrRescue')]) {
      const box = /** @type {HTMLInputElement} */ (rendered.getByRole('checkbox', { name }))
      expect(box.checked, name).toBe(true)
      expect(box.disabled, name).toBe(true)
    }
    expect(rendered.queryByRole('checkbox', { name: 'Ogkalu comic text & bubble detector (Small)' })).toBeNull()
    setFirstLaunchStep('dependencies')
    // The runtime, CTD, the gate pair, the reader's three files and the redraw.
    await waitFor(() => expect(rendered.getByText(t('onboarding.dependencies.total', {
      count: 8, bytes: RUNTIME_BYTES + 95 + 4 + 1 + HAYAI_BYTES + REDRAW_BYTES,
    }))).toBeTruthy())
    startFirstLaunchDownloads()
    expect(firstLaunch.queue).toEqual([RUNTIME_ID, 'textDetector', 'scriptGate', 'scriptGateLabels',
      'hayaiVision', 'hayaiDecoder', 'hayaiTokenizer', 'inpainter'])
    expect(backend.installSamTs).not.toHaveBeenCalled()
    expect(session.detectorModels).toEqual(['rtSmall', 'samTs'])
    expect(session.ocrRescue).toBe(false)
  })

  it('show the optional page review and provisional cleaner ratings', async () => {
    const rendered = open('detection')
    expect(rendered.getByText(t('settings.detection.setupReview'))).toBeTruthy()
    expect(rendered.queryByText(t('settings.detection.setupAllText'))).toBeNull()
    setFirstLaunchStep('cleaning')
    await waitFor(() => expect(rendered.getByRole('heading', { level: 1 }).textContent).toBe(t('onboarding.cleaning.heading')))
    expect(rendered.getByText(t('pipelines.workflow.ratingsNote'))).toBeTruthy()
  })

  it('installs a selected SAM-TS-L model during onboarding', async () => {
    setDetectorModels(['samTs'])
    setTextPolicy('all_text')
    const rendered = open('detection', view({ runtimeInstalled: true }))
    expect(rendered.getByRole('checkbox', { name: 'SAM-TS-L lettering mask' }).checked).toBe(true)
    firstLaunch.cleaners = { 'lama-manga': false }
    startFirstLaunchDownloads()
    await waitFor(() => expect(backend.installSamTs).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(firstLaunch.status.samTs).toBe('done'))
  })

  it('draw cleaners that cannot be fetched as disabled, and FLUX as the helper\'s', async () => {
    const rendered = open('cleaning')
    expect(/** @type {HTMLInputElement} */ (rendered.getByLabelText(/Qwen-Image-Edit-2511/)).disabled).toBe(true)
    expect(/** @type {HTMLInputElement} */ (rendered.getByLabelText(/LaMa Manga/)).checked).toBe(true)
    expect(rendered.getAllByText(t('pipelines.status.needsHelper')).length).toBeGreaterThan(0)
    expect(rendered.getByRole('button', { name: t('shell.action.chooseFolder') })).toBeTruthy()
  })

  it('mark FLUX models the helper lists, and pick the recommended one', async () => {
    capabilities.sidecar = true
    backend.listSidecarModels.mockResolvedValue([{ id: DEFAULT_FLUX_MODEL, label: 'FLUX.2 Klein 4B' }])
    const rendered = open('cleaning')
    await waitFor(() => expect(rendered.getByText(t('pipelines.status.found'))).toBeTruthy())
    await waitFor(() => expect(session.fluxModel).toBe(DEFAULT_FLUX_MODEL))
    expect(rendered.queryByRole('button', { name: t('shell.action.chooseFolder') })).toBeNull()
  })
})

describe('the dependencies step', () => {
  it('names the platform the backend reported, the runtime build, and what the choices cost', async () => {
    const rendered = open('dependencies')
    expect(rendered.getByText(t('onboarding.dependencies.body', { platform: t('onboarding.dependencies.platform.macArm') }))).toBeTruthy()
    expect(rendered.getByText('1.28.0')).toBeTruthy()
    expect(rendered.getByText(t('onboarding.dependencies.afterRuntime'))).toBeTruthy()
    expect(rendered.getByText(t('onboarding.dependencies.total', { count: 6, bytes: PRICE }))).toBeTruthy()
    // Download is the primary action and names its price.
    expect(rendered.getByRole('button', { name: new RegExp(t('onboarding.dependencies.start')) })).toBeTruthy()
  })

  it('says what a build needs installed by hand', async () => {
    const answer = view()
    answer.runtime.flavour = 'cuda12'
    answer.runtime.flavours = [{ id: 'cuda12', ortVersion: '1.28.0', bytes: 1, isDefault: false, userInstalled: ['CUDA 12', 'cuDNN 9'] }]
    const rendered = open('dependencies', answer)
    expect(rendered.getByText(t('onboarding.dependencies.needs', { items: 'CUDA 12, cuDNN 9' }))).toBeTruthy()
  })

  it('asks the runtime for its graphics acceleration once it is installed', async () => {
    const rendered = open('dependencies', view({ runtimeInstalled: true }))
    await waitFor(() => expect(backend.listAccelerators).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(rendered.getByText(t('onboarding.dependencies.cpuOnly'))).toBeTruthy())
  })

  it('offers the Hayai reader a provider only when the run reads', async () => {
    const answer = accelerators('auto')
    answer.models = [
      { id: 'ctd', modelName: 'Comic Text Detector (CTD)', backendStatus: [] },
      { id: 'hayai', modelName: 'Hayai OCR v2.5 Nova', backendStatus: [] },
      { id: 'inpainter', modelName: 'LaMa Manga', backendStatus: [] },
    ]
    backend.listAccelerators.mockImplementation(async () => answer)
    const label = t('settings.accel.modelLabel', { model: 'Hayai OCR v2.5 Nova' })

    const off = open('dependencies', view({ runtimeInstalled: true }))
    await waitFor(() => expect(off.getByText('Comic Text Detector (CTD)')).toBeTruthy())
    expect(off.queryByText('Hayai OCR v2.5 Nova')).toBeNull()
    cleanup()

    setOcrRescue(true)
    const on = open('dependencies', view({ runtimeInstalled: true }))
    await waitFor(() => expect(on.getByText('Hayai OCR v2.5 Nova')).toBeTruthy())
    expect(on.getByLabelText(label)).toBeTruthy()
  })
})

describe('the downloads', () => {
  it('fetch the runtime first, pause one file, carry on, and resume it in its place', async () => {
    const rendered = open('dependencies')
    await press(rendered, new RegExp(t('onboarding.dependencies.start')))
    expect(firstLaunch.step).toBe('downloads')
    await waitFor(() => expect(backend.downloadRuntime).toHaveBeenCalledTimes(1))
    // The choices are kept where the rest of the app reads them.
    expect(session.detection).toEqual({ ja: 'ctd-rtdetr', zh: 'ctd-rtdetr', ko: 'ctd-rtdetr' })

    await fireEvent.click(rendered.getByRole('button', { name: t('onboarding.downloads.pause', { name: t('settings.models.runtime.label') }) }))
    expect(backend.cancelDownload).toHaveBeenCalledWith({ id: RUNTIME_ID })
    finish(RUNTIME_ID, 'cancelled')
    await waitFor(() => expect(backend.downloadModel).toHaveBeenCalledWith({ id: 'textDetector' }))
    expect(firstLaunch.status[RUNTIME_ID]).toBe('paused')

    resumeFile(RUNTIME_ID)
    finish('textDetector')
    await waitFor(() => expect(backend.downloadRuntime).toHaveBeenCalledTimes(2))
    finish(RUNTIME_ID)
    await waitFor(() => expect(backend.downloadModel).toHaveBeenCalledWith({ id: 'balloonDetector' }))
    finish('balloonDetector')
    await waitFor(() => expect(backend.downloadModelGroup).toHaveBeenCalledWith({ id: 'scriptGate' }))
    finish('scriptGate')
    finish('scriptGateLabels')
    finishGroup('scriptGate')
    await waitFor(() => expect(backend.downloadModel).toHaveBeenCalledWith({ id: 'inpainter' }))
    finish('inpainter')
    await waitFor(() => expect(firstLaunch.running).toBe(false))
    expect(Object.values(firstLaunch.status).every((status) => status === 'done')).toBe(true)

    await press(rendered, t('home.action.newProject'))
    expect(firstLaunch.open).toBe(false)
    await waitFor(() => expect(app.modals.map((modal) => modal.kind)).toEqual(['newProject']))
  })

  it('mark a failed file, move on, and offer it again', async () => {
    const rendered = open('dependencies')
    startFirstLaunchDownloads()
    setFirstLaunchStep('downloads')
    await waitFor(() => expect(backend.downloadRuntime).toHaveBeenCalledTimes(1))
    finish(RUNTIME_ID, 'network down')
    await waitFor(() => expect(backend.downloadModel).toHaveBeenCalledWith({ id: 'textDetector' }))
    expect(firstLaunch.status[RUNTIME_ID]).toBe('failed')
    expect((await rendered.findByRole('alert')).textContent).toBe('network down')
    await fireEvent.click(rendered.getByRole('button', { name: t('onboarding.downloads.retry', { name: t('settings.models.runtime.label') }) }))
    expect(firstLaunch.status[RUNTIME_ID]).toBe('waiting')
    expect(firstLaunch.errors[RUNTIME_ID]).toBeUndefined()
  })

  it('installs the script gate as one atomic group and pauses/resumes both files together', async () => {
    open('dependencies')
    startFirstLaunchDownloads()
    await waitFor(() => expect(backend.downloadRuntime).toHaveBeenCalledTimes(1))
    finish(RUNTIME_ID)
    await waitFor(() => expect(backend.downloadModel).toHaveBeenCalledWith({ id: 'textDetector' }))
    finish('textDetector')
    await waitFor(() => expect(backend.downloadModel).toHaveBeenCalledWith({ id: 'balloonDetector' }))
    finish('balloonDetector')
    await waitFor(() => expect(backend.downloadModelGroup).toHaveBeenCalledWith({ id: 'scriptGate' }))
    expect(backend.downloadModel).not.toHaveBeenCalledWith({ id: 'scriptGate' })
    expect(firstLaunch.status.scriptGate).toBe('active')
    expect(firstLaunch.status.scriptGateLabels).toBe('active')

    await pauseFile('scriptGateLabels')
    expect(backend.cancelDownload).toHaveBeenCalledWith({ id: 'scriptGate' })
    expect(firstLaunch.status.scriptGate).toBe('paused')
    expect(firstLaunch.status.scriptGateLabels).toBe('paused')
    finishGroup('scriptGate', 'cancelled')
    await waitFor(() => expect(backend.downloadModel).toHaveBeenCalledWith({ id: 'inpainter' }))
    finish('inpainter')
    await waitFor(() => expect(firstLaunch.running).toBe(false))

    resumeFile('scriptGateLabels')
    await waitFor(() => expect(backend.downloadModelGroup).toHaveBeenCalledTimes(2))
    finish('scriptGate')
    finish('scriptGateLabels')
    finishGroup('scriptGate')
    await waitFor(() => expect(firstLaunch.status.scriptGate).toBe('done'))
    expect(firstLaunch.status.scriptGateLabels).toBe('done')
  })

  it('pause everything, then resume everything', async () => {
    const rendered = open('dependencies')
    startFirstLaunchDownloads()
    setFirstLaunchStep('downloads')
    await waitFor(() => expect(backend.downloadRuntime).toHaveBeenCalledTimes(1))
    await press(rendered, t('onboarding.downloads.pauseAll'))
    finish(RUNTIME_ID, 'cancelled')
    await waitFor(() => expect(firstLaunch.running).toBe(false))
    expect(backend.downloadModel).not.toHaveBeenCalled()
    await press(rendered, t('onboarding.downloads.resumeAll'))
    await waitFor(() => expect(backend.downloadRuntime).toHaveBeenCalledTimes(2))
  })

  it('store a detection choice at once, with nothing to download', async () => {
    const rendered = open('detection')
    await fireEvent.change(rendered.getByLabelText(t('pipelines.detectorFor', { language: 'Korean' })), { target: { value: '' } })
    expect(session.detection.ko).toBeNull()
  })

  it('never fetch a file that arrived through Settings before the press', async () => {
    open('theme')
    finish('textDetector')
    startFirstLaunchDownloads()
    await waitFor(() => expect(backend.downloadRuntime).toHaveBeenCalledTimes(1))
    finish(RUNTIME_ID)
    await waitFor(() => expect(backend.downloadModel).toHaveBeenCalledWith({ id: 'balloonDetector' }))
    expect(backend.downloadModel).not.toHaveBeenCalledWith({ id: 'textDetector' })
  })

  it('leave a paused file paused when Download is pressed again', async () => {
    open('dependencies')
    startFirstLaunchDownloads()
    await waitFor(() => expect(backend.downloadRuntime).toHaveBeenCalledTimes(1))
    await pauseFile('inpainter')
    expect(firstLaunch.status.inpainter).toBe('paused')
    startFirstLaunchDownloads()
    expect(firstLaunch.status.inpainter).toBe('paused')
  })

  it('keep going after the setup is closed', async () => {
    open('dependencies')
    startFirstLaunchDownloads()
    await waitFor(() => expect(backend.downloadRuntime).toHaveBeenCalledTimes(1))
    dismissFirstLaunch()
    finish(RUNTIME_ID)
    await waitFor(() => expect(backend.downloadModel).toHaveBeenCalledWith({ id: 'textDetector' }))
    expect(session.firstLaunchOffered).toBe(true)
  })

  it('keep a run in flight when the setup is opened again', async () => {
    open('dependencies')
    startFirstLaunchDownloads()
    await waitFor(() => expect(backend.downloadRuntime).toHaveBeenCalledTimes(1))
    dismissFirstLaunch()
    expect(offerFirstLaunch(view(), { force: true })).toBe(true)
    expect(firstLaunch.status[RUNTIME_ID]).toBe('active')
    finish(RUNTIME_ID)
    await waitFor(() => expect(backend.downloadModel).toHaveBeenCalledWith({ id: 'textDetector' }))
  })
})

/**
 * Setup's Detect on: This computer, or Cloud GPU once the cloud can be used.
 * It is written through the editor's own owner at once
 * (`cloudtargets.svelte.js#chooseDetectTarget`), and the download plan follows it.
 */
describe('Detect on in setup', () => {
  /** @param {ReturnType<typeof render>} rendered */
  const places = (rendered) => [...rendered.container.querySelectorAll('[data-detect-on] input[type="radio"]')]
    .map((input) => /** @type {HTMLInputElement} */ (input).value)

  it('offers This computer only until a cloud GPU can be used, and says where to set one up', () => {
    const rendered = open('detection')
    expect(places(rendered)).toEqual(['local'])
    expect(/** @type {HTMLInputElement} */ (rendered.getByRole('radio', { name: t('tools.option.onLocal') })).checked).toBe(true)
    const later = rendered.getByText(t('onboarding.detection.cloudLater'))
    expect(rendered.container.querySelector('[data-detect-on]')?.getAttribute('aria-describedby')).toBe(later.id)
  })

  it('swaps the picker for the fixed four on Cloud GPU, counts its downloads, and gives the own choice back', async () => {
    setCloudAllowed(true)
    setOcrRescue(false)
    const rendered = open('detection')
    expect(places(rendered)).toEqual(['local', 'cloud'])
    expect(rendered.queryByText(t('onboarding.detection.cloudLater'))).toBeNull()

    await fireEvent.click(rendered.getByRole('radio', { name: t('tools.option.onCloud') }))
    await waitFor(() => expect(session.analysisTargets).toEqual({ rtFull: 'cloud', samTs: 'cloud' }))
    expect(backend.writeSettings).toHaveBeenCalledWith({ analysisTargets: { rtFull: 'cloud', samTs: 'cloud' } })
    expect(rendered.queryByRole('checkbox', { name: 'Ogkalu comic text & bubble detector (Small)' })).toBeNull()
    const sam = /** @type {HTMLInputElement} */ (rendered.getByRole('checkbox', { name: 'SAM-TS-L lettering mask' }))
    expect(sam.checked && sam.disabled).toBe(true)
    const reader = /** @type {HTMLInputElement} */ (rendered.getByRole('checkbox', { name: t('pipelines.workflow.ocrRescue') }))
    expect(reader.checked && reader.disabled).toBe(true)
    expect(rendered.getByText(t('pipelines.cloudCombo'))).toBeTruthy()

    setFirstLaunchStep('dependencies')
    await waitFor(() => expect(rendered.getByText(t('onboarding.dependencies.total', {
      count: 8, bytes: RUNTIME_BYTES + 95 + 4 + 1 + HAYAI_BYTES + REDRAW_BYTES,
    }))).toBeTruthy())

    setFirstLaunchStep('detection')
    await waitFor(() => expect(rendered.getByRole('radio', { name: t('tools.option.onLocal') })).toBeTruthy())
    await fireEvent.click(rendered.getByRole('radio', { name: t('tools.option.onLocal') }))
    await waitFor(() => expect(session.analysisTargets).toEqual({ rtFull: 'local', samTs: 'local' }))
    expect(/** @type {HTMLInputElement} */ (rendered.getByRole('checkbox', { name: 'Ogkalu comic text & bubble detector (Small)' })).checked).toBe(true)
    expect(/** @type {HTMLInputElement} */ (rendered.getByRole('checkbox', { name: t('pipelines.workflow.ocrRescue') })).checked).toBe(false)
    expect(session.detectorModels).toEqual(['ctd', 'rtSmall'])
  })

  // The Cloud step comes after Detection, so a first-run user who sets up a
  // cloud GPU there is asked Detect on there too.
  it('asks Detect on in the Cloud step once its setup has turned cloud engines on', async () => {
    const rendered = open('cloud')
    expect(rendered.container.querySelector('[data-detect-on]')).toBeNull()
    await press(rendered, t('onboarding.cloud.setUp'))
    await fireEvent.click(rendered.getByTestId('provisioner-finish'))
    await waitFor(() => expect(session.cloudAllowed).toBe(true))
    await fireEvent.click(rendered.getByTestId('provisioner-close'))
    expect(places(rendered)).toEqual(['local', 'cloud'])
    await fireEvent.click(rendered.getByRole('radio', { name: t('tools.option.onCloud') }))
    await waitFor(() => expect(session.analysisTargets).toEqual({ rtFull: 'cloud', samTs: 'cloud' }))
    setFirstLaunchStep('dependencies')
    await waitFor(() => expect(rendered.getByText(t('onboarding.dependencies.total', {
      count: 8, bytes: RUNTIME_BYTES + 95 + 4 + 1 + HAYAI_BYTES + REDRAW_BYTES,
    }))).toBeTruthy())
  })

  it('puts a refused choice back and says so', async () => {
    setCloudAllowed(true)
    backend.writeSettings.mockRejectedValue(new Error('disk'))
    const rendered = open('detection')
    await fireEvent.click(rendered.getByRole('radio', { name: t('tools.option.onCloud') }))
    await waitFor(() => expect(rendered.getByRole('alert').textContent).toBe(t('tools.target.detectSaveFailed')))
    expect(session.analysisTargets).toEqual({ rtFull: 'local', samTs: 'local' })
    expect(/** @type {HTMLInputElement} */ (rendered.getByRole('radio', { name: t('tools.option.onLocal') })).checked).toBe(true)
  })
})

describe('the cloud step', () => {
  it('Not now leaves cloud cleaning off and moves on', async () => {
    const rendered = open('cloud')
    expect(rendered.getByText(t('onboarding.cloud.consent'))).toBeTruthy()
    await press(rendered, t('onboarding.action.notNow'))
    expect(firstLaunch.step).toBe('community')
    expect(session.cloudAllowed).toBe(false)
    expect(backend.writeSettings).not.toHaveBeenCalled()
  })

  it('Set up now opens the provisioner in place, and its success turns cloud cleaning on', async () => {
    const rendered = open('cloud')
    await press(rendered, t('onboarding.cloud.setUp'))
    const provisioner = rendered.getByTestId('provisioner')
    expect(provisioner.dataset.inline).toBe('true')
    expect(provisioner.dataset.provider).toBe('modal')
    await waitFor(() => expect(provisioner.parentElement?.contains(document.activeElement)).toBe(true))
    // The provisioner draws its own buttons until it has finished.
    expect(rendered.queryByRole('button', { name: t('onboarding.action.back') })).toBeNull()
    expect(rendered.queryByRole('button', { name: t('onboarding.action.next') })).toBeNull()

    await fireEvent.click(rendered.getByTestId('provisioner-finish'))
    await waitFor(() => expect(session.cloudAllowed).toBe(true))
    expect(backend.writeSettings).toHaveBeenLastCalledWith(expect.objectContaining({ cloudEngines: 'allowed' }))
    // What the step keeps is what it says back: never the endpoint or a credential.
    expect(firstLaunch.cloud).toEqual({ provider: 'beam', name: 'Beam (mc-ab12cd)', healthy: true })

    await fireEvent.click(rendered.getByTestId('provisioner-close'))
    expect(rendered.queryByTestId('provisioner')).toBeNull()
    expect(rendered.getByRole('status').textContent).toBe(t('onboarding.cloud.ready', { name: 'Beam (mc-ab12cd)' }))
    await press(rendered, t('onboarding.action.next'))
    expect(firstLaunch.step).toBe('community')
  })

  it('keeps an endpoint that did not answer its first check, leaves cloud cleaning off, and offers no second setup', async () => {
    const rendered = open('cloud')
    await press(rendered, t('onboarding.cloud.setUp'))
    await fireEvent.click(rendered.getByTestId('provisioner-finish-unchecked'))
    await waitFor(() => expect(firstLaunch.cloud).toEqual({ provider: 'beam', name: 'Beam (mc-ab12cd)', healthy: false }))
    expect(session.cloudAllowed).toBe(false)
    expect(backend.writeSettings).not.toHaveBeenCalled()
    expect(firstLaunch.cloudSaveFailed).toBe(false)

    await fireEvent.click(rendered.getByTestId('provisioner-close'))
    expect(rendered.getByRole('status').textContent).toBe(t('onboarding.cloud.unchecked', { name: 'Beam (mc-ab12cd)' }))
    // A second setup would be a second installation in the account.
    expect(rendered.queryByRole('button', { name: t('onboarding.cloud.setUp') })).toBeNull()
    await press(rendered, t('onboarding.action.next'))
    expect(firstLaunch.step).toBe('community')
  })

  it('Escape closes the provisioner first, and a refused permission is said rather than shown as on', async () => {
    const rendered = open('cloud')
    await press(rendered, t('onboarding.cloud.setUp'))
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(firstLaunch.provisioning).toBe(false)
    expect(firstLaunch.open).toBe(true)

    backend.writeSettings.mockRejectedValueOnce(new Error('disk full'))
    await press(rendered, t('onboarding.cloud.setUp'))
    await fireEvent.click(rendered.getByTestId('provisioner-finish'))
    await waitFor(() => expect(firstLaunch.cloudSaveFailed).toBe(true))
    expect(session.cloudAllowed).toBe(false)

    // Once it has finished, Continue is the dialog's as well as the provisioner's.
    await press(rendered, t('onboarding.action.next'))
    expect(firstLaunch.step).toBe('community')
    await press(rendered, t('onboarding.action.back'))
    expect(rendered.getByRole('alert').textContent).toBe(t('onboarding.cloud.saveFailed'))
  })

  it('sets up page denoise too when the Denoise step chose the cloud GPU', async () => {
    setDenoiseTarget('cloud')
    const rendered = open('cloud')
    expect(rendered.getByText(t('onboarding.cloud.withDenoise'))).toBeTruthy()
    await press(rendered, t('onboarding.cloud.setUp'))
    expect(rendered.getByTestId('provisioner').dataset.wantDenoise).toBe('true')
  })

  it('makes Update the way on when cloud is on but lacks the page denoise chosen before', async () => {
    setDenoiseTarget('cloud')
    setCloudAllowed(true)
    Object.assign(backend, { cloudDenoisePresets: vi.fn(async () => { throw new Error('capability_unavailable: gateway denoise is not configured') }) })
    const rendered = open('cloud')
    await waitFor(() => expect(rendered.getByRole('status').textContent).toBe(t('onboarding.cloud.addDenoise')))
    await fireEvent.keyDown(heading(rendered), { key: 'Enter' })
    expect(rendered.getByTestId('provisioner').dataset.wantDenoise).toBe('true')
  })

  it('offers no way out while the provisioner is working in the account', async () => {
    const rendered = open('cloud')
    await press(rendered, t('onboarding.cloud.setUp'))
    await fireEvent.click(rendered.getByTestId('provisioner-busy'))
    const skip = /** @type {HTMLButtonElement} */ (rendered.getByRole('button', { name: t('onboarding.action.skip') }))
    expect(skip.disabled).toBe(true)
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(firstLaunch.provisioning).toBe(true)
    expect(firstLaunch.open).toBe(true)

    // Once it has stopped, Escape closes the provisioner as before.
    await fireEvent.click(rendered.getByTestId('provisioner-idle'))
    expect(skip.disabled).toBe(false)
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(firstLaunch.provisioning).toBe(false)
    expect(firstLaunch.open).toBe(true)

    // A provisioner that closes itself mid-run takes its busy state with it.
    await press(rendered, t('onboarding.cloud.setUp'))
    await fireEvent.click(rendered.getByTestId('provisioner-busy'))
    await fireEvent.click(rendered.getByTestId('provisioner-close'))
    expect(firstLaunch.provisionerBusy).toBe(false)
    expect(skip.disabled).toBe(false)
  })

  it('leaves cloud cleaning off when the setup names no saved endpoint', async () => {
    open('cloud')
    await configureFirstLaunchCloud({ provider: 'modal', profileId: '  ', name: 'Modal (mc-ab12cd)' })
    expect(firstLaunch.cloud).toBeNull()
    expect(session.cloudAllowed).toBe(false)
    expect(backend.writeSettings).not.toHaveBeenCalled()
  })
})

describe('Run setup again', () => {
  it('opens the setup from Settings > General over a fresh catalogue, with Settings closed', async () => {
    session.firstLaunchOffered = true
    pushModal({ kind: 'settings' })
    const rendered = render(SettingsDialog, { props: { spec: SPEC } })
    backend.listModels.mockClear()

    await press(rendered, t('onboarding.replay.action'))
    await waitFor(() => expect(firstLaunch.open).toBe(true))
    expect(backend.listModels).toHaveBeenCalledTimes(1)
    expect(firstLaunch.step).toBe('welcome')
    expect(firstLaunch.plan?.files[RUNTIME_ID]).toBeTruthy()
    // The setup is drawn only while the modal stack is empty.
    expect(app.modals).toHaveLength(0)
  })

  it('says so, and leaves Settings open, when the catalogue cannot be read', async () => {
    backend.listModels.mockRejectedValue(new Error('offline'))
    pushModal({ kind: 'settings' })
    const rendered = render(SettingsDialog, { props: { spec: SPEC } })

    await press(rendered, t('onboarding.replay.action'))
    await waitFor(() => expect(rendered.getByText(t('onboarding.replay.failed'))).toBeTruthy())
    expect(firstLaunch.open).toBe(false)
    expect(app.modals).toHaveLength(1)
  })
})
