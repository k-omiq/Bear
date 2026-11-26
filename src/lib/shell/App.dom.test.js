/**
 * The shell's data folder gating, mounted over the mock: an unavailable
 * folder replaces every route with its screen and makes the rest inert, Home
 * waits for the first status so it never lists a library that is not there,
 * and a change of place (Home's Pending move, or a move elsewhere) holds the
 * window under the busy cover until it ends, then lists the library again.
 */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { t } from '../i18n/index.js'
import { dataFolder, resetDataFolder, startDialog, startFlow } from '../state/datafolder.svelte.js'
import { dismissFirstLaunch, firstLaunch, resetFirstLaunch } from '../dialogs/firstlaunch.svelte.js'
import { session } from '../state/session.svelte.js'
import { app } from '../state/app.svelte.js'
import App from '../../App.svelte'

const TIMING = { method: 0, openChapter: 0, export: 0, cloud: 0, analysis: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }

/** @param {Object} [options] */
function mockBackend(options = {}) {
  const backend = createMockBackend({ timing: TIMING, ...options })
  vi.spyOn(backend, 'listProjects')
  setBackend(backend)
  return backend
}

/** @param {HTMLElement} container */
const layer = (container) => /** @type {HTMLElement} */ (container.querySelector('.layer'))

const offered = session.firstLaunchOffered

beforeEach(() => {
  // jsdom has no layout: `bind:clientHeight` asks for one.
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} })
  resetDataFolder()
  // The first-launch offer is a dialog of its own; it is not what this file is about.
  session.firstLaunchOffered = true
  session.dataFolderPromptAnswered = false
  session.quietSetupNotice = null
  app.notices.length = 0
})

afterEach(() => {
  cleanup()
  resetDataFolder()
  setBackend(null)
  session.firstLaunchOffered = offered
  session.dataFolderPromptAnswered = false
  vi.unstubAllGlobals()
})

it('replaces every route with the unavailable screen, and lists no projects', async () => {
  const backend = mockBackend({ dataFolder: 'unavailable' })
  const { container } = render(App)
  await waitFor(() => expect(screen.getByRole('alertdialog', { name: t('shell.dataFolder.title') })).toBeTruthy())
  expect(container.querySelector('.home')).toBeNull()
  expect(layer(container).inert).toBe(true)
  expect(backend.listProjects).not.toHaveBeenCalled()
})

it('waits for the first status before Home lists the library', async () => {
  const backend = mockBackend()
  /** @type {(value: any) => void} */
  let answer = () => {}
  const real = backend.dataFolderStatus.bind(backend)
  backend.dataFolderStatus = () => new Promise((resolve) => (answer = resolve))
  const { container } = render(App)
  await new Promise((resolve) => setTimeout(resolve, 20))
  expect(container.querySelector('.home')).toBeNull()
  expect(backend.listProjects).not.toHaveBeenCalled()

  answer(await real())
  await waitFor(() => expect(container.querySelector('.home')).not.toBeNull())
  await waitFor(() => expect(backend.listProjects).toHaveBeenCalled())
  expect(layer(container).inert).toBe(false)
})

it('holds Home under the busy cover while the Pending move runs, then lists the library again', async () => {
  const backend = mockBackend({ dataFolder: 'pending' })
  /** @type {() => void} */
  let finish = () => {}
  const real = backend.dataFolderSetup.bind(backend)
  backend.dataFolderSetup = (spec) => new Promise((resolve, reject) => {
    finish = () => real(spec).then(resolve, reject)
  })
  // The Home card, not the start dialog: the start table has run already.
  startFlow.ran = true
  const { container } = render(App)
  const move = await screen.findByRole('button', { name: t('home.dataFolder.move') })
  await waitFor(() => expect(backend.listProjects).toHaveBeenCalledTimes(1))

  await fireEvent.click(move)
  await waitFor(() => expect(screen.getByText(t('settings.dataFolder.busy'), { selector: '#data-folder-busy' })).toBeTruthy())
  expect(layer(container).inert).toBe(true)

  finish()
  await waitFor(() => expect(container.querySelector('#data-folder-busy')).toBeNull())
  expect(layer(container).inert).toBe(false)
  expect(dataFolder.status?.mode).toBe('shared')
  await waitFor(() => expect(backend.listProjects).toHaveBeenCalledTimes(2))
})

