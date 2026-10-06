/**
 * The chapter's Denoise dialog, driven end to end against the browser mock:
 * the local path (fetch the model, run, read the summary), the cloud path
 * (review cost, the two statements, confirm, read the summary), the short
 * setup when denoise is off, and a proposal discarded when the consent is
 * left with Back.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { t } from '../i18n/index.js'
import { closeModal, notify } from '../state/app.svelte.js'
import { jobById, resetJobs, runningJobFor } from '../state/jobs.svelte.js'
import {
  session,
  setCloudAllowed,
  setDenoiseLocalSeconds,
  setDenoisePreset,
  setDenoiseTarget,
} from '../state/session.svelte.js'
import { resetDenoiseState } from '../state/denoise.svelte.js'
import DenoiseDialog from './DenoiseDialog.svelte'

vi.mock('../state/app.svelte.js', async (importOriginal) => ({
  ...(await importOriginal()),
  closeModal: vi.fn(),
  notify: vi.fn(),
}))

const CHAPTER_ID = 'tsuki-to-hane-ch107'
const MODAL_KEYS = { token_id: 'ak-fake', token_secret: 'as-fake' }

/** @type {ReturnType<typeof createMockBackend>} */
let mock

function makeMock() {
  return createMockBackend({ timing: { method: 0, cloud: 0, provision: 0, analysis: 0, region: 0, pageTail: 0 } })
}

/**
 * Cloud allowed and a Modal endpoint provisioned and selected, as `mock.cloud.test.js` sets it up,
 * with Page denoise on unless `denoise` is false.
 */
async function provision({ denoise = true } = {}) {
  await mock.writeSettings({ cloudEngines: 'allowed' })
  const plan = await mock.runCloudProvisioner({
    op: 'plan',
    provider: 'modal',
    params: { credentials: MODAL_KEYS, installation_id: 'mc-ready1', options: { denoise } },
  })
  const applied = await mock.runCloudProvisioner({
    op: 'apply',
    provider: 'modal',
    params: { credentials: MODAL_KEYS, installation_id: 'mc-ready1', approved_plan_hash: plan.data.plan_hash },
  })
  expect(applied.success).toBe(true)
  setCloudAllowed(true)
}

/** The chapter as Home holds it, with the source folder the output defaults under. */
async function chapter() {
  const projects = await mock.listProjects()
  const found = projects.flatMap((project) => project.chapters).find((entry) => entry.id === CHAPTER_ID)
  expect(found).toBeTruthy()
  return { ...found, sourcePath: found.sourcePath || '/Users/me/scans/ch107' }
}

async function open({ cleaned = false } = {}) {
  const held = await chapter()
  const titleKey = cleaned ? 'denoise.titleCleaned' : 'denoise.title'
  const spec = { id: 'modal-1', kind: 'denoise', titleKey, props: { projectId: 'tsuki-to-hane', chapter: held, cleaned } }
  const view = render(DenoiseDialog, { props: { spec } })
  const phase = () => view.container.querySelector('[data-phase]')?.getAttribute('data-phase')
  return { ...view, held, phase }
}

beforeEach(() => {
  resetJobs()
  vi.mocked(notify).mockClear()
  resetDenoiseState()
  setCloudAllowed(false)
  setDenoiseTarget('local')
  setDenoisePreset('')
  setDenoiseLocalSeconds(null)
  vi.mocked(closeModal).mockClear()
  mock = makeMock()
  setBackend(mock)
})

afterEach(cleanup)

