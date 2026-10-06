import { afterEach, describe, expect, it, vi } from 'vitest'
import { app, clearNotices } from '../state/app.svelte.js'
import { t } from '../i18n/index.js'
import { RUNTIME_LABEL_KEY } from '../dialogs/firstlaunch.js'
import { iconNames } from '../icons/paths.js'
import { MODELS, model, modelOfFile } from '../model/pipelines.js'
import {
  downloadFailureShown,
  downloadNameKey,
  reportModelDownloadFailure,
  settingsSectionOf,
  showSettingsSection,
} from './model-download-notices.js'

afterEach(() => clearNotices())

/**
 * A catalogue as `listModels` answers it: one file the logical table knows and
 * one it does not yet (a weight a newer backend added).
 */
function catalogue() {
  return {
    listModels: vi.fn(async () => ({
      models: [
        { id: 'inpainter', kindKey: 'models.kind.inpainter' },
        { id: 'futureWeight', kindKey: 'models.kind.sidecar' },
      ],
    })),
  }
}

/** @param {string} id @param {string} error */
const failed = (id, error) => ({ type: 'model-progress', id, done: true, error })

describe('background model download notices', () => {
  it('announces a completed failure that is not visible in an inline download row', async () => {
    const backend = catalogue()
    expect(await reportModelDownloadFailure(failed('inpainter', 'network unavailable'), { backend })).toBe(true)
    expect(app.notices).toHaveLength(1)
    expect(app.notices[0]).toMatchObject({
      key: 'notice.download.failed',
      params: { nameKey: modelOfFile('inpainter')?.nameKey, error: 'network unavailable' },
      tone: 'warn',
    })
  })

  it('draws only an icon the icon set has, since an unknown one throws and empties the stack', async () => {
    await reportModelDownloadFailure(failed('inpainter', 'network unavailable'), { backend: catalogue() })
    const { icon } = app.notices[0]
    // A warn notice with no icon draws `warning-triangle` (`ui/Notice.svelte`).
    if (icon !== undefined) expect(iconNames).toContain(icon)
    expect(iconNames).toContain('warning-triangle')
  })

  it('avoids duplicates for visible failures, progress, success, and user cancellations', async () => {
    const backend = catalogue()
    expect(await reportModelDownloadFailure(failed('inpainter', 'disk full'), { covered: true, backend })).toBe(false)
    expect(await reportModelDownloadFailure({ type: 'model-progress', id: 'inpainter', done: false, error: null }, { backend })).toBe(false)
    expect(await reportModelDownloadFailure({ type: 'model-progress', id: 'inpainter', done: true, error: null }, { backend })).toBe(false)
    expect(await reportModelDownloadFailure(failed('inpainter', 'cancelled'), { backend })).toBe(false)
    expect(app.notices).toHaveLength(0)
    expect(backend.listModels).not.toHaveBeenCalled()
  })
})

describe('the name a failure notice gives a download', () => {
  it('is the name Settings shows for a file group, never the raw id', async () => {
    const backend = catalogue()
    await reportModelDownloadFailure(failed('mangaOcr', 'network unavailable'), { backend })
    const notice = app.notices[0]
    expect(notice.params.nameKey).toBe(model('mangaOcr')?.nameKey)
    const text = t(notice.key, notice.params)
    expect(text).toContain(t(/** @type {string} */ (model('mangaOcr')?.nameKey)))
    expect(text).not.toContain('mangaOcr')
    // Named from the table, without a catalogue read.
    expect(backend.listModels).not.toHaveBeenCalled()
  })

  it('names every file and group the table lists by its logical model, with no catalogue read', async () => {
    const backend = catalogue()
    for (const entry of MODELS) {
      for (const id of [...entry.files, ...(entry.group ? [entry.group] : [])]) {
        expect(await downloadNameKey(id, backend), id).toBe(entry.nameKey)
      }
    }
    expect(backend.listModels).not.toHaveBeenCalled()
  })

  it('names the runtime with the label Settings gives it', async () => {
    expect(await downloadNameKey('runtime', catalogue())).toBe(RUNTIME_LABEL_KEY)
  })

  it('falls back to the catalogue kind for a file the table does not know yet', async () => {
    const backend = catalogue()
    expect(await downloadNameKey('futureWeight', backend)).toBe('models.kind.sidecar')
    expect(backend.listModels).toHaveBeenCalledTimes(1)
  })

  it('falls back to a sentence without a name, not to the id, when nothing names it', async () => {
    await reportModelDownloadFailure(failed('unheardOf', 'HTTP 404'), { backend: catalogue() })
    const broken = { listModels: vi.fn().mockRejectedValue(new Error('no seam')) }
    await reportModelDownloadFailure(failed('alsoUnheardOf', 'HTTP 404'), { backend: broken })
    expect(app.notices.map((notice) => notice.key)).toEqual(['notice.download.failedUnnamed', 'notice.download.failedUnnamed'])
    for (const notice of app.notices) {
      const text = t(notice.key, notice.params)
      expect(text).not.toMatch(/unheardOf|UnheardOf/)
      expect(text).toContain('HTTP 404')
    }
  })

  it('reads as one sentence, whatever punctuation the backend ended on', async () => {
    await reportModelDownloadFailure(failed('inpainter', 'disk full.'), { backend: catalogue() })
    const notice = app.notices[0]
    expect(t(notice.key, notice.params)).toBe(
      `${t(/** @type {string} */ (modelOfFile('inpainter')?.nameKey))} could not be downloaded: disk full. Try again from Settings.`,
    )
  })
})

