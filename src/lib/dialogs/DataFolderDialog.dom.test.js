/**
 * The centered start dialog (`docs/shared-data-folder.md` section 5.4),
 * mounted: the Set up and Join variants, what it says will happen (the size
 * and the time at 20 MB/s), Blocked while Pear runs, Working, Done with
 * "Choose titles…", and Failed. The store is the real one; the backend is a
 * stub.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import { app, closeAllModals } from '../state/app.svelte.js'
import { session } from '../state/session.svelte.js'
import { dataFolder, loadDataFolder, openStartDialog, resetDataFolder, startDialog } from '../state/datafolder.svelte.js'
import DataFolderDialog from './DataFolderDialog.svelte'

/** @param {Record<string, unknown>} [over] */
const status = (over = {}) => /** @type {any} */ ({
  mode: 'legacy',
  root: null,
  library: '/Users/reader/Library/Application Support/com.mangacleaner.studio/library',
  reason: null,
  defaultRoot: '/Users/reader/Manga Library',
  typesetterRunning: 'no',
  synced: false,
  oldCopies: [],
  recovered: null,
  sisterPresent: true,
  journal: false,
  titles: 4,
  home: '/Users/reader',
  ...over,
})

const SHARED = status({ mode: 'shared', root: '/Users/reader/Manga Library' })
/** @param {Record<string, unknown>} [over] */
const plan = (over = {}) => ({ target: '/Users/reader/Manga Library', bytes: 9_700_000_000, nothingToMove: false, sameVolume: false, synced: false, refusal: null, ...over })

/** @param {any} first @param {Record<string, any>} [over] */
const stub = (first, over = {}) => /** @type {any} */ ({
  dataFolderStatus: vi.fn(async () => first),
  dataFolderSetup: vi.fn(async () => SHARED),
  dataFolderPlan: vi.fn(async () => plan()),
  dataFolderProgress: vi.fn(async () => ({ copied: 0, total: 0 })),
  ...over,
})

/** @param {any} backend @param {'setup'|'join'} variant */
async function mount(backend, variant) {
  await loadDataFolder(backend)
  await openStartDialog(variant, backend)
  render(DataFolderDialog, { props: { backend } })
}

const button = (/** @type {string} */ name) => /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name }))

beforeEach(() => {
  resetDataFolder()
  session.dataFolderPromptAnswered = false
})

afterEach(() => {
  cleanup()
  closeAllModals()
  resetDataFolder()
  session.dataFolderPromptAnswered = false
})

