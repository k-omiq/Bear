/**
 * The file rows of the Settings screen, mounted: the three things a row says
 * that nothing on the row used to say at all.
 *
 * - **What a stopped download left**, and the press that gives it back.
 *   The bytes are kept so the next Download
 *   resumes from them, which is exactly why they need reporting: 180 MB of a
 *   207 MB transfer nobody came back for is disk the user did not agree to
 *   spend, under a row reading "Not installed".
 * - **Which runtime build is actually installed**, said only when it is
 *   not the one the row names - two true statements that read as one false one
 *   when only the chosen build is on screen.
 * - **Why the credential store would not answer**, which the note under
 *   the token field could not say while the reason was prose in the platform's
 *   own words.
 *
 * And the one thing the dialog asks *for*: the credential-store retry, on the
 * open and on nothing else.
 *
 * A hand-written seam stub rather than the mock, for the reason
 * `SettingsDialog.dom.test.js` beside this file gives: the dialog opens six
 * calls on mount and only the catalogue is what this file is about, so the rest
 * answer the emptiest true thing.
 *
 * The rows live in two sections: the model weights and the token under
 * **Models**, the runtime under **Performance**. `open()` presses the section
 * a test names and checks its panel is shown, and the assertions are scoped
 * to that panel: the panels are mounted and merely `hidden`, so a query over
 * the whole screen would find a row in a hidden panel too.
 *
 * The end of the file is the two pipelines themselves, as Models' groups:
 * which group a file is managed from, what the per-language pickers store
 * behind the collapsed language filtering, and the collapsed FLUX helper.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/svelte'

import { getBackend, setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { DEFAULT_DETECTOR } from '../model/pipelines.js'
import { capabilities } from '../state/capabilities.svelte.js'
import {
  session,
  setDetection,
  setFluxModel,
  setOcrRescue,
  setSidecarPath,
  setTextPolicy,
} from '../state/session.svelte.js'
import SettingsDialog from './SettingsDialog.svelte'
import { resetFirstLaunch } from './firstlaunch.svelte.js'
import { settingsLinkForModel } from './settingslinks.js'

/** The spec `pushModal({kind: 'settings'})` would have handed the dialog. */
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

/** One weight and the runtime, with whatever this test is about set on them. */
function view({ model = {}, runtime = {}, token = {} } = {}) {
  return {
    models: [
      {
        id: 'inpainter',
        fileName: 'lama-manga.onnx',
        bytes: 207_482_644,
        sha256: '4512adab295ee5a5e02ccd1bdf8d45dccbac88309d9cff1532ffd5de876f02a4',
        kindKey: 'models.kind.inpainter',
        requiredBy: ['lama'],
        installed: false,
        path: null,
        readOnly: false,
        sha256Ok: null,
        downloading: false,
        partialBytes: null,
        ...model,
      },
    ],
    runtime: {
      installed: true,
      path: '/app-data/runtimes/onnxruntime.dll',
      readOnly: false,
      downloading: false,
      version: '1.28.0',
      flavour: 'cuda12',
      bytes: 455_344_532,
      flavours: [],
      available: true,
      installedFlavour: null,
      installedVersion: null,
      partialBytes: null,
      ...runtime,
    },
    modelsDir: '/app-data/models',
    runtimeDir: '/app-data/runtimes',
    hasToken: false,
    tokenStore: 'keychain',
    tokenStoreReason: null,
    ...token,
  }
}

/** @type {ReturnType<typeof vi.fn>} */
let listModels
/** @type {ReturnType<typeof vi.fn>} */
let discardPartial
/** @type {ReturnType<typeof vi.fn>} */
let writeSettings
let downloadModelGroup
let verifyModelGroup
let deleteModelGroup
let downloadModel
let verifyModel
let deleteModel
let verifySamTs

/** The speech bubble finder and the three OCR files, as the catalogue sizes them. */
const BALLOON_BYTES = 11_380_294
const HAYAI_BYTES = 343_538_300 + 255_717_567 + 1_247_253

/**
 * What `listWorkflowCapabilities` answers with neither import present: the
 * shape the review panel reads, with the files it would name once imported.
 */
const CAPS = Object.freeze({
  runtimeInstalled: true,
  rtInstalled: true,
  fullRtInstalled: false,
  fullRtManaged: false,
  fullRtRevision: null,
  fullRtFile: { name: 'detector.onnx', bytes: 168_000_000, sha256: 'ab'.repeat(32) },
  samInstalled: false,
  samMemoryReady: true,
  samManaged: false,
  samRevision: null,
  samFiles: [
    { name: 'sam_encoder.onnx', bytes: 1_200_000_000, sha256: 'cd'.repeat(32) },
    { name: 'sam_decoder.onnx', bytes: 16_000_000, sha256: 'ef'.repeat(32) },
  ],
  cooStatus: 'excluded',
  rtBackends: [],
  samBackends: [],
  samWriteQualified: false,
  samWriteNote: null,
})
/** @type {any} */
let workflowCapabilities = CAPS
/** What the FLUX helper lists, for the tests that turn it on. */
let helperModels = []

