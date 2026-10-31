import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import DevLogSection from './DevLogSection.svelte'

const button = (/** @type {string} */ key) => /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t(key) }))
const status = () => screen.getByRole('status').textContent?.trim()

/** @type {{ readDevLog: ReturnType<typeof vi.fn>, clearDevLog: ReturnType<typeof vi.fn> }} */
let backend
/** @type {ReturnType<typeof vi.fn>} */
let writeText

beforeEach(() => {
  backend = {
    readDevLog: vi.fn(async () => 'first line\nsecond line\n'),
    clearDevLog: vi.fn(async () => {}),
  }
  setBackend(/** @type {any} */ (backend))
  writeText = vi.fn(async () => {})
  vi.stubGlobal('navigator', { ...globalThis.navigator, clipboard: { writeText } })
})

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('DevLogSection', () => {
  it('reads nothing until the section is shown', async () => {
    const { rerender } = render(DevLogSection, { visible: false })
    await new Promise((resolve) => setTimeout(resolve))
    expect(backend.readDevLog).not.toHaveBeenCalled()
    await rerender({ visible: true })
    await screen.findByText(/second line/)
    expect(backend.readDevLog).toHaveBeenCalledTimes(1)
  })

  it('reads again on Refresh and on coming back to the section', async () => {
    const { rerender } = render(DevLogSection, { visible: true })
    await screen.findByText(/first line/)

    backend.readDevLog.mockResolvedValue('a newer line\n')
    await fireEvent.click(button('settings.devlog.refresh'))
    await screen.findByText(/a newer line/)

    backend.readDevLog.mockResolvedValue('newest of all\n')
    await rerender({ visible: false })
    await rerender({ visible: true })
    await screen.findByText(/newest of all/)
  })

  it('copies the text as it is and says so', async () => {
    render(DevLogSection, { visible: true })
    await screen.findByText(/first line/)
    await fireEvent.click(button('settings.devlog.copy'))
    await waitFor(() => expect(status()).toBe(t('settings.devlog.copied')))
    expect(writeText).toHaveBeenCalledWith('first line\nsecond line\n')
  })

  it('says how to copy by hand when the clipboard refuses', async () => {
    writeText.mockRejectedValue(new Error('denied'))
    render(DevLogSection, { visible: true })
    await screen.findByText(/first line/)
    await fireEvent.click(button('settings.devlog.copy'))
    await waitFor(() => expect(status()).toBe(t('settings.devlog.copyFailed')))
  })

  it('clears, then has nothing to copy or clear', async () => {
    render(DevLogSection, { visible: true })
    await screen.findByText(/first line/)
    await fireEvent.click(button('settings.devlog.clear'))
    await screen.findByText(t('settings.devlog.empty'))
    expect(backend.clearDevLog).toHaveBeenCalledTimes(1)
    expect(button('settings.devlog.copy').disabled).toBe(true)
    expect(button('settings.devlog.clear').disabled).toBe(true)
  })

  it('keeps the lines when a clear fails', async () => {
    backend.clearDevLog.mockRejectedValue(new Error('locked'))
    render(DevLogSection, { visible: true })
    await screen.findByText(/first line/)
    await fireEvent.click(button('settings.devlog.clear'))
    await waitFor(() => expect(status()).toBe(t('settings.devlog.clearFailed')))
    expect(screen.getByText(/first line/)).toBeTruthy()
  })

  it('says a failed read failed, not that the log is empty', async () => {
    backend.readDevLog.mockRejectedValue(new Error('gone'))
    render(DevLogSection, { visible: true })
    await waitFor(() => expect(status()).toBe(t('settings.devlog.loadFailed')))
    expect(screen.queryByText(t('settings.devlog.empty'))).toBeNull()
  })

  it('does not let an older read overwrite a newer one', async () => {
    /** @type {(text: string) => void} */
    let finishOld = () => {}
    backend.readDevLog.mockImplementationOnce(() => new Promise((resolve) => { finishOld = resolve }))
    render(DevLogSection, { visible: true })
    await waitFor(() => expect(backend.readDevLog).toHaveBeenCalledTimes(1))
    await fireEvent.click(button('settings.devlog.refresh'))
    await screen.findByText(/second line/)
    finishOld('stale\n')
    await new Promise((resolve) => setTimeout(resolve))
    expect(screen.queryByText(/stale/)).toBeNull()
  })
})
