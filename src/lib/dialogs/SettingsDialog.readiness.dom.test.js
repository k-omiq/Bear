/**
 * Settings > Models, mounted, for three things the pipeline rows say:
 *
 * - **Readiness counts the runtime.** A native run refuses to start without
 *   ONNX Runtime, so a workflow with every file here is not complete while
 *   the runtime is missing, and the row names it rather than going green.
 * - **Language filtering says whether it runs** under the text policy the
 *   Text cleanup panel sets, and holds its language choices while it does.
 * - **A single-file model keeps per-file Check and Delete in File details**,
 *   like a group, except that the shared speech bubble finder's own Delete is
 *   held while the selected workflow uses it, with the reason beside it.
 *
 * A hand-written seam stub, for the reason `SettingsDialog.models.dom.test.js`
 * gives: only the catalogue and the workflow readiness answer matter here.
 */

import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { downloadFailureShown } from '../api/model-download-notices.js'
import { t } from '../i18n/index.js'
import { DEFAULT_DETECTOR } from '../model/pipelines.js'
import { session, setDetection, setOcrRescue, setTextPolicy } from '../state/session.svelte.js'
import SettingsDialog from './SettingsDialog.svelte'
import { resetFirstLaunch } from './firstlaunch.svelte.js'

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

/** One catalogue row. */
function file(id, fileName, bytes, kindKey, installed = true) {
  return {
    id,
    fileName,
    bytes,
    sha256: id.padEnd(64, '0').slice(0, 64).replace(/[^0-9a-f]/g, 'a'),
    kindKey,
    requiredBy: [],
    installed,
    path: installed ? `/models/${fileName}` : null,
    readOnly: false,
    sha256Ok: installed ? true : null,
    downloading: false,
    partialBytes: null,
  }
}

/**
 * Every file legacy cleaning needs, installed, and the runtime as given.
 *
 * @param {Record<string, unknown>} [runtime]
 */
function catalogue(runtime = {}) {
  return {
    models: [
      file('textDetector', 'comictextdetector.onnx', 94_669_756, 'models.kind.textDetector'),
      file('balloonDetector', 'detector.onnx', 11_380_294, 'models.kind.balloonDetector'),
      file('scriptGate', 'osd_lstm.onnx', 3_722_314, 'models.kind.scriptGate'),
      file('scriptGateLabels', 'osd_labels.json', 1_163, 'models.kind.scriptGateLabels'),
      file('inpainter', 'lama-manga.onnx', 207_482_644, 'models.kind.inpainter'),
    ],
    runtime: {
      installed: true,
      path: '/runtimes/libonnxruntime.dylib',
      readOnly: false,
      downloading: false,
      version: '1.28.0',
      flavour: 'stock',
      bytes: 34_000_000,
      flavours: [],
      available: true,
      installedFlavour: null,
      installedVersion: null,
      partialBytes: null,
      ...runtime,
    },
    modelsDir: '/models',
    runtimeDir: '/runtimes',
    hasToken: false,
    tokenStore: 'keychain',
    tokenStoreReason: null,
  }
}

/** The workflow readiness answer with both imports present. */
const CAPS = Object.freeze({
  runtimeInstalled: true,
  rtInstalled: true,
  fullRtInstalled: false,
  fullRtManaged: false,
  fullRtRevision: null,
  fullRtFile: { name: 'detector.onnx', bytes: 168_000_000, sha256: 'ab'.repeat(32) },
  samInstalled: true,
  samMemoryReady: true,
  samManaged: true,
  samRevision: null,
  samFiles: [],
  cooStatus: 'excluded',
  rtBackends: [],
  samBackends: [],
  samWriteQualified: false,
  samWriteNote: null,
})

/**
 * What `diagnostics` answers for a runtime that loads.
 *
 * @param {Partial<{available: boolean, reasonKey: string|null, detail: string|null}>} [runtime]
 */
function diagnosed(runtime = {}) {
  return {
    appVersion: '0.0.0-test',
    components: [{ name: 'onnxruntime', available: true, detail: null, reasonKey: null, ...runtime }],
  }
}

let verifyModel = vi.fn()
let deleteModel = vi.fn()
let diagnostics = vi.fn()