function stub(answer) {
  listModels = vi.fn(async () => answer())
  discardPartial = vi.fn(async () => true)
  writeSettings = vi.fn(async () => ({}))
  downloadModelGroup = vi.fn(async () => 'started')
  verifyModelGroup = vi.fn(async () => true)
  deleteModelGroup = vi.fn(async () => 'deleted')
  downloadModel = vi.fn(async () => 'started')
  verifyModel = vi.fn(async () => true)
  deleteModel = vi.fn(async () => 'deleted')
  verifySamTs = vi.fn(async () => true)
  setBackend(
    /** @type {any} */ ({
      listModels: (/** @type {any} */ options) => listModels(options),
      discardPartial: (/** @type {any} */ spec) => discardPartial(spec),
      listAccelerators: vi.fn(async () => ({ providers: [], models: [] })),
      listSidecarModels: vi.fn(async () => helperModels),
      sidecarAvailable: vi.fn(async () => ({ available: capabilities.sidecar, reasonKey: null })),
      about: vi.fn(async () => ({ appVersion: '0.0.0-test', facts: [] })),
      subscribe: vi.fn(() => () => {}),
      writeSettings: (/** @type {any} */ patch) => writeSettings(patch),
      downloadModelGroup: (/** @type {any} */ spec) => downloadModelGroup(spec),
      verifyModelGroup: (/** @type {any} */ spec) => verifyModelGroup(spec),
      deleteModelGroup: (/** @type {any} */ spec) => deleteModelGroup(spec),
      downloadModel: (/** @type {any} */ spec) => downloadModel(spec),
      verifyModel: (/** @type {any} */ spec) => verifyModel(spec),
      deleteModel: (/** @type {any} */ spec) => deleteModel(spec),
      listWorkflowCapabilities: vi.fn(async () => workflowCapabilities),
      verifySamTs: () => verifySamTs(),
      importSamTs: vi.fn(async () => ({})),
      importFullRt: vi.fn(async () => ({})),
      removeSamTs: vi.fn(async () => true),
      removeFullRt: vi.fn(async () => true),
    }),
  )
}

afterEach(() => {
  cleanup()
  resetFirstLaunch()
  setBackend(null)
  vi.clearAllMocks()
})

/**
 * Mount, open a section, and wait for the first catalogue to be drawn. The
 * queries returned are scoped to that section's panel.
 *
 * @param {() => any} answer
 * @param {string} sectionKey - the tab's label key
 */
async function open(answer, sectionKey = 'settings.section.models') {
  stub(answer)
  const rendered = render(SettingsDialog, { props: { spec: SPEC } })

  const tab = rendered.getByRole('tab', { name: t(sectionKey) })
  await fireEvent.click(tab)
  expect(tab.getAttribute('aria-selected')).toBe('true')
  const panel = /** @type {HTMLElement} */ (
    rendered.container.querySelector(`#${tab.getAttribute('aria-controls')}`)
  )
  expect(panel.hasAttribute('hidden')).toBe(false)

  await waitFor(() => expect(listModels).toHaveBeenCalled())
  return { ...within(panel), container: panel }
}

const PERFORMANCE = 'settings.section.performance'
const MODELS = 'settings.section.models'

describe('the bytes a stopped download left', () => {
  it('are reported under the row, with a press that gives them back', async () => {
    const rendered = await open(() => view({ model: { partialBytes: 104_857_600 } }))

    const line = t('settings.models.status.partial', { bytes: 104_857_600 })
    await waitFor(() => expect(rendered.getByText(line)).toBeTruthy())

    const discard = /** @type {HTMLButtonElement} */ (
      rendered.getByText(t('settings.models.action.discard')).closest('button')
    )
    await fireEvent.click(discard)
    expect(discardPartial).toHaveBeenCalledWith({ id: 'inpainter' })
    // And the press asks again, because the answer changes the row it was
    // pressed on - including when the answer is `false` for a download that
    // started underneath it.
    await waitFor(() => expect(listModels).toHaveBeenCalledTimes(2))
  })

  it('are not offered for a row with no unfinished download', async () => {
    const rendered = await open(() => view())
    // Models holds every pipeline's rows now, so the redraw weight's row is
    // the one asked about.
    const lama = () => within(/** @type {HTMLElement} */ (rendered.container.querySelector('[data-model="lama"]')))
    await waitFor(() => expect(lama().getByText(t('settings.models.action.download'))).toBeTruthy())
    expect(rendered.queryByText(t('settings.models.action.discard'))).toBe(null)
  })
})

describe('the runtime build that is actually installed', () => {
  it('is named when it is not the one the row would download', async () => {
    const rendered = await open(
      () => view({ runtime: { installedFlavour: 'directml', installedVersion: '1.24.4' } }),
      PERFORMANCE,
    )

    const line = t('settings.models.runtime.installedDiffers', {
      installed: 'directml',
      installedVersion: '1.24.4',
      chosen: 'cuda12',
      chosenVersion: '1.28.0',
    })
    await waitFor(() => expect(rendered.getByText(line)).toBeTruthy())
  })

  it('is said nothing about when it agrees, or when there is no record of it', async () => {
    const agreeing = await open(
      () => view({ runtime: { installedFlavour: 'cuda12', installedVersion: '1.28.0' } }),
      PERFORMANCE,
    )
    await waitFor(() => expect(agreeing.getByText(t('settings.models.runtime.label'))).toBeTruthy())
    expect(agreeing.container.querySelectorAll('.row-partial')).toHaveLength(0)
    cleanup()

    // `null` is unknown rather than none - a runtime this application did not
    // unpack - and an unknown build is nothing to report a difference about.
    const unknown = await open(() => view(), PERFORMANCE)
    await waitFor(() => expect(unknown.getByText(t('settings.models.runtime.label'))).toBeTruthy())
    expect(unknown.container.querySelectorAll('.row-partial')).toHaveLength(0)
  })
})

