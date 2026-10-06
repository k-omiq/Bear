/**
 * A chapter row's menu: Denoise is in it, and the secondary press opens the
 * same items at the pointer. Shift+F10 is asserted as a key because WebKit,
 * the engine the app ships on, raises no `contextmenu` event for it.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { pushModal } from '../state/app.svelte.js'
import { chapterMenuItems } from './actions.js'
import ChapterRow from './ChapterRow.svelte'
import { registerJob, resetJobs } from '../state/jobs.svelte.js'

vi.mock('../state/app.svelte.js', async (importOriginal) => ({
  ...(await importOriginal()),
  pushModal: vi.fn(),
}))

const CHAPTER = {
  id: 'tsuki-to-hane-ch107',
  number: 107,
  name: 'Paper Moon',
  sourcePath: '/scans/ch107',
  pages: [{ status: 'notStarted' }, { status: 'notStarted' }],
  lastOpened: { key: 'time.relative.yesterday', params: {} },
}
const PROJECT = { id: 'tsuki-to-hane', name: 'Tsuki to Hane', chapters: [CHAPTER] }
const PROGRESS = {
  id: CHAPTER.id,
  totalPages: 2,
  pagesCleaned: 0,
  regionsNeedingReview: 0,
  status: 'notStarted',
  statusKey: 'progress.status.notStarted',
}

function mount(chapter = CHAPTER) {
  const view = render(ChapterRow, {
    props: {
      chapter,
      progress: PROGRESS,
      mode: 'single',
      interruptedAt: null,
      active: true,
      project: PROJECT,
      onopen: vi.fn(),
    },
  })
  return { ...view, row: /** @type {HTMLElement} */ (view.container.querySelector('li.row')) }
}

beforeEach(() => vi.mocked(pushModal).mockClear())
afterEach(cleanup)

describe('the chapter menu', () => {
  it('offers Denoise first, and Delete alone behind a separator', () => {
    expect(chapterMenuItems().map((item) => item.id)).toEqual(['denoise', 'sep-1', 'delete'])
    expect(chapterMenuItems({ ...CHAPTER, denoiseReplacement: null }).map((item) => item.id))
      .toEqual(['denoise', 'sep-1', 'delete'])
  })

  it('offers Denoise cleaned chapter only once a page has cleaning on it', () => {
    const ids = (pages) => chapterMenuItems({ ...CHAPTER, pages }).map((item) => item.id)
    expect(ids([{ status: 'notStarted', doneCount: 0 }])).not.toContain('denoiseCleaned')
    expect(ids([{ status: 'detected', doneCount: 2 }, { status: 'notStarted' }])).toEqual(['denoise', 'denoiseCleaned', 'sep-1', 'delete'])
    expect(ids([{ status: 'cleaned' }])).toContain('denoiseCleaned')
  })

  it('offers Replace pages with denoised while a page can take its file, and says what stays', async () => {
    const denoised = { ...CHAPTER, denoiseReplacement: { pages: 1, kept: 1, missing: 0 } }
    expect(chapterMenuItems(denoised).map((item) => item.id)).toEqual(['denoise', 'replaceDenoised', 'sep-1', 'delete'])

    const { row, getByRole } = mount(denoised)
    row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 40, clientY: 12 }))
    await vi.waitFor(() => getByRole('menu'))
    await fireEvent.click(getByRole('menuitem', { name: t('home.action.replaceDenoised') }))
    expect(pushModal).toHaveBeenCalledWith(expect.objectContaining({
      kind: 'replaceDenoised',
      props: {
        bodyKey: 'home.replaceDenoised.bodyKept',
        bodyParams: { count: 1, kept: 1, missing: 0, number: 107 },
      },
    }))
  })

  it('says pages that were not denoised stay raw, apart from the ones kept', async () => {
    const bodyOf = async (offer) => {
      vi.mocked(pushModal).mockClear()
      const { row, getByRole, unmount } = mount({ ...CHAPTER, denoiseReplacement: offer })
      row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 40, clientY: 12 }))
      await vi.waitFor(() => getByRole('menu'))
      await fireEvent.click(getByRole('menuitem', { name: t('home.action.replaceDenoised') }))
      unmount()
      return vi.mocked(pushModal).mock.calls[0][0].props
    }
    expect(await bodyOf({ pages: 20, kept: 0, missing: 2 })).toEqual({
      bodyKey: 'home.replaceDenoised.bodyMissing', bodyParams: { count: 20, kept: 0, missing: 2, number: 107 },
    })
    expect((await bodyOf({ pages: 18, kept: 2, missing: 2 })).bodyKey).toBe('home.replaceDenoised.bodyMissingKept')
    expect((await bodyOf({ pages: 2, kept: 0, missing: 0 })).bodyKey).toBe('home.replaceDenoised.body')
    expect(t('home.replaceDenoised.bodyMissing', { count: 20, missing: 2, number: 107 })).toContain('(2)')
  })

  it('opens at the pointer on a right-click, and Denoise opens the dialog', async () => {
    const { row, getByRole } = mount()
    const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 40, clientY: 12 })
    row.dispatchEvent(event)
    expect(event.defaultPrevented).toBe(true)

    const menu = await vi.waitFor(() => getByRole('menu'))
    const items = [...menu.querySelectorAll('[role^="menuitem"]')].map((item) => item.textContent?.trim())
    expect(items).toEqual([t('home.action.denoise'), t('home.action.deleteChapter')])

    await fireEvent.click(getByRole('menuitem', { name: t('home.action.denoise') }))
    expect(pushModal).toHaveBeenCalledWith(expect.objectContaining({
      kind: 'denoise',
      titleKey: 'denoise.title',
      props: { projectId: PROJECT.id, chapter: CHAPTER, cleaned: false },
    }))
  })

  it('opens the same dialog for Denoise cleaned chapter, marked cleaned', async () => {
    const chapter = { ...CHAPTER, pages: [{ status: 'cleaned', doneCount: 1 }, { status: 'notStarted' }] }
    const { row, getByRole } = mount(chapter)
    row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 40, clientY: 12 }))
    await vi.waitFor(() => getByRole('menu'))
    await fireEvent.click(getByRole('menuitem', { name: t('home.action.denoiseCleaned') }))
    expect(pushModal).toHaveBeenCalledWith(expect.objectContaining({
      kind: 'denoise',
      titleKey: 'denoise.titleCleaned',
      props: { projectId: PROJECT.id, chapter, cleaned: true },
    }))
  })

  it('opens on Shift+F10 and on the context-menu key', async () => {
    const { container, queryByRole, getByRole } = mount()
    const hit = /** @type {HTMLElement} */ (container.querySelector('button.hit'))
    expect(queryByRole('menu')).toBeNull()

    await fireEvent.keyDown(hit, { key: 'F10', shiftKey: true })
    await vi.waitFor(() => getByRole('menu'))
    await fireEvent.keyDown(getByRole('menu'), { key: 'Escape' })
    await vi.waitFor(() => expect(queryByRole('menu')).toBeNull())

    await fireEvent.keyDown(hit, { key: 'ContextMenu' })
    await vi.waitFor(() => getByRole('menu'))
  })
})