/**
 * @param {() => any} answer
 * @param {() => Promise<any>} [load] - the `diagnostics` answer
 * @param {{caps?: Record<string, unknown>, verifySam?: () => Promise<boolean>}} [options] - the workflow readiness answer, and what a SAM Check finds
 */
async function openDetection(answer, load = async () => diagnosed(), { caps = {}, verifySam = async () => true } = {}) {
  verifyModel = vi.fn(async () => true)
  deleteModel = vi.fn(async () => 'deleted')
  diagnostics = vi.fn(load)
  const listModels = vi.fn(async () => answer())
  setBackend(
    /** @type {any} */ ({
      listModels,
      listAccelerators: vi.fn(async () => ({ providers: [], models: [] })),
      listSidecarModels: vi.fn(async () => []),
      sidecarAvailable: vi.fn(async () => ({ available: false, reasonKey: null })),
      about: vi.fn(async () => ({ appVersion: '0.0.0-test', facts: [] })),
      diagnostics: () => diagnostics(),
      subscribe: vi.fn(() => () => {}),
      writeSettings: vi.fn(async () => ({})),
      listWorkflowCapabilities: vi.fn(async () => ({ ...CAPS, ...caps })),
      verifySamTs: vi.fn(verifySam),
      verifyModel: (/** @type {any} */ spec) => verifyModel(spec),
      deleteModel: (/** @type {any} */ spec) => deleteModel(spec),
    }),
  )
  const rendered = render(SettingsDialog, { props: { spec: SPEC } })
  const tab = rendered.getByRole('tab', { name: t('settings.section.models') })
  await fireEvent.click(tab)
  const panel = /** @type {HTMLElement} */ (rendered.container.querySelector(`#${tab.getAttribute('aria-controls')}`))
  await waitFor(() => expect(listModels).toHaveBeenCalled())
  await waitFor(() => expect(panel.querySelector('.readiness')).toBeTruthy())
  return { rendered, panel, scoped: within(panel) }
}

/** @param {HTMLElement} panel */
const readiness = (panel) => /** @type {HTMLElement} */ (panel.querySelector('.readiness'))

afterEach(() => {
  cleanup()
  resetFirstLaunch()
  setBackend(null)
  for (const language of ['ja', 'zh', 'ko']) setDetection(language, DEFAULT_DETECTOR)
  setOcrRescue(false)
  setTextPolicy('legacy_gate')
  vi.clearAllMocks()
})