describe('a credential store that would not answer', () => {
  it('says which of the four things it did', async () => {
    const rendered = await open(
      () => view({ token: { tokenStore: 'fileStoreUnavailable', tokenStoreReason: 'locked' } }),
      MODELS,
    )

    await waitFor(() =>
      expect(rendered.getByText(t('settings.models.token.storeUnreachable'))).toBeTruthy(),
    )
    // The reason is the second line: what is wrong with the place the token
    // should be, rather than where it went instead.
    expect(rendered.getByText(t('settings.models.token.reason.locked'))).toBeTruthy()
  })

  it('says nothing extra when the store took the token', async () => {
    const rendered = await open(() => view({ token: { hasToken: true } }), MODELS)
    await waitFor(() => expect(rendered.getByText(t('settings.models.token.saved'))).toBeTruthy())
    for (const reason of ['locked', 'unreachable', 'ambiguous', 'unknown']) {
      expect(rendered.queryByText(t(`settings.models.token.reason.${reason}`))).toBe(null)
    }
  })
})

describe('the once-per-process credential-store retry', () => {
  it('is asked for on the open and on no other call', async () => {
    const rendered = await open(() => view({ model: { partialBytes: 4_096 } }))
    expect(listModels).toHaveBeenCalledWith({ retryStore: true })

    // Any press that refreshes the list is a refresh, not an open: asking again
    // there is the prompt-per-poll removed.
    await waitFor(() => expect(rendered.getByText(t('settings.models.action.discard'))).toBeTruthy())
    const discard = /** @type {HTMLButtonElement} */ (
      rendered.getByText(t('settings.models.action.discard')).closest('button')
    )
    await fireEvent.click(discard)
    await waitFor(() => expect(listModels).toHaveBeenCalledTimes(2))
    expect(listModels.mock.calls[1][0]).toEqual({})
  })
})

