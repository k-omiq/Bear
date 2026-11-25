/**
 * Home's first-run card for Legacy mode: no app has set up the shared data
 * folder, and this app asks once. Move sets it up at the default place; Choose
 * folder opens Settings > Data folder; Not now records the answer in this
 * app's own session store, so the card does not come back. Nothing is drawn
 * in any other mode, nor before the status has answered.
 */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import { app } from '../state/app.svelte.js'
import { dataFolder, resetDataFolder } from '../state/datafolder.svelte.js'
import { jobs, resetJobs } from '../state/jobs.svelte.js'
import { session } from '../state/session.svelte.js'
import { STORAGE_PREFIX } from '../state/persist.js'
import DataFolderPrompt from './DataFolderPrompt.svelte'
import DataFolderSettings from '../dialogs/DataFolderSettings.svelte'

const DEFAULT_ROOT = '/Users/reader/Manga Library'
const SESSION_KEY = `${STORAGE_PREFIX}session.v1`
const base = {
  root: null, library: '/Users/reader/Library/Application Support/com.mangacleaner.studio/library', reason: null,
  defaultRoot: DEFAULT_ROOT, typesetterRunning: 'no', synced: false, oldCopies: [], recovered: null,
}
const LEGACY = { ...base, mode: 'legacy' }
const SHARED = { ...base, mode: 'shared', root: DEFAULT_ROOT, library: `${DEFAULT_ROOT}/Cleaner` }

const title = () => screen.queryByText(t('home.dataFolderPrompt.title'))
const button = (/** @type {string} */ key) => /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t(key) }))

/** This jsdom has no `localStorage`; the session store writes into one of these. */
function memoryStorage() {
  const map = new Map()
  return {
    getItem: (/** @type {string} */ k) => (map.has(k) ? map.get(k) : null),
    setItem: (/** @type {string} */ k, /** @type {string} */ v) => void map.set(k, String(v)),
    removeItem: (/** @type {string} */ k) => void map.delete(k),
    clear: () => map.clear(),
  }
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
  resetDataFolder()
  session.dataFolderPromptAnswered = false
})

afterEach(() => {
  cleanup()
  resetDataFolder()
  resetJobs()
  session.dataFolderPromptAnswered = false
  app.modals.length = 0
  vi.unstubAllGlobals()
})

it('asks once in Legacy, naming the default place', () => {
  dataFolder.status = /** @type {any} */ (LEGACY)
  render(DataFolderPrompt, { props: { backend: /** @type {any} */ ({}) } })
  expect(title()).toBeTruthy()
  expect(screen.getByText(t('home.dataFolderPrompt.body', { path: DEFAULT_ROOT }))).toBeTruthy()
  expect(button('home.dataFolderPrompt.move').disabled).toBe(false)
})

it('draws nothing while the status is loading, without a default place, or outside Legacy', () => {
  const view = render(DataFolderPrompt, { props: { backend: /** @type {any} */ ({}) } })
  expect(view.queryByText(t('home.dataFolderPrompt.title'))).toBeNull()
  view.unmount()

  dataFolder.status = /** @type {any} */ ({ ...LEGACY, defaultRoot: '' })
  const empty = render(DataFolderPrompt, { props: { backend: /** @type {any} */ ({}) } })
  expect(empty.queryByText(t('home.dataFolderPrompt.title'))).toBeNull()
  empty.unmount()

  for (const mode of ['pending', 'shared', 'unavailable']) {
    dataFolder.status = /** @type {any} */ ({ ...base, root: DEFAULT_ROOT, mode })
    const other = render(DataFolderPrompt, { props: { backend: /** @type {any} */ ({}) } })
    expect(other.queryByText(t('home.dataFolderPrompt.title'))).toBeNull()
    other.unmount()
  }
})

it('moves the library to the default place, and records the answer', async () => {
  dataFolder.status = /** @type {any} */ (LEGACY)
  const backend = { dataFolderSetup: vi.fn(async () => SHARED), dataFolderStatus: vi.fn(async () => SHARED) }
  const onmoved = vi.fn()
  render(DataFolderPrompt, { props: { backend: /** @type {any} */ (backend), onmoved } })

  await fireEvent.click(button('home.dataFolderPrompt.move'))
  await waitFor(() => expect(backend.dataFolderSetup).toHaveBeenCalledWith({ target: DEFAULT_ROOT, confirmedClosed: false }))
  await waitFor(() => expect(onmoved).toHaveBeenCalled())
  expect(session.dataFolderPromptAnswered).toBe(true)
  expect(title()).toBeNull()
})

