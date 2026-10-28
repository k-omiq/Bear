/**
 * Settings > Post-processing against the mock backend: the method, the grey
 * switch and the every-model switch, each saved at once and put back when the
 * save fails.
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { t } from '../i18n/index.js'
import { app, clearNotices } from '../state/app.svelte.js'
import { session, setCloudFinishing } from '../state/session.svelte.js'
import PostProcessingSettings from './PostProcessingSettings.svelte'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }

/** Local storage for the test: the node running the suite has none. */
function memoryStorage() {
  /** @type {Map<string, string>} */
  const values = new Map()
  return {
    get length() {
      return values.size
    },
    key: (/** @type {number} */ index) => [...values.keys()][index] ?? null,
    getItem: (/** @type {string} */ key) => values.get(key) ?? null,
    setItem: (/** @type {string} */ key, /** @type {string} */ value) => void values.set(key, String(value)),
    removeItem: (/** @type {string} */ key) => void values.delete(key),
    clear: () => values.clear(),
  }
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
})

afterEach(() => {
  cleanup()
  clearNotices()
  setBackend(null)
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

/** A mock backend. Post-processing needs no cloud setup: LaMa runs here. */
async function seeded() {
  return createMockBackend({ timing: ZERO })
}

/** @param {import('../api/backend.js').Backend} backend */
function show(backend) {
  return render(PostProcessingSettings, { props: { backend } })
}

describe('the post-processing settings', () => {
  beforeEach(() => setCloudFinishing('copy', false, false))

  it('saves the version and the grey switch, and says what the version does', async () => {
    const backend = await seeded()
    const write = vi.spyOn(backend, 'writeSettings')
    show(backend)
    const pick = /** @type {HTMLSelectElement} */ (await screen.findByLabelText(t('settings.postprocessing.method')))
    expect(pick.value).toBe('copy')
    expect(pick.options).toHaveLength(12)
    await fireEvent.change(pick, { target: { value: 'dots-only' } })
    await waitFor(() => expect(session.cloudFinishing).toBe('dots-only'))
    await waitFor(() => expect(write).toHaveBeenLastCalledWith(expect.objectContaining({ cloudFinishing: 'dots-only', cloudFinishingGrayscale: false })))
    expect(screen.getByText(t('settings.postprocessing.hint.dotsOnly'))).toBeTruthy()

    await fireEvent.click(screen.getByLabelText(t('settings.postprocessing.grayscale')))
    await waitFor(() => expect(session.cloudFinishingGrayscale).toBe(true))
    await waitFor(() => expect(write).toHaveBeenLastCalledWith(expect.objectContaining({ cloudFinishing: 'dots-only', cloudFinishingGrayscale: true })))

    const every = /** @type {HTMLInputElement} */ (screen.getByLabelText(t('settings.postprocessing.everyModel')))
    expect(every.checked).toBe(false)
    await fireEvent.click(every)
    await waitFor(() => expect(session.finishingEveryModel).toBe(true))
    await waitFor(() => expect(write).toHaveBeenLastCalledWith(expect.objectContaining({
      cloudFinishing: 'dots-only', cloudFinishingGrayscale: true, finishingEveryModel: true })))
  })

  it('puts the version back when it cannot be saved', async () => {
    const backend = await seeded()
    vi.spyOn(backend, 'writeSettings').mockRejectedValueOnce(new Error('disk full'))
    show(backend)
    const pick = /** @type {HTMLSelectElement} */ (await screen.findByLabelText(t('settings.postprocessing.method')))
    await fireEvent.change(pick, { target: { value: 'tone-only' } })
    await waitFor(() => expect(app.notices.some((notice) => notice.key === 'notice.cloud.finishingFailed')).toBe(true))
    expect(session.cloudFinishing).toBe('copy')
  })

  it('unticks the grey and every-model switches when they cannot be saved', async () => {
    const backend = await seeded()
    vi.spyOn(backend, 'writeSettings').mockRejectedValue(new Error('disk full'))
    show(backend)
    for (const key of ['settings.postprocessing.grayscale', 'settings.postprocessing.everyModel']) {
      clearNotices()
      const box = /** @type {HTMLInputElement} */ (await screen.findByLabelText(t(key)))
      await fireEvent.click(box)
      await waitFor(() => expect(app.notices.some((notice) => notice.key === 'notice.cloud.finishingFailed')).toBe(true))
      await waitFor(() => expect(box.checked).toBe(false))
    }
    expect([session.cloudFinishing, session.cloudFinishingGrayscale, session.finishingEveryModel]).toEqual(['copy', false, false])
  })

  it('keeps a second switch clicked while the first one was failing to save', async () => {
    const backend = await seeded()
    /** @type {(error: Error) => void} */
    let fail = () => {}
    const write = vi.spyOn(backend, 'writeSettings')
      .mockImplementationOnce(() => new Promise((_, reject) => { fail = reject }))
    show(backend)
    const grey = /** @type {HTMLInputElement} */ (await screen.findByLabelText(t('settings.postprocessing.grayscale')))
    const every = /** @type {HTMLInputElement} */ (screen.getByLabelText(t('settings.postprocessing.everyModel')))
    await fireEvent.click(grey)
    await fireEvent.click(every)
    fail(new Error('disk full'))
    await waitFor(() => expect(write).toHaveBeenCalledTimes(2))
    await waitFor(() => expect(grey.checked).toBe(false))
    expect(every.checked).toBe(true)
    expect([session.cloudFinishingGrayscale, session.finishingEveryModel]).toEqual([false, true])
    expect(write).toHaveBeenLastCalledWith(expect.objectContaining({ cloudFinishingGrayscale: false, finishingEveryModel: true }))
  })

  it('shows what was last saved when two clicks both fail to save', async () => {
    const backend = await seeded()
    /** @type {(error: Error) => void} */
    let fail = () => {}
    vi.spyOn(backend, 'writeSettings')
      .mockImplementationOnce(() => new Promise((_, reject) => { fail = reject }))
      .mockRejectedValueOnce(new Error('disk full'))
    show(backend)
    const grey = /** @type {HTMLInputElement} */ (await screen.findByLabelText(t('settings.postprocessing.grayscale')))
    await fireEvent.click(grey)
    await fireEvent.click(grey)
    fail(new Error('disk full'))
    await waitFor(() => expect(app.notices.filter((notice) => notice.key === 'notice.cloud.finishingFailed').length).toBeGreaterThan(0))
    await waitFor(() => expect(session.cloudFinishingGrayscale).toBe(false))
    expect(grey.checked).toBe(false)
  })
})