describe('the start dialog', () => {
  it('Set up: the drawing, two lines, the path as ~, Change…, and the copy\'s size and time', async () => {
    await mount(stub(status()), 'setup')
    expect(screen.getByRole('img', { name: t('startFolder.drawing') })).toBeTruthy()
    expect(screen.getByText(t('startFolder.lineOne'))).toBeTruthy()
    expect(screen.getByText(t('startFolder.lineTwo'))).toBeTruthy()
    expect(screen.getByText('~/Manga Library')).toBeTruthy()
    expect(button(t('startFolder.change'))).toBeTruthy()
    // 9.7 GB at 20 MB/s is 485 s: 9 minutes, rounded up.
    await waitFor(() => expect(screen.getByText(t('startFolder.setupOtherVolume', { bytes: 9_700_000_000, minutes: 9 }))).toBeTruthy())
    expect(button(t('startFolder.use')).disabled).toBe(false)
  })

  it('Set up on the same drive says the library moves in place', async () => {
    await mount(stub(status(), { dataFolderPlan: vi.fn(async () => plan({ sameVolume: true })) }), 'setup')
    await waitFor(() => expect(screen.getByText(t('startFolder.setupSameVolume'))).toBeTruthy())
  })

  it('Join names Pear\'s folder and offers no other', async () => {
    const pending = status({ mode: 'pending', root: '/Volumes/Manga/Manga Library' })
    await mount(stub(pending, { dataFolderPlan: vi.fn(async () => plan({ target: pending.root })) }), 'join')
    expect(screen.getByText('/Volumes/Manga/Manga Library')).toBeTruthy()
    expect(screen.queryByRole('button', { name: t('startFolder.change') })).toBeNull()
    await waitFor(() => expect(screen.getByText(t('startFolder.joinOtherVolume', { bytes: 9_700_000_000, minutes: 9 }))).toBeTruthy())
  })

  it('is Blocked while Pear runs in the folder, with Check again', async () => {
    const running = status({ mode: 'pending', root: '/Volumes/Manga/Manga Library', typesetterRunning: 'yes' })
    const backend = stub(running)
    await mount(backend, 'join')
    expect(screen.getByText(t('startFolder.blocked'))).toBeTruthy()
    expect(button(t('startFolder.use')).disabled).toBe(true)
    backend.dataFolderStatus.mockResolvedValue(status({ mode: 'pending', root: '/Volumes/Manga/Manga Library' }))
    await fireEvent.click(button(t('startFolder.checkAgain')))
    await waitFor(() => expect(screen.queryByText(t('startFolder.blocked'))).toBeNull())
    expect(button(t('startFolder.use')).disabled).toBe(false)
  })

  it('shows the backend\'s refusal and holds the button', async () => {
    await mount(stub(status(), { dataFolderPlan: vi.fn(async () => plan({ refusal: 'That folder is not empty.' })) }), 'setup')
    await waitFor(() => expect(screen.getByText('That folder is not empty.')).toBeTruthy())
    expect(button(t('startFolder.use')).disabled).toBe(true)
  })

  it('Not now stores the answer and closes', async () => {
    await mount(stub(status()), 'setup')
    await fireEvent.click(button(t('startFolder.notNow')))
    expect(startDialog.open).toBe(false)
    expect(session.dataFolderPromptAnswered).toEqual({ setup: 'declined' })
  })

  it('Working shows the copy, then Done offers Choose titles…, which opens Send to Pear', async () => {
    /** @type {(value: any) => void} */
    let finish = () => {}
    const backend = stub(status(), { dataFolderSetup: vi.fn(() => new Promise((resolve) => { finish = resolve })) })
    await mount(backend, 'setup')
    await waitFor(() => expect(button(t('startFolder.use')).disabled).toBe(false))
    await fireEvent.click(button(t('startFolder.use')))
    expect(startDialog.phase).toBe('working')
    startDialog.progress = { copied: 2_000_000, total: 8_000_000 }
    await waitFor(() => expect(screen.getByText(t('startFolder.copying', { copied: 2_000_000, total: 8_000_000 }))).toBeTruthy())
    expect(screen.getByRole('progressbar')).toBeTruthy()
    expect(screen.queryByRole('button', { name: t('startFolder.notNow') })).toBeNull()
    finish(SHARED)
    await waitFor(() => expect(screen.getByText(t('startFolder.done', { path: '~/Manga Library' }))).toBeTruthy())
    expect(screen.getByText(t('startFolder.titles', { count: 4 }))).toBeTruthy()
    await fireEvent.click(button(t('startFolder.chooseTitles')))
    expect(startDialog.open).toBe(false)
    expect(app.modals.at(-1)?.kind).toBe('sendToPear')
  })

  it('Failed says why in plain words, and that nothing changed', async () => {
    const backend = stub(status(), { dataFolderSetup: vi.fn(async () => { throw new Error('There is not enough free space on that drive.') }) })
    await mount(backend, 'setup')
    await waitFor(() => expect(button(t('startFolder.use')).disabled).toBe(false))
    await fireEvent.click(button(t('startFolder.use')))
    await waitFor(() => expect(screen.getByText(t('startFolder.failed'))).toBeTruthy())
    expect(screen.getByText('There is not enough free space on that drive.')).toBeTruthy()
    expect(screen.getByText(t('startFolder.nothingChanged'))).toBeTruthy()
    expect(dataFolder.status?.mode).toBe('legacy')
  })
})
