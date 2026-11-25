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
