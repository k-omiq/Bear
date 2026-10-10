/**
 * The page readout, mounted, for the one thing it does beyond showing a
 * number: pressing it opens a field that jumps to the page typed into it.
 */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import { tick } from 'svelte'
import { editor } from '../state/editor.svelte.js'
import { app } from '../state/app.svelte.js'
import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import KeyboardLayer from '../shell/KeyboardLayer.svelte'
import PageControls from './PageControls.svelte'

const TOTAL = 12

beforeEach(() => {
  editor.project = /** @type {any} */ ({ id: 'p1', mode: 'single', readingDirection: 'rtl' })
  editor.chapter = /** @type {any} */ ({
    id: 'c1', review: [],
    pages: Array.from({ length: TOTAL }, (_, index) => ({
      id: `c1-p${index}`, chapterId: 'c1', index, number: index + 1,
      width: 800, height: 1600, status: 'unclean', regions: [], resident: true,
    })),
  })
  editor.loading = false
  editor.pageIndex = 2
  app.route = { name: 'editor', projectId: 'p1', chapterId: 'c1' }
  app.modals.length = 0
  setBackend(/** @type {any} */ ({ loadPages: vi.fn(async () => []) }))
})

afterEach(() => {
  cleanup()
  setBackend(null)
  editor.chapter = null
  editor.project = null
  editor.pageIndex = 0
})

/** @param {ReturnType<typeof render>} screen */
async function open(screen) {
  const name = t('editor.readout.pageJump', { index: editor.pageIndex + 1, total: TOTAL })
  await fireEvent.click(screen.getByRole('button', { name }))
  await tick()
  return /** @type {HTMLInputElement} */ (
    screen.getByRole('textbox', { name: t('editor.readout.pageJumpField', { total: TOTAL }) })
  )
}

/**
 * @param {HTMLInputElement} field
 * @param {string} value
 */
async function type(field, value) {
  field.value = value
  await fireEvent.input(field)
}

it('opens an empty field on the readout and jumps on Enter', async () => {
  const screen = render(PageControls)
  const field = await open(screen)
  expect(document.activeElement).toBe(field)
  expect(field.value).toBe('')
  expect(field.placeholder).toBe('03')

  await type(field, '9')
  await fireEvent.keyDown(field, { key: 'Enter' })
  await tick()

  expect(editor.pageIndex).toBe(8)
  expect(screen.queryByRole('textbox')).toBeNull()
  expect(screen.getByRole('button', {
    name: t('editor.readout.pageJump', { index: 9, total: TOTAL }),
  })).toBe(document.activeElement)
})

it('lands on the last page for a number past the end, and the first for zero', async () => {
  const screen = render(PageControls)
  let field = await open(screen)
  await type(field, '99')
  await fireEvent.keyDown(field, { key: 'Enter' })
  await tick()
  expect(editor.pageIndex).toBe(TOTAL - 1)

  field = await open(screen)
  await type(field, '0')
  await fireEvent.keyDown(field, { key: 'Enter' })
  await tick()
  expect(editor.pageIndex).toBe(0)
})

it('keeps the page on Escape, on blur and on an empty Enter', async () => {
  const screen = render(PageControls)

  let field = await open(screen)
  await type(field, '9')
  await fireEvent.keyDown(field, { key: 'Escape' })
  await tick()
  expect(screen.queryByRole('textbox')).toBeNull()

  field = await open(screen)
  await type(field, '9')
  await fireEvent.blur(field)
  await tick()
  expect(screen.queryByRole('textbox')).toBeNull()

  field = await open(screen)
  await fireEvent.keyDown(field, { key: 'Enter' })
  await tick()
  expect(screen.queryByRole('textbox')).toBeNull()

  expect(editor.pageIndex).toBe(2)
})

it('takes digits only, and keeps them from the editor shortcuts', async () => {
  const screen = render(PageControls)
  render(KeyboardLayer)
  const tool = editor.tool
  const field = await open(screen)

  await type(field, '1a-2')
  expect(field.value).toBe('12')

  // A digit is a tool slot and an arrow is a page turn everywhere else.
  await fireEvent.keyDown(field, { key: '4' })
  await fireEvent.keyDown(field, { key: 'ArrowLeft' })
  expect(editor.tool).toBe(tool)
  expect(editor.pageIndex).toBe(2)
})