describe('workflow readiness', () => {
  it('is complete only with the runtime, and names the runtime when it is missing', async () => {
    const ready = await openDetection(() => catalogue())
    await waitFor(() => expect(readiness(ready.panel).textContent).toContain(t('settings.detection.ready.legacy')))
    await waitFor(() => expect(readiness(ready.panel).classList.contains('complete')).toBe(true))
    expect(readiness(ready.panel).textContent).not.toContain('ONNX Runtime')
    expect(diagnostics).toHaveBeenCalled()
    cleanup()

    const missing = await openDetection(() => catalogue({ installed: false, installedFlavour: null }))
    const row = readiness(missing.panel)
    await waitFor(() => expect(row.textContent).toContain(t('settings.detection.ready.runtime')))
    // Every model is here, and it is still not complete: the run would refuse.
    expect(row.textContent).toContain(t('settings.detection.ready.legacy'))
    expect(row.classList.contains('complete')).toBe(false)
    expect(t('settings.detection.ready.runtime')).toContain('ONNX Runtime')
    // Nothing to load, so nothing is asked.
    expect(diagnostics).not.toHaveBeenCalled()

    // The press goes where the runtime is managed, and says so.
    await fireEvent.click(within(row).getByRole('button', { name: t('settings.detection.ready.openPerformance') }))
    const performance = missing.rendered.getByRole('tab', { name: t('settings.section.performance') })
    expect(performance.getAttribute('aria-selected')).toBe('true')
    expect(document.activeElement).toBe(performance)
  })

  it('says a runtime download in flight, and a computer with no build, in their own words', async () => {
    const downloading = await openDetection(() => catalogue({ installed: false, downloading: true }))
    await waitFor(() => expect(readiness(downloading.panel).textContent).toContain(t('settings.detection.ready.runtimeDownloading')))
    expect(readiness(downloading.panel).classList.contains('complete')).toBe(false)
    expect(within(readiness(downloading.panel)).queryByRole('button')).toBe(null)
    cleanup()

    const unavailable = await openDetection(() => catalogue({ installed: false, available: false }))
    await waitFor(() => expect(readiness(unavailable.panel).textContent).toContain(t('settings.detection.ready.runtimeUnavailable')))
    expect(readiness(unavailable.panel).classList.contains('complete')).toBe(false)
    expect(within(readiness(unavailable.panel)).queryByRole('button')).toBe(null)
  })

  it('holds all-text review to the same rule', async () => {
    setTextPolicy('all_text')
    const ready = await openDetection(() => catalogue())
    await waitFor(() => expect(readiness(ready.panel).textContent).toContain(t('settings.detection.ready.allText')))
    await waitFor(() => expect(readiness(ready.panel).classList.contains('complete')).toBe(true))
    cleanup()

    const missing = await openDetection(() => catalogue({ installed: false }))
    await waitFor(() => expect(readiness(missing.panel).textContent).toContain(t('settings.detection.ready.runtime')))
    expect(readiness(missing.panel).classList.contains('complete')).toBe(false)
  })

  // Installed is a file found. A run also loads it, and a quarantined library,
  // a CUDA build without CUDA or a missing system library is found and refused.
  it('is not complete while the installed runtime will not load, and says why', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const { rendered, panel } = await openDetection(
      () => catalogue(),
      async () => diagnosed({ available: false, reasonKey: 'diagnostics.runtime.quarantined', detail: 'code signature invalid' }),
    )
    const row = readiness(panel)
    await waitFor(() => expect(row.textContent).toContain(t('diagnostics.runtime.quarantined')))
    expect(row.textContent).toContain(
      t('settings.detection.ready.runtimeUnloadable', { reasonKey: 'diagnostics.runtime.quarantined' }),
    )
    expect(row.classList.contains('complete')).toBe(false)
    // The loader's own words go to the log, never to the screen.
    expect(rendered.container.textContent).not.toContain('code signature invalid')
    expect(warn).toHaveBeenCalled()
    warn.mockRestore()

    // The press goes to the runtime's own row, which repeats why beside the
    // buttons that replace it.
    await fireEvent.click(within(row).getByRole('button', { name: t('settings.detection.ready.openPerformance') }))
    const performance = rendered.getByRole('tab', { name: t('settings.section.performance') })
    expect(document.activeElement).toBe(performance)
    const performancePanel = /** @type {HTMLElement} */ (rendered.container.querySelector(`#${performance.getAttribute('aria-controls')}`))
    expect([...performancePanel.querySelectorAll('.row-error')].map((line) => line.textContent))
      .toContain(t('diagnostics.runtime.quarantined'))
  })

  it('names the remedy a failure has, and the plain reason for one it does not know', async () => {
    for (const [reasonKey, shown] of [
      ['diagnostics.runtime.missingDependency', 'diagnostics.runtime.missingDependency'],
      ['diagnostics.runtime.somethingNew', 'diagnostics.runtime.unloadable'],
      [null, 'diagnostics.runtime.unloadable'],
    ]) {
      const { panel } = await openDetection(() => catalogue(), async () => diagnosed({ available: false, reasonKey }))
      await waitFor(() => expect(readiness(panel).textContent).toContain(t(shown)))
      expect(readiness(panel).classList.contains('complete')).toBe(false)
      cleanup()
    }
    expect(t('diagnostics.runtime.missingDependency')).toContain('Install it from Microsoft')
  })

  it('does not go green before the load is known, or when it cannot be asked', async () => {
    const checking = await openDetection(() => catalogue(), () => new Promise(() => {}))
    await waitFor(() => expect(readiness(checking.panel).textContent).toContain(t('settings.detection.ready.runtimeChecking')))
    expect(readiness(checking.panel).classList.contains('complete')).toBe(false)
    expect(within(readiness(checking.panel)).queryByRole('button')).toBe(null)
    cleanup()

    const log = vi.spyOn(console, 'error').mockImplementation(() => {})
    const unchecked = await openDetection(() => catalogue(), async () => { throw new Error('ipc closed') })
    await waitFor(() => expect(readiness(unchecked.panel).textContent).toContain(t('settings.detection.ready.runtimeUnchecked')))
    expect(readiness(unchecked.panel).classList.contains('complete')).toBe(false)
    expect(log).toHaveBeenCalled()
    log.mockRestore()
  })

  // The review refuses a SAM graph that failed its checksum, and one this
  // machine has not the memory for (`readinessKeyOf`); the summary has to
  // say so rather than call the workflow ready. A graph not checked yet does
  // not hold it back: the review checks it itself.
  it('holds all-text review to a failed SAM checksum and says so', async () => {
    setTextPolicy('all_text')
    const { panel } = await openDetection(() => catalogue(), undefined, { verifySam: async () => false })
    await waitFor(() => expect(readiness(panel).classList.contains('complete')).toBe(true))
    const row = /** @type {HTMLElement} */ (await waitFor(() => panel.querySelector('[data-model="samTs"]')))
    await fireEvent.click(within(row).getByRole('button', { name: t('settings.models.action.verify') }))
    await waitFor(() => expect(readiness(panel).textContent).toContain(t('settings.detection.ready.allTextSamMismatch')))
    expect(readiness(panel).classList.contains('complete')).toBe(false)
    expect(readiness(panel).textContent).not.toContain(t('settings.detection.ready.allText'))
  })

  it('names short memory for all-text review, and what to do about it', async () => {
    setTextPolicy('all_text')
    const { panel } = await openDetection(() => catalogue(), undefined, { caps: { samMemoryReady: false } })
    await waitFor(() => expect(readiness(panel).textContent).toContain(t('settings.detection.ready.allTextMemory')))
    expect(readiness(panel).classList.contains('complete')).toBe(false)
    expect(t('settings.detection.ready.allTextMemory')).toMatch(/Close other apps/)
  })

  it('leaves the runtime out of a workflow that runs nothing', async () => {
    for (const language of ['ja', 'zh', 'ko']) setDetection(language, null)
    const { panel } = await openDetection(() => catalogue({ installed: false }))
    await waitFor(() => expect(readiness(panel).textContent).toContain(t('settings.detection.ready.nothing')))
    expect(readiness(panel).textContent).not.toContain(t('settings.detection.ready.runtime'))
  })
})