describe('the denoise dialog', () => {
  it('runs on this computer once the model is here, and reports every page', async () => {
    const { getByRole, getAllByRole, container, held, phase } = await open()
    expect(phase()).toBe('form')
    const run = () => getByRole('button', { name: t('denoise.action.run', { count: held.pages.length }) })

    // The model is not here yet: Run waits for it, and the download is offered.
    await waitFor(() => expect(container.querySelectorAll('[data-denoise-model]').length).toBe(2))
    expect(run().hasAttribute('disabled')).toBe(true)
    expect(container.querySelector('[data-basis]')?.getAttribute('data-basis')).toBe('unmeasured')

    for (const button of getAllByRole('button', { name: t('settings.models.action.download') })) await fireEvent.click(button)
    await waitFor(() => expect(run().hasAttribute('disabled')).toBe(false), { timeout: 3000 })

    await fireEvent.click(run())
    await waitFor(() => expect(phase()).toBe('summary'))
    const written = container.querySelector('[data-written]')
    expect(written?.getAttribute('data-written')).toBe(String(held.pages.length))
    expect(written?.textContent).toContain(t('denoise.summary.written', { count: held.pages.length }))
    expect(container.querySelector('code.path')?.textContent).toBe(`${held.sourcePath.replace(/[\\/]+$/, '')}/denoised`)
  })

  it('opened as Denoise cleaned chapter, says so and writes to denoised-cleaned', async () => {
    const { getByText, container, held, phase } = await open({ cleaned: true })
    expect(phase()).toBe('form')
    expect(getByText(t('denoise.titleCleaned'))).toBeTruthy()
    const field = /** @type {HTMLInputElement} */ (container.querySelector('input[id$="-path"]'))
    expect(field.value).toBe(`${held.sourcePath.replace(/[\\/]+$/, '')}/denoised-cleaned`)
  })

  it('names a page that failed on this computer, in words', async () => {
    mock = createMockBackend({ denoiseScenario: 'pageFail', timing: { method: 0, analysis: 0, region: 0, pageTail: 0 } })
    setBackend(mock)
    await mock.downloadModelGroup({ id: 'pageDenoise' })
    await vi.waitFor(async () => {
      const view = await mock.listModels()
      expect(view.models.filter((row) => row.requiredBy.includes('pageDenoise')).every((row) => row.installed)).toBe(true)
    })
    const { getByRole, container, held, phase } = await open()

    const run = getByRole('button', { name: t('denoise.action.run', { count: held.pages.length }) })
    await waitFor(() => expect(run.hasAttribute('disabled')).toBe(false))
    await fireEvent.click(run)
    await waitFor(() => expect(phase()).toBe('summary'))
    expect(container.querySelector('[data-written]')?.getAttribute('data-written')).toBe(String(held.pages.length - 1))
    expect(container.querySelector('[data-page="2"]')?.textContent).toContain(t('denoise.reason.inference'))
  })

  it('reviews the cost, asks both statements, and runs on the cloud GPU', async () => {
    await provision()
    setDenoiseTarget('cloud')
    const prepare = vi.spyOn(mock, 'prepareCloudDenoise')
    const start = vi.spyOn(mock, 'startCloudDenoise')
    const { getByRole, getByLabelText, container, held, phase } = await open()

    expect(container.querySelectorAll('[data-preset]')).toHaveLength(6)
    expect(container.querySelector('[data-basis]')?.getAttribute('data-basis')).toMatch(/^(pages|reference)$/)
    await fireEvent.click(getByRole('button', { name: t('denoise.action.review') }))
    await waitFor(() => expect(phase()).toBe('consent'))
    expect(prepare).toHaveBeenCalledWith(expect.objectContaining({ chapterId: CHAPTER_ID, pageIndices: null }))

    const confirm = getByRole('button', { name: t('shell.action.confirmSpend') })
    expect(confirm.hasAttribute('disabled')).toBe(true)
    expect(container.querySelector('[data-fact="what"]')?.textContent).toContain(String(held.pages.length))
    await fireEvent.click(getByLabelText(t('cloud.analysis.rights')))
    await fireEvent.click(getByLabelText(t('cloud.analysis.retention')))
    expect(confirm.hasAttribute('disabled')).toBe(false)

    await fireEvent.click(confirm)
    await waitFor(() => expect(phase()).toBe('summary'))
    expect(start).toHaveBeenCalledWith(expect.objectContaining({ outDir: expect.stringMatching(/denoised$/) }))
    expect(container.querySelector('[data-written]')?.getAttribute('data-written')).toBe(String(held.pages.length))
  })

  it('offers no Cloud, and says why, when the endpoint was set up without Page denoise', async () => {
    await provision({ denoise: false })
    setDenoiseTarget('cloud')
    const prepare = vi.spyOn(mock, 'prepareCloudDenoise')
    const { getByRole, getByText, container, phase } = await open()

    await waitFor(() => expect(getByText(t('denoise.target.cloudNotSetUp'))).toBeTruthy())
    expect(phase()).toBe('form')
    expect(getByRole('radio', { name: t('denoise.target.cloud') }).hasAttribute('disabled')).toBe(true)
    expect(getByRole('radio', { name: t('denoise.target.local') }).getAttribute('aria-checked')).toBe('true')
    expect([...container.querySelectorAll('[data-preset]')].map((row) => row.getAttribute('data-preset')))
      .toEqual(['waifu2x-scan-4x-n2'])
    expect(session.denoiseTarget).toBe('cloud')
    expect(prepare).not.toHaveBeenCalled()
  })

  it('shows how far a local run is, and Stop keeps the pages already saved', async () => {
    mock = createMockBackend({ timing: { method: 0, analysis: 4000, region: 400, pageTail: 0 } })
    setBackend(mock)
    await mock.downloadModelGroup({ id: 'pageDenoise' })
    await vi.waitFor(async () => {
      const view = await mock.listModels()
      expect(view.models.filter((row) => row.requiredBy.includes('pageDenoise')).every((row) => row.installed)).toBe(true)
    }, { timeout: 5000 })
    const cancel = vi.spyOn(mock, 'cancelDenoiseLocal')
    const { getByRole, container, held, phase } = await open()
    expect(held.pages.length).toBeGreaterThan(2)

    const run = getByRole('button', { name: t('denoise.action.run', { count: held.pages.length }) })
    await waitFor(() => expect(run.hasAttribute('disabled')).toBe(false))
    await fireEvent.click(run)
    const bar = () => container.querySelector('[role="progressbar"]')
    await waitFor(() => expect(Number(bar()?.getAttribute('aria-valuenow'))).toBeGreaterThan(0), { timeout: 5000 })
    // Past the first page: it is saved, and the second is in progress.
    await waitFor(() => expect(container.querySelector('[role="status"]')?.textContent)
      .toContain(t('denoise.busy.page', { page: 2, total: held.pages.length })), { timeout: 5000 })

    await fireEvent.click(getByRole('button', { name: t('denoise.action.stop') }))
    expect(cancel).toHaveBeenCalledWith({ runId: expect.stringMatching(/^den-/) })
    await waitFor(() => expect(phase()).toBe('summary'), { timeout: 5000 })
    const saved = Number(container.querySelector('[data-written]')?.getAttribute('data-written'))
    expect(saved).toBeGreaterThanOrEqual(1)
    expect(saved).toBeLessThan(held.pages.length)
    expect(container.querySelector('[data-stopped]')?.textContent).toBe(t('denoise.summary.stopped'))
  }, 15000)

  it('keeps a local run going when closed, and shows it again with its Stop when reopened', async () => {
    mock = createMockBackend({ timing: { method: 0, analysis: 4000, region: 400, pageTail: 0 } })
    setBackend(mock)
    await mock.downloadModelGroup({ id: 'pageDenoise' })
    await vi.waitFor(async () => {
      const view = await mock.listModels()
      expect(view.models.filter((row) => row.requiredBy.includes('pageDenoise')).every((row) => row.installed)).toBe(true)
    }, { timeout: 5000 })
    const cancel = vi.spyOn(mock, 'cancelDenoiseLocal')
    const first = await open()
    const run = first.getByRole('button', { name: t('denoise.action.run', { count: first.held.pages.length }) })
    await waitFor(() => expect(run.hasAttribute('disabled')).toBe(false))
    await fireEvent.click(run)
    await waitFor(() => expect(Number(first.container.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow'))).toBeGreaterThan(0), { timeout: 5000 })
    expect(first.container.textContent).toContain(t('denoise.busy.background'))

    // Closed: the run is the jobs list's now, and nothing stopped it.
    await fireEvent.click(first.getByRole('button', { name: t('shell.action.close') }))
    expect(closeModal).toHaveBeenCalledWith(null)
    first.unmount()
    const job = runningJobFor(CHAPTER_ID)
    expect(job).toMatchObject({ kind: 'denoise', status: 'running', projectId: 'tsuki-to-hane', chapterNumber: first.held.number })
    expect(cancel).not.toHaveBeenCalled()

    // Reopened for the same chapter: the same run, with its progress and Stop.
    const second = await open()
    expect(second.phase()).toBe('running')
    const bar = () => second.container.querySelector('[role="progressbar"]')
    await waitFor(() => expect(Number(bar()?.getAttribute('aria-valuenow'))).toBeGreaterThan(0))
    await fireEvent.click(second.getByRole('button', { name: t('denoise.action.stop') }))
    expect(cancel).toHaveBeenCalledWith({ runId: job?.runId })
    await waitFor(() => expect(second.phase()).toBe('summary'), { timeout: 5000 })
    expect(second.container.querySelector('[data-stopped]')).toBeTruthy()
    expect(jobById(/** @type {string} */ (job?.runId))?.status).toBe('cancelled')
    // The dialog said how it ended; no notice on top.
    expect(notify).not.toHaveBeenCalled()
  }, 20000)

  it('says how a run closed early ended, as a notice', async () => {
    const { getByRole, held, unmount } = await open()
    await mock.downloadModelGroup({ id: 'pageDenoise' })
    const run = getByRole('button', { name: t('denoise.action.run', { count: held.pages.length }) })
    await waitFor(() => expect(run.hasAttribute('disabled')).toBe(false), { timeout: 3000 })
    await fireEvent.click(run)
    unmount()
    await waitFor(() => expect(notify).toHaveBeenCalledWith(expect.objectContaining({
      key: 'denoise.notice.finished',
      params: expect.objectContaining({ count: held.pages.length, failed: 0 }),
    })))
  })

  it('shows a cloud run\'s progress page by page, and stops it', async () => {
    mock = createMockBackend({ timing: { method: 0, cloud: 4000, provision: 0, analysis: 0, region: 200, pageTail: 0 } })
    setBackend(mock)
    await provision()
    setDenoiseTarget('cloud')
    const cancel = vi.spyOn(mock, 'cancelDenoiseLocal')
    const start = vi.spyOn(mock, 'startCloudDenoise')
    const { getByRole, getByLabelText, container, held, phase } = await open()
    await fireEvent.click(getByRole('button', { name: t('denoise.action.review') }))
    await waitFor(() => expect(phase()).toBe('consent'))
    await fireEvent.click(getByLabelText(t('cloud.analysis.rights')))
    await fireEvent.click(getByLabelText(t('cloud.analysis.retention')))
    await fireEvent.click(getByRole('button', { name: t('shell.action.confirmSpend') }))

    const bar = () => container.querySelector('[role="progressbar"]')
    await waitFor(() => expect(Number(bar()?.getAttribute('aria-valuenow'))).toBeGreaterThan(0), { timeout: 5000 })
    expect(container.textContent).toContain(t('denoise.busy.cloud'))
    const runId = start.mock.calls[0][0].runId
    expect(runId).toMatch(/^den-/)
    expect(runningJobFor(CHAPTER_ID)).toMatchObject({ runId, kind: 'cloudDenoise' })

    await fireEvent.click(getByRole('button', { name: t('denoise.action.stop') }))
    expect(cancel).toHaveBeenCalledWith({ runId })
    await waitFor(() => expect(phase()).toBe('summary'), { timeout: 5000 })
    const saved = Number(container.querySelector('[data-written]')?.getAttribute('data-written'))
    expect(saved).toBeGreaterThanOrEqual(1)
    expect(saved).toBeLessThan(held.pages.length)
    expect(container.querySelector('[data-stopped]')).toBeTruthy()
  }, 20000)

  it('discards the proposal when the consent is left with Back', async () => {
    await provision()
    setDenoiseTarget('cloud')
    const cancel = vi.spyOn(mock, 'cancelCloudDenoise')
    const { getByRole, phase } = await open()

    await fireEvent.click(getByRole('button', { name: t('denoise.action.review') }))
    await waitFor(() => expect(phase()).toBe('consent'))
    await fireEvent.click(getByRole('button', { name: t('denoise.action.back') }))
    expect(phase()).toBe('form')
    expect(cancel).toHaveBeenCalledTimes(1)
  })

  it('asks where to run first when denoise is off, and keeps the answer', async () => {
    setDenoiseTarget('off')
    const { container, phase } = await open()
    expect(phase()).toBe('setup')
    const cloud = /** @type {HTMLButtonElement} */ (container.querySelector('[data-target="cloud"]'))
    expect(cloud.disabled).toBe(true)

    await fireEvent.click(/** @type {HTMLElement} */ (container.querySelector('[data-target="local"]')))
    await waitFor(() => expect(phase()).toBe('form'))
    expect(session.denoiseTarget).toBe('local')
    expect([...container.querySelectorAll('[data-preset]')].map((row) => row.getAttribute('data-preset')))
      .toEqual(['waifu2x-scan-4x-n2'])
  })
})