describe('a denoised chapter', () => {
  const RUNS = [
    { created: 1790000000, preset: 'waifu2x-scan-4x-n2', target: 'local', fromCleaned: false, folder: '/scans/denoised',
      pages: [{ pageIndex: 0, exists: true, taken: false, current: true, fromCleaned: false }] },
    { created: 1780000000, preset: null, target: null, fromCleaned: null, folder: '/scans/old',
      pages: [{ pageIndex: 0, exists: false, taken: false, current: false, fromCleaned: null }] },
  ]

  it('says so in its sub-line, and says when its pages were replaced', () => {
    const facts = (chapter) => [.../** @type {HTMLElement} */ (mount(chapter).container.querySelector('.facts')).children]
      .map((fact) => fact.textContent)
    expect(facts(CHAPTER)).not.toContain(t('home.chapter.denoised'))
    cleanup()
    expect(facts({ ...CHAPTER, denoiseHistory: { runs: 1, latest: 1, taken: false } })).toContain(t('home.chapter.denoised'))
    cleanup()
    expect(facts({ ...CHAPTER, denoiseHistory: { runs: 1, latest: 1, taken: true } })).toContain(t('home.chapter.denoisedTaken'))
  })

  it('has Denoise history in its menu, which opens the compare view on the newest run', async () => {
    const denoiseHistory = vi.fn().mockResolvedValue(RUNS)
    setBackend(/** @type {any} */ ({ denoiseHistory }))
    const chapter = { ...CHAPTER, denoiseHistory: { runs: 2, latest: 1790000000, taken: false } }
    expect(chapterMenuItems(chapter).map((item) => item.id)).toEqual(['denoise', 'denoiseHistory', 'sep-1', 'delete'])
    const { row, getByRole, container } = mount(chapter)
    expect(container.querySelector('.chip'), 'no separate control beside the row').toBeNull()
    row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 40, clientY: 12 }))
    await vi.waitFor(() => getByRole('menu'))
    await fireEvent.click(getByRole('menuitem', { name: t('home.action.denoiseHistory') }))
    await vi.waitFor(() => expect(pushModal).toHaveBeenCalledWith(expect.objectContaining({
      kind: 'denoiseCompare',
      props: { projectId: PROJECT.id, chapter, runs: RUNS, run: 1790000000 },
    })))
    expect(denoiseHistory).toHaveBeenCalledWith({ chapterId: CHAPTER.id })
  })
})

describe('a chapter with a job running on it', () => {
  afterEach(() => {
    resetJobs()
    setBackend(null)
  })

  it('leads its sub-line with what the job is doing and how far it is', async () => {
    setBackend(/** @type {any} */ ({ subscribe: () => () => {}, listJobs: async () => [], listProjects: async () => [] }))
    const { row } = mount()
    expect(row.querySelector('[data-running]')).toBeNull()

    registerJob({ runId: 'den-1', kind: 'denoise', chapterId: CHAPTER.id, total: 2, done: 1 })
    await vi.waitFor(() => expect(row.querySelector('[data-running]')?.textContent)
      .toContain(t('jobs.row.progress', { kindKey: 'jobs.active.denoise', page: 2, total: 2 })))
    expect(row.querySelector('[data-running]')?.getAttribute('data-running')).toBe('denoise')
    // Part of the row's button, so its name says it too.
    expect(row.querySelector('button.hit')?.textContent).toContain(t('jobs.active.denoise'))
  })
})
