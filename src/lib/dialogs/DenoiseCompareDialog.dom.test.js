/**
 * The compare view: a run's denoised page over the raw page, stepped a page at
 * a time, with the run picker switching runs, the run in use picked and a run
 * deleted after a question, zoom to 1:1 and 2:1, and a page whose file is gone
 * said to be gone rather than drawn half-compared.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { closeModal } from '../state/app.svelte.js'
import { t } from '../i18n/index.js'
import { runLabel } from '../model/denoisehistory.js'
import DenoiseCompareDialog from './DenoiseCompareDialog.svelte'

vi.mock('../state/app.svelte.js', async (importOriginal) => ({
  ...(await importOriginal()),
  closeModal: vi.fn(),
}))

const CHAPTER = { id: 'tsuki-to-hane-ch107', number: 107, name: 'Paper Moon', pages: [{}, {}, {}] }
const page = (pageIndex, over = {}) => ({ pageIndex, exists: true, taken: false, current: true, fromCleaned: false, ...over })
const RUNS = [
  { created: 200, preset: 'waifu2x-scan-4x-n2', target: 'local', fromCleaned: false, folder: '/scans/new',
    pages: [page(0, { taken: true, current: false }), page(1), page(2, { exists: false })] },
  { created: 100, preset: null, target: null, fromCleaned: null, folder: '/scans/old',
    pages: [page(1, { current: false })] },
]

/** @type {ReturnType<typeof vi.fn>} */
let denoiseCompareImage

beforeEach(() => {
  let next = 0
  vi.stubGlobal('URL', Object.assign(Object.create(URL), {
    createObjectURL: vi.fn(() => `blob:compare-${next++}`),
    revokeObjectURL: vi.fn(),
  }))
  denoiseCompareImage = vi.fn(async ({ side }) => new TextEncoder().encode(`<svg data-side="${side}"/>`))
  setBackend(/** @type {any} */ ({ denoiseCompareImage }))
})

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

function open(run = 200) {
  const spec = { id: 'modal-1', kind: 'denoiseCompare', props: { projectId: 'p', chapter: CHAPTER, runs: RUNS, run } }
  const view = render(DenoiseCompareDialog, { props: { spec } })
  const root = () => /** @type {HTMLElement} */ (view.container.ownerDocument.querySelector('.compare'))
  return { ...view, root }
}

