/**
 * The compare view: a run's denoised page over the raw page, stepped a page at
 * a time, with the run picker switching runs, and a page whose file is gone
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
    expect([...picker.options].map((option) => option.textContent?.trim())).toEqual(RUNS.map(runLabel))
    await fireEvent.change(picker, { target: { value: '100' } })
    await waitFor(() => expect(root().dataset.run).toBe('100'))
    expect(root().dataset.page).toBe('1')
    await waitFor(() => expect(denoiseCompareImage).toHaveBeenCalledWith(expect.objectContaining({ pageIndex: 1, run: 100 })))
  })

  it('fills the window on its toggle, and Escape leaves full window before it closes', async () => {
    vi.mocked(closeModal).mockClear()
    const { getByRole, getByAltText } = open()
    await waitFor(() => getByAltText(t('denoise.compare.denoised')))
    const dialog = () => getByRole('dialog')
    const toggle = getByRole('button', { name: t('denoise.compare.fullscreen') })
    expect(toggle.getAttribute('aria-pressed')).toBe('false')
    expect(dialog().classList.contains('fill')).toBe(false)

    await fireEvent.click(toggle)
    expect(toggle.getAttribute('aria-pressed')).toBe('true')
    expect(dialog().classList.contains('fill')).toBe(true)
    expect(dialog().style.width, 'the fixed width is dropped').toBe('')

    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(dialog().classList.contains('fill')).toBe(false)
    expect(closeModal).not.toHaveBeenCalled()
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(closeModal).toHaveBeenCalledWith(null)
  })

  it('zooms both pages together under a wipe that stays put, and goes back to fit', async () => {
    const { getByRole, getByAltText, root } = open()
    await waitFor(() => getByAltText(t('denoise.compare.denoised')))
    const denoised = () => /** @type {HTMLElement} */ (getByAltText(t('denoise.compare.denoised')))
    const raw = () => /** @type {HTMLElement} */ (getByAltText(t('denoise.compare.raw')))
    const zoomOut = /** @type {HTMLButtonElement} */ (getByRole('button', { name: t('editor.action.zoomOut') }))
    expect(denoised().style.transform).toBe('')
    expect(zoomOut.disabled, 'fitted pages have nothing to zoom out of').toBe(true)

    await fireEvent.click(getByRole('button', { name: t('editor.action.zoomIn') }))
    expect(denoised().style.transform).toBe('translate(0%, 0%) scale(1.5)')
    expect(raw().style.transform).toBe(denoised().style.transform)
    expect(denoised().style.clipPath, 'the wipe is not on the zoomed image').toBe('')
    expect(/** @type {HTMLElement} */ (denoised().parentElement).style.clipPath).toBe('inset(0 50% 0 0)')

    // A plain scroll moves the enlarged pages, and stops at their edge.
    const stage = /** @type {HTMLElement} */ (root().querySelector('.stage'))
    stage.getBoundingClientRect = () => /** @type {DOMRect} */ ({ left: 0, top: 0, width: 400, height: 600 })
    await fireEvent.wheel(stage, { deltaY: 60 })
    expect(denoised().style.transform).toBe('translate(0%, -10%) scale(1.5)')
    await fireEvent.wheel(stage, { deltaY: 6000 })
    expect(denoised().style.transform).toBe('translate(0%, -25%) scale(1.5)')

    await fireEvent.click(getByRole('button', { name: t('editor.action.zoomFit') }))
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
