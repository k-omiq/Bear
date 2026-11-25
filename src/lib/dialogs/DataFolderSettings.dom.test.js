/**
 * Settings > Data folder, mounted, in each mode: what it says, what it
 * offers, which command each press sends, and what holds a press back (an
 * open chapter, the typesetter open, a drive that cannot tell). The backend is
 * a stub that answers each change with the next status, as the commands do.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import { app } from '../state/app.svelte.js'
import { dataFolder, resetDataFolder } from '../state/datafolder.svelte.js'
import { jobs, resetJobs } from '../state/jobs.svelte.js'
import DataFolderSettings from './DataFolderSettings.svelte'

/** @param {Partial<import('../api/backend.js').DataFolderStatus>} [over] */
const status = (over = {}) => ({
  mode: 'legacy',
  root: null,
  library: '/Users/reader/Library/Application Support/com.mangacleaner.studio/library',
  reason: null,
  defaultRoot: '/Users/reader/Manga Library',
  typesetterRunning: 'no',
  synced: false,
  oldCopies: [],
  recovered: null,
  ...over,
})

const SHARED = status({ mode: 'shared', root: '/Users/reader/Manga Library', library: '/Users/reader/Manga Library/Cleaner' })

/** @param {ReturnType<typeof status>} first */
function stub(first) {
  return {
    dataFolderStatus: vi.fn(async () => first),
    dataFolderSetup: vi.fn(async ({ target }) => status({ ...SHARED, root: target })),
    dataFolderMove: vi.fn(async ({ target }) => status({ ...SHARED, root: target })),
    dataFolderUseExisting: vi.fn(async ({ target }) => status({ ...SHARED, root: target })),
    dataFolderDeleteOldCopy: vi.fn(async () => SHARED),
    readSettings: vi.fn(async () => ({})),
    writeSettings: vi.fn(async () => ({})),
  }
}

/** @param {ReturnType<typeof stub>} backend */
async function mount(backend) {
  render(DataFolderSettings, { props: { backend: /** @type {any} */ (backend) } })
  await waitFor(() => expect(dataFolder.status).not.toBeNull())
}

const button = (/** @type {string} */ name) => /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name }))

const route = app.route

beforeEach(() => resetDataFolder())

afterEach(() => {
  cleanup()
  app.route = route
  resetDataFolder()
  resetJobs()
})