describe('the denoise compare view', () => {
  it('shows the chosen run\'s first page, denoised over raw, each side labelled', async () => {
    const { getByAltText, root, getByText } = open()
    await waitFor(() => getByAltText(t('denoise.compare.denoised')))
    expect(getByAltText(t('denoise.compare.raw'))).toBeTruthy()
    expect(root().dataset.page).toBe('0')
    expect(denoiseCompareImage).toHaveBeenCalledWith({ chapterId: CHAPTER.id, pageIndex: 0, run: 200, side: 'denoised' })
    expect(denoiseCompareImage).toHaveBeenCalledWith({ chapterId: CHAPTER.id, pageIndex: 0, run: 200, side: 'raw' })
    expect(getByText(t('denoise.compare.page', { page: 1, count: 3 }))).toBeTruthy()
    expect(getByText(t('denoise.compare.taken')), 'page 1 was taken as the page').toBeTruthy()
    const tags = [...root().querySelectorAll('.tag')].map((tag) => tag.textContent)
    expect(tags).toEqual([t('denoise.compare.denoised'), t('denoise.compare.raw')])
  })

  it('steps pages, and says a page whose file is gone is gone', async () => {
    const { getByRole, getByAltText, queryByAltText, root, findByText } = open()
    await waitFor(() => getByAltText(t('denoise.compare.denoised')))
    await fireEvent.click(getByRole('button', { name: t('denoise.compare.next') }))
    await waitFor(() => expect(root().dataset.page).toBe('1'))
    await fireEvent.click(getByRole('button', { name: t('denoise.compare.next') }))
    await waitFor(() => expect(root().dataset.page).toBe('2'))
    await findByText(t('denoise.compare.missing', { page: 3, folder: '/scans/new' }))
    expect(queryByAltText(t('denoise.compare.denoised'))).toBeNull()
    expect(denoiseCompareImage).not.toHaveBeenCalledWith(expect.objectContaining({ pageIndex: 2 }))
    expect(/** @type {HTMLButtonElement} */ (getByRole('button', { name: t('denoise.compare.next') })).disabled).toBe(true)
  })

  it('switches runs from the picker, staying on the same page when the other run has it', async () => {
    const { getByRole, getByAltText, root } = open()
    await waitFor(() => getByAltText(t('denoise.compare.denoised')))
    await fireEvent.click(getByRole('button', { name: t('denoise.compare.next') }))
    await waitFor(() => expect(root().dataset.page).toBe('1'))
    const picker = /** @type {HTMLSelectElement} */ (getByRole('combobox', { name: t('denoise.compare.run') }))
    expect([...picker.options].map((option) => option.textContent?.trim())).toEqual([`${runLabel(RUNS[0])} · ${t('denoise.compare.inUse')}`, runLabel(RUNS[1])])
    await fireEvent.change(picker, { target: { value: '100' } })
    await waitFor(() => expect(root().dataset.run).toBe('100'))
    expect(root().dataset.page).toBe('1')
    await waitFor(() => expect(denoiseCompareImage).toHaveBeenCalledWith(expect.objectContaining({ pageIndex: 1, run: 100 })))
  })

  it('marks the run in use, makes another the one in use, and reads the history again', async () => {
    const useDenoiseRun = vi.fn(async () => {})
    const fresh = [{ ...RUNS[0] }, { ...RUNS[1], inUse: true }]
    const denoiseHistory = vi.fn(async () => fresh)
    setBackend(/** @type {any} */ ({ denoiseCompareImage, useDenoiseRun, denoiseHistory, listProjects: async () => [] }))
    const { getByRole, getByText, queryByRole, root } = open()
    expect(getByText(t('denoise.compare.inUse')), 'the newest is in use when none is marked').toBeTruthy()
    expect(queryByRole('button', { name: t('denoise.compare.use') })).toBeNull()

    const picker = /** @type {HTMLSelectElement} */ (getByRole('combobox', { name: t('denoise.compare.run') }))
    expect(picker.options[0].textContent).toContain(t('denoise.compare.inUse'))
    await fireEvent.change(picker, { target: { value: '100' } })
    await waitFor(() => expect(root().dataset.run).toBe('100'))
    await fireEvent.click(getByRole('button', { name: t('denoise.compare.use') }))
    await waitFor(() => expect(useDenoiseRun).toHaveBeenCalledWith({ chapterId: CHAPTER.id, run: 100 }))
    await waitFor(() => expect(queryByRole('button', { name: t('denoise.compare.use') })).toBeNull())
    expect(denoiseHistory).toHaveBeenCalledWith({ chapterId: CHAPTER.id })
    expect(picker.options[1].textContent).toContain(t('denoise.compare.inUse'))
  })

  it('deletes a run only after the inline question, and Keep leaves it', async () => {
    const deleteDenoiseRun = vi.fn(async () => ({ removed: 2, missing: 0, failed: [] }))
    const denoiseHistory = vi.fn(async () => [RUNS[1]])
    setBackend(/** @type {any} */ ({ denoiseCompareImage, deleteDenoiseRun, denoiseHistory, listProjects: async () => [] }))
    const { getByRole, findByText, root } = open()
    await fireEvent.click(getByRole('button', { name: t('denoise.compare.delete') }))
    await findByText(t('denoise.compare.deleteAsk', { count: 2 }))
    await fireEvent.click(getByRole('button', { name: t('denoise.compare.keep') }))
    expect(deleteDenoiseRun).not.toHaveBeenCalled()

    await fireEvent.click(getByRole('button', { name: t('denoise.compare.delete') }))
    await fireEvent.click(getByRole('button', { name: t('denoise.compare.deleteConfirm') }))
    await waitFor(() => expect(deleteDenoiseRun).toHaveBeenCalledWith({ chapterId: CHAPTER.id, run: 200 }))
    await waitFor(() => expect(root().dataset.run).toBe('100'))
    const picker = /** @type {HTMLSelectElement} */ (getByRole('combobox', { name: t('denoise.compare.run') }))
    expect(picker.options.length).toBe(1)
  })

  it('closes when the last run is deleted', async () => {
    vi.mocked(closeModal).mockClear()
    setBackend(/** @type {any} */ ({
      denoiseCompareImage,
      deleteDenoiseRun: async () => ({ removed: 1, missing: 0, failed: [] }),
      denoiseHistory: async () => [],
      listProjects: async () => [],
    }))
    const { getByRole } = open()
    await fireEvent.click(getByRole('button', { name: t('denoise.compare.delete') }))
    await fireEvent.click(getByRole('button', { name: t('denoise.compare.deleteConfirm') }))
    await waitFor(() => expect(closeModal).toHaveBeenCalledWith(null))
  })

  it('zooms to 1:1 and 2:1 of the denoised file, both pages together, and back to fit', async () => {
    const { getByRole, getByAltText, root } = open()
    await waitFor(() => getByAltText(t('denoise.compare.denoised')))
    const denoised = () => /** @type {HTMLImageElement} */ (getByAltText(t('denoise.compare.denoised')))
    const raw = () => /** @type {HTMLElement} */ (getByAltText(t('denoise.compare.raw')))
    const stage = /** @type {HTMLElement} */ (root().querySelector('.stage'))
    stage.getBoundingClientRect = () => /** @type {DOMRect} */ ({ left: 0, top: 0, width: 400, height: 600 })
    // A 1600 x 2400 file fits the box at a quarter of its size.
    Object.defineProperty(denoised(), 'naturalWidth', { value: 1600 })
    Object.defineProperty(denoised(), 'naturalHeight', { value: 2400 })
    await fireEvent.load(denoised())
    expect(denoised().style.transform).toBe('')

    await fireEvent.click(getByRole('radio', { name: t('denoise.compare.actual') }))
    expect(denoised().style.transform).toBe('translate(0%, 0%) scale(4)')
    expect(raw().style.transform).toBe(denoised().style.transform)
    expect(/** @type {HTMLElement} */ (denoised().parentElement).style.clipPath).toBe('inset(0 50% 0 0)')
    await fireEvent.click(getByRole('radio', { name: t('denoise.compare.double') }))
    expect(denoised().style.transform).toBe('translate(0%, 0%) scale(8)')

    // A plain scroll moves the enlarged pages, and stops at their edge.
    await fireEvent.wheel(stage, { deltaY: 60 })
    expect(denoised().style.transform).toBe('translate(0%, -10%) scale(8)')
    await fireEvent.wheel(stage, { deltaY: 600000 })
    expect(denoised().style.transform).toBe('translate(0%, -350%) scale(8)')

    await fireEvent.click(getByRole('radio', { name: t('denoise.compare.fit') }))
    expect(denoised().style.transform).toBe('')
    expect(raw().style.transform).toBe('')
  })

  it('says a page could not be read when the file went after the history was read', async () => {
    denoiseCompareImage.mockImplementation(async ({ side }) => {
      if (side === 'denoised') throw new Error('denoise_file_missing: No such file')
      return new TextEncoder().encode('<svg/>')
    })
    const { findByText } = open()
    await findByText(t('denoise.compare.missing', { page: 1, folder: '/scans/new' }))
  })
})
