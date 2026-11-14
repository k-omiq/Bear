/**
 * The New chapter dialog's number field, mounted.
 *
 * The number used to be derived twice - `max + 1` in this dialog for display,
 * and `max + 1` again in the backend - so it could not be wrong and could not
 * be chosen either. Now it is a field, and the three things worth asserting are
 * the three the user meets: what the field accepts, what reaches the seam, and
 * what the dialog refuses.
 *
 * The seam is stubbed through `setBackend` rather than driven with the mock:
 * the only call this file is about is `createChapter`, and a spy is what lets
 * it say which number arrived.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'

import { setBackend } from '../../api/backend.js'
import { t } from '../../i18n/index.js'
import { library } from '../library.svelte.js'
import NewChapterDialog from './NewChapterDialog.svelte'

/** The spec `pushModal({kind: 'newChapter'})` would have handed the dialog. */
const SPEC = {
  id: 'modal-1',
  kind: 'newChapter',
  titleKey: 'modal.title.newChapter',
  props: { projectId: 'p1' },
}

/** A project holding Ch. 1 and Ch. 7, so `max + 1` is 8 and 7 is taken. */
function project() {
  return {
    id: 'p1',
    name: 'Emberfall',
    mode: 'single',
    sourcePath: '/scans/emberfall',
    chapters: [
      { id: 'c2', number: 7, name: 'Ch 7' },
      { id: 'c1', number: 1, name: 'Ch 1' },
    ],
  }
}

let createChapter

beforeEach(() => {
  createChapter = vi.fn(async () => null)
  setBackend(
    /** @type {any} */ ({
      listProjects: async () => [project()],
      createChapter,
      subscribe: () => () => {},
    }),
  )
  library.projects = [/** @type {any} */ (project())]
  library.busy = false
})

afterEach(() => {
  cleanup()
  library.projects = []
})

/** @param {HTMLElement} container */
function numberField(container) {
  const field = container.querySelector('input[inputmode="numeric"]')
  if (!(field instanceof HTMLInputElement)) throw new Error('no number field')
  return field
}

/**
 * The primary action. Found by the text it carries rather than by position:
 * the label names the number, so a button found this way is also proof the
 * label followed the field.
 *
 * @param {HTMLElement} container
 * @param {number|string} number
 */
function createButton(container, number) {
  const label = t('home.newChapter.create', { number })
  const button = [...container.querySelectorAll('button')].find(
    (candidate) => candidate.textContent?.trim() === label,
  )
  if (!button) throw new Error(`no button labelled "${label}"`)
  return button
}

describe('the New chapter dialog', () => {
  it('starts the number at one above the highest the project holds', () => {
    const { container } = render(NewChapterDialog, { props: { spec: SPEC } })
    expect(numberField(container).value).toBe('8')
  })

  it('keeps digits and the split marks, so a typed word never becomes a number', async () => {
    const { container } = render(NewChapterDialog, { props: { spec: SPEC } })
    const field = numberField(container)

    await fireEvent.input(field, { target: { value: '1a2' } })
    expect(field.value).toBe('12')

    await fireEvent.input(field, { target: { value: 'Ch. 12,5 / 2-1' } })
    expect(field.value).toBe('.12,5/2-1')

    await fireEvent.input(field, { target: { value: 'twelve' } })
    expect(field.value).toBe('')
  })

  for (const typed of ['12.5', '12-1', '12/2', '12,5']) {
    it(`sends a split chapter ${typed} as typed, beside its whole number`, async () => {
      const { container } = render(NewChapterDialog, { props: { spec: SPEC } })
      await fireEvent.input(numberField(container), { target: { value: typed } })
      await fireEvent.click(createButton(container, typed))

      expect(createChapter).toHaveBeenCalledTimes(1)
      expect(createChapter.mock.calls[0][0]).toMatchObject({ projectId: 'p1', number: 12, label: typed })
    })
  }

  it('waits for a split number to be whole before it can be added', async () => {
    const { container } = render(NewChapterDialog, { props: { spec: SPEC } })
    await fireEvent.input(numberField(container), { target: { value: '12.' } })
    expect(createButton(container, 8).disabled).toBe(true)
    await fireEvent.input(numberField(container), { target: { value: '12..5' } })
    expect(createButton(container, 8).disabled).toBe(true)
  })

  it('refuses a split number already in the project under another spelling', async () => {
    library.projects = [/** @type {any} */ ({ ...project(), chapters: [...project().chapters, { id: 'c3', number: '7.5', name: 'Ch 7.5' }] })]
    const { container, getByText } = render(NewChapterDialog, { props: { spec: SPEC } })
    await fireEvent.input(numberField(container), { target: { value: '7-5' } })

    expect(getByText(t('home.newChapter.taken', { number: '7-5' }))).toBeTruthy()
    expect(createButton(container, '7-5').disabled).toBe(true)
  })

  it('sends the number the user typed, not the one it suggested', async () => {
    const { container } = render(NewChapterDialog, { props: { spec: SPEC } })
    await fireEvent.input(numberField(container), { target: { value: '12' } })
    await fireEvent.click(createButton(container, 12))

    expect(createChapter).toHaveBeenCalledTimes(1)
    expect(createChapter.mock.calls[0][0]).toMatchObject({ projectId: 'p1', number: 12 })
    expect(createChapter.mock.calls[0][0]).not.toHaveProperty('label')
  })

  it('refuses a number the project already holds, and says which', async () => {
    const { container, getByText } = render(NewChapterDialog, { props: { spec: SPEC } })
    await fireEvent.input(numberField(container), { target: { value: '7' } })

    expect(getByText(t('home.newChapter.taken', { number: 7 }))).toBeTruthy()
    expect(createButton(container, 7).disabled).toBe(true)

    await fireEvent.click(createButton(container, 7))
    expect(createChapter).not.toHaveBeenCalled()
  })

  it('refuses an empty number rather than sending nothing', async () => {
    const { container } = render(NewChapterDialog, { props: { spec: SPEC } })
    await fireEvent.input(numberField(container), { target: { value: '' } })

    // With no number in the field the label falls back to the suggestion.
    expect(createButton(container, 8).disabled).toBe(true)
  })
})
