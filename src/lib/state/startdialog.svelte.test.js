/**
 * The start flow as the store runs it (`docs/shared-data-folder.md` sections
 * 5.2 to 5.5): the quiet setup and its one-time notice, the dialog's phases
 * (ask, Blocked, working, done, failed), the stored answers, and the
 * first-run Join step. The backend is a stub per test.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { app } from './app.svelte.js'
import { session } from './session.svelte.js'
import {
  PROGRESS_POLL_MS,
  acceptStart,
  closeStartDialog,
  dataFolder,
  declineStart,
  joinFromFirstRun,
  keepSeparate,
  loadDataFolder,
  openStartDialog,
  resetDataFolder,
  runStartFlow,
  startAnswers,
  startBlocked,
  startDialog,
  startFlow,
} from './datafolder.svelte.js'

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
  sisterPresent: false,
  journal: false,
  titles: 0,
  home: '/Users/reader',
  ...over,
})

const SHARED = status({ mode: 'shared', root: '/Users/reader/Manga Library', library: '/Users/reader/Manga Library/Cleaner' })
const PLAN = { target: '/Users/reader/Manga Library', bytes: 9_700_000_000, nothingToMove: false, sameVolume: false, synced: false, refusal: null }

/** @param {any} first @param {Record<string, any>} [over] */
const stub = (first, over = {}) => /** @type {any} */ ({
  dataFolderStatus: vi.fn(async () => first),
  dataFolderSetup: vi.fn(async ({ target }) => ({ ...SHARED, root: target })),
  dataFolderQuietSetup: vi.fn(async () => ({ done: true, status: SHARED })),
  dataFolderPlan: vi.fn(async () => PLAN),
  dataFolderProgress: vi.fn(async () => ({ copied: 0, total: 0 })),
  ...over,
})

/** @param {any} backend */
async function start(backend) {
  await loadDataFolder(backend)
  await runStartFlow(backend)
}

beforeEach(() => {
  resetDataFolder()
  session.dataFolderPromptAnswered = false
  session.firstLaunchOffered = true
  app.notices.length = 0
})

afterEach(() => {
  resetDataFolder()
  session.dataFolderPromptAnswered = false
  app.notices.length = 0
  vi.useRealTimers()
})

describe('the quiet setup (row E)', () => {
  it('runs once, stores Set up as accepted, and says where with a Show line', async () => {
    const backend = stub(status())
    await start(backend)
    expect(backend.dataFolderQuietSetup).toHaveBeenCalledTimes(1)
    expect(dataFolder.status?.mode).toBe('shared')
    expect(startAnswers()).toEqual({ setup: 'accepted' })
    expect(dataFolder.generation).toBe(1)
    const notice = app.notices.find((n) => n.key === 'notice.dataFolder.quietSetup')
    expect(notice?.params).toEqual({ path: '~/Manga Library' })
    // Once per launch.
    await runStartFlow(backend)
    expect(backend.dataFolderQuietSetup).toHaveBeenCalledTimes(1)
  })

  it('says nothing and stores nothing when a check did not hold', async () => {
    const backend = stub(status(), { dataFolderQuietSetup: vi.fn(async () => ({ done: false, status: status() })) })
    await start(backend)
    expect(dataFolder.status?.mode).toBe('legacy')
    expect(startAnswers()).toEqual({})
    expect(app.notices).toHaveLength(0)
  })

  it('is not run over a version 1 Not now', async () => {
    session.dataFolderPromptAnswered = true
    const backend = stub(status())
    await start(backend)
    expect(backend.dataFolderQuietSetup).not.toHaveBeenCalled()
    expect(startDialog.open).toBe(false)
  })
})

