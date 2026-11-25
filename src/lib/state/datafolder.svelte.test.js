/**
 * The data folder store's reading of refusals that are about the folder, not
 * the call: the backend's text puts the subject first
 * (`library: data_folder_unavailable: ...`), any command can meet one, and a
 * move elsewhere (`data_folder_moving:`) holds the window until the library
 * answers again.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createTauriBackend } from '../api/tauri.js'
import { setBackend } from '../api/backend.js'
import { openEditorChapter } from './editor.svelte.js'
import {
  MOVING_POLL_MS,
  changeDataFolder,
  dataFolder,
  dataFolderHeld,
  isMovingError,
  isUnavailableError,
  noteLibraryError,
  resetDataFolder,
} from './datafolder.svelte.js'

const UNAVAILABLE = { mode: 'unavailable', root: '/Volumes/Manga/Manga Library', reason: 'Data folder not found.', oldCopies: [] }
const SHARED = { mode: 'shared', root: '/Volumes/Manga/Manga Library', reason: null, oldCopies: [] }

beforeEach(() => resetDataFolder())
afterEach(() => {
  resetDataFolder()
  setBackend(null)
  vi.useRealTimers()
})

describe('refusals about the folder', () => {
  it('finds the code anywhere in the text, as the backend writes it', () => {
    expect(isUnavailableError(new Error('library: data_folder_unavailable: Data folder not found.'))).toBe(true)
    expect(isMovingError('library: data_folder_moving: the typesetter is moving the data folder')).toBe(true)
    expect(isUnavailableError(new Error('no chapter c7'))).toBe(false)
  })

  it('reads the status again on an unavailable folder', async () => {
    const backend = /** @type {any} */ ({ dataFolderStatus: vi.fn(async () => UNAVAILABLE) })
    noteLibraryError(new Error('library: data_folder_unavailable: gone'), backend)
    await vi.waitFor(() => expect(dataFolder.status?.mode).toBe('unavailable'))
    expect(backend.dataFolderStatus).toHaveBeenCalledTimes(1)
  })

  it('holds the window while the folder moves, and reads everything again when it ends', async () => {
    vi.useFakeTimers()
    const listProjects = vi.fn()
      .mockRejectedValueOnce(new Error('library: data_folder_moving: a move is running'))
      .mockResolvedValueOnce([])
    const backend = /** @type {any} */ ({ listProjects, dataFolderStatus: vi.fn(async () => SHARED) })
    noteLibraryError(new Error('library: data_folder_moving: a move is running'), backend)
    expect(dataFolder.moving).toBe(true)
    expect(dataFolderHeld()).toBe(true)
    expect(dataFolder.status).toBeNull()

    await vi.advanceTimersByTimeAsync(MOVING_POLL_MS)
    expect(dataFolder.moving).toBe(true)
    await vi.advanceTimersByTimeAsync(MOVING_POLL_MS)
    expect(dataFolder.moving).toBe(false)
    expect(dataFolder.status?.mode).toBe('shared')
    expect(dataFolder.generation).toBe(1)
  })

  it('hears a refusal from any command through the adapter', async () => {
    const invoke = vi.fn(async (command) => {
      if (command === 'data_folder_status') return UNAVAILABLE
      throw 'library: data_folder_unavailable: Data folder not found.'
    })
    const backend = createTauriBackend({ invoke, fallback: /** @type {any} */ ({}) })
    setBackend(backend)
    await expect(backend.chapterPages({ chapterId: 'c1' })).rejects.toBe('library: data_folder_unavailable: Data folder not found.')
    await vi.waitFor(() => expect(dataFolder.status?.mode).toBe('unavailable'))
  })

  it('reads the status again when the editor cannot open a chapter for it', async () => {
    const dataFolderStatus = vi.fn(async () => UNAVAILABLE)
    setBackend(/** @type {any} */ ({
      subscribe: () => () => {},
      openChapter: vi.fn(async () => { throw new Error('library: data_folder_unavailable: gone') }),
      dataFolderStatus,
    }))
    await expect(openEditorChapter('p1', 'c1')).rejects.toThrow(/data_folder_unavailable/)
    await vi.waitFor(() => expect(dataFolderStatus).toHaveBeenCalled())
  })
})

describe('a change of place', () => {
  it('counts a generation, so Home lists the library again', async () => {
    const backend = /** @type {any} */ ({ dataFolderMove: vi.fn(async () => SHARED) })
    expect(await changeDataFolder('move', '/Volumes/Big/Manga Library', { backend })).toBe(true)
    expect(dataFolder.generation).toBe(1)
  })

  it('holds the window while it runs, but not for an old copy being deleted', async () => {
    dataFolder.busy = 'setup'
    expect(dataFolderHeld()).toBe(true)
    dataFolder.busy = 'delete'
    expect(dataFolderHeld()).toBe(false)
  })
})
