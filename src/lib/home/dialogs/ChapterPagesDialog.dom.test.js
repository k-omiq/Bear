/**
 * The Pages screen, mounted over a stubbed seam: it lists the pages, says why
 * a page with work cannot be removed, and removes only after its inline
 * confirmation names what goes.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'

import { setBackend } from '../../api/backend.js'
import { t } from '../../i18n/index.js'
import ChapterPagesDialog from './ChapterPagesDialog.svelte'

const SPEC = {
  id: 'modal-1',
  kind: 'chapterPages',
  titleKey: 'home.pagesScreen.title',
  props: { projectId: 'p1', chapterId: 'c1', number: '12.5', name: 'Split' },
}

/** @param {boolean[]} worked */
function listing(worked) {
  return {
    chapterId: 'c1',
    longstrip: false,
    busy: false,
    pages: worked.map((work, i) => ({
      id: `c1-p${i}`,
      sourceIndex: i,
      file: `${String(i + 1).padStart(3, '0')}.png`,
      status: 'notStarted',
      regionCount: work ? 2 : 0,
    })),
    worked,
    removed: [],
  }
}

let backend

beforeEach(() => {
  backend = {
    chapterPages: vi.fn(async () => listing([true, false, false])),
    removePages: vi.fn(async () => ({ ...listing([true, false]), removed: [{ sourceIndex: 2, file: '003.png', width: 1, height: 1 }] })),
    listProjects: async () => [],
    subscribe: () => () => {},
  }
  setBackend(/** @type {any} */ (backend))
})

afterEach(() => {
  cleanup()
  setBackend(null)
})

async function mount() {
  const view = render(ChapterPagesDialog, { props: { spec: /** @type {any} */ (SPEC) } })
  await vi.waitFor(() => expect(view.getAllByRole('option')).toHaveLength(3))
  return view
}

describe('the Pages screen', () => {
  it('lists every page with its number, file and state', async () => {
    const { getAllByRole, getByText } = await mount()
    const names = getAllByRole('option').map((tile) => tile.getAttribute('aria-label'))
    expect(names[0]).toBe(t('home.pagesScreen.tile', { number: 1, file: '001.png', stateLabel: t('home.pagesScreen.state.masks') }))
    expect(names[2]).toBe(t('home.pagesScreen.tile', { number: 3, file: '003.png', stateLabel: t('home.pagesScreen.state.untouched') }))
    expect(getByText(t('home.pagesScreen.meta', { number: '12.5', name: 'Split' }))).toBeTruthy()
    expect(backend.chapterPages).toHaveBeenCalledWith({ chapterId: 'c1' })
  })

  it('says why a page before a page with work cannot go', async () => {
    backend.chapterPages.mockResolvedValue(listing([false, true, false]))
    const { getAllByRole, getByText } = await mount()
    await fireEvent.click(getAllByRole('option')[0])
    expect(getByText(t('home.pagesScreen.why.pinned', { page: 2 }))).toBeTruthy()
    const remove = getByText(t('home.pagesScreen.remove', { count: 1 })).closest('button')
    expect(remove?.disabled).toBe(true)
  })

  it('removes the selected page only after the confirmation', async () => {
    const { getAllByRole, getByRole, queryByRole, getByText } = await mount()
    await fireEvent.click(getAllByRole('option')[2])
    expect(getAllByRole('option')[2].getAttribute('aria-selected')).toBe('true')

    await fireEvent.click(getByText(t('home.pagesScreen.remove', { count: 1 })).closest('button'))
    const confirm = getByRole('alertdialog')
    expect(confirm.textContent).toContain(t('home.pagesScreen.confirm.noLoss'))
    expect(backend.removePages).not.toHaveBeenCalled()

    await fireEvent.click(getByText(t('shell.action.cancel')))
    expect(queryByRole('alertdialog')).toBeNull()

    await fireEvent.click(getByText(t('home.pagesScreen.remove', { count: 1 })).closest('button'))
    const yes = [...getByRole('alertdialog').querySelectorAll('button')].at(-1)
    await fireEvent.click(/** @type {HTMLElement} */ (yes))
    await vi.waitFor(() => expect(getAllByRole('option')).toHaveLength(2))
    expect(backend.removePages).toHaveBeenCalledWith({ chapterId: 'c1', indices: [2] })
  })
})
