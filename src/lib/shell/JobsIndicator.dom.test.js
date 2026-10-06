/**
 * The Jobs button and its panel: hidden with nothing to show, the running
 * count on the button, and per job its name, progress, Stop, Open and
 * Dismiss.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { app, goLibrary } from '../state/app.svelte.js'
import { jobById, registerJob, resetJobs } from '../state/jobs.svelte.js'
import JobsIndicator from './JobsIndicator.svelte'

function fakeBackend() {
  const handlers = new Set()
  return {
    emit: (/** @type {any} */ event) => [...handlers].forEach((handler) => handler(event)),
    subscribe: (/** @type {any} */ handler) => {
      handlers.add(handler)
      return () => handlers.delete(handler)
    },
    onDenoiseProgress: async () => () => {},
    onQuitRequested: async () => () => {},
    listJobs: vi.fn(async () => []),
    listProjects: vi.fn(async () => []),
    cancelRun: vi.fn(async ({ runId }) => runId),
    cancelDenoiseLocal: vi.fn(async () => true),
  }
}

/** @type {ReturnType<typeof fakeBackend>} */
let backend

const NAMES = { projectId: 'p1', projectName: 'Tsuki to Hane', chapterName: 'Feather Weight', chapterNumber: 107 }

beforeEach(() => {
  resetJobs()
  backend = fakeBackend()
  setBackend(/** @type {any} */ (backend))
  goLibrary()
})

afterEach(() => {
  cleanup()
  resetJobs()
  setBackend(null)
})

describe('the Jobs button', () => {
  it('draws nothing while there are no jobs', () => {
    const { container } = render(JobsIndicator)
    expect(container.querySelector('[data-jobs-trigger]')).toBeNull()
  })

  it('counts what is running, and lists each job with its progress and Stop', async () => {
    registerJob({ runId: 'run-1', kind: 'detect', chapterId: 'c1', total: 4, ...NAMES })
    registerJob({ runId: 'den-1', kind: 'cloudDenoise', chapterId: 'c2', total: 10, ...NAMES, chapterName: 'Small Hours' })
    const { container, getByRole, getAllByRole } = render(JobsIndicator)

    const trigger = getByRole('button', { name: t('jobs.indicator.label', { count: 2 }) })
    expect(trigger.textContent).toContain(t('jobs.indicator.running', { count: 2 }))
    expect(trigger.getAttribute('aria-expanded')).toBe('false')
    // The count is announced from a live region.
    expect(container.querySelector('[aria-live="polite"]')?.textContent).toBe(t('jobs.indicator.label', { count: 2 }))

    await fireEvent.click(trigger)
    const panel = getByRole('dialog', { name: t('jobs.title') })
    expect(panel.textContent).toContain(t('jobs.item.title', { project: 'Tsuki to Hane', chapter: 'Feather Weight' }))
    expect(panel.textContent).toContain(t('jobs.kind.cloudDenoise'))
    expect(getAllByRole('progressbar')).toHaveLength(2)

    backend.emit({ type: 'page-done', runId: 'run-1', chapterId: 'c1', pageIndex: 0, page: {} })
    await waitFor(() => expect(panel.querySelector('[data-job="run-1"] [role="progressbar"]')?.getAttribute('aria-valuenow')).toBe('25'))
    expect(panel.querySelector('[data-job="run-1"]')?.textContent).toContain(t('jobs.status.page', { page: 2, total: 4 }))

    const stop = /** @type {HTMLElement} */ (panel.querySelector('[data-job="run-1"] [data-action="stop"]'))
    await fireEvent.click(stop)
    expect(backend.cancelRun).toHaveBeenCalledWith({ runId: 'run-1' })
    await waitFor(() => expect(stop.getAttribute('aria-disabled')).toBe('true'))
    // Stopping one job leaves the panel up for the next.
    expect(getByRole('dialog', { name: t('jobs.title') })).toBeTruthy()
  })

  it('says a finished job ended, and Dismiss takes it off', async () => {
    registerJob({ runId: 'run-1', kind: 'clean', chapterId: 'c1', total: 2, ...NAMES })
    backend.emit({ type: 'run-finished', runId: 'run-1', chapterId: 'c1', reason: 'completed', pagesQueued: 2, pagesCleaned: 2, regionsCleaned: 3, nextPageIndex: null })
    const { container, getByRole } = render(JobsIndicator, { props: { variant: 'editor' } })

    const trigger = getByRole('button', { name: t('jobs.indicator.label', { count: 0 }) })
    expect(trigger.textContent).toContain(t('jobs.indicator.finished', { count: 1 }))
    await fireEvent.click(trigger)
    const row = /** @type {HTMLElement} */ (container.querySelector('[data-job="run-1"]'))
    expect(row.getAttribute('data-status')).toBe('completed')
    expect(row.textContent).toContain(t('jobs.status.completed'))
    expect(row.querySelector('[role="progressbar"]')).toBeNull()

    await fireEvent.click(/** @type {HTMLElement} */ (row.querySelector('[data-action="dismiss"]')))
    expect(jobById('run-1')).toBeNull()
    await waitFor(() => expect(container.querySelector('[data-jobs-trigger]')).toBeNull())
  })

  it('opens the job\'s chapter, and offers no Open for the chapter already in the editor', async () => {
    registerJob({ runId: 'run-1', kind: 'clean', chapterId: 'c1', total: 2, ...NAMES })
    const { getByRole, queryByRole } = render(JobsIndicator)
    await fireEvent.click(getByRole('button', { name: t('jobs.indicator.label', { count: 1 }) }))
    const title = t('jobs.item.title', { project: 'Tsuki to Hane', chapter: 'Feather Weight' })
    await fireEvent.click(getByRole('button', { name: t('jobs.action.openLabel', { chapter: title }) }))
    await waitFor(() => expect(app.route).toMatchObject({ name: 'editor', projectId: 'p1', chapterId: 'c1' }))
    expect(queryByRole('button', { name: t('jobs.action.openLabel', { chapter: title }) })).toBeNull()
  })
})
