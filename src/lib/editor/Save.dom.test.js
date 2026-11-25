/**
 * Save in the editor: the button beside Export and Cmd/Ctrl+S. Off, with
 * the reason as its tooltip, outside Shared mode; busy while the command
 * runs; a failed command is said as a notice (the backend raises the others).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import { app } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { setBackend } from '../api/backend.js'
import { dataFolder, resetDataFolder } from '../state/datafolder.svelte.js'
import { matchShortcut } from '../shortcuts.js'
import ClusterRight from './ClusterRight.svelte'

const shared = {
  mode: 'shared', root: '/Users/reader/Manga Library', library: '/Users/reader/Manga Library/Cleaner', reason: null,
  defaultRoot: '/Users/reader/Manga Library', typesetterRunning: 'no', synced: false, oldCopies: [], recovered: null,
}

const saveButton = () => /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t('editor.action.save') }))

beforeEach(() => {
  resetDataFolder()
  editor.chapter = /** @type {any} */ ({ id: 'ch-1', pages: [], review: [] })
  editor.loading = false
})

afterEach(() => {
  cleanup()
  resetDataFolder()
  editor.chapter = null
  app.notices.length = 0
  setBackend(null)
})

describe('the Save button', () => {
  it('is off outside Shared mode, and says why', () => {
    dataFolder.status = /** @type {any} */ ({ ...shared, mode: 'legacy', root: null })
    setBackend(/** @type {any} */ ({ subscribe: () => () => {} }))
    render(ClusterRight)
    expect(saveButton().disabled).toBe(true)
    expect(saveButton().title).toBe(t('editor.save.needsDataFolder'))
  })

  it('saves the open chapter in Shared mode, busy until the command answers', async () => {
    dataFolder.status = /** @type {any} */ (shared)
    /** @type {(value: any) => void} */
    let answer = () => {}
    const saveToTypesetter = vi.fn(() => new Promise((resolve) => (answer = resolve)))
    setBackend(/** @type {any} */ ({ subscribe: () => () => {}, saveToTypesetter }))
    render(ClusterRight)
    expect(saveButton().disabled).toBe(false)

    await fireEvent.click(saveButton())
    expect(saveToTypesetter).toHaveBeenCalledWith({ chapterId: 'ch-1' })
    const busy = /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t('editor.save.saving') }))
    expect(busy.disabled).toBe(true)
    expect(busy.getAttribute('aria-busy')).toBe('true')

    answer({ status: 'saved', pages: 3 })
    await waitFor(() => expect(saveButton().disabled).toBe(false))
  })

  it('says a failed command as a notice', async () => {
    dataFolder.status = /** @type {any} */ (shared)
    setBackend(/** @type {any} */ ({ subscribe: () => () => {}, saveToTypesetter: vi.fn(async () => { throw new Error('disk full') }) }))
    render(ClusterRight)
    await fireEvent.click(saveButton())
    await waitFor(() => expect(app.notices.at(-1)).toMatchObject({ key: 'notice.save.failed', params: { detail: 'disk full' }, tone: 'warn' }))
  })
})

describe('Cmd/Ctrl+S', () => {
  const press = (/** @type {Record<string, unknown>} */ extra) => ({ key: 's', shiftKey: false, metaKey: false, ctrlKey: false, altKey: false, target: null, ...extra })

  it('is Save in the editor, with either modifier, and leaves the bare S to the shape key', () => {
    expect(matchShortcut(press({ metaKey: true }), { scope: 'editor' })?.id).toBe('app.save')
    expect(matchShortcut(press({ ctrlKey: true }), { scope: 'editor' })?.id).toBe('app.save')
    expect(matchShortcut(press({}), { scope: 'editor' })?.id).toBe('tool.shape.next')
    expect(matchShortcut(press({ metaKey: true }), { scope: 'home' })).toBeNull()
  })
})