describe('Settings > Data folder', () => {
  it('in Legacy, proposes the default place and sets the folder up there', async () => {
    const backend = stub(status())
    await mount(backend)
    expect(screen.getByText(t('settings.dataFolder.mode.legacy'))).toBeTruthy()
    const field = /** @type {HTMLInputElement} */ (screen.getByLabelText(t('settings.dataFolder.newLabel')))
    await waitFor(() => expect(field.value).toBe('/Users/reader/Manga Library'))

    await fireEvent.click(button(t('settings.dataFolder.setup')))
    await waitFor(() => expect(backend.dataFolderSetup).toHaveBeenCalledWith({ target: '/Users/reader/Manga Library', confirmedClosed: false }))
    await waitFor(() => expect(screen.getByText(t('settings.dataFolder.mode.shared'))).toBeTruthy())
  })

  it('in Pending, moves this library into the folder the typesetter set up', async () => {
    const root = '/Volumes/Manga/Manga Library'
    const backend = stub(status({ mode: 'pending', root }))
    await mount(backend)
    expect(screen.getByText(t('settings.dataFolder.mode.pending', { path: root }))).toBeTruthy()
    await fireEvent.click(button(t('settings.dataFolder.moveInto', { path: root })))
    await waitFor(() => expect(backend.dataFolderSetup).toHaveBeenCalledWith({ target: root, confirmedClosed: false }))
  })

  it('in Shared, shows the path and moves the whole folder to a new place', async () => {
    const backend = stub(SHARED)
    await mount(backend)
    expect(screen.getByText(t('settings.dataFolder.mode.shared'))).toBeTruthy()
    expect(screen.getByText('/Users/reader/Manga Library')).toBeTruthy()

    await fireEvent.click(button(t('settings.dataFolder.move')))
    const field = screen.getByLabelText(t('settings.dataFolder.moveLabel'))
    await fireEvent.input(field, { target: { value: '/Volumes/Big/Manga Library' } })
    await fireEvent.click(button(t('settings.dataFolder.moveConfirm')))
    await waitFor(() => expect(backend.dataFolderMove).toHaveBeenCalledWith({ target: '/Volumes/Big/Manga Library', confirmedClosed: false }))
    await waitFor(() => expect(screen.getByText('/Volumes/Big/Manga Library')).toBeTruthy())
  })

  it('in Shared, points at another existing data folder', async () => {
    const backend = stub(SHARED)
    await mount(backend)
    await fireEvent.click(button(t('settings.dataFolder.useExisting')))
    await fireEvent.input(screen.getByLabelText(t('settings.dataFolder.existingLabel')), { target: { value: '/Volumes/Other/Manga Library' } })
    await fireEvent.click(button(t('settings.dataFolder.useConfirm')))
    await waitFor(() => expect(backend.dataFolderUseExisting).toHaveBeenCalledWith({ target: '/Volumes/Other/Manga Library', confirmedClosed: false }))
  })

  it('in Unavailable, says why', async () => {
    const reason = 'Data folder not found at /Volumes/Manga/Manga Library.'
    await mount(stub(status({ mode: 'unavailable', root: '/Volumes/Manga/Manga Library', library: null, reason })))
    expect(screen.getByText(t('settings.dataFolder.mode.unavailable', { reason }))).toBeTruthy()
  })

  it('warns about a synced folder, and holds moves while the typesetter is open', async () => {
    await mount(stub({ ...SHARED, synced: true, typesetterRunning: 'yes' }))
    expect(screen.getByText(t('settings.dataFolder.synced'))).toBeTruthy()
    expect(screen.getByText(t('settings.dataFolder.typesetterOpen'))).toBeTruthy()
    expect(button(t('settings.dataFolder.move')).disabled).toBe(true)
    expect(button(t('settings.dataFolder.useExisting')).disabled).toBe(true)
  })

  it('in Pending, moves with the typesetter open and does not ask to close it', async () => {
    const root = '/Users/reader/Manga Library'
    await mount(stub(status({ mode: 'pending', root, typesetterRunning: 'yes' })))
    expect(screen.queryByText(t('settings.dataFolder.typesetterOpen'))).toBeNull()
    expect(button(t('settings.dataFolder.moveInto', { path: root })).disabled).toBe(false)
  })

  it('holds every change while jobs run, and says why', async () => {
    jobs.list.push(/** @type {any} */ ({ runId: 'run-1', kind: 'clean', chapterId: 'c1', status: 'running', done: 0, total: 3 }))
    await mount(stub(SHARED))
    expect(screen.getByText(t('settings.dataFolder.blocked.jobsRunning'))).toBeTruthy()
    expect(button(t('settings.dataFolder.move')).disabled).toBe(true)
    expect(button(t('settings.dataFolder.useExisting')).disabled).toBe(true)
  })

  it('holds every change while a chapter is open, and says why', async () => {
    app.route = { name: 'editor', projectId: 'p1', chapterId: 'c1' }
    await mount(stub(status()))
    expect(screen.getByText(t('settings.dataFolder.blocked.chapterOpen'))).toBeTruthy()
    expect(button(t('settings.dataFolder.setup')).disabled).toBe(true)
  })

  it('asks for the typesetter to be closed when the drive cannot tell, then sends the move again', async () => {
    const backend = stub(SHARED)
    backend.dataFolderMove.mockRejectedValueOnce(new Error('typesetter_unknown: This drive cannot tell whether the typesetter is open.'))
    await mount(backend)
    await fireEvent.click(button(t('settings.dataFolder.move')))
    await fireEvent.input(screen.getByLabelText(t('settings.dataFolder.moveLabel')), { target: { value: '/Volumes/Big/Manga Library' } })
    await fireEvent.click(button(t('settings.dataFolder.moveConfirm')))

    await waitFor(() => expect(screen.getByText(t('settings.dataFolder.confirmClosed.body'))).toBeTruthy())
    await fireEvent.click(button(t('settings.dataFolder.confirmClosed.action')))
    await waitFor(() => expect(backend.dataFolderMove).toHaveBeenLastCalledWith({ target: '/Volumes/Big/Manga Library', confirmedClosed: true }))
    await waitFor(() => expect(screen.queryByText(t('settings.dataFolder.confirmClosed.body'))).toBeNull())
  })

  it('shows a refusal as the backend said it', async () => {
    const backend = stub(status())
    backend.dataFolderSetup.mockRejectedValueOnce(new Error('/x is not empty.'))
    await mount(backend)
    await fireEvent.click(button(t('settings.dataFolder.setup')))
    await waitFor(() => expect(screen.getByText(t('settings.dataFolder.failed', { detail: '/x is not empty.' }))).toBeTruthy())
  })

  it('shows the busy line while a move runs', async () => {
    const backend = stub(SHARED)
    /** @type {(value: any) => void} */
    let finish = () => {}
    backend.dataFolderMove.mockImplementationOnce(() => new Promise((resolve) => (finish = resolve)))
    await mount(backend)
    await fireEvent.click(button(t('settings.dataFolder.move')))
    await fireEvent.input(screen.getByLabelText(t('settings.dataFolder.moveLabel')), { target: { value: '/Volumes/Big/Manga Library' } })
    await fireEvent.click(button(t('settings.dataFolder.moveConfirm')))
    await waitFor(() => expect(screen.getByText(t('settings.dataFolder.busy'))).toBeTruthy())
    expect(dataFolder.busy).toBe('move')
    finish(SHARED)
    await waitFor(() => expect(dataFolder.busy).toBeNull())
  })

  it('deletes an old copy only after a second press', async () => {
    const copy = { path: '/Users/reader/Manga Library.moved-2026-11-25', bytes: 9_878_000_000, movedAt: '2026-11-25T10:00:00Z' }
    const backend = stub({ ...SHARED, oldCopies: [copy] })
    await mount(backend)
    expect(screen.getByText(copy.path)).toBeTruthy()
    await fireEvent.click(button(t('settings.dataFolder.oldCopies.delete')))
    expect(backend.dataFolderDeleteOldCopy).not.toHaveBeenCalled()
    expect(screen.getByText(t('settings.dataFolder.oldCopies.confirm'))).toBeTruthy()
    await fireEvent.click(button(t('settings.dataFolder.oldCopies.confirmAction')))
    await waitFor(() => expect(backend.dataFolderDeleteOldCopy).toHaveBeenCalledWith({ path: copy.path }))
    await waitFor(() => expect(screen.queryByText(copy.path)).toBeNull())
  })

  it('shows what the start-up recovery did, until dismissed', async () => {
    const recovered = 'A move of the data folder was finished. Your data is at /Volumes/Big/Manga Library.'
    await mount(stub({ ...SHARED, recovered }))
    const line = t('settings.dataFolder.recovered', { detail: recovered })
    expect(screen.getByText(line)).toBeTruthy()
    await fireEvent.click(button(t('shell.action.dismissNotice')))
    expect(screen.queryByText(line)).toBeNull()
    expect(dataFolder.recoveredSeen).toBe(true)
  })

  it('reads and writes where Save puts the cleaned pages', async () => {
    const backend = stub(SHARED)
    backend.readSettings.mockResolvedValue({ saveCleanedTo: 'next-to-raws' })
    await mount(backend)
    const radio = (/** @type {string} */ name) => screen.getByRole('radio', { name })
    await waitFor(() => expect(radio(t('settings.dataFolder.save.nextToRaws')).getAttribute('aria-checked')).toBe('true'))
    await fireEvent.click(radio(t('settings.dataFolder.save.dataFolder')))
    await waitFor(() => expect(backend.writeSettings).toHaveBeenCalledWith({ saveCleanedTo: 'data-folder' }))
  })
})