describe('language filtering', () => {
  /** @param {ReturnType<typeof within>} scoped */
  const summary = (scoped) => scoped.getByRole('button', { name: new RegExp(t('settings.detection.capability.japanese')) })

  it('is collapsed, and says in its summary whether it runs', async () => {
    const { scoped } = await openDetection(() => catalogue())
    expect(summary(scoped).getAttribute('aria-expanded')).toBe('false')
    expect(summary(scoped).textContent).toContain(t('settings.detection.filtering.on'))
    // The language pickers wait behind the fold.
    expect(scoped.queryByRole('combobox', { name: t('pipelines.detectorFor', { language: t('pipelines.language.ja') }) })).toBe(null)

    await fireEvent.click(summary(scoped))
    expect(scoped.getByRole('combobox', { name: t('pipelines.detectorFor', { language: t('pipelines.language.ja') }) })).toBeTruthy()
  })

  it('leaves the text policy to Text cleanup, and says filtering is off under all text', async () => {
    setTextPolicy('all_text')
    const { scoped, rendered } = await openDetection(() => catalogue())
    expect(summary(scoped).textContent).toContain(t('settings.detection.filtering.off'))
    await fireEvent.click(summary(scoped))
    expect(scoped.getByText(t('settings.detection.languagesAllText'))).toBeTruthy()
    expect(scoped.queryByRole('combobox', { name: t('pipelines.detectorFor', { language: t('pipelines.language.ja') }) })).toBe(null)
    // No policy picker anywhere in Settings.
    expect(rendered.container.querySelector('#settings-text-policy')).toBe(null)
    expect(session.textPolicy).toBe('all_text')
  })
})

