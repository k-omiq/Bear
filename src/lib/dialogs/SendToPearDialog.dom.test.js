/**
 * Send to Pear's title list (`docs/shared-data-folder.md` section 8.4.1),
 * mounted: the default checks, "12 chapters, 9 cleaned", "Nothing cleaned
 * yet", "Sent before", "Send all again", and the background send with its
 * progress and Stop (`shell/SendToPearStatus.svelte`).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import { app, closeAllModals } from '../state/app.svelte.js'
import { SEND_POLL_MS, openSendToPear, resetSendToPear, sendToPear } from '../state/sendtopear.svelte.js'
import SendToPearDialog from './SendToPearDialog.svelte'
import SendToPearStatus from '../shell/SendToPearStatus.svelte'

const TITLES = [
  { projectId: 'p1', name: 'Dungeon streamer', chapters: 12, cleaned: 9, sent: 0, pending: 9 },
  { projectId: 'p2', name: 'Surviving the Apocalypse', chapters: 5, cleaned: 3, sent: 3, pending: 1 },
  { projectId: 'p3', name: 'Raw only', chapters: 4, cleaned: 0, sent: 0, pending: 0 },
]

const idle = { running: false, done: 0, total: 0, sent: 0, failed: 0, stopped: false }

const stub = () => /** @type {any} */ ({
  sendToPearTitles: vi.fn(async () => structuredClone(TITLES)),
  sendToPearStart: vi.fn(async () => ({ ...idle, running: true, total: 10 })),
  sendToPearProgress: vi.fn(async () => ({ ...idle, running: true, done: 4, total: 10 })),
  sendToPearStop: vi.fn(async () => ({ ...idle, running: true, done: 4, total: 10, stopped: true })),
})

/** @param {any} backend */
async function mount(backend) {
  openSendToPear()
  const spec = app.modals.at(-1)
  const rendered = render(SendToPearDialog, { props: { spec, backend } })
  await waitFor(() => expect(sendToPear.titles).not.toBeNull())
  return rendered
}

const box = (/** @type {string} */ name) => /** @type {HTMLInputElement} */ (screen.getByRole('checkbox', { name: new RegExp(name) }))
const button = (/** @type {string} */ name) => /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name }))

beforeEach(() => resetSendToPear())
afterEach(() => {
  cleanup()
  closeAllModals()
  resetSendToPear()
  vi.useRealTimers()
})

describe('the title list', () => {
  it('checks every title with a cleaned chapter, and says what each holds', async () => {
    await mount(stub())
    expect(box('Dungeon streamer').checked).toBe(true)
    expect(box('Surviving the Apocalypse').checked).toBe(true)
    expect(box('Raw only').checked).toBe(false)
    expect(box('Raw only').disabled).toBe(true)
    expect(screen.getByText(t('sendToPear.counts', { chapters: 12, cleaned: 9 }))).toBeTruthy()
    expect(screen.getByText(t('sendToPear.nothingCleaned'))).toBeTruthy()
    expect(screen.getAllByText(t('sendToPear.sentBefore'))).toHaveLength(1)
    expect(button(t('sendToPear.send', { count: 2 })).disabled).toBe(false)
  })

  it('sends only what changed, unless Send all again is on', async () => {
    const backend = stub()
    await mount(backend)
    await fireEvent.click(box('Dungeon streamer'))
    // Only the title sent before is left, with one changed chapter.
    expect(button(t('sendToPear.send', { count: 1 })).disabled).toBe(false)
    await fireEvent.click(box(t('sendToPear.again')))
    await fireEvent.click(button(t('sendToPear.send', { count: 1 })))
    expect(backend.sendToPearStart).toHaveBeenCalledWith({ projectIds: ['p2'], again: true })
  })

  it('says there is nothing new when every chosen chapter was sent and did not change', async () => {
    const backend = stub()
    backend.sendToPearTitles.mockResolvedValue([{ ...TITLES[1], pending: 0 }])
    await mount(backend)
    expect(screen.getByText(t('sendToPear.nothingNew'))).toBeTruthy()
    expect(button(t('sendToPear.send', { count: 1 })).disabled).toBe(true)
  })

  it('starts the send in the background, closes, and the shell shows it with Stop', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const backend = stub()
    const dialog = await mount(backend)
    await fireEvent.click(button(t('sendToPear.send', { count: 2 })))
    await waitFor(() => expect(app.modals.find((modal) => modal.kind === 'sendToPear')).toBeUndefined())
    // The host would unmount it now; the pill outlives it.
    dialog.unmount()
    render(SendToPearStatus)
    expect(backend.sendToPearStart).toHaveBeenCalledWith({ projectIds: ['p1', 'p2'], again: false })
    await vi.advanceTimersByTimeAsync(SEND_POLL_MS)
    await waitFor(() => expect(screen.getByText(t('sendToPear.sending', { done: 4, total: 10 }))).toBeTruthy())
    await fireEvent.click(button(t('sendToPear.stop')))
    expect(backend.sendToPearStop).toHaveBeenCalled()
    backend.sendToPearProgress.mockResolvedValue({ ...idle, done: 4, total: 10, sent: 4, stopped: true })
    await vi.advanceTimersByTimeAsync(SEND_POLL_MS)
    await waitFor(() => expect(screen.queryByText(t('sendToPear.sending', { done: 4, total: 10 }))).toBeNull())
  })

  it('says why when the list cannot be read', async () => {
    const backend = stub()
    backend.sendToPearTitles.mockRejectedValue(new Error('The data folder cannot be found.'))
    openSendToPear()
    render(SendToPearDialog, { props: { spec: app.modals.at(-1), backend } })
    await waitFor(() => expect(screen.getByText(t('sendToPear.failed', { detail: 'The data folder cannot be found.' }))).toBeTruthy())
  })
})