describe('the two pipelines', () => {
  /** The catalogue with one detection weight beside the redraw weight. */
  function both() {
    const answer = view()
    answer.models.push({
      ...answer.models[0],
      id: 'textDetector',
      fileName: 'comictextdetector.onnx',
      bytes: 94_669_756,
      sha256: '1a86ace74961413cbd650002e7bb4dcec4980ffa21b2f19b86933372071d718f',
      kindKey: 'models.kind.textDetector',
      requiredBy: ['autoClean'],
    })
    return answer
  }

  function withModelGroups({ installed = false } = {}) {
    const answer = both()
    for (const model of [
      { id: 'balloonDetector', fileName: 'detector.onnx', bytes: BALLOON_BYTES, sha256: 'c5a1b2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1', kindKey: 'models.kind.balloonDetector' },
      { id: 'fullRt', fileName: 'detector.onnx', bytes: 168_481_531, sha256: CAPS.fullRtFile.sha256, kindKey: 'models.kind.fullRt' },
      { id: 'scriptGate', fileName: 'osd_lstm.onnx', bytes: 3_722_314, sha256: 'b18e0c1479d9eb67394993098f7e1079c9a93ef6f7b0416ee333fccb865c6e72', kindKey: 'models.kind.scriptGate' },
      { id: 'scriptGateLabels', fileName: 'osd_labels.json', bytes: 1_163, sha256: 'a1888156b005065039c356e13a7bbef1ec454b45bf6aaf18c11f4a59b1ee35c5', kindKey: 'models.kind.scriptGateLabels' },
      { id: 'ocrEncoder', fileName: 'encoder_model.onnx', bytes: 343_454_249, sha256: '15fa8155fe9bc1a7d25d9bb353debaa4def033d0174e907dbd2dd6d995def85f', kindKey: 'models.kind.ocr' },
      { id: 'ocrDecoder', fileName: 'decoder_model.onnx', bytes: 117_480_262, sha256: 'ef7765261e9d1cdc34d89356986c2bbc2a082897f753a89605ae80fdfa61f5e8', kindKey: 'models.kind.ocrDecoder' },
      { id: 'ocrVocab', fileName: 'vocab.txt', bytes: 30_216, sha256: '5cb5c5586d98a2f331d9f8828e4586479b0611bfba5d8c3b6dadffc84d6a36a3', kindKey: 'models.kind.ocrVocab' },
      { id: 'hayaiVision', fileName: 'hayai-ocr-vision.onnx', bytes: 343_538_300, sha256: '379ec20e7d5b134e6bd0e7c5cf0a4e705318129bfb710e25e69c6ca027b84021', kindKey: 'models.kind.hayaiVision' },
      { id: 'hayaiDecoder', fileName: 'hayai-ocr-decoder.onnx', bytes: 255_717_567, sha256: '23342ad16efee65486347b7ac15c98d5f78412eb4c4bee3236d03d8478cb0e20', kindKey: 'models.kind.hayaiDecoder' },
      { id: 'hayaiTokenizer', fileName: 'hayai-ocr-tokenizer.json', bytes: 1_247_253, sha256: 'f8a0a909c628a684fe463094614e236a8b1d3609e7770f77e7beafaf1056bf13', kindKey: 'models.kind.hayaiTokenizer' },
    ]) {
      answer.models.push({ ...answer.models[0], ...model, installed, path: installed ? `/models/${model.fileName}` : null,
        readOnly: false, sha256Ok: installed ? true : null, requiredBy: [], downloading: false, partialBytes: null })
    }
    return answer
  }

  /** @param {any} rendered @param {string} language */
  function languagePicker(rendered, language) {
    return /** @type {HTMLSelectElement} */ (
      rendered.getByRole('combobox', { name: t('pipelines.detectorFor', { language: t(`pipelines.language.${language}`) }) })
    )
  }

  /** @param {any} rendered */
  function rescueBox(rendered) {
    return /** @type {HTMLInputElement} */ (rendered.getByRole('checkbox', { name: t('pipelines.workflow.ocrRescue') }))
  }

  /** @param {any} rendered @param {string} id */
  function modelRow(rendered, id) {
    return /** @type {HTMLElement} */ (rendered.container.querySelector(`[data-model="${id}"]`))
  }

  /**
   * Open one of the collapsed groups by its summary: language filtering or
   * the FLUX helper.
   *
   * @param {any} rendered @param {string} titleKey
   */
  async function expand(rendered, titleKey) {
    const summary = rendered.getByRole('button', { name: new RegExp(t(titleKey).replace(/[()]/g, '\\$&')) })
    if (summary.getAttribute('aria-expanded') !== 'true') await fireEvent.click(summary)
  }
  /** @param {any} rendered */
  const filtering = (rendered) => expand(rendered, 'settings.detection.capability.japanese')
  /** @param {any} rendered */
  const flux = (rendered) => expand(rendered, 'settings.sidecar.heading')
  /** @param {any} rendered @param {string} anchor */
  const group = (rendered, anchor) =>
    /** @type {HTMLElement} */ (rendered.container.querySelector(`[data-settings-anchor="${anchor}"]`))

  afterEach(() => {
    for (const language of ['ja', 'zh', 'ko']) setDetection(language, DEFAULT_DETECTOR)
    setOcrRescue(false)
    setTextPolicy('legacy_gate')
    workflowCapabilities = CAPS
    capabilities.sidecar = false
    helperModels = []
    setFluxModel('')
    setSidecarPath('')
  })

  it('manage each model in the group whose pipeline needs it', async () => {
    const models = await open(() => withModelGroups())
    await waitFor(() => expect(group(models, 'detection').querySelector('[data-model="ctd"]')).toBeTruthy())
    expect(group(models, 'detection').querySelector('[data-model="lama"]')).toBe(null)
    await waitFor(() => expect(group(models, 'cleaning').querySelector('[data-model="lama"]')).toBeTruthy())
    expect(group(models, 'cleaning').querySelector('[data-model="ctd"]')).toBe(null)
    // The Hayai reader's switch sits in Detection itself, named, not behind
    // the collapsed filtering; the script gate and the files wait in there.
    expect(group(models, 'filtering').contains(rescueBox(models))).toBe(false)
    expect(rescueBox(models).labels?.[0]?.textContent).toContain('Hayai OCR')
    expect(modelRow(models, 'scriptGate')).toBe(null)
    await filtering(models)
    expect(group(models, 'filtering').querySelector('[data-model="scriptGate"]')).toBeTruthy()
    expect(group(models, 'detection').contains(group(models, 'filtering'))).toBe(true)
  })

  it('store Skip as null, and a detector by its id', async () => {
    const detection = await open(both)
    await filtering(detection)
    const japanese = languagePicker(detection, 'ja')
    expect(japanese.value).toBe(DEFAULT_DETECTOR)

    await fireEvent.change(japanese, { target: { value: '' } })
    expect(session.detection.ja).toBe(null)
    expect(japanese.value).toBe('')

    await fireEvent.change(japanese, { target: { value: DEFAULT_DETECTOR } })
    expect(session.detection.ja).toBe(DEFAULT_DETECTOR)
  })

  it('offer every language the one detector and Skip, with no OCR variant', async () => {
    const detection = await open(both)
    await filtering(detection)
    for (const language of ['ja', 'zh', 'ko']) {
      expect([...languagePicker(detection, language).options].map((option) => option.value)).toEqual(['ctd-rtdetr', ''])
    }
  })

  it('read a retired OCR detector choice as the plain detector with the rescue ticked', async () => {
    const detection = await open(() => withModelGroups())
    await filtering(detection)
    setDetection('ja', 'ctd-rtdetr-ocr')
    expect(session.detection.ja).toBe('ctd-rtdetr')
    expect(session.ocrRescue).toBe(true)
    await waitFor(() => expect(rescueBox(detection).checked).toBe(true))
    expect(languagePicker(detection, 'ja').value).toBe('ctd-rtdetr')
  })

  it('show the selected choices and each model’s download state, once, with no summary line repeating them', async () => {
    const detection = await open(both)
    await waitFor(() => expect(modelRow(detection, 'ctd').textContent).toContain(t('settings.models.status.missing')))
    expect(/** @type {HTMLInputElement} */ (detection.getByRole('checkbox', { name: 'Comic Text Detector (CTD)' })).checked).toBe(true)
    expect(/** @type {HTMLInputElement} */ (detection.getByRole('checkbox', { name: 'Ogkalu comic text & bubble detector (Small)' })).checked).toBe(true)
    expect(detection.container.textContent).not.toContain('RT-DETR')
    // No engine roadmap: a choice that cannot run is not drawn as an engine.
    expect(detection.queryByRole('table')).toBe(null)
    expect(detection.container.textContent).not.toContain(t('pipelines.status.soon'))
  })

  it('draw the groups in order, with no row for an excluded model', async () => {
    const models = await open(() => withModelGroups())
    const headings = [...models.container.querySelectorAll('h3')].map((heading) => heading.textContent?.trim())
    expect(headings).toEqual([
      t('pipelines.detection'),
      t('pipelines.cleaning'),
      t('settings.models.access.heading'),
    ])
    expect(modelRow(models, 'coo')).toBe(null)
    expect(models.container.textContent).not.toContain('SFX')
  })

  it('make the text reader opt-in, and say when its files are missing with the download beside it', async () => {
    const detection = await open(() => withModelGroups())
    await filtering(detection)
    const box = rescueBox(detection)
    expect(box.checked).toBe(false)
    const status = /** @type {HTMLElement} */ (detection.container.querySelector('.option [role="status"]'))
    expect(box.getAttribute('aria-describedby')?.split(' ')).toContain(status.id)
    expect(status.textContent).toBe('')

    await fireEvent.click(box)
    expect(session.ocrRescue).toBe(true)
    await waitFor(() => expect(status.textContent).toBe(t('settings.detection.rescue.missing')))
    const option = within(/** @type {HTMLElement} */ (detection.container.querySelector('.option')))
    await fireEvent.click(option.getByRole('button', { name: t('settings.detection.download', { bytes: HAYAI_BYTES }) }))
    expect(downloadModelGroup).toHaveBeenCalledWith({ id: 'hayaiOcr' })

    // The reader reads Chinese and Korean too, so skipping Japanese changes nothing.
    await fireEvent.change(languagePicker(detection, 'ja'), { target: { value: '' } })
    expect(status.textContent).toBe(t('settings.detection.rescue.missing'))
  })

  it('download what the selected workflow needs: the readiness line never asks for OCR, all-text never for the gate', async () => {
    setOcrRescue(true)
    const detection = await open(() => withModelGroups())
    const readiness = () => /** @type {HTMLElement} */ (detection.container.querySelector('.readiness'))
    const legacyBytes = 94_669_756 + BALLOON_BYTES + 3_722_314 + 1_163
    await waitFor(() => expect(readiness().textContent).toContain(t('settings.detection.ready.legacyMissing', { bytes: legacyBytes })))
    await fireEvent.click(within(readiness()).getByRole('button', { name: t('settings.detection.download', { bytes: legacyBytes }) }))
    await waitFor(() => expect(downloadModelGroup).toHaveBeenCalledWith({ id: 'scriptGate' }))
    expect(downloadModel.mock.calls.map(([spec]) => spec.id)).toEqual(['textDetector', 'balloonDetector'])
    expect(downloadModelGroup).not.toHaveBeenCalledWith({ id: 'hayaiOcr' })
    downloadModel.mockClear()
    downloadModelGroup.mockClear()

    // The policy is the Text cleanup panel's; Settings follows it.
    setTextPolicy('all_text')
    await waitFor(() => expect(readiness().textContent).toContain(t('settings.detection.ready.allTextMissing', { bytes: BALLOON_BYTES + 94_669_756 })))
    await fireEvent.click(within(readiness()).getByRole('button', { name: t('settings.detection.download', { bytes: BALLOON_BYTES + 94_669_756 }) }))
    await waitFor(() => expect(downloadModel).toHaveBeenCalledWith({ id: 'balloonDetector' }))
    expect(downloadModel).toHaveBeenCalledWith({ id: 'textDetector' })
    expect(downloadModel).toHaveBeenCalledTimes(2)
    expect(downloadModelGroup).not.toHaveBeenCalled()

    // All-text reads no language, so no picker is drawn; the reader runs under both policies.
    await filtering(detection)
    expect(detection.queryByRole('combobox', { name: t('pipelines.detectorFor', { language: t('pipelines.language.ja') }) })).toBe(null)
    expect(rescueBox(detection).checked).toBe(true)
    expect(detection.getByText(t('settings.detection.languagesAllText'))).toBeTruthy()
  })

  it('offers the SAM-TS-L lettering mask import and the Full detector download', async () => {
    const detection = await open(() => withModelGroups())
    await waitFor(() => expect(modelRow(detection, 'samTs').textContent).toContain(t('settings.models.status.importToEnable')))
    expect(within(modelRow(detection, 'samTs')).getByRole('button', { name: t('settings.models.action.import') })).toBeTruthy()
    await waitFor(() => expect(within(modelRow(detection, 'rtFull')).getByRole('button', { name: t('settings.models.action.download') })).toBeTruthy())
    cleanup()

    workflowCapabilities = { ...CAPS, samInstalled: true, samManaged: true, fullRtInstalled: true, fullRtManaged: true, fullRtRevision: 'abc123' }
    const imported = await open(() => withModelGroups())
    await waitFor(() => expect(modelRow(imported, 'samTs').textContent).toContain(t('settings.models.status.imported')))
    const sam = within(modelRow(imported, 'samTs'))
    await fireEvent.click(sam.getByRole('button', { name: t('settings.models.action.verify') }))
    expect(verifySamTs).toHaveBeenCalled()
    await fireEvent.click(within(modelRow(imported, 'rtFull')).getByRole('button', { name: t('settings.models.details') }))
    expect(modelRow(imported, 'rtFull').textContent).toContain(CAPS.fullRtFile.sha256)
  })

  it('manage the script gate as one row, with per-file Check and Delete under File details', async () => {
    const detection = await open(() => withModelGroups({ installed: true }))
    await filtering(detection)
    const element = modelRow(detection, 'scriptGate')
    const gate = within(element)
    await waitFor(() => expect(element.textContent).toContain(t('settings.models.fileCount', { count: 2 })))
    expect(gate.getByText('ogkalu Image Script Identification')).toBeTruthy()
    await fireEvent.click(gate.getAllByRole('button', { name: t('settings.models.action.verify') })[0])
    expect(verifyModelGroup).toHaveBeenCalledWith({ id: 'scriptGate' })

    await fireEvent.click(gate.getByRole('button', { name: t('settings.models.details') }))
    expect(gate.getByText('osd_lstm.onnx')).toBeTruthy()
    expect(element.querySelector('code')?.textContent).toBe('b18e0c1479d9eb67394993098f7e1079c9a93ef6f7b0416ee333fccb865c6e72')
    const labels = /** @type {HTMLElement} */ (gate.getByText('osd_labels.json').closest('li'))
    await fireEvent.click(within(labels).getByRole('button', { name: t('settings.models.action.verify') }))
    expect(verifyModel).toHaveBeenCalledWith({ id: 'scriptGateLabels' })

    // A file Delete removes what the backend removes, the whole group, and
    // asks first in those words.
    await fireEvent.click(within(labels).getByRole('button', { name: t('settings.models.action.delete') }))
    const confirm = /** @type {HTMLElement} */ (element.querySelector('.confirm'))
    expect(confirm.textContent).toContain(t('settings.models.remove.scriptGate'))
    await fireEvent.click(within(confirm).getByRole('button', { name: t('settings.models.action.delete') }))
    await waitFor(() => expect(deleteModelGroup).toHaveBeenCalledWith({ id: 'scriptGate' }))
    expect(deleteModel).not.toHaveBeenCalled()
  })

  it('name what a removal stops before it happens; Keep and Escape change nothing', async () => {
    const detection = await open(() => withModelGroups({ installed: true }))
    const element = modelRow(detection, 'rtSmall')
    const row = within(element)
    const deleteButton = await waitFor(() => row.getByRole('button', { name: t('settings.models.action.delete') }))

    await fireEvent.click(deleteButton)
    const confirm = () => /** @type {HTMLElement|null} */ (element.querySelector('.confirm'))
    expect(confirm()?.textContent).toContain(t('settings.models.remove.rtSmall'))
    expect(confirm()?.textContent).toContain(t('settings.models.remove.inUse'))
    const keep = within(/** @type {HTMLElement} */ (confirm())).getByRole('button', { name: t('settings.models.remove.keep') })
    await waitFor(() => expect(document.activeElement).toBe(keep))
    await fireEvent.click(keep)
    expect(confirm()).toBe(null)
    await waitFor(() => expect(document.activeElement?.id).toBe(deleteButton.id))

    await fireEvent.click(deleteButton)
    const escape = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })
    within(/** @type {HTMLElement} */ (confirm())).getByRole('button', { name: t('settings.models.remove.keep') }).dispatchEvent(escape)
    // Escape answered the question and nothing else: Settings stays open.
    expect(escape.defaultPrevented).toBe(true)
    await waitFor(() => expect(confirm()).toBe(null))
    expect(deleteModel).not.toHaveBeenCalled()

    await fireEvent.click(deleteButton)
    await fireEvent.click(within(/** @type {HTMLElement} */ (confirm())).getByRole('button', { name: t('settings.models.action.delete') }))
    await waitFor(() => expect(deleteModel).toHaveBeenCalledWith({ id: 'balloonDetector' }))
    // Focus carries on from the row's first action, not from the page top.
    await waitFor(() => expect(element.contains(document.activeElement)).toBe(true))
  })

  it('delete the OCR rescue as a unit and never take the shared speech bubble finder with it', async () => {
    const detection = await open(() => withModelGroups({ installed: true }))
    await filtering(detection)
    const element = modelRow(detection, 'mangaOcr')
    const ocr = within(element)
    await fireEvent.click(await waitFor(() => ocr.getByRole('button', { name: t('settings.models.action.verify') })))
    expect(verifyModelGroup).toHaveBeenCalledWith({ id: 'mangaOcr' })
    await fireEvent.click(ocr.getByRole('button', { name: t('settings.models.action.delete') }))
    const confirm = /** @type {HTMLElement} */ (element.querySelector('.confirm'))
    expect(confirm.textContent).toContain(t('settings.models.remove.mangaOcr'))
    await fireEvent.click(within(confirm).getByRole('button', { name: t('settings.models.action.delete') }))
    await waitFor(() => expect(deleteModelGroup).toHaveBeenCalledWith({ id: 'mangaOcr' }))
    expect(deleteModel).not.toHaveBeenCalled()
    expect(deleteModelGroup).not.toHaveBeenCalledWith({ id: 'scriptGate' })
  })

  it('keep the text-shaped review collapsed and optional under legacy, inside Detection', async () => {
    const detection = await open(both)
    const review = /** @type {HTMLElement} */ (detection.container.querySelector('.review'))
    expect(group(detection, 'detection').contains(review)).toBe(true)
    expect(review.textContent).toContain(t('settings.detection.review.optional'))
    expect(detection.queryByText('Independent model analysis')).toBe(null)
    const summary = within(review).getByRole('button', { name: new RegExp(t('settings.detection.review.summary')) })
    expect(summary.getAttribute('aria-expanded')).toBe('false')
    await fireEvent.click(summary)
    await waitFor(() => expect(detection.getByText('Independent model analysis')).toBeTruthy())
    cleanup()

    setTextPolicy('all_text')
    const allText = await open(both)
    await waitFor(() => expect(allText.getByText('Independent model analysis')).toBeTruthy())
  })

  it.each(['', '/opt/flux'])('reports the available FLUX helper with folder %j and writes the model it offers', async (path) => {
    capabilities.sidecar = true
    helperModels = [{ id: 'flux2-klein-4b', label: 'FLUX.2 Klein 4B' }]
    setSidecarPath(path)
    const cleaning = await open(both)
    const summary = cleaning.getByRole('button', { name: new RegExp(t('settings.sidecar.heading').replace(/[()]/g, '\\$&')) })
    expect(summary.getAttribute('aria-expanded')).toBe('false')
    expect(summary.textContent).toContain(t('settings.sidecar.ready'))
    expect(cleaning.queryByLabelText(t('settings.sidecar.label'))).toBe(null)
    // The fallback model the helper chose is written, not only remembered.
    await waitFor(() => expect(writeSettings).toHaveBeenCalledWith(expect.objectContaining({ fluxModel: 'flux2-klein-4b', sidecarPath: path })))
    await flux(cleaning)
    const folder = cleaning.getByLabelText(t('settings.sidecar.label'))
    expect(folder.value).toBe(path)
    expect(folder.placeholder).toBe(t('settings.sidecar.automaticFolder'))
    expect(session.sidecarPath).toBe(path)
    cleanup()

    capabilities.sidecar = false
    setSidecarPath('')
    const bare = await open(both)
    expect(bare.getByRole('button', { name: new RegExp(t('settings.sidecar.heading').replace(/[()]/g, '\\$&')) }).textContent)
      .toContain(t('settings.sidecar.notSetUp'))
  })

  it('does not report FLUX as set up just because a folder was entered', async () => {
    setSidecarPath('/opt/missing-flux')
    const cleaning = await open(both)
    expect(group(cleaning, 'flux').textContent).toContain(t('settings.sidecar.notSetUp'))
    await flux(cleaning)
    expect(cleaning.getByText(t('settings.sidecar.notFound'))).toBeTruthy()
    expect(cleaning.queryByLabelText(t('settings.sidecarModel.label'))).toBe(null)
  })

  it('keeps the detected FLUX helper set up when its explicit folder is cleared', async () => {
    capabilities.sidecar = true
    helperModels = [{ id: 'flux2-klein-4b', label: 'FLUX.2 Klein 4B' }]
    setFluxModel('flux2-klein-4b')
    setSidecarPath('/opt/flux')
    const cleaning = await open(both)
    await flux(cleaning)
    const field = cleaning.getByLabelText(t('settings.sidecar.label'))

    await fireEvent.input(field, { target: { value: '' } })
    await fireEvent.blur(field)
    await waitFor(() => expect(writeSettings).toHaveBeenCalledWith(expect.objectContaining({ sidecarPath: '' })))
    await waitFor(() => expect(getBackend().sidecarAvailable).toHaveBeenCalled())
    expect(session.sidecarPath).toBe('')
    expect(group(cleaning, 'flux').textContent).toContain(t('settings.sidecar.ready'))
    expect(cleaning.queryByText(t('settings.sidecar.notFound'))).toBe(null)
  })

  it('reports the managed FLUX install as set up after refreshing capabilities, without a folder override', async () => {
    const cleaning = await open(() => view({ runtime: { platform: 'macos-arm64' } }))
    await flux(cleaning)
    const backend = getBackend()
    backend.installFluxHelper = vi.fn(async () => {
      helperModels = [{ id: 'flux2-klein-4b', label: 'FLUX.2 Klein 4B' }]
      backend.sidecarAvailable = vi.fn(async () => ({ available: true, reasonKey: null }))
      return { ready: true }
    })
    expect(group(cleaning, 'flux').textContent).toContain(t('settings.sidecar.notSetUp'))

    await fireEvent.click(cleaning.getByRole('button', { name: t('settings.sidecar.install') }))
    await waitFor(() => expect(capabilities.sidecar).toBe(true))
    await waitFor(() => expect(cleaning.getByLabelText(t('settings.sidecarModel.label'))).toBeTruthy())
    await waitFor(() => expect(cleaning.getByRole('button', { name: t('settings.sidecar.install') }).disabled).toBe(false))
    expect(backend.installFluxHelper).toHaveBeenCalledWith({ backend: 'auto', accelerator: 'auto' })
    expect(session.sidecarPath).toBe('')
    expect(session.fluxModel).toBe('flux2-klein-4b')
    expect(group(cleaning, 'flux').textContent).toContain(t('settings.sidecar.ready'))
  })

  it('draw a stored FLUX model the helper no longer lists as itself', async () => {
    capabilities.sidecar = true
    helperModels = [{ id: 'flux2-klein-4b', label: 'FLUX.2 Klein 4B' }]
    setFluxModel('flux1-dev')
    const cleaning = await open(both)
    await flux(cleaning)
    const model = /** @type {HTMLSelectElement} */ (
      await waitFor(() => cleaning.getByLabelText(t('settings.sidecarModel.label')))
    )
    await waitFor(() => expect(model.options).toHaveLength(2))
    expect(model.value).toBe('flux1-dev')
    expect(model.selectedOptions[0].textContent?.trim()).toBe(t('settings.sidecarModel.missing', { id: 'flux1-dev' }))
    expect(session.fluxModel).toBe('flux1-dev')
  })

  it('write the helper folder when the field is left, not on every keystroke', async () => {
    const cleaning = await open(both)
    await flux(cleaning)
    const field = /** @type {HTMLInputElement} */ (cleaning.getByLabelText(t('settings.sidecar.label')))
    writeSettings.mockClear()

    await fireEvent.input(field, { target: { value: '/opt/fl' } })
    await fireEvent.input(field, { target: { value: '/opt/flux' } })
    expect(writeSettings).not.toHaveBeenCalled()
    expect(session.sidecarPath).toBe('')

    await fireEvent.blur(field)
    expect(session.sidecarPath).toBe('/opt/flux')
    await waitFor(() => expect(writeSettings).toHaveBeenCalledTimes(1))
    expect(writeSettings).toHaveBeenCalledWith(expect.objectContaining({ sidecarPath: '/opt/flux' }))
  })

  it('keeps MLX visible with its platform requirement and disables Metal setup on Windows', async () => {
    const cleaning = await open(() => view({ runtime: { platform: 'windows-x64' } }))
    await flux(cleaning)
    const mlx = /** @type {HTMLButtonElement} */ (cleaning.getByRole('radio', { name: t('settings.fluxBackend.mfluxUnsupported') }))
    expect(mlx.disabled).toBe(true)
    expect(mlx.title).toBe(t('settings.fluxBackend.mfluxReason'))
    const accelerator = /** @type {HTMLSelectElement} */ (cleaning.getByLabelText(t('settings.sidecar.accelerator')))
    expect(accelerator.querySelector('option[value="mps"]')?.disabled).toBe(true)
  })

  describe('a link to one row', () => {
    /** @type {ReturnType<typeof vi.fn>} */
    let scrolled
    beforeEach(() => {
      scrolled = vi.fn()
      Element.prototype.scrollIntoView = /** @type {any} */ (scrolled)
    })
    afterEach(() => {
      delete (/** @type {any} */ (Element.prototype)).scrollIntoView
    })

    /**
     * Open Settings the way `openModelSettings` does, with the catalogue held
     * back until `release()`, as a real `listModels` arrives after mount.
     *
     * @param {() => any} answer @param {string} id - what the notice names
     */
    function openAt(answer, id) {
      /** @type {() => void} */
      let release = () => {}
      const gate = new Promise((resolve) => { release = () => resolve(undefined) })
      stub(answer)
      listModels = vi.fn(async () => { await gate; return answer() })
      const link = settingsLinkForModel(id)
      const rendered = render(SettingsDialog, { props: { spec: { ...SPEC, props: { tab: link.section, anchor: link.anchor } } } })
      return { rendered, release }
    }

    /** @param {string} anchor */
    const row = (anchor) => /** @type {HTMLElement} */ (document.querySelector(`[data-settings-anchor="${anchor}"]`))

    it('holds on the group heading until the catalogue draws the row, then lands on the row', async () => {
      const { rendered, release } = openAt(both, 'inpainter')
      const heading = rendered.getByRole('heading', { name: t('pipelines.cleaning') })
      await waitFor(() => expect(document.activeElement).toBe(heading))
      expect(row('lama')).toBe(null)

      release()
      await waitFor(() => expect(document.activeElement).toBe(row('lama')))
      expect(scrolled.mock.contexts).toContain(row('lama'))
    })

    it('opens language filtering for one of its models, then focuses that model’s row', async () => {
      const { rendered, release } = openAt(() => withModelGroups(), 'scriptGateLabels')
      const summary = rendered.getByRole('button', { name: new RegExp(t('settings.detection.capability.japanese')) })
      await waitFor(() => expect(document.activeElement).toBe(summary))
      expect(summary.getAttribute('aria-expanded')).toBe('true')

      release()
      await waitFor(() => expect(document.activeElement).toBe(row('scriptGate')))
      expect(scrolled.mock.contexts).toContain(row('scriptGate'))
    })

    it('lands on the runtime’s row in Performance', async () => {
      const { rendered, release } = openAt(both, 'runtime')
      expect(rendered.getByRole('tab', { name: t('settings.section.performance') }).getAttribute('aria-selected')).toBe('true')
      release()
      await waitFor(() => expect(document.activeElement).toBe(row('runtime')))
      expect(scrolled.mock.contexts).toContain(row('runtime'))
    })

    it('lands on the list of files no model claims', async () => {
      const unclaimed = () => {
        const answer = both()
        answer.models.push({ ...answer.models[0], id: 'somethingNew', fileName: 'new.onnx', kindKey: 'models.kind.inpainter' })
        return answer
      }
      const { rendered, release } = openAt(unclaimed, 'somethingNew')
      release()
      const heading = await waitFor(() => rendered.getByRole('heading', { name: t('settings.models.heading') }))
      await waitFor(() => expect(document.activeElement).toBe(heading))
    })

    it.each([
      ['filtering', 'settings.detection.capability.japanese'],
      ['flux', 'settings.sidecar.heading'],
    ])('focuses the %s group by its summary, opened', async (anchor, titleKey) => {
      stub(both)
      const rendered = render(SettingsDialog, { props: { spec: { ...SPEC, props: { tab: 'models', anchor } } } })
      const summary = rendered.getByRole('button', { name: new RegExp(t(titleKey).replace(/[()]/g, '\\$&')) })
      await waitFor(() => expect(document.activeElement).toBe(summary))
      expect(summary.getAttribute('aria-expanded')).toBe('true')
      expect(scrolled.mock.contexts).toContain(row(anchor))
    })

    it('leaves focus alone when the reader moved it before the row arrived', async () => {
      const { rendered, release } = openAt(both, 'inpainter')
      await waitFor(() => expect(document.activeElement).toBe(rendered.getByRole('heading', { name: t('pipelines.cleaning') })))
      const back = rendered.getByRole('button', { name: t('shell.action.done') })
      back.focus()
      release()
      await waitFor(() => expect(row('lama')).not.toBe(null))
      expect(document.activeElement).toBe(back)
    })
  })
})