describe('File details for a single-file model', () => {
  /** @param {HTMLElement} panel @param {string} id */
  const modelRow = (panel, id) => /** @type {HTMLElement} */ (panel.querySelector(`[data-model="${id}"]`))

  /** @param {HTMLElement} row */
  async function details(row) {
    await fireEvent.click(within(row).getByRole('button', { name: t('settings.models.details') }))
    return /** @type {HTMLElement} */ (row.querySelector('.files li'))
  }

  it('offers the file its own Check and Delete, and Delete asks first', async () => {
    const { panel } = await openDetection(() => catalogue())
    const row = await waitFor(() => modelRow(panel, 'ctd'))
    const line = await details(row)
    expect(line.textContent).toContain('comictextdetector.onnx')

    await fireEvent.click(within(line).getByRole('button', { name: t('settings.models.action.verify') }))
    expect(verifyModel).toHaveBeenCalledWith({ id: 'textDetector' })

    const remove = within(line).getByRole('button', { name: t('settings.models.action.delete') })
    expect(remove.hasAttribute('aria-disabled')).toBe(false)
    await fireEvent.click(remove)
    const confirm = /** @type {HTMLElement} */ (row.querySelector('.confirm'))
    expect(confirm.textContent).toContain(t('settings.models.remove.ctd'))
    await fireEvent.click(within(confirm).getByRole('button', { name: t('settings.models.action.delete') }))
    await waitFor(() => expect(deleteModel).toHaveBeenCalledWith({ id: 'textDetector' }))
  })

  it('holds the shared speech bubble finder’s own Delete while the selected workflow uses it, and says why', async () => {
    const { panel } = await openDetection(() => catalogue())
    const row = await waitFor(() => modelRow(panel, 'rtSmall'))
    const line = await details(row)
    const remove = within(line).getByRole('button', { name: t('settings.models.action.delete') })

    // Held, not disabled: still a tab stop, with the reason read beside it.
    expect(remove.getAttribute('aria-disabled')).toBe('true')
    expect(/** @type {HTMLButtonElement} */ (remove).disabled).toBe(false)
    const reason = document.getElementById(/** @type {string} */ (remove.getAttribute('aria-describedby')?.split(' ').at(-1)))
    expect(reason?.textContent).toBe(t('settings.models.fileShared'))
    await fireEvent.click(remove)
    expect(row.querySelector('.confirm')).toBe(null)
    expect(deleteModel).not.toHaveBeenCalled()
    // Check is never held: it changes nothing.
    await fireEvent.click(within(line).getByRole('button', { name: t('settings.models.action.verify') }))
    expect(verifyModel).toHaveBeenCalledWith({ id: 'balloonDetector' })

    // The row's own Delete stays, and names what stops before it happens.
    const rowDelete = within(/** @type {HTMLElement} */ (row.querySelector('.row-actions'))).getByRole('button', { name: t('settings.models.action.delete') })
    await fireEvent.click(rowDelete)
    expect(row.querySelector('.confirm')?.textContent).toContain(t('settings.models.remove.rtSmall'))
  })

  it('lets the shared file go from the details once no selected workflow reads it', async () => {
    for (const language of ['ja', 'zh', 'ko']) setDetection(language, null)
    const { panel } = await openDetection(() => catalogue())
    const row = await waitFor(() => modelRow(panel, 'rtSmall'))
    const line = await details(row)
    const remove = within(line).getByRole('button', { name: t('settings.models.action.delete') })
    expect(remove.hasAttribute('aria-disabled')).toBe(false)
    expect(line.textContent).not.toContain(t('settings.models.fileShared'))
    await fireEvent.click(remove)
    await fireEvent.click(within(/** @type {HTMLElement} */ (row.querySelector('.confirm'))).getByRole('button', { name: t('settings.models.action.delete') }))
    await waitFor(() => expect(deleteModel).toHaveBeenCalledWith({ id: 'balloonDetector' }))
  })
})

// A background download's failure is said once: on its row when the row's
// section is on screen, as a notice otherwise. The dialog says which section
// that is, and stops saying so when it closes.
describe('the section Settings shows, for download failure notices', () => {
  const failure = (/** @type {string} */ id) => ({ type: 'model-progress', id, done: true, error: 'disk full' })
  const over = { firstLaunch: null, topModal: 'settings' }

  it('follows the tab, and ends with the dialog', async () => {
    const { rendered } = await openDetection(() => catalogue())
    expect(downloadFailureShown(failure('textDetector'), over)).toBe(true)
    expect(downloadFailureShown(failure('runtime'), over)).toBe(false)

    await fireEvent.click(rendered.getByRole('tab', { name: t('settings.section.general') }))
    expect(downloadFailureShown(failure('textDetector'), over)).toBe(false)
    await fireEvent.click(rendered.getByRole('tab', { name: t('settings.section.performance') }))
    expect(downloadFailureShown(failure('runtime'), over)).toBe(true)

    cleanup()
    expect(downloadFailureShown(failure('runtime'), over)).toBe(false)
  })
})