/**
 * Whether a failure is already on screen, so the notice would say it twice.
 * Settings shows a download's error on that download's own row, and a row is
 * on one section: Settings over General, Cloud or Shortcuts shows nothing of
 * it, and the notice is the only place the failure is said.
 */
describe('a failure already on screen', () => {
  afterEach(() => showSettingsSection(null))

  it('files each download under the Settings section that lists its row', () => {
    expect(settingsSectionOf('runtime')).toBe('performance')
    // Detection and Cleaning are two groups of one Models section now.
    for (const entry of MODELS) {
      for (const id of [...entry.files, ...(entry.group ? [entry.group] : [])]) {
        expect(settingsSectionOf(id), id).toBe('models')
      }
    }
    expect(settingsSectionOf('inpainter')).toBe('models')
    expect(settingsSectionOf('textDetector')).toBe('models')
    // A file the table does not know yet: Models, where Settings lists it.
    expect(settingsSectionOf('futureWeight')).toBe('models')
  })

  it('counts Settings as showing a failure only on the section with its row', () => {
    const settings = { firstLaunch: null, topModal: 'settings' }
    showSettingsSection('general')
    expect(downloadFailureShown(failed('inpainter', 'disk full'), settings)).toBe(false)
    expect(downloadFailureShown(failed('runtime', 'disk full'), settings)).toBe(false)
    showSettingsSection('models')
    expect(downloadFailureShown(failed('inpainter', 'disk full'), settings)).toBe(true)
    expect(downloadFailureShown(failed('textDetector', 'disk full'), settings)).toBe(true)
    expect(downloadFailureShown(failed('runtime', 'disk full'), settings)).toBe(false)
    showSettingsSection('performance')
    expect(downloadFailureShown(failed('runtime', 'disk full'), settings)).toBe(true)
    // Something over Settings hides the row, whatever section is behind it.
    expect(downloadFailureShown(failed('runtime', 'disk full'), { firstLaunch: null, topModal: 'cloudConsent' })).toBe(false)
    showSettingsSection(null)
    expect(downloadFailureShown(failed('runtime', 'disk full'), settings)).toBe(false)
  })

  it('counts the setup screen as showing a failure it is downloading, group members included', () => {
    const firstLaunch = { open: true, step: 'downloads', queue: ['scriptGate', 'scriptGateLabels', 'runtime'] }
    expect(downloadFailureShown(failed('scriptGate', 'x'), { firstLaunch, topModal: null })).toBe(true)
    expect(downloadFailureShown(failed('runtime', 'x'), { firstLaunch, topModal: null })).toBe(true)
    expect(downloadFailureShown(failed('inpainter', 'x'), { firstLaunch, topModal: null })).toBe(false)
    expect(downloadFailureShown(failed('mangaOcr', 'x'), {
      firstLaunch: { ...firstLaunch, queue: ['ocrDecoder'] }, topModal: null,
    })).toBe(true)
    expect(downloadFailureShown(failed('runtime', 'x'), { firstLaunch: { ...firstLaunch, step: 'done' }, topModal: null })).toBe(false)
  })
})