it('shows a move elsewhere as the busy cover, not the unavailable screen', async () => {
  const backend = mockBackend({ dataFolder: 'shared' })
  backend.listProjects.mockRejectedValueOnce(new Error('library: data_folder_moving: the typesetter is moving the data folder'))
  const { container } = render(App)
  await waitFor(() => expect(screen.getByText(t('shell.dataFolder.moving'))).toBeTruthy())
  expect(screen.queryByRole('alertdialog', { name: t('shell.dataFolder.title') })).toBeNull()
  expect(layer(container).inert).toBe(true)
  expect(dataFolder.moving).toBe(true)
})


it('sets the folder up quietly when Pear never ran here, and says where with Show', async () => {
  const backend = mockBackend()
  const show = vi.spyOn(backend, 'dataFolderShow')
  render(App)
  await waitFor(() => expect(dataFolder.status?.mode).toBe('shared'))
  await waitFor(() => expect(app.notices.filter((n) => n.key === 'notice.dataFolder.quietSetup').map((n) => n.params)).toEqual([{ path: '~/Manga Library' }]))
  const line = (await screen.findAllByText(t('notice.dataFolder.quietSetup', { path: '~/Manga Library' })))[0]
  // Show is the notice's own button: Home has others by that name.
  const button = screen
    .getAllByRole('button', { name: t('notice.dataFolder.show') })
    .find((candidate) => candidate.closest('[aria-live]')?.contains(line))
  if (!button) throw new Error('the notice has no Show button')
  expect(session.dataFolderPromptAnswered).toEqual({ setup: 'accepted' })
  await fireEvent.click(button)
  expect(show).toHaveBeenCalledTimes(1)
})

it('opens the Set up dialog when Pear ran here, outside the inert layer, and Not now keeps the library', async () => {
  const backend = mockBackend({ sisterPresent: true })
  const quiet = vi.spyOn(backend, 'dataFolderQuietSetup')
  const { container } = render(App)
  const dialog = await screen.findByRole('dialog', { name: t('startFolder.title') })
  expect(layer(container).contains(dialog)).toBe(false)
  expect(quiet).not.toHaveBeenCalled()
  await fireEvent.click(screen.getByRole('button', { name: t('startFolder.notNow') }))
  await waitFor(() => expect(screen.queryByRole('dialog', { name: t('startFolder.title') })).toBeNull())
  expect(session.dataFolderPromptAnswered).toEqual({ setup: 'declined' })
  expect(dataFolder.status?.mode).toBe('legacy')
})

it('asks Join in the dialog when Pear set the folder up and this app was used before, with no Home card beside it', async () => {
  mockBackend({ dataFolder: 'pending' })
  render(App)
  await screen.findByRole('dialog', { name: t('startFolder.title') })
  expect(startDialog.variant).toBe('join')
  expect(screen.queryByRole('button', { name: t('home.dataFolder.move') })).toBeNull()
  await fireEvent.click(screen.getByRole('button', { name: t('startFolder.use') }))
  await waitFor(() => expect(dataFolder.status?.mode).toBe('shared'))
  await screen.findByText(t('startFolder.done', { path: '~/Manga Library' }), { exact: false })
  expect(screen.getByRole('button', { name: t('startFolder.chooseTitles') })).toBeTruthy()
})

it('holds the quiet setup notice until the first-launch dialog closes, then keeps it', async () => {
  session.firstLaunchOffered = false
  mockBackend()
  try {
    render(App)
    await waitFor(() => expect(dataFolder.status?.mode).toBe('shared'))
    await waitFor(() => expect(firstLaunch.open).toBe(true))
    expect(app.notices.some((n) => n.key === 'notice.dataFolder.quietSetup')).toBe(false)
    expect(session.quietSetupNotice).toBe('~/Manga Library')
    dismissFirstLaunch()
    await waitFor(() => expect(app.notices.filter((n) => n.key === 'notice.dataFolder.quietSetup')).toHaveLength(1))
    expect(session.quietSetupNotice).toBeNull()
    expect((await screen.findAllByText(t('notice.dataFolder.quietSetup', { path: '~/Manga Library' }))).length).toBeGreaterThan(0)
  } finally {
    resetFirstLaunch()
  }
})
