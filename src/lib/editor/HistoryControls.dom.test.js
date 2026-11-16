/**
 * The Undo and Redo buttons, mounted: they are the page on screen's, so
 * their state and their names follow the page, and pressing one asks the
 * journal about that page alone.
 */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import { tick } from 'svelte'
import { editor } from '../state/editor.svelte.js'
import { setBackend } from '../api/backend.js'
import { adopt, createHistory, settled } from '../model/history.js'
import { t } from '../i18n/index.js'
import HistoryControls from './HistoryControls.svelte'

const undoNamed = (/** @type {string} */ commandKey) => t('editor.action.undoCommand', { commandKey })
const redoNamed = (/** @type {string} */ commandKey) => t('editor.action.redoCommand', { commandKey })

beforeEach(() => {
  editor.chapter = /** @type {any} */ ({
    id: 'c1', review: [],
    pages: [0, 1].map((index) => ({ id: `c1-p00${index + 1}`, chapterId: 'c1', index, number: index + 1, regions: [], resident: true })),
  })
  editor.pageIndex = 0
  editor.history = adopt(createHistory(), { limit: 500, cursor: 2, entries: [
    { seq: 1, label: 'masks.command.deleteMask', page: 0, undone: false },
    { seq: 2, label: 'masks.command.rerunMask', page: 1, undone: false },
    { seq: 3, label: 'masks.command.layerStyle', page: 1, undone: true },
  ] })
})

afterEach(() => {
  cleanup()
  setBackend(null)
  editor.chapter = null
  editor.pageIndex = 0
  editor.history = createHistory()
})

it('shows the page on screen\'s steps, and changes with the page', async () => {
  const screen = render(HistoryControls)
  expect(screen.getByRole('button', { name: undoNamed('masks.command.deleteMask') }).hasAttribute('disabled')).toBe(false)
  expect(screen.getByRole('button', { name: t('editor.action.redo') }).hasAttribute('disabled')).toBe(true)

  editor.pageIndex = 1
  await tick()
  expect(screen.getByRole('button', { name: undoNamed('masks.command.rerunMask') }).hasAttribute('disabled')).toBe(false)
  expect(screen.getByRole('button', { name: redoNamed('masks.command.layerStyle') }).hasAttribute('disabled')).toBe(false)
})

it('undoes on the page on screen only', async () => {
  const historyMove = vi.fn(async () => ({ cursor: 1, entry: null }))
  setBackend(/** @type {any} */ ({ historyMove }))
  editor.pageIndex = 1
  const screen = render(HistoryControls)
  await fireEvent.click(screen.getByRole('button', { name: undoNamed('masks.command.rerunMask') }))
  await settled(editor.history)
  expect(historyMove).toHaveBeenCalledWith({ chapterId: 'c1', pageIndex: 1, direction: 'undo' })
  await tick()
  expect(screen.getByRole('button', { name: t('editor.action.undo') }).hasAttribute('disabled')).toBe(true)

  // Page 0's step is still there, untouched.
  editor.pageIndex = 0
  await tick()
  expect(screen.getByRole('button', { name: undoNamed('masks.command.deleteMask') }).hasAttribute('disabled')).toBe(false)
})
