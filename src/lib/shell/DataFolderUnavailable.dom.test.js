/**
 * The blocking screen for an unavailable data folder: the reason the backend
 * gave, Try again (the status read again), and another folder chosen in its
 * place. The shell draws it instead of every route (`App.svelte`).
 */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import { dataFolder, resetDataFolder } from '../state/datafolder.svelte.js'
import DataFolderUnavailable from './DataFolderUnavailable.svelte'

const REASON = 'Data folder not found at /Volumes/Manga/Manga Library. Reconnect the drive, or choose another folder.'
const UNAVAILABLE = {
  mode: 'unavailable', root: '/Volumes/Manga/Manga Library', library: null, reason: REASON,
  defaultRoot: '/Users/reader/Manga Library', typesetterRunning: 'no', synced: false, oldCopies: [], recovered: null,
}
const SHARED = { ...UNAVAILABLE, mode: 'shared', reason: null, library: '/Volumes/Manga/Manga Library/Cleaner' }

beforeEach(() => {
  resetDataFolder()
  dataFolder.status = /** @type {any} */ (UNAVAILABLE)
  dataFolder.checked = true
})

afterEach(() => {
  cleanup()
  resetDataFolder()
})

const button = (/** @type {string} */ name) => /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name }))

it('says why the folder cannot be used, and Try again reads the status again', async () => {
  const backend = { dataFolderStatus: vi.fn(async () => SHARED) }
  render(DataFolderUnavailable, { props: { backend: /** @type {any} */ (backend) } })
  expect(screen.getByRole('alertdialog', { name: t('shell.dataFolder.title') })).toBeTruthy()
  expect(screen.getByText(REASON)).toBeTruthy()

  await fireEvent.click(button(t('shell.dataFolder.retry')))
  await waitFor(() => expect(backend.dataFolderStatus).toHaveBeenCalledTimes(1))
  expect(dataFolder.status?.mode).toBe('shared')
})

it('points at another data folder', async () => {
  const backend = { dataFolderUseExisting: vi.fn(async ({ target }) => ({ ...SHARED, root: target })), dataFolderStatus: vi.fn(async () => UNAVAILABLE) }
  render(DataFolderUnavailable, { props: { backend: /** @type {any} */ (backend) } })
  await fireEvent.click(button(t('shell.dataFolder.choose')))
  await fireEvent.input(screen.getByLabelText(t('shell.dataFolder.pathLabel')), { target: { value: '/Users/reader/Manga Library' } })
  await fireEvent.click(button(t('settings.dataFolder.useConfirm')))
  await waitFor(() => expect(backend.dataFolderUseExisting).toHaveBeenCalledWith({ target: '/Users/reader/Manga Library', confirmedClosed: false }))
  expect(dataFolder.status?.mode).toBe('shared')
})

it('shows a refusal and stays', async () => {
  const backend = {
    dataFolderUseExisting: vi.fn(async () => { throw new Error('/tmp is not a data folder (it has no manga-library.json).') }),
    dataFolderStatus: vi.fn(async () => UNAVAILABLE),
  }
  render(DataFolderUnavailable, { props: { backend: /** @type {any} */ (backend) } })
  await fireEvent.click(button(t('shell.dataFolder.choose')))
  await fireEvent.input(screen.getByLabelText(t('shell.dataFolder.pathLabel')), { target: { value: '/tmp' } })
  await fireEvent.click(button(t('settings.dataFolder.useConfirm')))
  await waitFor(() => expect(screen.getByText(t('settings.dataFolder.failed', { detail: '/tmp is not a data folder (it has no manga-library.json).' }))).toBeTruthy())
  expect(dataFolder.status?.mode).toBe('unavailable')
})

it('takes focus on Try again when it comes up', () => {
  render(DataFolderUnavailable, { props: { backend: /** @type {any} */ ({}) } })
  expect(document.activeElement).toBe(button(t('shell.dataFolder.retry')))
})

it('says why an unfinished move holds the folder, and Try again asks again', async () => {
  const reason = 'Close the typesetter to finish the move.'
  dataFolder.status = /** @type {any} */ ({ ...UNAVAILABLE, reason })
  const backend = { dataFolderStatus: vi.fn().mockResolvedValueOnce({ ...UNAVAILABLE, reason }).mockResolvedValueOnce(SHARED) }
  render(DataFolderUnavailable, { props: { backend: /** @type {any} */ (backend) } })
  expect(screen.getByText(reason)).toBeTruthy()
  await fireEvent.click(button(t('shell.dataFolder.retry')))
  await waitFor(() => expect(backend.dataFolderStatus).toHaveBeenCalledTimes(1))
  expect(dataFolder.status?.mode).toBe('unavailable')
  await waitFor(() => expect(button(t('shell.dataFolder.retry')).disabled).toBe(false))
  await fireEvent.click(button(t('shell.dataFolder.retry')))
  await waitFor(() => expect(dataFolder.status?.mode).toBe('shared'))
})

it('asks for the typesetter to be closed when the drive cannot tell, then uses the folder', async () => {
  const backend = {
    dataFolderUseExisting: vi.fn()
      .mockRejectedValueOnce(new Error('typesetter_unknown: This drive cannot tell whether the typesetter is open.'))
      .mockImplementationOnce(async ({ target }) => ({ ...SHARED, root: target })),
    dataFolderStatus: vi.fn(async () => UNAVAILABLE),
  }
  render(DataFolderUnavailable, { props: { backend: /** @type {any} */ (backend) } })
  await fireEvent.click(button(t('shell.dataFolder.choose')))
  await fireEvent.input(screen.getByLabelText(t('shell.dataFolder.pathLabel')), { target: { value: '/Volumes/Other/Manga Library' } })
  await fireEvent.click(button(t('settings.dataFolder.useConfirm')))
  await waitFor(() => expect(screen.getByText(t('settings.dataFolder.confirmClosed.body'))).toBeTruthy())
  await fireEvent.click(button(t('settings.dataFolder.confirmClosed.action')))
  await waitFor(() => expect(backend.dataFolderUseExisting).toHaveBeenLastCalledWith({ target: '/Volumes/Other/Manga Library', confirmedClosed: true }))
  expect(dataFolder.status?.mode).toBe('shared')
})

it('names an unfinished move as such, and checks again by itself every three seconds', async () => {
  vi.useFakeTimers()
  try {
    dataFolder.status = /** @type {any} */ ({ ...UNAVAILABLE, journal: true, reason: 'The last move of the data folder did not finish.' })
    const backend = { dataFolderStatus: vi.fn(async () => SHARED) }
    render(DataFolderUnavailable, { props: { backend: /** @type {any} */ (backend) } })
    expect(screen.getByRole('alertdialog', { name: t('shell.dataFolder.titleMoveUnfinished') })).toBeTruthy()
    expect(screen.getByText(t('shell.dataFolder.bodyMoveUnfinished'))).toBeTruthy()
    expect(screen.getByText(t('shell.dataFolder.checking'))).toBeTruthy()
    await vi.advanceTimersByTimeAsync(2999)
    expect(backend.dataFolderStatus).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(1)
    expect(backend.dataFolderStatus).toHaveBeenCalledTimes(1)
    expect(dataFolder.status?.mode).toBe('shared')
  } finally {
    vi.useRealTimers()
  }
})
