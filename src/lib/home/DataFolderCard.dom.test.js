/**
 * Home's Pending card: the typesetter set up a shared data folder and this
 * library is not in it yet. Move sends the setup into that folder; Later
 * hides the card for the session. Nothing is drawn in any other mode.
 */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import { dataFolder, resetDataFolder } from '../state/datafolder.svelte.js'
import DataFolderCard from './DataFolderCard.svelte'

const ROOT = '/Users/reader/Manga Library'
const base = {
  root: ROOT, library: '/Users/reader/Library/Application Support/com.mangacleaner.studio/library', reason: null,
  defaultRoot: ROOT, typesetterRunning: 'no', synced: false, oldCopies: [], recovered: null,
}
const PENDING = { ...base, mode: 'pending' }
const SHARED = { ...base, mode: 'shared', library: `${ROOT}/Cleaner` }

beforeEach(() => resetDataFolder())
afterEach(() => {
  cleanup()
  resetDataFolder()
})

it('asks to move the library into the folder the typesetter set up, and moves it', async () => {
  dataFolder.status = /** @type {any} */ (PENDING)
  const backend = { dataFolderSetup: vi.fn(async () => SHARED), dataFolderStatus: vi.fn(async () => SHARED) }
  const onmoved = vi.fn()
  render(DataFolderCard, { props: { backend: /** @type {any} */ (backend), onmoved } })
  expect(screen.getByText(t('home.dataFolder.body', { path: ROOT }))).toBeTruthy()

  await fireEvent.click(screen.getByRole('button', { name: t('home.dataFolder.move') }))
  await waitFor(() => expect(backend.dataFolderSetup).toHaveBeenCalledWith({ target: ROOT, confirmedClosed: false }))
  await waitFor(() => expect(onmoved).toHaveBeenCalled())
  expect(screen.queryByText(t('home.dataFolder.body', { path: ROOT }))).toBeNull()
})

it('goes away for the session on Later', async () => {
  dataFolder.status = /** @type {any} */ (PENDING)
  render(DataFolderCard, { props: { backend: /** @type {any} */ ({}) } })
  await fireEvent.click(screen.getByRole('button', { name: t('home.dataFolder.later') }))
  expect(screen.queryByText(t('home.dataFolder.title'))).toBeNull()
  expect(dataFolder.pendingLater).toBe(true)
})

it('draws nothing outside Pending', () => {
  for (const mode of ['legacy', 'shared', 'unavailable']) {
    dataFolder.status = /** @type {any} */ ({ ...base, mode })
    const view = render(DataFolderCard, { props: { backend: /** @type {any} */ ({}) } })
    expect(view.queryByText(t('home.dataFolder.title'))).toBeNull()
    view.unmount()
  }
})

it('is not drawn once Join was answered Not now, nor beside the start dialog', async () => {
  const { session } = await import('../state/session.svelte.js')
  const { startDialog } = await import('../state/datafolder.svelte.js')
  dataFolder.status = /** @type {any} */ (PENDING)
  session.dataFolderPromptAnswered = { join: 'declined' }
  let view = render(DataFolderCard, { props: { backend: /** @type {any} */ ({}) } })
  expect(view.queryByText(t('home.dataFolder.title'))).toBeNull()
  view.unmount()
  session.dataFolderPromptAnswered = false
  startDialog.open = true
  view = render(DataFolderCard, { props: { backend: /** @type {any} */ ({}) } })
  expect(view.queryByText(t('home.dataFolder.title'))).toBeNull()
  view.unmount()
  startDialog.open = false
})