describe('the dialog', () => {
  it('opens in the Set up variant at row F, proposing the default place', async () => {
    const backend = stub(status({ sisterPresent: true }))
    await start(backend)
    expect(backend.dataFolderQuietSetup).not.toHaveBeenCalled()
    expect(startDialog.open).toBe(true)
    expect(startDialog.variant).toBe('setup')
    expect(startDialog.target).toBe('/Users/reader/Manga Library')
    expect(backend.dataFolderPlan).toHaveBeenCalledWith({ target: '/Users/reader/Manga Library' })
    expect(startDialog.plan).toEqual(PLAN)
  })

  it('opens in the Join variant at row D, on the folder Pear uses', async () => {
    const pending = status({ mode: 'pending', root: '/Volumes/Manga/Manga Library' })
    const backend = stub(pending)
    await start(backend)
    expect(startDialog.variant).toBe('join')
    expect(startDialog.target).toBe('/Volumes/Manga/Manga Library')
    expect(backend.dataFolderPlan).toHaveBeenCalledWith({ target: null })
  })

  it('adds the Join step instead at row C, a first launch', async () => {
    session.firstLaunchOffered = false
    const backend = stub(status({ mode: 'pending', root: '/Volumes/Manga/Manga Library' }))
    await start(backend)
    expect(startFlow.joinStep).toBe(true)
    expect(startDialog.open).toBe(false)
  })

  it('is Blocked while Pear runs in the folder a Join moves into, until checked again', async () => {
    const backend = stub(status({ mode: 'pending', root: '/Volumes/Manga/Manga Library', typesetterRunning: 'yes' }))
    await start(backend)
    expect(startBlocked()).toBe(true)
    expect(await acceptStart({ backend })).toBe(false)
    expect(backend.dataFolderSetup).not.toHaveBeenCalled()
    backend.dataFolderStatus.mockResolvedValue(status({ mode: 'pending', root: '/Volumes/Manga/Manga Library' }))
    await loadDataFolder(backend)
    expect(startBlocked()).toBe(false)
  })

  it('polls the copy while working, then is Done and stores the answer', async () => {
    vi.useFakeTimers()
    /** @type {(value: any) => void} */
    let finish = () => {}
    const backend = stub(status({ sisterPresent: true }), {
      dataFolderSetup: vi.fn(() => new Promise((resolve) => { finish = resolve })),
      dataFolderProgress: vi.fn(async () => ({ copied: 4, total: 10 })),
    })
    await start(backend)
    const going = acceptStart({ backend })
    expect(startDialog.phase).toBe('working')
    // Working cannot be closed or declined.
    closeStartDialog()
    declineStart()
    expect(startDialog.open).toBe(true)
    await vi.advanceTimersByTimeAsync(PROGRESS_POLL_MS)
    expect(startDialog.progress).toEqual({ copied: 4, total: 10 })
    finish(SHARED)
    expect(await going).toBe(true)
    expect(startDialog.phase).toBe('done')
    expect(startAnswers()).toEqual({ setup: 'accepted' })
    const calls = backend.dataFolderProgress.mock.calls.length
    await vi.advanceTimersByTimeAsync(PROGRESS_POLL_MS * 3)
    expect(backend.dataFolderProgress.mock.calls.length).toBe(calls)
  })

  it('is Failed with the reason, and says nothing changed when the mode stayed', async () => {
    const backend = stub(status({ sisterPresent: true }), {
      dataFolderSetup: vi.fn(async () => { throw new Error('There is not enough free space on that drive.') }),
    })
    await start(backend)
    expect(await acceptStart({ backend })).toBe(false)
    expect(startDialog.phase).toBe('failed')
    expect(startDialog.error).toBe('There is not enough free space on that drive.')
    expect(startDialog.nothingChanged).toBe(true)
    expect(startAnswers()).toEqual({})
  })

  it('puts the question back with the confirm when the drive cannot tell whether Pear runs', async () => {
    const backend = stub(status({ sisterPresent: true }), {
      dataFolderSetup: vi.fn(async () => { throw new Error('typesetter_unknown: This drive cannot tell whether Pear is open.') }),
    })
    await start(backend)
    expect(await acceptStart({ backend })).toBe(false)
    expect(startDialog.phase).toBe('ask')
    expect(dataFolder.confirm).not.toBeNull()
  })

  it('stores Not now as declined, per question, and never opens again for it', async () => {
    const backend = stub(status({ sisterPresent: true }))
    await start(backend)
    declineStart()
    expect(startDialog.open).toBe(false)
    expect(startAnswers()).toEqual({ setup: 'declined' })
    resetDataFolder()
    await start(stub(status({ sisterPresent: true })))
    expect(startDialog.open).toBe(false)
    // Join is its own question.
    resetDataFolder()
    await start(stub(status({ mode: 'pending', root: '/Volumes/Manga/Manga Library' })))
    expect(startDialog.open).toBe(true)
    expect(startDialog.variant).toBe('join')
  })

  it('carries a version 1 answer over when a Join answer is stored', async () => {
    session.dataFolderPromptAnswered = true
    const backend = stub(status({ mode: 'pending', root: '/Volumes/Manga/Manga Library' }))
    await start(backend)
    await openStartDialog('join', backend)
    declineStart()
    expect(session.dataFolderPromptAnswered).toEqual({ setup: 'declined', join: 'declined' })
  })
})

describe('the first-run Join step', () => {
  it('Use the same folder sets the folder up there and stores Join as accepted', async () => {
    const root = '/Volumes/Manga/Manga Library'
    const backend = stub(status({ mode: 'pending', root }))
    await loadDataFolder(backend)
    expect(await joinFromFirstRun(backend)).toBe(true)
    expect(backend.dataFolderSetup).toHaveBeenCalledWith({ target: root, confirmedClosed: false })
    expect(startAnswers().join).toBe('accepted')
  })

  it('Keep separate stores Join as declined and moves nothing', async () => {
    const backend = stub(status({ mode: 'pending', root: '/Volumes/Manga/Manga Library' }))
    await loadDataFolder(backend)
    keepSeparate()
    expect(startAnswers().join).toBe('declined')
    expect(backend.dataFolderSetup).not.toHaveBeenCalled()
  })
})

describe('an empty Cleaner folder with an accepted answer (contract 5.1)', () => {
  const ROOT = '/Volumes/Manga/Manga Library'
  const emptyPending = status({ mode: 'pending', root: ROOT, cleanerEmpty: true, titles: 0 })

  it('is filled without asking, and the app is Shared, for a version 1 true', async () => {
    session.dataFolderPromptAnswered = true
    const fill = vi.fn(async () => ({ ...SHARED, root: ROOT }))
    const backend = stub(emptyPending, { dataFolderFillEmpty: fill })
    await start(backend)
    expect(fill).toHaveBeenCalledTimes(1)
    expect(dataFolder.status?.mode).toBe('shared')
    expect(startDialog.open).toBe(false)
    expect(startFlow.joinStep).toBe(false)
    expect(backend.dataFolderSetup).not.toHaveBeenCalled()
  })

  it('asks Join as usual with no answer stored', async () => {
    const fill = vi.fn()
    const backend = stub(emptyPending, { dataFolderFillEmpty: fill })
    await start(backend)
    expect(fill).not.toHaveBeenCalled()
    expect(startDialog.open).toBe(true)
    expect(startDialog.variant).toBe('join')
  })
})