it('asks for the typesetter to be closed when the drive cannot tell, then sends the setup again', async () => {
  dataFolder.status = /** @type {any} */ (LEGACY)
  const backend = {
    dataFolderSetup: vi.fn()
      .mockRejectedValueOnce(new Error('typesetter_unknown: cannot tell'))
      .mockResolvedValueOnce(SHARED),
    dataFolderStatus: vi.fn(async () => LEGACY),
  }
  render(DataFolderPrompt, { props: { backend: /** @type {any} */ (backend) } })

  await fireEvent.click(button('home.dataFolderPrompt.move'))
  await waitFor(() => expect(screen.getByText(t('settings.dataFolder.confirmClosed.body'))).toBeTruthy())
  expect(session.dataFolderPromptAnswered).toBe(false)

  await fireEvent.click(button('settings.dataFolder.confirmClosed.action'))
  await waitFor(() => expect(backend.dataFolderSetup).toHaveBeenLastCalledWith({ target: DEFAULT_ROOT, confirmedClosed: true }))
  await waitFor(() => expect(session.dataFolderPromptAnswered).toBe(true))
})

it('keeps asking when the setup fails', async () => {
  dataFolder.status = /** @type {any} */ (LEGACY)
  const backend = {
    dataFolderSetup: vi.fn(async () => { throw new Error('no space') }),
    dataFolderStatus: vi.fn(async () => LEGACY),
  }
  render(DataFolderPrompt, { props: { backend: /** @type {any} */ (backend) } })
  await fireEvent.click(button('home.dataFolderPrompt.move'))
  await waitFor(() => expect(screen.getByText(t('settings.dataFolder.failed', { detail: 'no space' }))).toBeTruthy())
  expect(session.dataFolderPromptAnswered).toBe(false)
  expect(title()).toBeTruthy()
})

it('opens Settings at Data folder on Choose folder, and keeps asking', async () => {
  dataFolder.status = /** @type {any} */ (LEGACY)
  render(DataFolderPrompt, { props: { backend: /** @type {any} */ ({}) } })
  await fireEvent.click(button('home.dataFolderPrompt.choose'))
  expect(app.modals.at(-1)).toMatchObject({ kind: 'settings', props: { tab: 'dataFolder' } })
  expect(session.dataFolderPromptAnswered).toBe(false)
  expect(title()).toBeTruthy()
})

it('holds Move while jobs run, and says why', () => {
  jobs.list.push(/** @type {any} */ ({ runId: 'run-1', kind: 'clean', chapterId: 'c1', status: 'running', done: 0, total: 3 }))
  dataFolder.status = /** @type {any} */ (LEGACY)
  render(DataFolderPrompt, { props: { backend: /** @type {any} */ ({}) } })
  expect(screen.getByText(t('settings.dataFolder.blocked.jobsRunning'))).toBeTruthy()
  expect(button('home.dataFolderPrompt.move').disabled).toBe(true)
  expect(button('home.dataFolderPrompt.notNow').disabled).toBe(false)
})

it('goes away for good on Not now: the answer is stored and read back after a reload', async () => {
  dataFolder.status = /** @type {any} */ (LEGACY)
  const backend = { dataFolderSetup: vi.fn() }
  render(DataFolderPrompt, { props: { backend: /** @type {any} */ (backend) } })
  await fireEvent.click(button('home.dataFolderPrompt.notNow'))
  expect(title()).toBeNull()
  expect(backend.dataFolderSetup).not.toHaveBeenCalled()
  expect(JSON.parse(localStorage.getItem(SESSION_KEY) ?? '{}').dataFolderPromptAnswered).toBe(true)

  // A fresh start reads the answer back from this app's own store.
  vi.resetModules()
  const fresh = await import('../state/session.svelte.js')
  expect(fresh.session.dataFolderPromptAnswered).toBe(true)

  // And the data folder state read again in Legacy still draws no card.
  cleanup()
  resetDataFolder()
  dataFolder.status = /** @type {any} */ (LEGACY)
  render(DataFolderPrompt, { props: { backend: /** @type {any} */ ({}) } })
  expect(title()).toBeNull()
})

it('leaves Setup in Settings > Data folder after Not now', async () => {
  dataFolder.status = /** @type {any} */ (LEGACY)
  render(DataFolderPrompt, { props: { backend: /** @type {any} */ ({}) } })
  await fireEvent.click(button('home.dataFolderPrompt.notNow'))
  cleanup()

  const backend = {
    dataFolderStatus: vi.fn(async () => LEGACY),
    readSettings: vi.fn(async () => ({})),
    writeSettings: vi.fn(async () => ({})),
  }
  render(DataFolderSettings, { props: { backend: /** @type {any} */ (backend) } })
  await waitFor(() => expect(screen.getByText(t('settings.dataFolder.mode.legacy'))).toBeTruthy())
  expect(button('settings.dataFolder.setup').disabled).toBe(false)
})
